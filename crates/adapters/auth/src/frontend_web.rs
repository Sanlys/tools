//! Browser-redirect + PKCE login flow and the `LoginWidget` egui component,
//! for a tool frontend compiled to wasm (embedded in the portal, or served
//! standalone at the tool's own subdomain).
//!
//! The actual passkey ceremony never happens here -- signing in is a plain
//! top-level navigation to the IDP's own `/oauth/authorize` (and, on
//! success, back again with `?code=...`), so this module never touches
//! WebAuthn/`navigator.credentials` itself. That keeps every tool's wasm
//! bundle small and means the exact same flow works for a hypothetical
//! external, non-Rust OAuth client redirecting a browser at the same IDP.

use crate::{pkce, AuthConfig};
use serde::Deserialize;

const VERIFIER_KEY: &str = "auth_pkce_verifier";
const STATE_KEY: &str = "auth_pkce_state";
const SILENT_KEY: &str = "auth_silent_attempt";

/// Bare-minimum claims read out of the (unverified, client-side) JWT payload
/// purely to drive the UI -- e.g. showing initials and gating which panels
/// are enabled. This is **not** a security boundary: every backend call
/// still carries the raw token and gets it verified server-side by
/// [`crate::backend::AuthUser`]. A user who edits their own browser's copy
/// of these claims only fools their own client-side UI, never the backend.
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

/// Drop-in egui widget: draws a "Sign in" button when logged out, or a small
/// initials avatar with a sign-out menu when logged in. Add one field of
/// this type to your panel/app state, call [`LoginWidget::tick`] once per
/// frame, and [`LoginWidget::ui`] wherever you want it drawn (e.g. a
/// `TopBottomPanel`).
#[derive(Default)]
pub struct LoginWidget {
    config: Option<AuthConfig>,
    session: Option<Session>,
    checked_redirect: bool,
    menu_open: bool,
    error: Option<String>,
}

impl LoginWidget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Supply the runtime auth config (fetched the same way as the tool
    /// registry, via `platform_config::JsonResource<AuthConfig>` against
    /// this app's own `/config/auth.json`). Safe to call every frame; it's
    /// a no-op once already set.
    pub fn set_config(&mut self, config: AuthConfig) {
        if self.config.is_none() {
            self.config = Some(config);
        }
    }

    /// The runtime auth config once known (after the first [`set_config`]
    /// call) -- lets a host panel that already holds a `LoginWidget` (e.g.
    /// the portal's own top-bar login) find the IDP's `issuer_url` to make
    /// its own authenticated calls against, without a second fetch.
    pub fn config(&self) -> Option<&AuthConfig> {
        self.config.as_ref()
    }

    /// Call once per frame (e.g. from `Panel::tick`). Finishes the
    /// redirect-back code exchange the first time it runs after a login
    /// redirect, and restores a previously-stored session on first load.
    pub fn tick(&mut self, ctx: &egui::Context) {
        if self.checked_redirect {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        self.checked_redirect = true;

        if self.session.is_none() {
            self.session = restore_session();
        }

        if let Some((code, state)) = redirect_code_and_state() {
            clear_query_params();
            let expected_state = take_storage(STATE_KEY);
            if expected_state.as_deref() != Some(state.as_str()) {
                self.error = Some("login response had an unexpected state parameter".into());
                return;
            }
            let Some(verifier) = take_storage(VERIFIER_KEY) else {
                self.error = Some("missing PKCE verifier (was sessionStorage cleared?)".into());
                return;
            };
            let ctx = ctx.clone();
            exchange_code(config, code, verifier, move |result| {
                ctx.request_repaint();
                match result {
                    Ok(session) => {
                        store_session(&session);
                        SESSION_RESULT.with(|cell| *cell.borrow_mut() = Some(Ok(session)));
                    }
                    Err(err) => {
                        SESSION_RESULT.with(|cell| *cell.borrow_mut() = Some(Err(err)));
                    }
                }
            });
        } else if let Some(error) = redirect_error() {
            clear_query_params();
            let was_silent = take_storage(SILENT_KEY).is_some();
            if !was_silent {
                self.error = Some(format!("sign-in failed: {error}"));
            }
        }

        // Pick up a completed exchange from a previous frame's callback.
        if let Some(result) = SESSION_RESULT.with(|cell| cell.borrow_mut().take()) {
            match result {
                Ok(session) => self.session = Some(session),
                Err(err) => self.error = Some(err),
            }
        }
    }

    /// `true` once a valid (unexpired, per the client-side JWT claim) token
    /// is held.
    pub fn is_authenticated(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| !is_expired(&s.claims))
    }

    /// `true` if the current user was granted `role` for *this app's*
    /// client_id (per the token's `roles` claim). Client-side only -- the
    /// backend independently re-checks this via `AuthUser::require_role`.
    pub fn has_role(&self, role: &str) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| !is_expired(&s.claims) && s.claims.roles.iter().any(|r| r == role))
    }

    /// The bearer token to send as `Authorization: Bearer <token>` on
    /// requests to this app's own backend. `None` when logged out.
    pub fn bearer_token(&self) -> Option<String> {
        self.session
            .as_ref()
            .filter(|s| !is_expired(&s.claims))
            .map(|s| s.access_token.clone())
    }

    /// Begin an interactive login: navigates the whole page to the IDP.
    /// Call this from a "Sign in" button click.
    pub fn start_login(&self) {
        if let Some(config) = &self.config {
            navigate_to_authorize(config, false);
        }
    }

    /// Opt-in silent SSO: attempts `prompt=none` against the IDP so a
    /// second tool's tab can pick up an existing IDP session without a
    /// visible passkey prompt. Causes one visible top-level redirect on
    /// first load if there's no existing IDP session (which then bounces
    /// straight back with `error=login_required`, silently, per `tick`'s
    /// handling above) -- not called automatically, since that redirect
    /// flash is a real UX cost; call it explicitly if your tool wants it.
    pub fn attempt_silent_sso(&self) {
        if self.session.is_none() {
            if let Some(config) = &self.config {
                set_storage(SILENT_KEY, "1");
                navigate_to_authorize(config, true);
            }
        }
    }

    pub fn sign_out(&mut self) {
        self.session = None;
        self.menu_open = false;
        clear_storage("auth_session");
    }

    /// Draws the widget: a "Sign in" button, or an initials avatar + menu.
    /// Typically called inside a `egui::TopBottomPanel`/`egui::menu::bar`.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = self.error.clone() {
            ui.colored_label(egui::Color32::RED, &err);
            if ui.small_button("dismiss").clicked() {
                self.error = None;
            }
            return;
        }

        if self.config.is_none() {
            ui.spinner();
            return;
        }

        if self.is_authenticated() {
            let label = self
                .session
                .as_ref()
                .and_then(|s| s.claims.preferred_username.clone())
                .unwrap_or_else(|| "?".to_string());
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
    let now = (js_sys_now_secs()) as i64;
    claims.exp != 0 && claims.exp < now
}

// ── Browser plumbing ─────────────────────────────────────────────────────

fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

fn session_storage() -> Option<web_sys::Storage> {
    window()?.session_storage().ok()?
}

fn set_storage(key: &str, value: &str) {
    if let Some(s) = session_storage() {
        let _ = s.set_item(key, value);
    }
}

fn take_storage(key: &str) -> Option<String> {
    let storage = session_storage()?;
    let value = storage.get_item(key).ok().flatten();
    let _ = storage.remove_item(key);
    value
}

fn clear_storage(key: &str) {
    if let Some(s) = session_storage() {
        let _ = s.remove_item(key);
    }
}

fn current_origin_and_path() -> Option<(String, String)> {
    let loc = window()?.location();
    let origin = loc.origin().ok()?;
    let pathname = loc.pathname().ok()?;
    Some((origin, pathname))
}

fn redirect_uri() -> Option<String> {
    let (origin, path) = current_origin_and_path()?;
    Some(format!("{origin}{path}"))
}

fn query_params() -> Option<web_sys::UrlSearchParams> {
    let search = window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search).ok()
}

fn redirect_code_and_state() -> Option<(String, String)> {
    let params = query_params()?;
    let code = params.get("code")?;
    let state = params.get("state")?;
    Some((code, state))
}

fn redirect_error() -> Option<String> {
    query_params()?.get("error")
}

fn clear_query_params() {
    let Some((origin, path)) = current_origin_and_path() else {
        return;
    };
    if let Some(win) = window() {
        if let Ok(history) = win.history() {
            let _ = history.replace_state_with_url(
                &wasm_bindgen::JsValue::NULL,
                "",
                Some(&format!("{origin}{path}")),
            );
        }
    }
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let _ = getrandom::getrandom(&mut buf);
    buf
}

fn random_url_safe_string(n_bytes: usize) -> String {
    pkce::verifier_from_bytes(&random_bytes(n_bytes))
}

fn js_sys_now_secs() -> f64 {
    js_sys::Date::now() / 1000.0
}

fn navigate_to_authorize(config: &AuthConfig, silent: bool) {
    let Some(redirect_uri) = redirect_uri() else {
        return;
    };
    let verifier = random_url_safe_string(32);
    let challenge = pkce::challenge_from_verifier(&verifier);
    let state = random_url_safe_string(16);
    set_storage(VERIFIER_KEY, &verifier);
    set_storage(STATE_KEY, &state);

    let mut url = format!(
        "{}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&scope=openid+profile&state={}&code_challenge={}&code_challenge_method=S256",
        config.issuer_url.trim_end_matches('/'),
        urlencoding_encode(&config.client_id),
        urlencoding_encode(&redirect_uri),
        urlencoding_encode(&state),
        urlencoding_encode(&challenge),
    );
    if silent {
        url.push_str("&prompt=none");
    }
    if let Some(win) = window() {
        let _ = win.location().set_href(&url);
    }
}

fn urlencoding_encode(s: &str) -> String {
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

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

thread_local! {
    static SESSION_RESULT: std::cell::RefCell<Option<Result<Session, String>>> =
        const { std::cell::RefCell::new(None) };
}

fn exchange_code(
    config: AuthConfig,
    code: String,
    verifier: String,
    on_done: impl FnOnce(Result<Session, String>) + Send + 'static,
) {
    let Some(redirect_uri) = redirect_uri() else {
        on_done(Err("could not determine redirect_uri".into()));
        return;
    };
    let url = format!("{}/oauth/token", config.issuer_url.trim_end_matches('/'));
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding_encode(&code),
        urlencoding_encode(&redirect_uri),
        urlencoding_encode(&config.client_id),
        urlencoding_encode(&verifier),
    );
    let mut request = ehttp::Request::post(url, body.into_bytes());
    request.headers = ehttp::Headers::new(&[
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Accept", "application/json"),
    ]);
    ehttp::fetch(request, move |response| {
        let result = (|| -> Result<Session, String> {
            let resp = response.map_err(|e| e.to_string())?;
            if !resp.ok {
                return Err(format!("token endpoint returned {}", resp.status));
            }
            let token: TokenResponse = serde_json::from_slice(&resp.bytes)
                .map_err(|e| format!("decoding token response: {e}"))?;
            let claims = decode_claims_unverified(&token.access_token)?;
            if let Some(refresh) = &token.refresh_token {
                set_storage("auth_refresh_token", refresh);
            }
            Ok(Session {
                access_token: token.access_token,
                claims,
            })
        })();
        on_done(result);
    });
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

fn store_session(session: &Session) {
    set_storage("auth_access_token", &session.access_token);
}

fn restore_session() -> Option<Session> {
    let token = session_storage()?
        .get_item("auth_access_token")
        .ok()
        .flatten()?;
    let claims = decode_claims_unverified(&token).ok()?;
    if is_expired(&claims) {
        return None;
    }
    Some(Session {
        access_token: token,
        claims,
    })
}
