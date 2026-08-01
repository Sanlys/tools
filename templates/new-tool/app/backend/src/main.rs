//! {{project-name}} backend -- {{description}}
//!
//! Generated from `templates/new-tool`. This starting point wires up
//! Postgres, an S3 bucket, and Prometheus metrics exactly like
//! `apps/hello/backend` does -- delete whichever of those you don't need,
//! along with the matching `bucket`/`postgres` blocks in
//! `deploy/{{project-name}}/values.yaml`. See docs/adding-a-tool.md for the
//! full checklist (workspace member, portal panel, tools registry, RBAC/DNS
//! if needed).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    s3_client: aws_sdk_s3::Client,
    bucket: String,
    // TODO: use this for your own queries -- kept here (with the allow) so
    // the generated template has no Postgres connection to wire up later.
    #[allow(dead_code)]
    pg_pool: sqlx::PgPool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let (s3_client, s3_cfg) =
        s3_adapter::client_from_env().map_err(|err| anyhow::anyhow!("s3 config: {err}"))?;
    let pg_pool = postgres_adapter::pool_from_env()
        .await
        .map_err(|err| anyhow::anyhow!("postgres config: {err}"))?;

    // TODO: create your own tables here instead.
    sqlx::query("SELECT 1").fetch_one(&pg_pool).await?;

    let state = AppState {
        s3_client,
        bucket: s3_cfg.bucket_name,
        pg_pool,
    };

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer();
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/status", get(get_status))
        .merge(metrics_router)
        .layer(metrics_layer)
        .layer(cors)
        .with_state(state);

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:{{container_port}}".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("{{project-name}}-backend listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    message: String,
    bucket_object_count: u64,
}

async fn get_status(State(state): State<AppState>) -> Result<Json<StatusResponse>, ApiError> {
    let listing = state
        .s3_client
        .list_objects_v2()
        .bucket(&state.bucket)
        .send()
        .await
        .map_err(|err| ApiError(err.to_string()))?;

    Ok(Json(StatusResponse {
        message: format!("{{project-name}}-backend is up, bucket `{}`", state.bucket),
        bucket_object_count: listing.contents().len() as u64,
    }))
}

struct ApiError(String);

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self(err.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0).into_response()
    }
}
