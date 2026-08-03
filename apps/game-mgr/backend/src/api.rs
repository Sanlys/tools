//! HTTP surface: probes and the `/api/v1` API (PLAN.md §8.2), plus this
//! tool's own compiled wasm UI (`apps/game-mgr/frontend`, built by the
//! Dockerfile's `trunk` stage) served as a fallback for any unmatched path
//! -- same pattern as `apps/hello/backend`. Handlers stay thin; SQL lives
//! in [`crate::repo`].

mod catalog;
mod ingest;
mod machines;
mod profiles;

use auth_adapter::backend::AuthState;
use axum::Router;
use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, patch, post, put};
use sqlx::PgPool;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub auth: AuthState,
}

impl FromRef<AppState> for AuthState {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

impl AppState {
    pub fn db(&self) -> &PgPool {
        &self.db
    }
}

/// CORS is wide open here for the same reason as `apps/hello/backend`: the
/// portal's wasm UI (and `GameMgrPanel`'s own standalone build) call this
/// backend directly from a different subdomain (subdomain-per-tool
/// routing). Tighten to an explicit allow-list before this leaves the
/// reference stage if that ever matters here.
pub fn router(state: AppState) -> Router {
    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "./dist".to_string());
    let index_html = format!("{static_dir}/index.html");
    let static_service = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index_html));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api/v1", api_v1())
        .merge(auth_adapter::backend::config_route(
            state.auth.public_config(),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
        .fallback_service(static_service)
}

fn api_v1() -> Router<AppState> {
    Router::new()
        .route("/ping", get(ping))
        .route("/me", get(profiles::me))
        .route("/users", get(profiles::list_users))
        .route("/profiles", get(profiles::list).post(profiles::create))
        .route(
            "/profiles/:id",
            patch(profiles::rename).delete(profiles::delete),
        )
        .route("/profiles/:id/transfer", post(profiles::transfer))
        .route("/machines", get(machines::list))
        .route("/machines/:id", put(machines::register))
        .route("/games", get(catalog::list_games))
        .route(
            "/games/:id",
            get(catalog::get_game).put(catalog::upsert_game),
        )
        .route("/sessions", get(ingest::list_sessions))
        .route("/sessions:batch", post(ingest::sessions_batch))
        .route("/events:batch", post(ingest::events_batch))
        .fallback(api_not_found)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> Result<&'static str, ApiError> {
    sqlx::query("SELECT 1")
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("database unreachable: {e}")))?;
    Ok("ok")
}

async fn ping() -> Json<serde_json::Value> {
    // version lets clients spot a stale server image (PLAN.md §15)
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn api_not_found() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "not found" })),
    )
}

/// Shared input validation for profile / machine names.
fn validate_name(name: &str) -> Result<&str, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(ApiError::Unprocessable(
            "name must be between 1 and 64 characters".into(),
        ));
    }
    Ok(trimmed)
}

const MAX_BATCH_LEN: usize = 1000;

fn validate_batch_len(len: usize) -> Result<(), ApiError> {
    if len > MAX_BATCH_LEN {
        return Err(ApiError::Unprocessable(format!(
            "batch too large: {len} items (max {MAX_BATCH_LEN})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// A pool that never actually connects (`connect_lazy` defers dialing
    /// until first query) -- fine for the tests below, none of which touch
    /// the database; DB-backed behaviour is covered in `tests/` against a
    /// real Postgres service container.
    fn test_state() -> AppState {
        let db = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://gamemgr:gamemgr@localhost:5432/gamemgr")
            .expect("connect_lazy never fails eagerly");
        AppState {
            db,
            auth: AuthState::new("http://localhost:4000", "game-mgr"),
        }
    }

    async fn body_string(req: Request<Body>) -> (StatusCode, String) {
        let res = router(test_state()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn healthz_is_ok() {
        let (status, body) =
            body_string(Request::get("/healthz").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn unknown_api_route_is_json_404() {
        let (status, body) =
            body_string(Request::get("/api/v1/nope").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("not found"));
    }

    #[tokio::test]
    async fn api_without_token_is_401() {
        let (status, body) =
            body_string(Request::get("/api/v1/me").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("bearer") || body.contains("Bearer"));
    }

    #[test]
    fn name_validation_rules() {
        assert!(validate_name("  ok name ").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
        assert_eq!(validate_name(" trimmed ").unwrap(), "trimmed");
    }

    #[test]
    fn batch_length_rules() {
        assert!(validate_batch_len(0).is_ok());
        assert!(validate_batch_len(MAX_BATCH_LEN).is_ok());
        assert!(validate_batch_len(MAX_BATCH_LEN + 1).is_err());
    }
}
