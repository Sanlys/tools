//! Shared harness for DB-backed tests (PLAN.md §15).
//!
//! Each test gets its own freshly-created database (migrated via the
//! embedded migrator) so tests are isolated and parallel-safe. Without
//! `DATABASE_URL` tests skip with a notice; with `GM_REQUIRE_DB_TESTS`
//! (set in CI) skipping turns into failure.
//!
//! Auth is exercised for real here (no fake-auth escape hatch -- `tools`
//! has none anywhere, see `docs/local-development.md` §6): a background
//! thread runs a tiny mock IDP that serves nothing but a JWKS document over
//! HTTP, [`req`] signs a real RS256 token against it per call, and
//! `game_mgr_backend::api::router` verifies it through the exact same
//! `auth_adapter::backend::AuthState` a deployed instance would use.

#![allow(dead_code)] // each test binary uses a different subset

use std::sync::OnceLock;

use auth_adapter::backend::AuthState;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use base64::Engine as _;
use http_body_util::BodyExt;
use jsonwebtoken::{EncodingKey, Header};
use rsa::RsaPrivateKey;
use rsa::traits::PublicKeyParts;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;

use game_mgr_backend::api::AppState;

pub struct TestDb {
    pub pool: PgPool,
    admin: PgPool,
    name: String,
}

impl TestDb {
    /// `None` = skipped (no DATABASE_URL and GM_REQUIRE_DB_TESTS unset).
    pub async fn create() -> Option<TestDb> {
        let Ok(base_url) = std::env::var("DATABASE_URL") else {
            if std::env::var("GM_REQUIRE_DB_TESTS").is_ok() {
                panic!("GM_REQUIRE_DB_TESTS is set but DATABASE_URL is missing");
            }
            eprintln!("skipping DB test: DATABASE_URL not set (see PLAN.md §15)");
            return None;
        };

        let name = format!("gm_test_{}", Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base_url)
            .await
            .expect("connect to DATABASE_URL");
        sqlx::query(&format!(r#"CREATE DATABASE "{name}""#))
            .execute(&admin)
            .await
            .expect("create test database");

        let mut url = reqwest::Url::parse(&base_url).expect("DATABASE_URL must be a valid URL");
        url.set_path(&format!("/{name}"));
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url.as_str())
            .await
            .expect("connect to test database");
        game_mgr_backend::db::MIGRATOR
            .run(&pool)
            .await
            .expect("run migrations");

        Some(TestDb { pool, admin, name })
    }

    /// Call at the end of each test. (A panicking test leaves its
    /// `gm_test_*` database behind — harmless in CI, droppable locally.)
    pub async fn cleanup(self) {
        self.pool.close().await;
        let _ = sqlx::query(&format!(r#"DROP DATABASE "{}" WITH (FORCE)"#, self.name))
            .execute(&self.admin)
            .await;
        self.admin.close().await;
    }
}

const TEST_KID: &str = "test-key";
const TEST_CLIENT_ID: &str = "game-mgr";

struct MockIdp {
    issuer_url: String,
    encoding_key: EncodingKey,
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Starts (once per test binary process) a background thread running its
/// own single-threaded Tokio runtime hosting nothing but
/// `GET /.well-known/jwks.json` -- decoupled from any individual test's own
/// `#[tokio::test]` runtime, which is torn down when that test returns.
fn mock_idp() -> &'static MockIdp {
    static IDP: OnceLock<MockIdp> = OnceLock::new();
    IDP.get_or_init(|| {
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("generate test RSA key");
        let pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&key, rsa::pkcs8::LineEnding::LF)
            .expect("encode test key as PKCS8 PEM");
        let encoding_key =
            EncodingKey::from_rsa_pem(pem.as_bytes()).expect("jsonwebtoken accepts the PEM");

        let jwks = serde_json::json!({
            "keys": [{
                "kid": TEST_KID,
                "kty": "RSA",
                "n": b64url(&key.n().to_bytes_be()),
                "e": b64url(&key.e().to_bytes_be()),
            }]
        });

        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("build mock-idp runtime");
            rt.block_on(async move {
                let router = Router::new().route(
                    "/.well-known/jwks.json",
                    axum::routing::get(move || {
                        let jwks = jwks.clone();
                        async move { axum::Json(jwks) }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind mock-idp listener");
                addr_tx
                    .send(listener.local_addr().unwrap())
                    .expect("send mock-idp addr");
                axum::serve(listener, router)
                    .await
                    .expect("mock-idp server");
            });
        });
        let addr = addr_rx.recv().expect("mock-idp thread reports its addr");

        MockIdp {
            issuer_url: format!("http://{addr}"),
            encoding_key,
        }
    })
}

/// Full router wired to the given pool and the mock IDP above.
pub fn app(pool: PgPool) -> Router {
    let idp = mock_idp();
    let auth = AuthState::new(idp.issuer_url.clone(), TEST_CLIENT_ID);
    game_mgr_backend::api::router(AppState { db: pool, auth })
}

fn sign(sub: &str) -> String {
    let idp = mock_idp();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let claims = serde_json::json!({
        "iss": idp.issuer_url,
        "sub": sub,
        "aud": TEST_CLIENT_ID,
        "exp": now + 600,
        "iat": now,
        "preferred_username": sub,
        "roles": [],
    });
    let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(TEST_KID.to_string());
    jsonwebtoken::encode(&header, &claims, &idp.encoding_key).expect("sign test token")
}

/// Issue a JSON request as `sub` (None = no Authorization header at all,
/// for testing the unauthenticated path) and decode the response body as
/// JSON (falls back to a JSON string for plain text).
pub async fn req(
    app: &Router,
    method: &str,
    uri: &str,
    sub: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    // `None` signs a token for a default sub (mirrors the old fake-auth
    // backend's "no x-fake-sub override" default) rather than sending no
    // token at all -- most callers just want *a* signed-in caller and
    // don't care which. Nothing in this test suite exercises the
    // genuinely-unauthenticated path through this helper; `api.rs`'s own
    // unit tests cover that directly against a raw `Request`.
    let sub = sub.unwrap_or("dev-user");
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {}", sign(sub)));
    let request = match body {
        Some(value) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string())),
        None => builder.body(Body::empty()),
    }
    .expect("build request");

    let response = app.clone().oneshot(request).await.expect("send request");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, value)
}

/// GET /me as `sub`, returning the user's id.
pub async fn user_id_of(app: &Router, sub: &str) -> Uuid {
    let (status, body) = req(app, "GET", "/api/v1/me", Some(sub), None).await;
    assert_eq!(status, StatusCode::OK, "GET /me failed: {body}");
    body["user"]["id"]
        .as_str()
        .expect("user id")
        .parse()
        .expect("uuid")
}
