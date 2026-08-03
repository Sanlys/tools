//! Client configuration: TOML file at the XDG config path, every key
//! overridable via `GM_*` environment variables (PLAN.md §6.6).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientConfig {
    /// Stats server base URL, e.g. `https://games.lysakermoen.com`. Also the
    /// gateway to bucket access -- there is no client-side S3 config
    /// anymore; the backend mediates every bucket access instead (PLAN.md
    /// §4.3, see `crate::s3`'s module doc).
    pub server_url: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_native_client_id: Option<String>,
    /// Where game install roots live. Defaults to `~/Games/game-mgr`.
    pub library_dir: Option<PathBuf>,
    pub syncthing: SyncthingConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncthingConfig {
    /// REST address of the local Syncthing instance. When unset, M2 will
    /// autodetect address + API key from the local Syncthing config.xml.
    pub url: Option<String>,
    pub api_key: Option<String>,
}

impl ClientConfig {
    /// Load from the XDG config file (if present), then apply `GM_*` env overrides.
    pub fn load() -> Result<Self> {
        let path = crate::paths::config_file();
        let mut cfg = if path.is_file() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?
        } else {
            Self::default()
        };
        cfg.apply_env_from(|key| std::env::var(key).ok());
        Ok(cfg)
    }

    /// Apply environment overrides through a lookup function (injectable for tests).
    pub fn apply_env_from(&mut self, get: impl Fn(&str) -> Option<String>) {
        fn set(slot: &mut Option<String>, val: Option<String>) {
            if let Some(v) = val.filter(|v| !v.is_empty()) {
                *slot = Some(v);
            }
        }
        set(&mut self.server_url, get("GM_SERVER_URL"));
        set(&mut self.oidc_issuer, get("GM_OIDC_ISSUER"));
        set(
            &mut self.oidc_native_client_id,
            get("GM_OIDC_NATIVE_CLIENT_ID"),
        );
        if let Some(dir) = get("GM_LIBRARY_DIR").filter(|v| !v.is_empty()) {
            self.library_dir = Some(PathBuf::from(dir));
        }
        set(&mut self.syncthing.url, get("GM_SYNCTHING_URL"));
        set(&mut self.syncthing.api_key, get("GM_SYNCTHING_API_KEY"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_file_values() {
        let mut cfg: ClientConfig = toml::from_str(
            r#"
            server_url = "https://from-file.example"
            [syncthing]
            api_key = "file-key"
            "#,
        )
        .unwrap();
        cfg.apply_env_from(|key| match key {
            "GM_SERVER_URL" => Some("https://from-env.example".into()),
            "GM_SYNCTHING_URL" => Some("http://127.0.0.1:8384".into()),
            _ => None,
        });
        assert_eq!(cfg.server_url.as_deref(), Some("https://from-env.example"));
        assert_eq!(cfg.syncthing.url.as_deref(), Some("http://127.0.0.1:8384"));
        // untouched keys keep their file values
        assert_eq!(cfg.syncthing.api_key.as_deref(), Some("file-key"));
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let mut cfg = ClientConfig::default();
        cfg.apply_env_from(|_| Some(String::new()));
        assert_eq!(cfg, ClientConfig::default());
    }
}
