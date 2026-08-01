//! `hello` -- the reference example tool's backend.
//!
//! Exercises every adapter a real tool would use: a Postgres table it owns,
//! its own S3 bucket, a Prometheus `/metrics` endpoint, an HTTP `/health`
//! check for both the dashboard and Kubernetes probes, and a `/ws`
//! websocket endpoint to prove that connection style works end to end too
//! (the browser-side counterpart would use `ewebsock`, which -- like
//! `ehttp` -- works unmodified on both native and wasm).
//!
//! CORS is wide open here because the portal's wasm UI is served from a
//! different subdomain than this tool's own backend (subdomain-per-tool
//! routing) and needs to call it directly from the browser. Tighten this to
//! an explicit allow-list of your real domains before this leaves the
//! reference-example stage.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    s3_client: aws_sdk_s3::Client,
    bucket: String,
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

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS greetings ( \
            id SERIAL PRIMARY KEY, \
            name TEXT NOT NULL, \
            created_at TIMESTAMPTZ NOT NULL DEFAULT now() \
        )",
    )
    .execute(&pg_pool)
    .await?;

    let state = AppState {
        s3_client,
        bucket: s3_cfg.bucket_name,
        pg_pool,
    };

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer()?;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/status", get(get_status))
        .route("/api/greetings", post(post_greeting))
        .route("/ws", get(ws_handler))
        .merge(metrics_router)
        .layer(metrics_layer)
        .layer(cors)
        .with_state(state);

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("hello-backend listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    message: String,
    greeting_count: i64,
    bucket_object_count: u64,
}

async fn get_status(State(state): State<AppState>) -> Result<Json<StatusResponse>, ApiError> {
    let greeting_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM greetings")
        .fetch_one(&state.pg_pool)
        .await?;

    let listing = state
        .s3_client
        .list_objects_v2()
        .bucket(&state.bucket)
        .send()
        .await
        .map_err(|err| ApiError(err.to_string()))?;
    let bucket_object_count = listing.contents().len() as u64;

    Ok(Json(StatusResponse {
        message: format!("hello-backend is up, bucket `{}`", state.bucket),
        greeting_count,
        bucket_object_count,
    }))
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
    sqlx::query("INSERT INTO greetings (name) VALUES ($1)")
        .bind(name)
        .execute(&state.pg_pool)
        .await?;
    Ok(StatusCode::CREATED)
}

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

/// Sends an incrementing counter once a second until the client disconnects.
/// Stands in for whatever a real tool would push over a websocket (live
/// progress, log tail, etc).
async fn handle_socket(mut socket: WebSocket) {
    let mut counter: u64 = 0;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                counter += 1;
                if socket.send(Message::Text(counter.to_string())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
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
