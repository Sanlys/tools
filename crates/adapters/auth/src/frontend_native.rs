//! RFC 8252 ("OAuth 2.0 for Native Apps") loopback-redirect login flow for a
//! tool's standalone `eframe` binary (`platform_core::standalone::run`).
//!
//! There's no browser environment here to redirect through, so instead:
//! open the *system* browser to the IDP's `/oauth/authorize` (where the
//! passkey ceremony runs, same as the web flow), and catch the redirect
//! with a one-shot HTTP listener bound to `127.0.0.1:0` (OS-assigned port).
//! The IDP allows any loopback port for a client marked `native: true` in
//! its `IDP_CLIENTS_JSON` entry -- see `apps/idp/backend`'s redirect_uri
//! validation.
//!
//! The refresh token is persisted in the OS keyring (via the `keyring`
//! crate) so a second run of the standalone binary doesn't need a fresh
//! passkey prompt.

use crate::{pkce, AuthConfig};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

const KEYRING_SERVICE: &str = "tools-idp";

#[derive(Debug, Clone, Default, Deserialize)]
struct UnverifiedClaims {
    #[allow(dead_code)] // part of the JWT payload; not currently shown in the UI
    sub: String,
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    exp: i64,
}

struct Session {
    access_token: String,
    claims: UnverifiedClaims,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Blocking fetch of this app's own `/config/auth.json` (served by
/// `crate::backend::config_route`) -- native binaries construct their
/// `LoginWidget` synchronously at startup rather than polling a
/// `JsonResource` every frame the way the wasm build does.
pub fn fetch_auth_config(api_base_url: &str) -> Result<AuthConfig, String> {
    let url = format!("{}/config/auth.json", api_base_url.trim_end_matches('/'));
    ureq::get(&url)
        .call()
        .map_err(|e| format!("fetching auth config: {e}"))?
        .into_json()
        .map_err(|e| format!("decoding auth config: {e}"))
}

/// Same widget shape as the wasm version (`is_authenticated`, `has_role`,
/// `bearer_token`, `ui`), driven by a background `std::thread` instead of
/// browser JS callbacks -- egui's `update` loop can't block, so the loopback
/// listener and the token exchange both happen off the UI thread and post
/// their result into a shared `Mutex`.
pub struct LoginWidget {
    config: AuthConfig,
    /// Set instead of ever constructing with a broken/empty [`AuthConfig`]
    /// (e.g. `fetch_auth_config` failed) -- shown permanently in [`ui`](Self::ui)
    /// instead of a "Sign in" button that would otherwise navigate to a
    /// malformed `issuer_url` and hang [`start_login`] forever waiting for a
    /// browser redirect that can never arrive.
    config_error: Option<String>,
    session: Arc<Mutex<Option<Session>>>,
    error: Arc<Mutex<Option<String>>>,
    login_in_progress: Arc<Mutex<bool>>,
    tried_restore: bool,
    menu_open: bool,
}

impl LoginWidget {
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config,
            config_error: None,
            session: Arc::new(Mutex::new(None)),
            error: Arc::new(Mutex::new(None)),
            login_in_progress: Arc::new(Mutex::new(false)),
            tried_restore: false,
            menu_open: false,
        }
    }

    /// Build a widget that can never attempt to sign in -- for when fetching
    /// this app's own `/config/auth.json` failed at startup. Without this,
    /// callers used to fall back to an empty `AuthConfig`, which let a user
    /// click "Sign in" and hang indefinitely: `start_login` would open the
    /// browser at a malformed (empty-issuer) URL that never redirects back,
    /// and the pre-fix `accept_one_callback` blocked on that forever with no
    /// timeout.
    pub fn with_config_error(err: impl Into<String>) -> Self {
        Self {
            config: AuthConfig {
                issuer_url: String::new(),
                client_id: String::new(),
            },
            config_error: Some(err.into()),
            session: Arc::new(Mutex::new(None)),
            error: Arc::new(Mutex::new(None)),
            login_in_progress: Arc::new(Mutex::new(false)),
            tried_restore: false,
            menu_open: false,
        }
    }

    fn keyring_entry(&self) -> Option<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, &self.config.client_id).ok()
    }

    /// The runtime auth config, once known -- mirrors
    /// `frontend_web::LoginWidget::config`'s signature (`None` there means
    /// "hasn't loaded yet"; here it means "failed to load", since native
    /// construction is synchronous) so a host that embeds this widget (e.g.
    /// the portal's own top bar) can read `issuer_url` the same way on
    /// either platform without a `cfg`-gated branch of its own.
    pub fn config(&self) -> Option<&AuthConfig> {
        if self.config_error.is_some() {
            None
        } else {
            Some(&self.config)
        }
    }

    /// Call once per frame. On first call, tries to silently restore a
    /// session from a refresh token in the OS keyring (left by a previous
    /// run); otherwise a no-op until [`LoginWidget::start_login`] is
    /// called.
    pub fn tick(&mut self, _ctx: &egui::Context) {
        if self.tried_restore || self.config_error.is_some() {
            return;
        }
        self.tried_restore = true;
        let Some(entry) = self.keyring_entry() else {
            return;
        };
        let Ok(refresh_token) = entry.get_password() else {
            return;
        };
        let config = self.config.clone();
        let session = self.session.clone();
        std::thread::spawn(move || {
            match refresh_tokens(&config, &refresh_token) {
                Ok((sess, new_refresh)) => {
                    if let (Some(rt), Ok(entry)) = (
                        new_refresh,
                        keyring::Entry::new(KEYRING_SERVICE, &config.client_id),
                    ) {
                        let _ = entry.set_password(&rt);
                    }
                    *session.lock().unwrap() = Some(sess);
                }
                Err(_) => {
                    // Stale/expired/revoked refresh token: silently fall
                    // back to logged-out rather than surfacing an error for
                    // something the user never explicitly asked to happen.
                }
            }
        });
    }

    pub fn is_authenticated(&self) -> bool {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| !is_expired(&s.claims))
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| !is_expired(&s.claims) && s.claims.roles.iter().any(|r| r == role))
    }

    pub fn bearer_token(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .filter(|s| !is_expired(&s.claims))
            .map(|s| s.access_token.clone())
    }

    fn username(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.claims.preferred_username.clone())
    }

    fn take_error(&self) -> Option<String> {
        self.error.lock().unwrap().take()
    }

    /// Opens the system browser to sign in. Runs the whole loopback +
    /// token-exchange dance on a background thread; poll [`is_authenticated`]
    /// on subsequent frames.
    pub fn start_login(&self) {
        if self.config_error.is_some() {
            return;
        }
        {
            let mut in_progress = self.login_in_progress.lock().unwrap();
            if *in_progress {
                return;
            }
            *in_progress = true;
        }
        let config = self.config.clone();
        let session = self.session.clone();
        let error = self.error.clone();
        let in_progress = self.login_in_progress.clone();
        std::thread::spawn(move || {
            let result = run_loopback_login(&config);
            *in_progress.lock().unwrap() = false;
            match result {
                Ok((sess, refresh_token)) => {
                    if let (Some(rt), Ok(entry)) = (
                        refresh_token,
                        keyring::Entry::new(KEYRING_SERVICE, &config.client_id),
                    ) {
                        let _ = entry.set_password(&rt);
                    }
                    *session.lock().unwrap() = Some(sess);
                }
                Err(err) => {
                    *error.lock().unwrap() = Some(err);
                }
            }
        });
    }

    pub fn sign_out(&mut self) {
        *self.session.lock().unwrap() = None;
        self.menu_open = false;
        if let Some(entry) = self.keyring_entry() {
            let _ = entry.delete_credential();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = &self.config_error {
            ui.colored_label(egui::Color32::RED, format!("sign-in unavailable: {err}"));
            return;
        }

        if let Some(err) = self.take_error() {
            ui.colored_label(egui::Color32::RED, &err);
            return;
        }

        if *self.login_in_progress.lock().unwrap() {
            ui.spinner();
            ui.label("waiting for browser sign-in...");
            // The background thread has its own timeout (see
            // `accept_one_callback`) and will give up on its own even if
            // the user never completes the browser flow, but that can take
            // minutes -- this lets them stop waiting immediately without
            // needing to kill the thread (harmless: it just keeps polling
            // its own loopback listener in the background until it times
            // out or succeeds, and a late success still logs them in).
            if ui.small_button("Cancel").clicked() {
                *self.login_in_progress.lock().unwrap() = false;
            }
            return;
        }

        if self.is_authenticated() {
            let label = self.username().unwrap_or_else(|| "?".to_string());
            let initials = initials_for(&label);
            let avatar = egui::Button::new(egui::RichText::new(initials).strong())
                .fill(egui::Color32::from_rgb(70, 110, 200))
                .rounding(egui::Rounding::same(14.0))
                .min_size(egui::vec2(28.0, 28.0));
            let resp = ui.add(avatar).on_hover_text(label.clone());
            if resp.clicked() {
                self.menu_open = !self.menu_open;
            }
            if self.menu_open {
                egui::Window::new("account")
                    .id(egui::Id::new("login_widget_menu"))
                    .title_bar(false)
                    .resizable(false)
                    .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 40.0))
                    .show(ui.ctx(), |ui| {
                        ui.label(egui::RichText::new(&label).strong());
                        ui.separator();
                        if ui.button("Sign out").clicked() {
                            self.sign_out();
                        }
                    });
            }
        } else if ui.button("Sign in").clicked() {
            self.start_login();
        }
    }
}

fn initials_for(name: &str) -> String {
    let up = name.to_uppercase();
    let mut chars = up.chars();
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        (Some(a), None) => a.to_string(),
        _ => "?".to_string(),
    }
}

fn is_expired(claims: &UnverifiedClaims) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    claims.exp != 0 && claims.exp < now
}

fn decode_claims_unverified(token: &str) -> Result<UnverifiedClaims, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "malformed JWT".to_string())?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| format!("decoding JWT payload: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parsing JWT claims: {e}"))
}

fn run_loopback_login(config: &AuthConfig) -> Result<(Session, Option<String>), String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("binding loopback listener: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let mut verifier_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut verifier_bytes);
    let verifier = pkce::verifier_from_bytes(&verifier_bytes);
    let challenge = pkce::challenge_from_verifier(&verifier);

    let mut state_bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut state_bytes);
    let state = pkce::verifier_from_bytes(&state_bytes);

    let url = format!(
        "{}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&scope=openid+profile&state={}&code_challenge={}&code_challenge_method=S256",
        config.issuer_url.trim_end_matches('/'),
        percent_encode(&config.client_id),
        percent_encode(&redirect_uri),
        percent_encode(&state),
        percent_encode(&challenge),
    );

    webbrowser::open(&url).map_err(|e| format!("opening system browser: {e}"))?;

    let (code, returned_state) = accept_one_callback(&listener)?;
    if returned_state != state {
        return Err("login response had an unexpected state parameter".into());
    }

    let (session, refresh_token) = exchange_code(config, &code, &verifier, &redirect_uri)?;
    Ok((session, refresh_token))
}

/// Waits (up to `LOGIN_TIMEOUT`) for exactly one HTTP request to land on the
/// loopback listener, replies with a small "you can close this tab" page,
/// and returns the `code`/`state` query params from the request line.
///
/// Uses a non-blocking poll loop rather than a plain blocking `accept()`:
/// the previous version blocked forever if the browser never redirected
/// back at all (bad `issuer_url`, the user closing the tab, the passkey
/// ceremony being abandoned, ...), which is exactly what left the "waiting
/// for browser sign-in..." spinner stuck permanently with no way out.
const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

fn accept_one_callback(listener: &TcpListener) -> Result<(String, String), String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("setting loopback listener non-blocking: {e}"))?;
    let deadline = std::time::Instant::now() + LOGIN_TIMEOUT;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("timed out waiting for the browser to complete sign-in".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(format!("accepting loopback connection: {e}")),
        }
    };
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("setting loopback stream blocking: {e}"))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("reading callback request: {e}"))?;
    // Drain the rest of the headers so the client doesn't see a broken pipe.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line.is_empty() {
            break;
        }
    }

    let body = "<html><body><p>Signed in -- you can close this tab.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());

    // request_line looks like: "GET /callback?code=XXX&state=YYY HTTP/1.1"
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "malformed callback request".to_string())?;
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query(query);
    if let Some(err) = params.get("error") {
        return Err(format!("login failed: {err}"));
    }
    let code = params.get("code").cloned().ok_or("callback missing code")?;
    let state = params
        .get("state")
        .cloned()
        .ok_or("callback missing state")?;
    Ok((code, state))
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            let key = it.next()?;
            let value = it.next().unwrap_or("");
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
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

fn exchange_code(
    config: &AuthConfig,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<(Session, Option<String>), String> {
    let url = format!("{}/oauth/token", config.issuer_url.trim_end_matches('/'));
    let response = ureq::post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &config.client_id),
            ("code_verifier", verifier),
        ])
        .map_err(|e| format!("token exchange failed: {e}"))?;
    let token: TokenResponse = response
        .into_json()
        .map_err(|e| format!("decoding token response: {e}"))?;
    let claims = decode_claims_unverified(&token.access_token)?;
    Ok((
        Session {
            access_token: token.access_token,
            claims,
        },
        token.refresh_token,
    ))
}

fn refresh_tokens(
    config: &AuthConfig,
    refresh_token: &str,
) -> Result<(Session, Option<String>), String> {
    let url = format!("{}/oauth/token", config.issuer_url.trim_end_matches('/'));
    let response = ureq::post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &config.client_id),
        ])
        .map_err(|e| format!("refresh failed: {e}"))?;
    let token: TokenResponse = response
        .into_json()
        .map_err(|e| format!("decoding token response: {e}"))?;
    let claims = decode_claims_unverified(&token.access_token)?;
    Ok((
        Session {
            access_token: token.access_token,
            claims,
        },
        token.refresh_token,
    ))
}
