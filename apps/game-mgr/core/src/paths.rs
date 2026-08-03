//! XDG-based filesystem layout (PLAN.md §6.6).
//!
//! All path construction goes through `dirs` + `PathBuf::join` — never string
//! concatenation — to keep the future Windows port mechanical (PLAN.md §14).

use std::path::PathBuf;

use crate::config::ClientConfig;

fn base(dir: Option<PathBuf>, kind: &str) -> PathBuf {
    dir.unwrap_or_else(|| panic!("could not determine the {kind} directory for this platform"))
        .join("game-mgr")
}

/// `$XDG_CONFIG_HOME/game-mgr`
pub fn config_dir() -> PathBuf {
    base(dirs::config_dir(), "config")
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// `$XDG_DATA_HOME/game-mgr` — prefixes/, tools/, state.db
pub fn data_dir() -> PathBuf {
    base(dirs::data_dir(), "data")
}

/// `$XDG_CACHE_HOME/game-mgr` — resumable downloads
pub fn cache_dir() -> PathBuf {
    base(dirs::cache_dir(), "cache")
}

pub fn downloads_dir() -> PathBuf {
    cache_dir().join("downloads")
}

pub fn state_db() -> PathBuf {
    data_dir().join("state.db")
}

/// `$XDG_DATA_HOME/game-mgr/tools` — managed tool installs (PLAN.md §4.4).
pub fn tools_dir() -> PathBuf {
    data_dir().join("tools")
}

/// `$XDG_DATA_HOME/game-mgr/logs` — rolling daily log files.
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// OIDC refresh-token storage (written 0600, PLAN.md §6.5).
pub fn auth_token_file() -> PathBuf {
    base(dirs::state_dir().or_else(dirs::data_dir), "state").join("auth.json")
}

pub fn default_library_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine the home directory for this platform")
        .join("Games")
        .join("game-mgr")
}

/// Game install roots: config override or `~/Games/game-mgr`.
pub fn library_dir(cfg: &ClientConfig) -> PathBuf {
    cfg.library_dir.clone().unwrap_or_else(default_library_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_dir_prefers_config_override() {
        let cfg = ClientConfig {
            library_dir: Some(PathBuf::from("/tmp/lib")),
            ..Default::default()
        };
        assert_eq!(library_dir(&cfg), PathBuf::from("/tmp/lib"));
    }

    #[test]
    fn default_library_dir_is_under_home() {
        let dir = default_library_dir();
        assert!(dir.ends_with("Games/game-mgr"), "got {}", dir.display());
    }
}
