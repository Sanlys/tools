use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use sqlx::PgPool;
use webauthn_rs::Webauthn;

use crate::{keys::JwtKeys, rate_limit::RateLimiter};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub webauthn: std::sync::Arc<Webauthn>,
    pub jwt_keys: JwtKeys,
    pub base_url: String,
    pub cookie_key: Key,
    /// Rate limiter for the passkey register/auth endpoints.
    pub auth_limiter: RateLimiter,
    /// Rate limiter for `/oauth/token`.
    pub token_limiter: RateLimiter,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}
