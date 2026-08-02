//! Stats server client + offline spool uploader (PLAN.md §6.4).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use game_mgr_api_types::{
    BatchResponse, EventsBatchRequest, GameDefinition, GameDto, MeResponse, ProfileDto,
    RegisterMachineRequest, SessionDto, SessionsBatchRequest, UpsertGameRequest,
};
use uuid::Uuid;

use crate::oidc::TokenProvider;
use crate::statedb::StateDb;

pub struct ServerClient {
    base: reqwest::Url,
    http: reqwest::Client,
    token: Arc<dyn TokenProvider>,
}

impl ServerClient {
    pub fn new(base_url: &str, token: Arc<dyn TokenProvider>) -> Result<Self> {
        Ok(Self {
            base: reqwest::Url::parse(base_url).context("parsing server_url")?,
            http: reqwest::Client::new(),
            token,
        })
    }

    fn url(&self, path: &str) -> Result<reqwest::Url> {
        Ok(self.base.join(path)?)
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&impl serde::Serialize>,
    ) -> Result<T> {
        let bearer = self.token.bearer().await?;
        let mut request = self
            .http
            .request(method, self.url(path)?)
            .bearer_auth(bearer);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("server {path} answered {status}: {text}");
        }
        Ok(response.json().await?)
    }

    pub async fn me(&self) -> Result<MeResponse> {
        self.request(reqwest::Method::GET, "api/v1/me", None::<&()>)
            .await
    }

    /// Server build version from `/api/v1/ping` — used to surface
    /// client/server skew (a stale server image makes newer endpoints 404).
    pub async fn server_version(&self) -> Result<Option<String>> {
        let response: serde_json::Value = self
            .request(reqwest::Method::GET, "api/v1/ping", None::<&()>)
            .await?;
        Ok(response
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string))
    }

    pub async fn create_profile(&self, name: &str) -> Result<ProfileDto> {
        self.request(
            reqwest::Method::POST,
            "api/v1/profiles",
            Some(&serde_json::json!({ "name": name })),
        )
        .await
    }

    pub async fn delete_profile(&self, id: Uuid) -> Result<()> {
        let bearer = self.token.bearer().await?;
        let response = self
            .http
            .delete(self.url(&format!("api/v1/profiles/{id}"))?)
            .bearer_auth(bearer)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("delete profile answered {status}: {text}");
        }
        Ok(())
    }

    /// The full game catalog (definitions + aggregates) — the client's
    /// source of truth for which games exist.
    pub async fn games(&self) -> Result<Vec<GameDto>> {
        self.request(reqwest::Method::GET, "api/v1/games", None::<&()>)
            .await
    }

    /// Create or update a game definition (Add Game UI).
    pub async fn upsert_game(&self, id: &str, req: &UpsertGameRequest) -> Result<GameDefinition> {
        self.request(
            reqwest::Method::PUT,
            &format!("api/v1/games/{id}"),
            Some(req),
        )
        .await
    }

    pub async fn register_machine(
        &self,
        machine_id: Uuid,
        req: &RegisterMachineRequest,
    ) -> Result<()> {
        let _: serde_json::Value = self
            .request(
                reqwest::Method::PUT,
                &format!("api/v1/machines/{machine_id}"),
                Some(req),
            )
            .await?;
        Ok(())
    }

    pub async fn sessions_batch(&self, sessions: Vec<SessionDto>) -> Result<BatchResponse> {
        self.request(
            reqwest::Method::POST,
            "api/v1/sessions:batch",
            Some(&SessionsBatchRequest { sessions }),
        )
        .await
    }

    pub async fn events_batch(
        &self,
        events: Vec<game_mgr_api_types::EventDto>,
    ) -> Result<BatchResponse> {
        self.request(
            reqwest::Method::POST,
            "api/v1/events:batch",
            Some(&EventsBatchRequest { events }),
        )
        .await
    }

    pub async fn recent_sessions(&self, limit: usize) -> Result<Vec<SessionDto>> {
        self.request(
            reqwest::Method::GET,
            &format!("api/v1/sessions?limit={limit}"),
            None::<&()>,
        )
        .await
    }

    /// Install-state reporting. The server endpoint lands in M3 — failures
    /// are logged, never propagated, so the client doesn't depend on it.
    pub async fn report_install(
        &self,
        machine_id: Uuid,
        game_id: &str,
        version: &str,
        state: &str,
        proton: Option<&str>,
    ) {
        let result: Result<serde_json::Value> = self
            .request(
                reqwest::Method::PUT,
                &format!("api/v1/machines/{machine_id}/installs/{game_id}"),
                Some(&serde_json::json!({
                    "version": version, "state": state, "proton": proton
                })),
            )
            .await;
        if let Err(err) = result {
            tracing::debug!(%err, "install-state report skipped");
        }
    }
}

/// Drain result for one uploader pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainStats {
    pub sessions_uploaded: usize,
    pub sessions_rejected: usize,
    pub events_uploaded: usize,
    pub events_rejected: usize,
}

const BATCH_LIMIT: usize = 200;

/// One spool drain pass: upload finished sessions/events, mark outcomes.
/// Server idempotency (client UUIDs) makes retries safe; rejected items are
/// marked so they never retry forever.
pub async fn drain_spool(db: &StateDb, server: &ServerClient) -> Result<DrainStats> {
    let mut stats = DrainStats::default();

    let sessions = db.sessions_pending(BATCH_LIMIT).await?;
    if !sessions.is_empty() {
        let ids: Vec<Uuid> = sessions.iter().map(|s| s.id).collect();
        let response = server.sessions_batch(sessions).await?;
        let rejected: Vec<Uuid> = response.errors.iter().map(|e| e.id).collect();
        let ok: Vec<Uuid> = ids
            .iter()
            .copied()
            .filter(|id| !rejected.contains(id))
            .collect();
        stats.sessions_uploaded = ok.len();
        stats.sessions_rejected = rejected.len();
        for error in &response.errors {
            tracing::warn!(id = %error.id, reason = %error.reason, "session rejected by server");
        }
        db.sessions_mark(ok, 1).await?;
        db.sessions_mark(rejected, 2).await?;
    }

    let events = db.events_pending(BATCH_LIMIT).await?;
    if !events.is_empty() {
        let ids: Vec<Uuid> = events.iter().map(|e| e.client_event_id).collect();
        let response = server.events_batch(events).await?;
        let rejected: Vec<Uuid> = response.errors.iter().map(|e| e.id).collect();
        let ok: Vec<Uuid> = ids
            .iter()
            .copied()
            .filter(|id| !rejected.contains(id))
            .collect();
        stats.events_uploaded = ok.len();
        stats.events_rejected = rejected.len();
        db.events_mark(ok, 1).await?;
        db.events_mark(rejected, 2).await?;
    }

    Ok(stats)
}

/// Background uploader: periodic drain + on-demand pokes; survives server
/// outages (items stay pending until a pass succeeds).
pub fn spawn_uploader(
    db: StateDb,
    server: Arc<ServerClient>,
    mut poke: tokio::sync::mpsc::UnboundedReceiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                poked = poke.recv() => {
                    if poked.is_none() {
                        return; // core gone
                    }
                }
            }
            match drain_spool(&db, &server).await {
                Ok(stats) if stats != DrainStats::default() => {
                    tracing::info!(?stats, "spool drained");
                }
                Ok(_) => {}
                Err(err) => tracing::debug!(%err, "spool drain failed; will retry"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oidc::StaticToken;
    use axum::{Json, Router, routing::post};
    use game_mgr_api_types::{BatchItemError, SessionEndReason};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use time::OffsetDateTime;

    struct StubState {
        sessions_calls: AtomicUsize,
        reject_first_id: Mutex<Option<Uuid>>,
        fail: AtomicBool,
        seen_bearer: Mutex<Option<String>>,
    }

    async fn stub_server(state: Arc<StubState>) -> String {
        let sessions_state = state.clone();
        let events_state = state.clone();
        let app = Router::new()
            .route(
                "/api/v1/sessions:batch",
                post(
                    move |headers: axum::http::HeaderMap, Json(req): Json<SessionsBatchRequest>| {
                        let state = sessions_state.clone();
                        async move {
                            if state.fail.load(Ordering::SeqCst) {
                                return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                            }
                            *state.seen_bearer.lock().unwrap() = headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(String::from);
                            state.sessions_calls.fetch_add(1, Ordering::SeqCst);
                            let errors = state
                                .reject_first_id
                                .lock()
                                .unwrap()
                                .take()
                                .into_iter()
                                .map(|id| BatchItemError {
                                    id,
                                    reason: "unknown game".into(),
                                })
                                .collect::<Vec<_>>();
                            let inserted = req.sessions.len() as u32 - errors.len() as u32;
                            Ok(Json(BatchResponse {
                                inserted,
                                duplicates: 0,
                                errors,
                            }))
                        }
                    },
                ),
            )
            .route(
                "/api/v1/events:batch",
                post(move |Json(req): Json<EventsBatchRequest>| {
                    let state = events_state.clone();
                    async move {
                        if state.fail.load(Ordering::SeqCst) {
                            return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                        }
                        Ok(Json(BatchResponse {
                            inserted: req.events.len() as u32,
                            duplicates: 0,
                            errors: vec![],
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/")
    }

    async fn seeded_db() -> (StateDb, Uuid, Uuid) {
        let db = StateDb::open_in_memory().unwrap();
        let (good, bad) = (Uuid::new_v4(), Uuid::new_v4());
        let start = OffsetDateTime::now_utc() - time::Duration::hours(2);
        for (id, game) in [(good, "bg3"), (bad, "nope")] {
            db.session_start(id, game, Uuid::new_v4(), Uuid::new_v4(), start)
                .await
                .unwrap();
            db.session_finish(
                id,
                start + time::Duration::hours(1),
                Some(0),
                SessionEndReason::Exited,
            )
            .await
            .unwrap();
        }
        db.event_record(Uuid::new_v4(), None, None, "launch", serde_json::json!({}))
            .await
            .unwrap();
        (db, good, bad)
    }

    #[tokio::test]
    async fn drain_uploads_and_marks_rejections() {
        let state = Arc::new(StubState {
            sessions_calls: AtomicUsize::new(0),
            reject_first_id: Mutex::new(None),
            fail: AtomicBool::new(false),
            seen_bearer: Mutex::new(None),
        });
        let base = stub_server(state.clone()).await;
        let (db, _good, bad) = seeded_db().await;
        *state.reject_first_id.lock().unwrap() = Some(bad);

        let server = ServerClient::new(&base, Arc::new(StaticToken("tok".into()))).unwrap();
        let stats = drain_spool(&db, &server).await.unwrap();
        assert_eq!(stats.sessions_uploaded, 1);
        assert_eq!(stats.sessions_rejected, 1);
        assert_eq!(stats.events_uploaded, 1);
        assert_eq!(
            state.seen_bearer.lock().unwrap().as_deref(),
            Some("Bearer tok"),
            "token provider output must reach the wire"
        );

        // nothing left after the pass — uploaded AND rejected are settled
        let stats = drain_spool(&db, &server).await.unwrap();
        assert_eq!(stats, DrainStats::default());
        assert_eq!(state.sessions_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn outage_keeps_items_pending_until_recovery() {
        let state = Arc::new(StubState {
            sessions_calls: AtomicUsize::new(0),
            reject_first_id: Mutex::new(None),
            fail: AtomicBool::new(true),
            seen_bearer: Mutex::new(None),
        });
        let base = stub_server(state.clone()).await;
        let (db, _, _) = seeded_db().await;
        let server = ServerClient::new(&base, Arc::new(StaticToken("tok".into()))).unwrap();

        assert!(drain_spool(&db, &server).await.is_err());
        assert_eq!(
            db.sessions_pending(10).await.unwrap().len(),
            2,
            "still spooled"
        );

        state.fail.store(false, Ordering::SeqCst);
        let stats = drain_spool(&db, &server).await.unwrap();
        assert_eq!(stats.sessions_uploaded, 2);
        assert!(db.sessions_pending(10).await.unwrap().is_empty());
    }
}
