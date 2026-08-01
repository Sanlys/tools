//! Shared auth building block: any tool (backend and/or frontend) that wants
//! to sit behind `apps/idp` depends on this crate rather than reimplementing
//! OIDC token verification or the login redirect dance itself.
//!
//! - [`backend`] (behind the `backend` feature): an axum extractor that
//!   verifies a Bearer JWT against the IDP's JWKS and exposes the caller's
//!   granted roles for *this* app.
//! - `frontend_web` (wasm target only): drives the browser-redirect + PKCE
//!   login flow and draws the `LoginWidget` (sign-in button / user-icon
//!   menu) for an egui/wasm tool frontend.
//! - `frontend_native` (native target only): the RFC 8252 loopback-redirect
//!   flow for a tool's standalone `eframe` binary, with the refresh token
//!   persisted in the OS keyring.
//!
//! The actual passkey/WebAuthn ceremony never happens in this crate -- it
//! always happens on the IDP's own origin (a plain HTML/JS page, see
//! `apps/idp/frontend`), which is what lets *any* OAuth client redirect a
//! browser there, not just tools built with this workspace's egui stack.

pub mod pkce;

#[cfg(feature = "backend")]
pub mod backend;

#[cfg(target_arch = "wasm32")]
pub mod frontend_web;

#[cfg(not(target_arch = "wasm32"))]
pub mod frontend_native;

use serde::{Deserialize, Serialize};

/// Public (non-secret) OIDC client config a tool's backend serves at
/// `/config/auth.json`, mirroring how the portal serves `/config/tools.json`
/// -- resolved by the frontend at runtime rather than baked into the wasm
/// binary, so the same build works unmodified across environments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// The IDP's issuer/base URL, e.g. `https://idp.k8s.lysakermoen.com`.
    pub issuer_url: String,
    /// This app's own OAuth client_id, as declared in the IDP's
    /// `IDP_CLIENTS_JSON` (see `deploy/idp/values.yaml`).
    pub client_id: String,
}

/// Claims carried in the access/ID token JWT the IDP issues. Mirrors
/// `apps/idp/backend`'s own `Claims` type; duplicated deliberately (same
/// reasoning as `hello_frontend`'s duplicated `HelloStatus` DTO) so this
/// crate never has to depend on the IDP's own crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Roles this user was granted for *this* client_id -- never other
    /// apps' roles, see `apps/idp/backend`'s token issuance.
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_time: Option<i64>,
}
