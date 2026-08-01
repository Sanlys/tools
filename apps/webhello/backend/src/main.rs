//! `webhello` -- a second reference example tool, deliberately *not* using
//! egui. `apps/hello` demonstrates the "egui panel embedded in the portal
//! and standalone" path (see `docs/architecture.md`); this one demonstrates
//! the other path it documents but never actually showed: "a tool can ship
//! a completely different frontend stack... the platform doesn't require
//! [egui], it just makes it the path of least resistance." Here that's a
//! hand-written static HTML page (`static/index.html`) with plain `fetch()`
//! calls -- no wasm, no build step, no `Panel` impl, no `ToolPanel` variant
//! in the portal. It only shows up via the portal's Home panel link-out
//! list (see `deploy/portal/values.yaml`'s `TOOLS_REGISTRY_JSON`), the same
//! way `hello`'s standalone deployment does.
//!
//! Deliberately skips Postgres/S3 (unlike `hello`, which exercises both) to
//! keep the point of this example -- the frontend stack, not the backend
//! adapters -- unobscured. State is an in-memory `Vec`, not persisted.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone, Default)]
struct AppState {
    greetings: Arc<Mutex<Vec<String>>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let state = AppState::default();

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer()?;
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "./static".to_string());
    let index_html = format!("{static_dir}/index.html");
    let static_service = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index_html));

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/status", get(get_status))
        .route("/api/greetings", post(post_greeting))
        .merge(metrics_router)
        .layer(metrics_layer)
        .layer(cors)
        .with_state(state)
        .fallback_service(static_service);

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("webhello-backend listening on {addr}, serving static assets from {static_dir}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    message: String,
    greetings: Vec<String>,
}

async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let greetings = state
        .greetings
        .lock()
        .expect("greetings mutex poisoned")
        .clone();
    Json(StatusResponse {
        message: "webhello-backend is up".to_string(),
        greetings,
    })
}

#[derive(Debug, Deserialize)]
struct NewGreeting {
    name: String,
}

async fn post_greeting(
    State(state): State<AppState>,
    Json(body): Json<NewGreeting>,
) -> Result<StatusCode, ApiError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError("name must not be empty".to_string()));
    }
    state
        .greetings
        .lock()
        .expect("greetings mutex poisoned")
        .push(name.to_string());
    Ok(StatusCode::CREATED)
}

struct ApiError(String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, self.0).into_response()
    }
}
