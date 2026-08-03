//! Native client auth (PLAN.md §6.5): authorization code + PKCE with a
//! loopback redirect, refresh-token storage at 0600. `StaticToken` is a
//! test-only fixture for a mock server that doesn't verify tokens; against
//! the real deployed backend, an unconfigured client uses `NotConfigured`
//! instead, which fails loudly rather than authenticating with a token the
//! real server can never accept.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// A bearer token ready to attach, refreshing if needed.
    async fn bearer(&self) -> Result<String>;
    fn describe(&self) -> String;

    /// Run an interactive login (open the system browser, catch the
    /// loopback redirect) if this provider supports one. Named
    /// differently from `OidcPkce`'s own inherent `login` (rather than
    /// overriding a same-named trait method) purely to avoid any reader
    /// wondering whether `self.login()` inside that impl recurses.
    ///
    /// Default: unsupported. Only `OidcPkce` overrides this --
    /// `StaticToken` (test-only, see its own doc comment) and
    /// `NotConfigured` (see its own doc comment) have nothing to
    /// interactively log into.
    async fn interactive_login(&self) -> Result<()> {
        bail!("{} does not support interactive login", self.describe())
    }

    /// Whether a usable token is currently held, without a network call.
    /// Default `false` -- `OidcPkce` overrides this to check its own
    /// cached/persisted tokens.
    async fn logged_in(&self) -> bool {
        false
    }
}

/// Test-only provider: fixed token, for constructing a `ServerClient` in a
/// test against a local mock server that doesn't verify tokens at all --
/// see this crate's own `stats`/`s3`/`scan` test modules. Never wired up
/// against the real deployed backend: `apps/game-mgr/backend` always
/// verifies a real RS256 JWT (`auth_adapter::backend::AuthUser`) and has no
/// "accept any token" mode to pair with this.
pub struct StaticToken(pub String);

#[async_trait::async_trait]
impl TokenProvider for StaticToken {
    async fn bearer(&self) -> Result<String> {
        Ok(self.0.clone())
    }

    fn describe(&self) -> String {
        "static test token (only valid against a mock server)".into()
    }
}

/// Returned by `provider_from_config` when `oidc_issuer`/`oidc_native_client_id`
/// aren't set. Errors loudly the moment anything actually tries to
/// authenticate, rather than the previous behavior: silently handing out
/// `StaticToken`'s fixed "dev-token", which the real backend has no way to
/// accept (there is no "fake auth" mode anywhere in
/// `apps/game-mgr/backend` -- see that crate's `tests/common/mod.rs`,
/// which documents this repo has no such escape hatch at all). That used
/// to mean an unconfigured client looked "signed in" (`describe()` gave no
/// hint anything was wrong) while every real API call silently 401'd.
struct NotConfigured;

#[async_trait::async_trait]
impl TokenProvider for NotConfigured {
    async fn bearer(&self) -> Result<String> {
        bail!(
            "no OIDC issuer configured -- set oidc_issuer and oidc_native_client_id \
             in config.toml (or GM_OIDC_ISSUER/GM_OIDC_NATIVE_CLIENT_ID) before signing in"
        )
    }

    fn describe(&self) -> String {
        "not signed in -- no OIDC issuer configured".into()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: Option<String>,
    /// Unix seconds when the access token expires.
    expires_at: i64,
}

#[derive(Debug, Deserialize)]
struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// PKCE verifier + S256 challenge (RFC 7636).
pub fn pkce_pair() -> (String, String) {
    let raw: [u8; 32] = rand::thread_rng().r#gen();
    let verifier = URL_SAFE_NO_PAD.encode(raw);
    let challenge = pkce_challenge(&verifier);
    (verifier, challenge)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Parse `code` and `state` out of the loopback redirect's request line,
/// e.g. `GET /callback?code=abc&state=xyz HTTP/1.1`.
pub fn parse_redirect(request_line: &str) -> Result<(String, String)> {
    let path = request_line
        .split_whitespace()
        .nth(1)
        .context("malformed redirect request")?;
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("code", v)) => code = Some(urldecode(v)),
            Some(("state", v)) => state = Some(urldecode(v)),
            Some(("error", v)) => bail!("authorization error: {}", urldecode(v)),
            _ => {}
        }
    }
    Ok((
        code.context("redirect missing code")?,
        state.context("redirect missing state")?,
    ))
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct OidcPkce {
    issuer: String,
    client_id: String,
    http: reqwest::Client,
    store_path: PathBuf,
    tokens: tokio::sync::Mutex<Option<StoredTokens>>,
}

impl OidcPkce {
    pub fn new(issuer: String, client_id: String, store_path: PathBuf) -> Self {
        let tokens = std::fs::read(&store_path)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok());
        Self {
            issuer,
            client_id,
            http: reqwest::Client::new(),
            store_path,
            tokens: tokio::sync::Mutex::new(tokens),
        }
    }

    async fn discovery(&self) -> Result<Discovery> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer.trim_end_matches('/')
        );
        Ok(self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Interactive login: open the browser, catch the loopback redirect,
    /// exchange the code, persist tokens (0600).
    pub async fn login(&self) -> Result<()> {
        let discovery = self.discovery().await?;
        let (verifier, challenge) = pkce_pair();
        let state: String = {
            let raw: [u8; 16] = rand::thread_rng().r#gen();
            URL_SAFE_NO_PAD.encode(raw)
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        // Must match the path declared for the "game-mgr" client in
        // deploy/idp/values.yaml's IDP_CLIENTS_JSON ("/callback") --
        // `clients::redirect_uri_allowed` matches a native client's
        // loopback redirect by path only (port is deliberately ignored,
        // since the OS assigns it at bind time), so a mismatched path
        // here made every login attempt fail with `redirect_uri not
        // allowed for this client` before a code was ever issued.
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");

        let mut auth_url = reqwest::Url::parse(&discovery.authorization_endpoint)?;
        auth_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", "openid offline_access")
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");

        tracing::info!(%auth_url, "opening browser for login");
        let _ = open::that(auth_url.as_str());

        // accept exactly one redirect, 5 minute timeout
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(300), listener.accept())
                .await
                .context("timed out waiting for the browser redirect")??;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or_default();
        let parsed = parse_redirect(first_line);
        let body = match &parsed {
            Ok(_) => "Login complete - you can close this tab.",
            Err(_) => "Login failed - check the application logs.",
        };
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await;
        let (code, returned_state) = parsed?;
        if returned_state != state {
            bail!("OAuth state mismatch — possible interception, aborting");
        }

        let tokens = self
            .exchange(
                &discovery.token_endpoint,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", &redirect_uri),
                    ("client_id", &self.client_id),
                    ("code_verifier", &verifier),
                ],
            )
            .await?;
        self.persist(tokens).await
    }

    async fn exchange(&self, token_endpoint: &str, form: &[(&str, &str)]) -> Result<StoredTokens> {
        let response: TokenResponse = self
            .http
            .post(token_endpoint)
            .form(form)
            .send()
            .await?
            .error_for_status()
            .context("token endpoint rejected the request")?
            .json()
            .await?;
        Ok(StoredTokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: time::OffsetDateTime::now_utc().unix_timestamp()
                + response.expires_in.unwrap_or(300),
        })
    }

    async fn persist(&self, tokens: StoredTokens) -> Result<()> {
        crate::platform::write_private(&self.store_path, &serde_json::to_vec(&tokens)?)?;
        *self.tokens.lock().await = Some(tokens);
        Ok(())
    }

    pub async fn logged_in(&self) -> bool {
        self.tokens.lock().await.is_some()
    }
}

#[async_trait::async_trait]
impl TokenProvider for OidcPkce {
    async fn bearer(&self) -> Result<String> {
        let current = self.tokens.lock().await.clone();
        let Some(tokens) = current else {
            bail!("not signed in — click \"Sign in\" in the client");
        };
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if tokens.expires_at - 60 > now {
            return Ok(tokens.access_token);
        }
        let Some(refresh) = tokens.refresh_token.clone() else {
            bail!("access token expired and no refresh token — log in again");
        };
        let discovery = self.discovery().await?;
        let refreshed = self
            .exchange(
                &discovery.token_endpoint,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh),
                    ("client_id", &self.client_id),
                ],
            )
            .await
            .context("token refresh failed — log in again")?;
        let access = refreshed.access_token.clone();
        self.persist(refreshed).await?;
        Ok(access)
    }

    fn describe(&self) -> String {
        format!("OIDC PKCE against {}", self.issuer)
    }

    async fn interactive_login(&self) -> Result<()> {
        self.login().await
    }

    async fn logged_in(&self) -> bool {
        OidcPkce::logged_in(self).await
    }
}

/// Provider selection: an OIDC issuer + native client_id configured (the
/// default now that `ClientConfig`'s own defaults point at the real
/// deployed IDP -- see that struct's doc comment) gets the real PKCE
/// flow; otherwise `NotConfigured`, which fails loudly rather than
/// silently faking a session.
pub fn provider_from_config(config: &crate::config::ClientConfig) -> Arc<dyn TokenProvider> {
    match (&config.oidc_issuer, &config.oidc_native_client_id) {
        (Some(issuer), Some(client_id)) => Arc::new(OidcPkce::new(
            issuer.clone(),
            client_id.clone(),
            crate::paths::auth_token_file(),
        )),
        _ => {
            tracing::warn!(
                "no OIDC issuer configured -- set oidc_issuer/oidc_native_client_id in \
                 config.toml; every authenticated request will fail until then"
            );
            Arc::new(NotConfigured)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 appendix B
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_pair_is_random_and_consistent() {
        let (v1, c1) = pkce_pair();
        let (v2, _) = pkce_pair();
        assert_ne!(v1, v2);
        assert_eq!(pkce_challenge(&v1), c1);
        assert!(v1.len() >= 43, "verifier must satisfy RFC length rules");
    }

    #[test]
    fn redirect_parsing_extracts_code_and_state() {
        let (code, state) = parse_redirect("GET /cb?code=abc%2F123&state=xyz HTTP/1.1").unwrap();
        assert_eq!(code, "abc/123");
        assert_eq!(state, "xyz");

        assert!(parse_redirect("GET /cb?error=access_denied HTTP/1.1").is_err());
        assert!(parse_redirect("GET /cb?code=only HTTP/1.1").is_err());
    }

    #[tokio::test]
    async fn static_token_returns_configured_value() {
        let provider = StaticToken("hello".into());
        assert_eq!(provider.bearer().await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn refresh_uses_token_endpoint() {
        use axum::{Router, routing::get, routing::post};

        // stub IdP: discovery + token endpoint that returns a fresh token
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(|| async {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": "http://unused.invalid/auth",
                        "token_endpoint": format!("http://{}/token", ADDR.get().unwrap()),
                    }))
                }),
            )
            .route(
                "/token",
                post(|body: String| async move {
                    assert!(body.contains("grant_type=refresh_token"));
                    assert!(body.contains("refresh_token=old-refresh"));
                    axum::Json(serde_json::json!({
                        "access_token": "new-access",
                        "refresh_token": "new-refresh",
                        "expires_in": 3600
                    }))
                }),
            );
        static ADDR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        ADDR.set(listener.local_addr().unwrap().to_string())
            .unwrap();
        let addr = ADDR.get().unwrap().clone();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("auth.json");
        let provider = OidcPkce::new(format!("http://{addr}"), "gm-native".into(), store.clone());
        // seed an expired token with a refresh token
        provider
            .persist(StoredTokens {
                access_token: "stale".into(),
                refresh_token: Some("old-refresh".into()),
                expires_at: 0,
            })
            .await
            .unwrap();

        let bearer = provider.bearer().await.unwrap();
        assert_eq!(bearer, "new-access");

        // persisted to disk with private permissions (Unix only -- see
        // crate::platform's `#[cfg(windows)]` `write_private` doc comment on
        // why there's no equivalent enforced-permissions assertion here yet)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let stored: StoredTokens = serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
        assert_eq!(stored.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[tokio::test]
    async fn valid_access_token_is_reused_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let provider = OidcPkce::new(
            "http://idp.invalid".into(), // any network use would fail
            "gm-native".into(),
            dir.path().join("auth.json"),
        );
        provider
            .persist(StoredTokens {
                access_token: "current".into(),
                refresh_token: None,
                expires_at: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
            })
            .await
            .unwrap();
        assert_eq!(provider.bearer().await.unwrap(), "current");
    }
}
