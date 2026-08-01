use axum::{http::StatusCode, response::IntoResponse, response::Response, Json};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("webauthn error: {0}")]
    WebAuthn(String),
    #[error("jwt error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("rate limited")]
    RateLimited,
    #[error("internal: {0}")]
    Internal(String),
    /// RFC 6749 §5.2 token endpoint error: `invalid_grant`, HTTP 400.
    #[error("invalid_grant: {0}")]
    InvalidGrant(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Database(e) => {
                tracing::error!("database error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal server error" })),
                )
                    .into_response()
            }
            AppError::Jwt(e) => {
                tracing::error!("jwt error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal server error" })),
                )
                    .into_response()
            }
            AppError::Internal(m) => {
                tracing::error!("internal error: {m}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal server error" })),
                )
                    .into_response()
            }
            AppError::WebAuthn(m) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "error": m }))).into_response()
            }
            AppError::NotFound(m) => {
                (StatusCode::NOT_FOUND, Json(json!({ "error": m }))).into_response()
            }
            AppError::Unauthorized(m) => {
                (StatusCode::UNAUTHORIZED, Json(json!({ "error": m }))).into_response()
            }
            AppError::BadRequest(m) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "error": m }))).into_response()
            }
            AppError::Forbidden(m) => {
                (StatusCode::FORBIDDEN, Json(json!({ "error": m }))).into_response()
            }
            AppError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "error": "too many requests" })),
            )
                .into_response(),
            AppError::InvalidGrant(m) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_grant", "error_description": m })),
            )
                .into_response(),
        }
    }
}
