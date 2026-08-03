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
//!
//! Every storage key here is namespaced by `client_id` (`storage_key`) --
//! the portal's own top-bar `LoginWidget` (client_id "portal") and an
//! embedded tool's own `LoginWidget` (e.g. client_id "hello") run in the
//! exact same browser tab/origin, so unscoped keys would have them silently
//! clobber each other's session (whichever signed in most recently would
//! win, and the other would show that session's claims/roles under the
//! wrong audience). This was a real, previously unnoticed bug: `sign_out`
//! also used to clear a key ("auth_session") that was never the one
//! actually written, so "sign out" never forgot anything at all -- that
//! plus the sharing bug together produced "sign out, refresh, still signed
//! in" and "the wrong tool's roles show up".
//!
//! Two different Web Storage backends are used deliberately, not just one:
//! the access/refresh tokens live in `localStorage`, since the IDP issues a
//! refresh token good for 30 days (`apps/idp/backend`'s `REFRESH_TTL_DAYS`)
//! and the whole point of `tick`'s silent-refresh logic is that a session
//! should survive well past a single tab's lifetime. `sessionStorage` (tied
//! to one tab, cleared on close) previously held these too, which meant
//! closing the tab -- or the whole browser -- silently discarded a
//! perfectly valid 30-day refresh token, forcing a fresh passkey prompt far
//! more often than the IDP's own token lifetimes ever intended. The PKCE
//! verifier/state and the silent-SSO marker stay in `sessionStorage`: they
//! only ever need to survive the one top-level redirect round trip to the
//! IDP and back in the *same* tab, and letting them die with the tab (an
//! abandoned login attempt) is the correct behavior, not a bug.

use crate::{pkce, AuthConfig};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;

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

#[derive(Clone)]
struct Session {
    access_token: String,
    refresh_token: Option<String>,
    claims: UnverifiedClaims,
}

/// Drop-in egui widget: draws a "Sign in" button when logged out, or a small
/// initials avatar with a sign-out menu when logged in. Add one field of
/// this type to your panel/app state, call [`LoginWidget::tick`] once per
/// frame, and [`LoginWidget::ui`] wherever you want it drawn (e.g. a
/// `TopBottomPanel`). Use [`LoginWidget::ui_compact`] instead when this
/// widget is embedded in a host (like the portal) that already shows the
/// signed-in user elsewhere -- see that method's doc comment.
#[derive(Default)]
pub struct LoginWidget {
    config: Option<AuthConfig>,
    session: Option<Session>,
    checked_redirect: bool,
    /// The `access_token` we last attempted (successfully or not) to
    /// refresh away from, so a hard refresh failure (e.g. a revoked
    /// refresh token) doesn't retry every single frame forever -- but a
    /// *later*, different expired token (after a successful refresh, or a
    /// fresh login) still gets its own attempt.
    refresh_attempted_for: Option<String>,
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
    /// redirect, restores a previously-stored session on first load, and
    /// silently refreshes an expired access token in the background if a
    /// refresh token is available (without this, a session left open past
    /// the 15-minute access-token lifetime would look "signed in" one
    /// moment and silently revert to "Sign in" the next, with no visible
    /// cause).
    pub fn tick(&mut self, ctx: &egui::Context) {
        let Some(config) = self.config.clone() else {
            return;
        };

        if !self.checked_redirect {
            self.checked_redirect = true;

            if self.session.is_none() {
                self.session = restore_session(&config.client_id);
            }

            if let Some((code, state)) = redirect_code_and_state() {
                clear_query_params();
                let expected_state = take_storage(&storage_key(&config.client_id, "pkce_state"));
                if expected_state.as_deref() != Some(state.as_str()) {
                    self.error = Some("login response had an unexpected state parameter".into());
                    return;
                }
                let Some(verifier) = take_storage(&storage_key(&config.client_id, "pkce_verifier"))
                else {
                    self.error = Some("missing PKCE verifier (was sessionStorage cleared?)".into());
                    return;
                };
                let ctx2 = ctx.clone();
                let client_id = config.client_id.clone();
                exchange_code(config.clone(), code, verifier, move |result| {
                    if let Ok(session) = &result {
                        store_session(&client_id, session);
                    }
                    set_pending_result(&client_id, result);
                    ctx2.request_repaint();
                });
            } else if let Some(error) = redirect_error() {
                clear_query_params();
                let was_silent =
                    take_storage(&storage_key(&config.client_id, "silent_attempt")).is_some();
                if !was_silent {
                    self.error = Some(format!("sign-in failed: {error}"));
                }
            }
        }

        // Pick up a completed exchange (or refresh, below) from a previous
        // frame's callback.
        if let Some(result) = take_pending_result(&config.client_id) {
            match result {
                Ok(session) => self.session = Some(session),
                Err(err) => {
                    // A refresh can fail with a legitimate-looking
                    // "invalid_grant" (`apps/idp/backend`'s
                    // `token_refresh` rejects a refresh token that's
                    // already been consumed) even though the session
                    // itself is fine: refresh tokens are single-use and
                    // rotate on every use, and the access/refresh tokens
                    // now live in `localStorage` (shared across every tab
                    // of this origin, unlike the `sessionStorage` they
                    // used to live in) -- so a second tab, or a page
                    // reload racing an in-flight refresh from the tab
                    // being replaced, can land its own refresh attempt
                    // against a token another one already consumed and
                    // rotated a moment earlier. Check storage again before
                    // surfacing this as a real error: if it now holds a
                    // *different*, still-valid session, some other
                    // in-flight attempt already won that race and rotated
                    // the token first, so adopt its result instead of
                    // bouncing this tab to "signed out".
                    match restore_session(&config.client_id) {
                        Some(fresh) if !is_expired(&fresh.claims) => self.session = Some(fresh),
                        _ => self.error = Some(err),
                    }
                }
            }
        }

        // Silently refresh an expired access token if we have a refresh
        // token stashed for it -- see this method's doc comment. Guarded by
        // `refresh_attempted_for` so a hard failure (revoked refresh token)
        // doesn't retry every frame forever, while still allowing a fresh
        // attempt once the token that expired is a *different* one (a
        // successful refresh, or a brand new login, since either replaces
        // `access_token`).
        if let Some(session) = self.session.clone() {
            let already_expired = is_expired(&session.claims);
            if already_expired {
                // Before racing anyone else for a refresh, check whether
                // someone already won: another tab (or a previous
                // in-flight attempt from before a reload) may have already
                // refreshed this exact session and written a fresher one
                // to `localStorage` -- see the doc comment above on why
                // that's possible now. Adopting it here avoids firing a
                // redundant `refresh_token` request that would otherwise
                // try to consume an already-rotated single-use token.
                if let Some(fresh) = restore_session(&config.client_id) {
                    if fresh.access_token != session.access_token && !is_expired(&fresh.claims) {
                        self.session = Some(fresh);
                        return;
                    }
                }
            }
            let already_attempted =
                self.refresh_attempted_for.as_deref() == Some(&session.access_token);
            if already_expired && !already_attempted {
                self.refresh_attempted_for = Some(session.access_token.clone());
                if let Some(refresh_token) = session.refresh_token {
                    let ctx2 = ctx.clone();
                    let client_id = config.client_id.clone();
                    refresh_session(config, refresh_token, move |result| {
                        if let Ok(session) = &result {
                            store_session(&client_id, session);
                        }
                        set_pending_result(&client_id, result);
                        ctx2.request_repaint();
                    });
                }
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
                set_storage(&storage_key(&config.client_id, "silent_attempt"), "1");
                navigate_to_authorize(config, true);
            }
        }
    }

    pub fn sign_out(&mut self) {
        self.session = None;
        self.menu_open = false;
        if let Some(config) = &self.config {
            clear_local(&storage_key(&config.client_id, "access_token"));
            clear_local(&storage_key(&config.client_id, "refresh_token"));
        }
    }

    /// Draws the widget: a "Sign in" button, or an initials avatar + menu.
    /// Typically called inside a `egui::TopBottomPanel`/`egui::menu::bar`.
    /// Use this for the *one* place per page that should show who's signed
    /// in (e.g. the portal's own top bar) -- see [`ui_compact`](Self::ui_compact)
    /// for every other embedded panel.
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

    /// Same sign-in/sign-out capability as [`ui`](Self::ui), but without
    /// drawing the avatar/dropdown when already signed in -- for a tool's
    /// panel embedded in a host (the portal) that already shows the
    /// signed-in user somewhere else on the page. A "Sign in" button still
    /// appears when this specific client_id's *own* session (separately
    /// authenticated, per standard OIDC audience scoping -- see this
    /// crate's docs) isn't established yet; once it is, this draws nothing
    /// beyond a small text "Sign out" link, no circular avatar.
    pub fn ui_compact(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = self.error.clone() {
            ui.colored_label(egui::Color32::RED, &err);
            if ui.small_button("dismiss").clicked() {
                self.error = None;
            }
            return;
        }

        if self.config.is_none() {
            return;
        }

        if self.is_authenticated() {
            if ui.small_button("Sign out").clicked() {
                self.sign_out();
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

/// Every `sessionStorage` key this module uses is scoped by `client_id` --
/// see this module's doc comment for why an unscoped key is a real bug,
/// not just a style preference.
fn storage_key(client_id: &str, name: &str) -> String {
    format!("auth:{client_id}:{name}")
}

// ── Per-client-id pending async results ──────────────────────────────────
//
// `ehttp::fetch`'s callback must be `Send` (even on wasm, where nothing is
// actually multi-threaded), which rules out capturing something like `Rc`
// directly. A thread-local keyed by `client_id` sidesteps that: only
// `String`/`Result<Session, String>` (themselves `Send`) cross into the
// closure, and the thread-local itself is read back out on the same
// (only) thread from `tick`.

thread_local! {
    static PENDING_RESULTS: RefCell<HashMap<String, Result<Session, String>>> =
        RefCell::new(HashMap::new());
}

fn set_pending_result(client_id: &str, result: Result<Session, String>) {
    PENDING_RESULTS.with(|cell| {
        cell.borrow_mut().insert(client_id.to_string(), result);
    });
}

fn take_pending_result(client_id: &str) -> Option<Result<Session, String>> {
    PENDING_RESULTS.with(|cell| cell.borrow_mut().remove(client_id))
}

// ── Browser plumbing ─────────────────────────────────────────────────────

fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

fn session_storage() -> Option<web_sys::Storage> {
    window()?.session_storage().ok()?
}

/// Backs the access/refresh tokens -- survives tab closes and browser
/// restarts, matching the IDP's own 30-day refresh token lifetime. See this
/// module's doc comment for why this differs from [`session_storage`].
fn local_storage() -> Option<web_sys::Storage> {
    window()?.local_storage().ok()?
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

fn set_local(key: &str, value: &str) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(key, value);
    }
}

fn get_local(key: &str) -> Option<String> {
    local_storage()?.get_item(key).ok().flatten()
}

fn clear_local(key: &str) {
    if let Some(s) = local_storage() {
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
    set_storage(&storage_key(&config.client_id, "pkce_verifier"), &verifier);
    set_storage(&storage_key(&config.client_id, "pkce_state"), &state);

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
    post_token_request(url, body, on_done);
}

/// Exchanges a refresh token for a fresh access token (and, if the IDP
/// rotates it, a fresh refresh token) -- see `LoginWidget::tick`'s doc
/// comment on why this needs to happen automatically rather than leaving
/// an expired-but-refreshable session to just look silently signed out.
fn refresh_session(
    config: AuthConfig,
    refresh_token: String,
    on_done: impl FnOnce(Result<Session, String>) + Send + 'static,
) {
    let url = format!("{}/oauth/token", config.issuer_url.trim_end_matches('/'));
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlencoding_encode(&refresh_token),
        urlencoding_encode(&config.client_id),
    );
    post_token_request(url, body, on_done);
}

fn post_token_request(
    url: String,
    body: String,
    on_done: impl FnOnce(Result<Session, String>) + Send + 'static,
) {
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
            Ok(Session {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
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

fn store_session(client_id: &str, session: &Session) {
    set_local(
        &storage_key(client_id, "access_token"),
        &session.access_token,
    );
    if let Some(refresh) = &session.refresh_token {
        set_local(&storage_key(client_id, "refresh_token"), refresh);
    }
}

fn restore_session(client_id: &str) -> Option<Session> {
    let access_token = get_local(&storage_key(client_id, "access_token"))?;
    let claims = decode_claims_unverified(&access_token).ok()?;
    let refresh_token = get_local(&storage_key(client_id, "refresh_token"));
    // Deliberately not gated on `is_expired` here (unlike the old version):
    // an expired-but-present session is still returned so `tick` can try a
    // silent refresh using `refresh_token` instead of just forgetting it.
    Some(Session {
        access_token,
        refresh_token,
        claims,
    })
}
