//! Playtime watcher (PLAN.md §6.3): own the spawned child for exit-code
//! capture, then keep the session alive while the process tree / hint
//! matches survive (Wine and launchers detach). Sessions end after a grace
//! period with no matches.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use game_mgr_api_types::SessionEndReason;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use time::OffsetDateTime;

use crate::game::{WatchHint, WatchScope, WatcherSpec};

#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub started_at: OffsetDateTime,
    pub ended_at: OffsetDateTime,
    pub exit_code: Option<i32>,
    pub end_reason: SessionEndReason,
}

/// One refresh of the process table, narrowed to what matching needs.
/// Abstracted so the matcher logic is unit-testable without real processes.
pub trait ProcessSnapshot {
    fn processes(&self) -> Vec<ProcessInfo>;
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub cmdline: String,
    pub exe: Option<std::path::PathBuf>,
}

pub struct SysinfoSnapshot {
    system: System,
}

impl SysinfoSnapshot {
    pub fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    pub fn refresh(&mut self) {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::Always),
        );
    }
}

impl Default for SysinfoSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSnapshot for SysinfoSnapshot {
    fn processes(&self) -> Vec<ProcessInfo> {
        self.system
            .processes()
            .iter()
            .map(|(pid, p)| ProcessInfo {
                pid: pid.as_u32(),
                parent: p.parent().map(Pid::as_u32),
                name: p.name().to_string_lossy().into_owned(),
                cmdline: p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                exe: p.exe().map(Path::to_path_buf),
            })
            .collect()
    }
}

fn hint_matches(info: &ProcessInfo, hint: &WatchHint, scope: &WatchScope) -> bool {
    if let Some(root) = &scope.under_path {
        let in_scope = info
            .exe
            .as_deref()
            .map(|exe| exe.starts_with(root))
            .unwrap_or(false)
            // Wine processes often report the unix wine binary as exe but
            // carry the windows path in the cmdline.
            || info.cmdline.contains(&*root.to_string_lossy());
        if !in_scope {
            return false;
        }
    }
    match hint {
        WatchHint::ExeNames(names) => names.iter().any(|wanted| {
            info.name.eq_ignore_ascii_case(wanted)
                || info
                    .exe
                    .as_deref()
                    .and_then(Path::file_name)
                    .map(|f| f.to_string_lossy().eq_ignore_ascii_case(wanted))
                    .unwrap_or(false)
        }),
        WatchHint::CmdlineContains(needle) => info.cmdline.contains(needle.as_str()),
    }
}

/// Pids that are `root` or any descendant of it.
fn descendants(processes: &[ProcessInfo], root: u32) -> HashSet<u32> {
    let mut tree: HashSet<u32> = HashSet::from([root]);
    // iterate until fixpoint (handles arbitrary ordering)
    loop {
        let before = tree.len();
        for p in processes {
            if let Some(parent) = p.parent
                && tree.contains(&parent)
            {
                tree.insert(p.pid);
            }
        }
        if tree.len() == before {
            return tree;
        }
    }
}

/// Decide which processes keep the session alive.
pub fn session_alive(
    processes: &[ProcessInfo],
    root_pid: Option<u32>,
    tracked: &mut HashSet<u32>,
    spec: &WatcherSpec,
) -> bool {
    // 1) the spawned tree, while the root is known to be alive
    if let Some(root) = root_pid {
        let tree = descendants(processes, root);
        tracked.extend(tree.iter().copied());
        if !tree.is_empty() {
            // root pid itself is always in `tree`; check it actually exists
            if processes.iter().any(|p| tree.contains(&p.pid)) {
                return true;
            }
        }
    }
    // 2) previously-seen pids that still exist (detached children)
    if processes.iter().any(|p| tracked.contains(&p.pid)) {
        return true;
    }
    // 3) class hint (Wine double-fork: brand-new pids we never saw spawn)
    if let Some(hint) = &spec.hint {
        let matched: Vec<u32> = processes
            .iter()
            .filter(|p| hint_matches(p, hint, &spec.scope))
            .map(|p| p.pid)
            .collect();
        if !matched.is_empty() {
            tracked.extend(matched);
            return true;
        }
    }
    false
}

/// Watch a launched game until the session ends. Owns the child to capture
/// its exit status; afterwards keeps polling tree/hints until the grace
/// period elapses with no survivors.
pub async fn watch_session(
    mut child: tokio::process::Child,
    spec: WatcherSpec,
    mut on_tick: impl FnMut(OffsetDateTime),
) -> SessionOutcome {
    let started_at = OffsetDateTime::now_utc();
    let root_pid = child.id();
    let mut tracked: HashSet<u32> = HashSet::new();
    let mut exit_code: Option<i32> = None;
    let mut sys = SysinfoSnapshot::new();
    let mut last_alive = std::time::Instant::now();
    let mut last_tick = std::time::Instant::now();
    let mut child_running = true;

    loop {
        if child_running && let Ok(Some(status)) = child.try_wait() {
            exit_code = status.code();
            child_running = false;
        }

        sys.refresh();
        let processes = sys.processes().to_vec();
        let alive = if child_running {
            // the child is alive by definition; calling the matcher anyway
            // keeps `tracked` up to date so detachment is seen later
            let _ = session_alive(&processes, root_pid, &mut tracked, &spec);
            true
        } else {
            session_alive(&processes, None, &mut tracked, &spec)
        };

        let now = std::time::Instant::now();
        if alive {
            last_alive = now;
        } else if now.duration_since(last_alive) >= spec.grace {
            break;
        }

        if now.duration_since(last_tick) >= Duration::from_secs(60) {
            last_tick = now;
            on_tick(OffsetDateTime::now_utc());
        }

        tokio::time::sleep(spec.poll).await;
    }

    let ended_at = OffsetDateTime::now_utc();
    let end_reason = if exit_code.is_some() {
        SessionEndReason::Exited
    } else {
        SessionEndReason::TreeDrained
    };

    SessionOutcome {
        started_at,
        ended_at,
        exit_code,
        end_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn spec(poll_ms: u64, grace_ms: u64, hint: Option<WatchHint>) -> WatcherSpec {
        WatcherSpec {
            hint,
            scope: WatchScope::default(),
            poll: Duration::from_millis(poll_ms),
            grace: Duration::from_millis(grace_ms),
        }
    }

    fn spawn(cmd: &str) -> tokio::process::Child {
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn test process")
    }

    #[tokio::test]
    async fn clean_exit_captures_code_zero() {
        let outcome = watch_session(spawn("exit 0"), spec(30, 150, None), |_| {}).await;
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.end_reason, SessionEndReason::Exited);
        assert!(outcome.ended_at >= outcome.started_at);
    }

    #[tokio::test]
    async fn crash_exit_code_is_visible() {
        let outcome = watch_session(spawn("exit 42"), spec(30, 150, None), |_| {}).await;
        assert_eq!(outcome.exit_code, Some(42));
    }

    #[tokio::test]
    async fn hint_keeps_session_alive_after_launcher_exits() {
        // launcher exits immediately, leaving a detached uniquely-named child
        let marker = format!("0.9{}", std::process::id());
        let outcome = watch_session(
            spawn(&format!("sleep {marker} & exit 0")),
            spec(40, 200, Some(WatchHint::CmdlineContains(marker.clone()))),
            |_| {},
        )
        .await;
        assert_eq!(outcome.exit_code, Some(0));
        // ~0.9s detached child must extend the session well beyond the
        // launcher's instant exit + 200ms grace
        let lived = outcome.ended_at - outcome.started_at;
        assert!(
            lived >= time::Duration::milliseconds(800),
            "session ended too early: {lived}"
        );
    }

    #[test]
    fn matcher_respects_scope_and_names() {
        let scoped = WatchScope {
            under_path: Some("/games/bg3".into()),
        };
        let in_scope = ProcessInfo {
            pid: 10,
            parent: None,
            name: "bg3.exe".into(),
            cmdline: "Z:\\stuff /games/bg3/game/app/bin/bg3.exe".into(),
            exe: None,
        };
        let out_of_scope = ProcessInfo {
            pid: 11,
            parent: None,
            name: "bg3.exe".into(),
            cmdline: "/elsewhere/bg3.exe".into(),
            exe: Some("/elsewhere/bg3.exe".into()),
        };
        let hint = WatchHint::ExeNames(vec!["BG3.exe".into()]);
        assert!(hint_matches(&in_scope, &hint, &scoped));
        assert!(!hint_matches(&out_of_scope, &hint, &scoped));
        // unscoped matches both
        assert!(hint_matches(&out_of_scope, &hint, &WatchScope::default()));
    }

    #[test]
    fn descendants_walk_the_tree() {
        let procs = vec![
            ProcessInfo {
                pid: 1,
                parent: None,
                name: "init".into(),
                cmdline: String::new(),
                exe: None,
            },
            ProcessInfo {
                pid: 5,
                parent: Some(1),
                name: "root".into(),
                cmdline: String::new(),
                exe: None,
            },
            ProcessInfo {
                pid: 6,
                parent: Some(5),
                name: "child".into(),
                cmdline: String::new(),
                exe: None,
            },
            ProcessInfo {
                pid: 7,
                parent: Some(6),
                name: "grandchild".into(),
                cmdline: String::new(),
                exe: None,
            },
            ProcessInfo {
                pid: 9,
                parent: Some(1),
                name: "unrelated".into(),
                cmdline: String::new(),
                exe: None,
            },
        ];
        let tree = descendants(&procs, 5);
        assert_eq!(tree, HashSet::from([5, 6, 7]));
    }
}
