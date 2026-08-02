use idp_backend::{clients, db, keys, rate_limit::RateLimiter, routes, state::AppState};

use axum::{
    routing::{delete, get, post},
    Router,
};
use axum_extra::extract::cookie::Key;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use webauthn_rs::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "idp_backend=debug,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let db = postgres_adapter::pool_from_env()
        .await
        .map_err(|err| anyhow::anyhow!("postgres config: {err}"))?;
    sqlx::migrate!("./migrations").run(&db).await?;

    let client_configs = clients::load_client_configs()?;
    db::reconcile_clients(&db, &client_configs).await?;
    tracing::info!(
        count = client_configs.len(),
        "reconciled OAuth client registry"
    );

    let user_count = db::count_users(&db).await.unwrap_or(0);
    idp_backend::metrics::registered_users_gauge(user_count as f64);

    let base_url =
        std::env::var("BASE_URL").unwrap_or_else(|_| "http://localhost:4000".to_string());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4000);
    let static_dir =
        std::env::var("STATIC_DIR").unwrap_or_else(|_| "../frontend/static".to_string());

    // The WebAuthn RP ID should be the *registrable* domain suffix
    // (e.g. `lysakermoen.com`), not necessarily this deployment's exact
    // hostname (`idp.k8s.lysakermoen.com`) -- so passkeys keep working
    // unchanged if this ever moves from the `internal` ingress class to
    // `public` on the bare domain (see docs/architecture.md). Defaults to
    // the origin's own host when unset, which is fine for local dev.
    let rp_origin = Url::parse(&base_url)?;
    let rp_id = std::env::var("RP_ID")
        .unwrap_or_else(|_| rp_origin.host_str().unwrap_or("localhost").to_string());
    let webauthn = WebauthnBuilder::new(&rp_id, &rp_origin)?
        .rp_name("Tools IDP")
        .build()?;

    let jwt_keys = keys::JwtKeys::load_or_generate(&db).await?;
    let cookie_key = load_or_generate_cookie_key(&db).await?;

    let auth_max: u32 = env_u32("AUTH_RATE_LIMIT_MAX", 10);
    let auth_window: u64 = env_u64("AUTH_RATE_LIMIT_WINDOW_SECS", 60);
    let token_max: u32 = env_u32("TOKEN_RATE_LIMIT_MAX", 30);
    let token_window: u64 = env_u64("TOKEN_RATE_LIMIT_WINDOW_SECS", 60);

    let auth_limiter = RateLimiter::new(auth_max, auth_window);
    let token_limiter = RateLimiter::new(token_max, token_window);
    {
        let al = auth_limiter.clone();
        let tl = token_limiter.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                al.gc();
                tl.gc();
            }
        });
    }

    let state = AppState {
        db,
        webauthn: std::sync::Arc::new(webauthn),
        jwt_keys,
        base_url: base_url.clone(),
        cookie_key,
        auth_limiter,
        token_limiter,
    };

    let (metrics_layer, metrics_router) = metrics_adapter::metrics_layer()?;

    let app = Router::new()
        .route("/health", get(health))
        // ── OIDC discovery ──────────────────────────────────────────────
        .route(
            "/.well-known/openid-configuration",
            get(routes::oauth::openid_configuration),
        )
        .route("/.well-known/jwks.json", get(routes::oauth::jwks))
        // ── OAuth 2.0 / OIDC ────────────────────────────────────────────
        .route(
            "/oauth/authorize",
            get(routes::oauth::authorize).post(routes::oauth::authorize_post),
        )
        .route("/oauth/token", post(routes::oauth::token))
        .route(
            "/oauth/userinfo",
            get(routes::oauth::userinfo).post(routes::oauth::userinfo_post),
        )
        .route("/oauth/revoke", post(routes::oauth::revoke))
        // ── Passkey auth API ────────────────────────────────────────────
        .route("/api/setup/status", get(routes::passkey::setup_status))
        .route("/api/register/start", post(routes::passkey::register_start))
        .route(
            "/api/register/finish",
            post(routes::passkey::register_finish),
        )
        .route("/api/auth/start", post(routes::passkey::auth_start))
        .route("/api/auth/finish", post(routes::passkey::auth_finish))
        .route("/api/auth/logout", post(routes::passkey::logout))
        .route("/api/me", get(routes::passkey::me))
        // ── Profile ──────────────────────────────────────────────────────
        .route("/api/profile", post(routes::passkey::update_profile))
        .route("/api/passkeys", get(routes::passkey::list_passkeys))
        .route(
            "/api/passkeys/start",
            post(routes::passkey::add_passkey_start),
        )
        .route(
            "/api/passkeys/finish",
            post(routes::passkey::add_passkey_finish),
        )
        .route(
            "/api/passkeys/{id}",
            delete(routes::passkey::delete_passkey),
        )
        .route(
            "/api/sessions",
            get(routes::passkey::list_sessions).delete(routes::passkey::revoke_all_sessions),
        )
        .route(
            "/api/sessions/{id}",
            delete(routes::passkey::revoke_session),
        )
        // ── Admin ────────────────────────────────────────────────────────
        .route("/api/admin/users", get(routes::admin::list_users))
        .route("/api/admin/users/{id}", delete(routes::admin::delete_user))
        .route(
            "/api/admin/users/{id}/roles",
            get(routes::admin::list_roles_for_user),
        )
        .route(
            "/api/admin/clients",
            get(routes::admin::list_clients).post(routes::admin::create_client),
        )
        .route(
            "/api/admin/clients/{client_id}",
            axum::routing::put(routes::admin::update_client).delete(routes::admin::delete_client),
        )
        .route(
            "/api/admin/roles",
            post(routes::admin::grant_role).delete(routes::admin::revoke_role),
        )
        .route(
            "/api/admin/access",
            post(routes::admin::grant_access).delete(routes::admin::revoke_access),
        )
        .route(
            "/api/admin/users/{id}/access",
            get(routes::admin::list_access_for_user),
        )
        .route(
            "/api/admin/invites",
            get(routes::admin::list_invites).post(routes::admin::create_invite),
        )
        .route(
            "/api/admin/invites/{id}",
            delete(routes::admin::delete_invite),
        )
        // ── Static UI (plain HTML/JS, one file per page -- see
        // apps/idp/frontend/static; not a SPA, so each page gets its own
        // route rather than a catch-all index.html fallback) ────────────
        .route_service("/", ServeFile::new(format!("{static_dir}/index.html")))
        .route_service("/login", ServeFile::new(format!("{static_dir}/login.html")))
        .route_service(
            "/register",
            ServeFile::new(format!("{static_dir}/register.html")),
        )
        .route_service(
            "/profile",
            ServeFile::new(format!("{static_dir}/profile.html")),
        )
        .route_service("/admin", ServeFile::new(format!("{static_dir}/admin.html")))
        .fallback_service(ServeDir::new(&static_dir))
        .merge(metrics_router)
        .layer(metrics_layer)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("idp-backend listening on http://{addr} (base_url={base_url}, rp_id={rp_id})");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

/// Loads the private-cookie encryption key from `COOKIE_SECRET` if set,
/// otherwise generates one and persists it in the `config` table so it
/// survives restarts without needing a sops secret at all (same reasoning
/// as `keys::JwtKeys::load_or_generate`).
async fn load_or_generate_cookie_key(db: &sqlx::PgPool) -> anyhow::Result<Key> {
    if let Ok(secret) = std::env::var("COOKIE_SECRET") {
        return Ok(key_from_hex_or_bytes(&secret));
    }
    if let Some(stored) = db::get_config(db, "cookie_secret").await? {
        return Ok(key_from_hex_or_bytes(&stored));
    }
    tracing::info!("no COOKIE_SECRET set; generating one and persisting it in the config table");
    let generated = hex::encode(rand_bytes::<64>());
    db::set_config(db, "cookie_secret", &generated).await?;
    Ok(key_from_hex_or_bytes(&generated))
}

fn key_from_hex_or_bytes(secret: &str) -> Key {
    let raw = hex::decode(secret).unwrap_or_else(|_| secret.as_bytes().to_vec());
    let padded: Vec<u8> = raw
        .into_iter()
        .chain(std::iter::repeat(0u8))
        .take(64)
        .collect();
    Key::from(&padded)
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    use rand::RngCore;
    let mut buf = [0u8; N];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
