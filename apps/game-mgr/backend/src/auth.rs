//! Thin wrapper around `auth_adapter::backend::AuthUser`: every
//! authenticated request upserts a `users` row for the caller's OIDC `sub`
//! (auto-provisioning, no signup flow -- PLAN.md §8.0) and the handler
//! receives a [`UserCtx`] carrying the resulting internal `user_id`.
//! Provisioning stays invisible to clients regardless of which route they
//! hit -- every `api/*` handler takes `Authed` instead of touching
//! `AuthUser`/`repo::users` directly.

use auth_adapter::backend::AuthUser;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use uuid::Uuid;

use crate::api::AppState;
use crate::error::ApiError;
use crate::repo;

#[derive(Debug, Clone)]
pub struct UserCtx {
    pub user_id: Uuid,
    pub sub: String,
}

/// Extractor for authenticated routes: verifies the Bearer token against
/// the IDP (see `crate::api`'s `impl FromRef<AppState> for AuthState`),
/// then upserts the user row.
pub struct Authed(pub UserCtx);

#[axum::async_trait]
impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state)
            .await
            .map_err(|err| ApiError::Unauthorized(err.to_string()))?;
        let row = repo::users::upsert_by_sub(&state.db, &user.sub).await?;
        Ok(Authed(UserCtx {
            user_id: row.id,
            sub: row.sub,
        }))
    }
}
