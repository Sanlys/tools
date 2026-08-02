//! One `impl GameMgrPanel` block per tab -- split out of `lib.rs` purely for
//! readability; every fn here still has full access to `GameMgrPanel`'s
//! private fields/methods since Rust's privacy is scoped to the module
//! *tree*, not the file.

mod dashboard;
mod games;
mod machines;
mod profiles;
mod settings;

pub(super) fn format_playtime(total_seconds: i64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    format!("{hours}h {minutes}m")
}

/// `time::OffsetDateTime` has no built-in "5 minutes ago" formatting;
/// good enough for a liveness column without pulling in another crate.
pub(super) fn format_relative(ts: Option<time::OffsetDateTime>) -> String {
    match ts {
        None => "never".to_string(),
        Some(ts) => {
            let now = time::OffsetDateTime::now_utc();
            let delta = now - ts;
            if delta.whole_minutes() < 1 {
                "just now".to_string()
            } else if delta.whole_hours() < 1 {
                format!("{}m ago", delta.whole_minutes())
            } else if delta.whole_days() < 1 {
                format!("{}h ago", delta.whole_hours())
            } else {
                format!("{}d ago", delta.whole_days())
            }
        }
    }
}
