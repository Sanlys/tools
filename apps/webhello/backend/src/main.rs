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
//! Also the reference example of auth from a *plain JS* frontend (no
//! `auth_adapter::frontend_web`, which is wasm-only): `static/index.html`
//! hand-rolls the same redirect+PKCE dance `frontend_web.rs` does, scoped
//! to its own `client_id` ("webhello", per `deploy/idp/values.yaml`).
//! Posting a greeting requires being signed in (any authenticated user, no
//! specific role) -- see `post_greeting` below.
//!
//! This is a genuinely separate deployable process from `hello`, with its
//! own client_id/tokens -- but it is **not** a separate app/dataset:
//! it reads and writes `hello`'s own `greetings` table and S3 bucket
//! (`deploy/webhello/values.yaml`'s `envFrom` points straight at the
//! Secrets/ConfigMap `deploy/hello`'s chart creates), deployed into
//! `hello`'s own namespace. The point of this example is the *frontend*
//! stack difference, not a second isolated backend -- posting a greeting
//! here shows up in `hello`'s own count too, and vice versa.

use auth_adapter::backend::{AuthState, AuthUser};
use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    pg_pool: sqlx::PgPool,
    s3_client: aws_sdk_s3::Client,
    bucket: String,
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

    // Same table `hello` creates -- idempotent, and a harmless safety net
    // regardless of which of the two apps happens to start first (there's
    // no ordering guarantee between their separate ArgoCD Applications).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS greetings ( \
            id SERIAL PRIMARY KEY, \
            name TEXT NOT NULL, \
            created_at TIMESTAMPTZ NOT NULL DEFAULT now() \
        )",
    )
    .execute(&pg_pool)
    .await?;

    let auth = AuthState::from_env("webhello");
    let state = AppState {
        pg_pool,
        s3_client,
        bucket: s3_cfg.bucket_name,
        auth: auth.clone(),
    };

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer()?;

    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "./static".to_string());
    let index_html = format!("{static_dir}/index.html");
    let static_service = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index_html));

    // No CORS layer: unlike `hello-backend` (which the portal's wasm UI
    // calls directly cross-origin, since `hello` has an embedded
    // `ToolPanel::Hello`), `webhello` has no portal panel at all -- it
    // only ever shows up as a link-out (see this module's doc comment),
    // and every fetch `static/index.html` itself makes is same-origin. The
    // only cross-context reader of this backend's `/health` is the
    // portal-backend's own server-side `reqwest` call for the dashboard,
    // which isn't subject to browser CORS in the first place. So a wide-open
    // CORS layer here would just be needless attack-surface widening on a
    // backend that reads/writes `hello`'s shared Postgres table.
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/status", get(get_status))
        .route("/api/greetings", post(post_greeting))
        .merge(metrics_router)
        .merge(auth_adapter::backend::config_route(auth.public_config()))
        .layer(metrics_layer)
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

/// Requires being signed in -- the greeting list below is `hello`'s own
/// data (see this module's doc comment), and shouldn't be visible to a
/// logged-out caller any more than `hello`'s own status endpoint is.
async fn get_status(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<StatusResponse>, ApiError> {
    let greetings: Vec<String> =
        sqlx::query_scalar("SELECT name FROM greetings ORDER BY created_at ASC")
            .fetch_all(&state.pg_pool)
            .await?;
    Ok(Json(StatusResponse {
        message: format!(
            "webhello-backend is up, sharing hello's bucket `{}`",
            state.bucket
        ),
        greetings,
    }))
}

#[derive(Debug, Deserialize)]
struct NewGreeting {
    name: String,
}

/// Requires being signed in (any authenticated `webhello` user, no specific
/// role) -- see this module's doc comment. Unlike `hello`'s `reset_greetings`
/// (which additionally requires the `operator` role), this only checks that
/// `AuthUser` extracted at all: proof of login is the whole gate here.
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

    // Same S3 demonstration as `hello`'s own `post_greeting` -- same
    // bucket, so this shows up in `hello`'s own object count too.
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
