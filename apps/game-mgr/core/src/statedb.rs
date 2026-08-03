//! Local state: SQLite (rusqlite, WAL) behind a dedicated actor thread with
//! an async facade (PLAN.md §4.5). Holds install/step state, the offline
//! stats spool and a small kv store.

use std::path::Path;

use anyhow::{Context, Result};
use game_mgr_api_types::{SessionDto, SessionEndReason};
use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use tokio::sync::oneshot;
use uuid::Uuid;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct StateDb {
    tx: std::sync::mpsc::Sender<Job>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS installs (
  game_id TEXT PRIMARY KEY,
  version TEXT NOT NULL,
  state   TEXT NOT NULL,
  error   TEXT,
  proton_override TEXT,
  options TEXT,
  chosen_exe TEXT,
  launch_opts TEXT
);
CREATE TABLE IF NOT EXISTS install_steps (
  game_id TEXT NOT NULL,
  version TEXT NOT NULL,
  step_id TEXT NOT NULL,
  status  TEXT NOT NULL,
  error   TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (game_id, version, step_id)
);
CREATE TABLE IF NOT EXISTS sessions_spool (
  id TEXT PRIMARY KEY,
  game_id TEXT NOT NULL,
  profile_id TEXT NOT NULL,
  machine_id TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  duration_s INTEGER NOT NULL DEFAULT 0,
  exit_code INTEGER,
  end_reason TEXT NOT NULL DEFAULT 'exited',
  last_tick INTEGER NOT NULL,
  uploaded INTEGER NOT NULL DEFAULT 0   -- 0 pending, 1 uploaded, 2 rejected
);
CREATE TABLE IF NOT EXISTS events_spool (
  client_event_id TEXT PRIMARY KEY,
  machine_id TEXT NOT NULL,
  profile_id TEXT,
  game_id TEXT,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL DEFAULT '{}',
  occurred_at INTEGER NOT NULL,
  uploaded INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS kv (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

/// Local install lifecycle (mirrors the server's `installs.state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    Installing,
    Installed,
    ManualWait,
    Failed,
    Outdated,
}

impl InstallState {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallState::Installing => "installing",
            InstallState::Installed => "installed",
            InstallState::ManualWait => "manual_wait",
            InstallState::Failed => "failed",
            InstallState::Outdated => "outdated",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "installing" => InstallState::Installing,
            "installed" => InstallState::Installed,
            "manual_wait" => InstallState::ManualWait,
            "outdated" => InstallState::Outdated,
            _ => InstallState::Failed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallRow {
    pub game_id: String,
    pub version: String,
    pub state: InstallState,
    pub error: Option<String>,
    pub proton_override: Option<String>,
    /// JSON-serialized [`crate::game::InstallOptions`] chosen at install
    /// time, reused by updates/reinstalls.
    pub options: Option<String>,
    /// Executable (relative to the extracted tree) the user picked at first
    /// launch when the definition left it blank.
    pub chosen_exe: Option<String>,
    /// JSON-serialized [`crate::game::LaunchOpts`] (MangoHud/Gamescope/favourites).
    pub launch_opts: Option<String>,
}

impl StateDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path).context("opening state.db")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        // dev databases created before these columns existed
        let _ = conn.execute("ALTER TABLE installs ADD COLUMN options TEXT", []);
        let _ = conn.execute("ALTER TABLE installs ADD COLUMN chosen_exe TEXT", []);
        let _ = conn.execute("ALTER TABLE installs ADD COLUMN launch_opts TEXT", []);

        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("gm-statedb".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .context("spawning statedb thread")?;
        Ok(Self { tx })
    }

    /// In-memory database for tests.
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                job(&mut conn);
            }
        });
        Ok(Self { tx })
    }

    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let (otx, orx) = oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = otx.send(f(conn));
            }))
            .map_err(|_| anyhow::anyhow!("statedb thread is gone"))?;
        Ok(orx.await.context("statedb job dropped")??)
    }

    // ----- kv -----

    pub async fn kv_get(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_string();
        self.call(move |conn| {
            conn.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
        })
        .await
    }

    pub async fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_string(), value.to_string());
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
        .await
    }

    // ----- installs -----

    pub async fn install_get(&self, game_id: &str) -> Result<Option<InstallRow>> {
        let game_id = game_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                "SELECT game_id, version, state, error, proton_override, options, chosen_exe, \
                 launch_opts
                 FROM installs WHERE game_id = ?1",
                params![game_id],
                |r| {
                    Ok(InstallRow {
                        game_id: r.get(0)?,
                        version: r.get(1)?,
                        state: InstallState::parse(&r.get::<_, String>(2)?),
                        error: r.get(3)?,
                        proton_override: r.get(4)?,
                        options: r.get(5)?,
                        chosen_exe: r.get(6)?,
                        launch_opts: r.get(7)?,
                    })
                },
            )
            .optional()
        })
        .await
    }

    /// Remember the optional-group selection for this install (JSON).
    pub async fn install_options_set(&self, game_id: &str, options: String) -> Result<()> {
        let game_id = game_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE installs SET options = ?2 WHERE game_id = ?1",
                params![game_id, options],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn install_set(
        &self,
        game_id: &str,
        version: &str,
        state: InstallState,
        error: Option<String>,
    ) -> Result<()> {
        let (game_id, version, state) = (game_id.to_string(), version.to_string(), state);
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO installs (game_id, version, state, error)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (game_id) DO UPDATE SET
                   version = excluded.version, state = excluded.state, error = excluded.error",
                params![game_id, version, state.as_str(), error],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn install_remove(&self, game_id: &str) -> Result<()> {
        let game_id = game_id.to_string();
        self.call(move |conn| {
            conn.execute("DELETE FROM installs WHERE game_id = ?1", params![game_id])?;
            conn.execute(
                "DELETE FROM install_steps WHERE game_id = ?1",
                params![game_id],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn proton_override_set(&self, game_id: &str, value: Option<String>) -> Result<()> {
        let game_id = game_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE installs SET proton_override = ?2 WHERE game_id = ?1",
                params![game_id, value],
            )
            .map(|_| ())
        })
        .await
    }

    /// Remember the executable the user picked at first launch.
    pub async fn install_chosen_exe_set(&self, game_id: &str, value: Option<String>) -> Result<()> {
        let game_id = game_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE installs SET chosen_exe = ?2 WHERE game_id = ?1",
                params![game_id, value],
            )
            .map(|_| ())
        })
        .await
    }

    /// Persist the per-game launch settings (JSON [`crate::game::LaunchOpts`]).
    pub async fn install_launch_opts_set(&self, game_id: &str, value: String) -> Result<()> {
        let game_id = game_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE installs SET launch_opts = ?2 WHERE game_id = ?1",
                params![game_id, value],
            )
            .map(|_| ())
        })
        .await
    }

    // ----- install step status -----

    pub async fn step_mark(
        &self,
        game_id: &str,
        version: &str,
        step_id: &str,
        status: &str,
        error: Option<String>,
    ) -> Result<()> {
        let (game_id, version, step_id, status) = (
            game_id.to_string(),
            version.to_string(),
            step_id.to_string(),
            status.to_string(),
        );
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO install_steps (game_id, version, step_id, status, error, attempts, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
                 ON CONFLICT (game_id, version, step_id) DO UPDATE SET
                   status = excluded.status, error = excluded.error,
                   attempts = install_steps.attempts + 1, updated_at = excluded.updated_at",
                params![game_id, version, step_id, status, error, now_unix()],
            )
            .map(|_| ())
        })
        .await
    }

    // ----- session spool -----

    pub async fn session_start(
        &self,
        id: Uuid,
        game_id: &str,
        profile_id: Uuid,
        machine_id: Uuid,
        started_at: OffsetDateTime,
    ) -> Result<()> {
        let game_id = game_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO sessions_spool
                   (id, game_id, profile_id, machine_id, started_at, last_tick)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    id.to_string(),
                    game_id,
                    profile_id.to_string(),
                    machine_id.to_string(),
                    started_at.unix_timestamp()
                ],
            )
            .map(|_| ())
        })
        .await
    }

    /// Crash insurance: bump `last_tick` periodically while playing.
    pub async fn session_tick(&self, id: Uuid, at: OffsetDateTime) -> Result<()> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE sessions_spool SET last_tick = ?2 WHERE id = ?1",
                params![id.to_string(), at.unix_timestamp()],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn session_finish(
        &self,
        id: Uuid,
        ended_at: OffsetDateTime,
        exit_code: Option<i32>,
        end_reason: SessionEndReason,
    ) -> Result<()> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE sessions_spool SET
                   ended_at = ?2,
                   duration_s = MAX(0, ?2 - started_at),
                   exit_code = ?3,
                   end_reason = ?4
                 WHERE id = ?1",
                params![
                    id.to_string(),
                    ended_at.unix_timestamp(),
                    exit_code,
                    end_reason.as_str()
                ],
            )
            .map(|_| ())
        })
        .await
    }

    /// Close sessions left open by a crash at their last tick (PLAN.md §6.4).
    pub async fn sessions_recover(&self) -> Result<usize> {
        self.call(|conn| {
            conn.execute(
                "UPDATE sessions_spool SET
                   ended_at = last_tick,
                   duration_s = MAX(0, last_tick - started_at),
                   end_reason = 'recovered'
                 WHERE ended_at IS NULL",
                [],
            )
        })
        .await
    }

    /// Finished, not-yet-uploaded sessions as wire DTOs.
    pub async fn sessions_pending(&self, limit: usize) -> Result<Vec<SessionDto>> {
        self.call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, game_id, profile_id, machine_id, started_at, ended_at,
                        duration_s, exit_code, end_reason
                 FROM sessions_spool
                 WHERE uploaded = 0 AND ended_at IS NOT NULL
                 ORDER BY started_at LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| {
                Ok(SessionDto {
                    id: parse_uuid(r.get::<_, String>(0)?),
                    game_id: r.get(1)?,
                    profile_id: parse_uuid(r.get::<_, String>(2)?),
                    machine_id: parse_uuid(r.get::<_, String>(3)?),
                    started_at: from_unix(r.get(4)?),
                    ended_at: from_unix(r.get(5)?),
                    duration_s: r.get(6)?,
                    exit_code: r.get(7)?,
                    end_reason: SessionEndReason::parse(&r.get::<_, String>(8)?),
                })
            })?;
            rows.collect()
        })
        .await
    }

    pub async fn sessions_mark(&self, ids: Vec<Uuid>, uploaded: i64) -> Result<()> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            for id in ids {
                tx.execute(
                    "UPDATE sessions_spool SET uploaded = ?2 WHERE id = ?1",
                    params![id.to_string(), uploaded],
                )?;
            }
            tx.commit()
        })
        .await
    }

    // ----- event spool -----

    pub async fn event_record(
        &self,
        machine_id: Uuid,
        profile_id: Option<Uuid>,
        game_id: Option<String>,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let kind = kind.to_string();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO events_spool
                   (client_event_id, machine_id, profile_id, game_id, kind, payload, occurred_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id.to_string(),
                    machine_id.to_string(),
                    profile_id.map(|p| p.to_string()),
                    game_id,
                    kind,
                    payload.to_string(),
                    now_unix()
                ],
            )
            .map(|_| id)
        })
        .await
    }

    pub async fn events_pending(&self, limit: usize) -> Result<Vec<game_mgr_api_types::EventDto>> {
        self.call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT client_event_id, machine_id, profile_id, game_id, kind, payload, occurred_at
                 FROM events_spool WHERE uploaded = 0 ORDER BY occurred_at LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| {
                Ok(game_mgr_api_types::EventDto {
                    client_event_id: parse_uuid(r.get::<_, String>(0)?),
                    machine_id: parse_uuid(r.get::<_, String>(1)?),
                    profile_id: r.get::<_, Option<String>>(2)?.map(parse_uuid),
                    game_id: r.get(3)?,
                    kind: r.get(4)?,
                    payload: serde_json::from_str(&r.get::<_, String>(5)?)
                        .unwrap_or(serde_json::Value::Null),
                    occurred_at: from_unix(r.get(6)?),
                })
            })?;
            rows.collect()
        })
        .await
    }

    pub async fn events_mark(&self, ids: Vec<Uuid>, uploaded: i64) -> Result<()> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            for id in ids {
                tx.execute(
                    "UPDATE events_spool SET uploaded = ?2 WHERE client_event_id = ?1",
                    params![id.to_string(), uploaded],
                )?;
            }
            tx.commit()
        })
        .await
    }
}

fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn from_unix(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn parse_uuid(s: String) -> Uuid {
    s.parse().unwrap_or(Uuid::nil())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kv_roundtrip() {
        let db = StateDb::open_in_memory().unwrap();
        assert_eq!(db.kv_get("machine_id").await.unwrap(), None);
        db.kv_set("machine_id", "abc").await.unwrap();
        db.kv_set("machine_id", "def").await.unwrap();
        assert_eq!(
            db.kv_get("machine_id").await.unwrap().as_deref(),
            Some("def")
        );
    }

    #[tokio::test]
    async fn install_state_lifecycle() {
        let db = StateDb::open_in_memory().unwrap();
        db.install_set("bg3", "1.0.0", InstallState::Installing, None)
            .await
            .unwrap();
        db.install_set("bg3", "1.0.0", InstallState::Installed, None)
            .await
            .unwrap();
        let row = db.install_get("bg3").await.unwrap().unwrap();
        assert_eq!(row.state, InstallState::Installed);

        db.proton_override_set("bg3", Some("GE-Proton9-20".into()))
            .await
            .unwrap();
        let row = db.install_get("bg3").await.unwrap().unwrap();
        assert_eq!(row.proton_override.as_deref(), Some("GE-Proton9-20"));

        db.install_remove("bg3").await.unwrap();
        assert!(db.install_get("bg3").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn session_spool_finish_and_drain() {
        let db = StateDb::open_in_memory().unwrap();
        let (sid, profile, machine) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let start = OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();

        db.session_start(sid, "bg3", profile, machine, start)
            .await
            .unwrap();
        // unfinished sessions are not pending
        assert!(db.sessions_pending(10).await.unwrap().is_empty());

        db.session_finish(
            sid,
            start + time::Duration::hours(1),
            Some(0),
            SessionEndReason::Exited,
        )
        .await
        .unwrap();
        let pending = db.sessions_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].duration_s, 3600);
        assert_eq!(pending[0].exit_code, Some(0));

        db.sessions_mark(vec![sid], 1).await.unwrap();
        assert!(db.sessions_pending(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn crash_recovery_closes_at_last_tick() {
        let db = StateDb::open_in_memory().unwrap();
        let sid = Uuid::new_v4();
        let start = OffsetDateTime::from_unix_timestamp(2_000_000).unwrap();
        db.session_start(sid, "bg3", Uuid::new_v4(), Uuid::new_v4(), start)
            .await
            .unwrap();
        db.session_tick(sid, start + time::Duration::minutes(5))
            .await
            .unwrap();

        assert_eq!(db.sessions_recover().await.unwrap(), 1);
        let pending = db.sessions_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].duration_s, 300);
        assert_eq!(pending[0].end_reason, SessionEndReason::Recovered);
        assert_eq!(pending[0].exit_code, None);
        // recovery is idempotent
        assert_eq!(db.sessions_recover().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn event_spool_roundtrip() {
        let db = StateDb::open_in_memory().unwrap();
        let machine = Uuid::new_v4();
        let id = db
            .event_record(
                machine,
                None,
                Some("bg3".into()),
                "launch",
                serde_json::json!({"a":1}),
            )
            .await
            .unwrap();
        let pending = db.events_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, "launch");
        assert_eq!(pending[0].payload["a"], 1);
        db.events_mark(vec![id], 2).await.unwrap(); // rejected
        assert!(db.events_pending(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn step_marks_accumulate_attempts() {
        let db = StateDb::open_in_memory().unwrap();
        db.step_mark("bg3", "1.0.0", "fetch", "running", None)
            .await
            .unwrap();
        db.step_mark("bg3", "1.0.0", "fetch", "failed", Some("boom".into()))
            .await
            .unwrap();
        let attempts: i64 = db
            .call(|conn| {
                conn.query_row(
                    "SELECT attempts FROM install_steps WHERE step_id = 'fetch'",
                    [],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(attempts, 2);
    }
}
