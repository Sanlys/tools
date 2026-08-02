//! `hello` -- the reference example tool's backend.
//!
//! Exercises every adapter a real tool would use: a Postgres table it owns,
//! its own S3 bucket, a Prometheus `/metrics` endpoint, an HTTP `/health`
//! check for both the dashboard and Kubernetes probes, and a `/ws`
//! websocket endpoint to prove that connection style works end to end too
//! (the browser-side counterpart would use `ewebsock`, which -- like
//! `ehttp` -- works unmodified on both native and wasm).
//!
//! Also serves this tool's own compiled wasm UI (`apps/hello/frontend`,
//! built by the Dockerfile's `trunk` stage into `dist/`) as a fallback for
//! any path that isn't one of the API routes below -- that's what makes
//! this tool's own ingress host (`hello.k8s.lysakermoen.com`) render
//! `HelloPanel` directly instead of exposing a bare API with nothing at
//! `/`. Same pattern as `apps/portal/backend`, just one tool's UI instead
//! of the unified one.
//!
//! CORS is wide open here because the portal's wasm UI is served from a
//! different subdomain than this tool's own backend (subdomain-per-tool
//! routing) and needs to call it directly from the browser. Tighten this to
//! an explicit allow-list of your real domains before this leaves the
//! reference-example stage.

use auth_adapter::backend::{AuthState, AuthUser};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    s3_client: aws_sdk_s3::Client,
    bucket: String,
    pg_pool: sqlx::PgPool,
    auth: AuthState,
}

impl FromRef<AppState> for AuthState {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
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

    let auth = AuthState::from_env("hello");

    let state = AppState {
        s3_client,
        bucket: s3_cfg.bucket_name,
        pg_pool,
        auth: auth.clone(),
    };

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer()?;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "./dist".to_string());
    let index_html = format!("{static_dir}/index.html");
    let static_service = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index_html));

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/status", get(get_status))
        .route("/api/greetings", post(post_greeting))
        .route("/api/greetings/reset", delete(reset_greetings))
        .route("/ws", get(ws_handler))
        .merge(metrics_router)
        .merge(auth_adapter::backend::config_route(auth.public_config()))
        .layer(metrics_layer)
        .layer(cors)
        .with_state(state)
        .fallback_service(static_service);

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("hello-backend listening on {addr}, serving static assets from {static_dir}");
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

/// Requires being signed in -- the greeting count and bucket object count
/// below used to be visible to anyone, logged in or not. `AuthUser` here
/// only proves login, no specific role (contrast `reset_greetings`, which
/// additionally requires `operator`).
async fn get_status(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<StatusResponse>, ApiError> {
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

/// Requires being signed in (any authenticated `hello` user, no specific
/// role -- contrast `reset_greetings` below, which additionally requires
/// the `operator` role). Posting used to have no `AuthUser` param at all,
/// so anyone -- signed in or not -- could post regardless of what the
/// frontend showed.
async fn post_greeting(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<NewGreeting>,
) -> Result<StatusCode, ApiError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError("name must not be empty".to_string()));
    }
    let id: i32 = sqlx::query_scalar("INSERT INTO greetings (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(&state.pg_pool)
        .await?;

    // The other half of what this tool is meant to demonstrate: one small
    // object per greeting, so `bucket_object_count` below (and the bucket
    // itself) actually reflects real S3 usage instead of `list_objects_v2`
    // always reading back an empty bucket.
    state
        .s3_client
        .put_object()
        .bucket(&state.bucket)
        .key(format!("greetings/{id}.txt"))
        .body(aws_sdk_s3::primitives::ByteStream::from(
            name.as_bytes().to_vec(),
        ))
        .send()
        .await
        .map_err(|err| ApiError(err.to_string()))?;

    Ok(StatusCode::CREATED)
}

/// Reference example of an auth-gated route: requires a valid Bearer token
/// *and* the `operator` role for this app's own client_id (declared in
/// `deploy/idp/values.yaml`'s `IDP_CLIENTS_JSON` entry for `hello`). Copy
/// this shape -- `AuthUser` as a handler parameter, `require_role` checked
/// first so a missing role responds 403 rather than the generic 500 an
/// `ApiError` would give -- for any route a new tool wants to gate.
async fn reset_greetings(
    State(state): State<AppState>,
    user: AuthUser,
) -> axum::response::Response {
    if let Err(err) = user.require_role("operator") {
        return err.into_response();
    }
    match sqlx::query("DELETE FROM greetings")
        .execute(&state.pg_pool)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
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
