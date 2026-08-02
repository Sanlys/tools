//! `game-mgr-backend` -- ported from game-mgr's `gm-server` (see
//! `apps/game-mgr/backend`'s module docs). Stats/catalog API + this tool's
//! own compiled `GameMgrPanel` wasm UI, on `tools`' shared Postgres/auth/
//! metrics adapters instead of hand-rolled equivalents.

use auth_adapter::backend::AuthState;

use game_mgr_backend::api::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let pool = postgres_adapter::pool_from_env()
        .await
        .map_err(|err| anyhow::anyhow!("postgres config: {err}"))?;
    game_mgr_backend::db::MIGRATOR.run(&pool).await?;
    tracing::info!("database migrations applied");

    let auth = AuthState::from_env("game-mgr");
    let state = AppState { db: pool, auth };

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer()?;

    let app = game_mgr_backend::api::router(state)
        .merge(metrics_router)
        .layer(metrics_layer);

    // 8083: the next free port in tools' local-dev convention (portal
    // 8080, hello 8081, webhello 8082, idp 4000 -- see
    // docs/local-development.md) and matches deploy/game-mgr/values.yaml's
    // `containerPort`.
    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8083".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("game-mgr-backend listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
