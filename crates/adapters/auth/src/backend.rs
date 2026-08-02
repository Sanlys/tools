//! Axum-side auth: verify a Bearer JWT against `apps/idp`'s JWKS and expose
//! the caller's roles for this app. A tool's backend embeds [`AuthState`] as
//! a field of its own `AppState` and implements `FromRef` for it (the same
//! pattern `axum_extra::extract::cookie::Key` already uses in `apps/idp`
//! itself) -- then any handler can just take an [`AuthUser`] parameter.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::{AuthConfig, Claims};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing bearer token")]
    Missing,
    #[error("malformed token")]
    Malformed,
    #[error("unknown signing key (jwks may be stale)")]
    UnknownKey,
    #[error("invalid token: {0}")]
    Invalid(String),
    #[error("fetching IDP jwks: {0}")]
    Jwks(String),
    #[error("missing required role")]
    Forbidden,
    #[error("checking cross-app access with the IDP: {0}")]
    CrossAudience(String),
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let status = match self {
            AuthError::Forbidden => StatusCode::FORBIDDEN,
            _ => StatusCode::UNAUTHORIZED,
        };
        (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<Jwk>,
}

struct Inner {
    issuer_url: String,
    client_id: String,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, DecodingKey>>,
}

/// Holds this app's own `client_id` and the IDP's issuer URL, plus a cache
/// of the IDP's JWKS keys (refetched on a cache miss -- cheap, since a miss
/// only happens on the IDP's own key-rotation schedule). Clone freely: it's
/// an `Arc` internally, matching `sqlx::PgPool`'s own cheap-clone contract.
#[derive(Clone)]
pub struct AuthState {
    inner: Arc<Inner>,
}

impl AuthState {
    pub fn new(issuer_url: impl Into<String>, client_id: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner: Arc::new(Inner {
                issuer_url: issuer_url.into(),
                client_id: client_id.into(),
                http,
                keys: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Reads `IDP_ISSUER_URL` and `AUTH_CLIENT_ID` -- the two env vars every
    /// auth-gated tool backend needs (see `docs/adding-a-tool.md`'s auth
    /// section). Falls back to `http://localhost:4000` and
    /// `default_client_id` (with a warning) rather than panicking, so
    /// `cargo run` works out of the box against a local `idp-backend`
    /// without any env vars set -- same "dev default, real value in
    /// deploy/*/values.yaml" pattern `apps/portal/backend`'s tool registry
    /// already uses.
    pub fn from_env(default_client_id: &str) -> Self {
        let issuer_url = std::env::var("IDP_ISSUER_URL").unwrap_or_else(|_| {
            tracing::warn!("IDP_ISSUER_URL not set, defaulting to http://localhost:4000 -- see deploy/*/values.yaml");
            "http://localhost:4000".to_string()
        });
        let client_id =
            std::env::var("AUTH_CLIENT_ID").unwrap_or_else(|_| default_client_id.to_string());
        Self::new(issuer_url, client_id)
    }

    /// The public (non-secret) config this app's own `/config/auth.json`
    /// serves for its frontend to discover at runtime.
    pub fn public_config(&self) -> AuthConfig {
        AuthConfig {
            issuer_url: self.inner.issuer_url.clone(),
            client_id: self.inner.client_id.clone(),
        }
    }

    async fn refresh_jwks(&self) -> Result<(), AuthError> {
        let url = format!(
            "{}/.well-known/jwks.json",
            self.inner.issuer_url.trim_end_matches('/')
        );
        let resp: JwksResponse = self
            .inner
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AuthError::Jwks(e.to_string()))?
            .json()
            .await
            .map_err(|e| AuthError::Jwks(e.to_string()))?;

        let mut keys = self.inner.keys.write().await;
        keys.clear();
        for jwk in resp.keys {
            if let Ok(key) = DecodingKey::from_rsa_components(&jwk.n, &jwk.e) {
                keys.insert(jwk.kid, key);
            }
        }
        Ok(())
    }

    async fn decoding_key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        if let Some(key) = self.inner.keys.read().await.get(kid) {
            return Ok(key.clone());
        }
        // Cache miss: refetch once (covers both "never fetched yet" and
        // "IDP rotated its signing key") before giving up.
        self.refresh_jwks().await?;
        self.inner
            .keys
            .read()
            .await
            .get(kid)
            .cloned()
            .ok_or(AuthError::UnknownKey)
    }

    /// Verifies `token`'s signature, issuer and expiry, returning the
    /// decoded claims with `roles` filtered to this app's own client_id.
    ///
    /// Tries the fast, self-contained path first: a token minted for
    /// *this app's own* client_id trusts its embedded `roles` claim
    /// directly, no extra round trip. If the audience doesn't match --
    /// e.g. a tool embedded in the portal reusing the *portal's* own
    /// token to prove who's signed in, rather than running its own
    /// separate OAuth flow (see `apps/portal/frontend`'s `HelloPanel`
    /// wiring) -- the token still proves *who* the caller is (same
    /// signature, same issuer), so fall back to asking the IDP fresh for
    /// this app's own roles (and `access_restricted` gate) for that
    /// subject via `/oauth/roles`, rather than trusting a roles claim
    /// that was never scoped to this app in the first place.
    pub async fn verify(&self, token: &str) -> Result<Claims, AuthError> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| AuthError::Malformed)?;
        let kid = header.kid.ok_or(AuthError::Malformed)?;
        let key = self.decoding_key(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.inner.client_id]);
        validation.set_issuer(&[&self.inner.issuer_url]);
        match jsonwebtoken::decode::<Claims>(token, &key, &validation) {
            Ok(data) => return Ok(data.claims),
            Err(e) if *e.kind() == jsonwebtoken::errors::ErrorKind::InvalidAudience => {}
            Err(e) => return Err(AuthError::Invalid(e.to_string())),
        }

        let mut cross_audience_validation = Validation::new(Algorithm::RS256);
        cross_audience_validation.validate_aud = false;
        cross_audience_validation.set_issuer(&[&self.inner.issuer_url]);
        let data = jsonwebtoken::decode::<Claims>(token, &key, &cross_audience_validation)
            .map_err(|e| AuthError::Invalid(e.to_string()))?;
        let roles = self.fetch_cross_audience_roles(token).await?;
        Ok(Claims {
            aud: self.inner.client_id.clone(),
            roles,
            ..data.claims
        })
    }

    async fn fetch_cross_audience_roles(&self, token: &str) -> Result<Vec<String>, AuthError> {
        let url = format!(
            "{}/oauth/roles?client_id={}",
            self.inner.issuer_url.trim_end_matches('/'),
            urlencoding_encode(&self.inner.client_id),
        );
        let resp = self
            .inner
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AuthError::CrossAudience(e.to_string()))?;
        if resp.status() == axum::http::StatusCode::FORBIDDEN {
            return Err(AuthError::Forbidden);
        }
        if !resp.status().is_success() {
            return Err(AuthError::CrossAudience(format!(
                "roles lookup returned {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AuthError::CrossAudience(e.to_string()))?;
        Ok(body
            .get("roles")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The authenticated caller, extracted from a validated Bearer token. Add it
/// as a handler parameter to require login; call [`AuthUser::require_role`]
/// inside the handler body to additionally require a specific role.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub sub: String,
    pub username: Option<String>,
    pub roles: Vec<String>,
}

impl AuthUser {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    pub fn require_role(&self, role: &str) -> Result<(), AuthError> {
        if self.has_role(role) {
            Ok(())
        } else {
            Err(AuthError::Forbidden)
        }
    }
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    AuthState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_state = AuthState::from_ref(state);
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AuthError::Missing)?;
        let claims = auth_state.verify(token).await?;
        Ok(AuthUser {
            sub: claims.sub,
            username: claims.preferred_username,
            roles: claims.roles,
        })
    }
}

/// A small router exposing this app's own `/config/auth.json` (the
/// `AuthConfig` its frontend fetches at runtime) -- merge it into your own
/// `Router<AppState>` the same way `metrics_adapter::metrics_layer()`'s
/// router gets merged in.
pub fn config_route<S>(cfg: AuthConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().route(
        "/config/auth.json",
        get(move || {
            let cfg = cfg.clone();
            async move { Json(cfg) }
        }),
    )
}
