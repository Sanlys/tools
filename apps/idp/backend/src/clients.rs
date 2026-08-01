//! Static OAuth client + role registry, reconciled into the `clients` table
//! at boot -- the same "GitOps-declared, not admin-CRUD" pattern
//! `apps/portal/backend` already uses for its own tool registry
//! (`TOOLS_REGISTRY_JSON`). Adding a new auth-gated app means adding an
//! entry here (in `deploy/idp/values.yaml`), not calling an admin API.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    /// This app's own declared role vocabulary -- a flat list of opaque
    /// strings an admin can grant to specific users for this client_id.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Allows the RFC 8252 loopback exception: any `redirect_uri` whose
    /// host is a loopback address and whose path matches one of
    /// `redirect_uris` is accepted regardless of port (a native binary
    /// can't know its port in advance -- the OS assigns it at bind time).
    #[serde(default)]
    pub native: bool,
}

/// Reads `IDP_CLIENTS_JSON` (an inline JSON array, set by
/// `deploy/idp/values.yaml`), falling back to `IDP_CLIENTS_FILE` (a path to
/// the same JSON), then a small built-in dev default so `cargo run` works
/// out of the box.
pub fn load_client_configs() -> anyhow::Result<Vec<ClientConfig>> {
    if let Ok(json) = std::env::var("IDP_CLIENTS_JSON") {
        return Ok(serde_json::from_str(&json)?);
    }

    let path =
        std::env::var("IDP_CLIENTS_FILE").unwrap_or_else(|_| "/etc/idp/clients.json".to_string());
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(serde_json::from_str(&contents)?),
        Err(_) => {
            tracing::warn!(
                "{path} not found, using a built-in dev client registry pointing at localhost -- \
                 see deploy/idp/values.yaml for how this is populated in a real deployment"
            );
            Ok(serde_json::from_str(include_str!("dev_clients.json"))?)
        }
    }
}

/// `true` if `redirect_uri` is allowed for `client`: an exact match against
/// its declared `redirect_uris`, or (for `native` clients only) a loopback
/// host with a matching path, ignoring port.
pub fn redirect_uri_allowed(client: &crate::db::Client, redirect_uri: &str) -> bool {
    if client.redirect_uris.iter().any(|u| u == redirect_uri) {
        return true;
    }
    if !client.native {
        return false;
    }
    let Ok(candidate) = url::Url::parse(redirect_uri) else {
        return false;
    };
    let is_loopback = matches!(
        candidate.host_str(),
        Some("127.0.0.1") | Some("::1") | Some("localhost")
    );
    if !is_loopback {
        return false;
    }
    client.redirect_uris.iter().any(|declared| {
        url::Url::parse(declared)
            .map(|d| d.path() == candidate.path())
            .unwrap_or(false)
    })
}
