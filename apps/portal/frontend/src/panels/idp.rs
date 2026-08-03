//! Egui panel for managing this IDP account -- profile, passkeys, sessions,
//! and (for admins) the whole IDP: users, apps/clients, invites, per-app
//! role grants, and per-app login access grants. All of it lives here in
//! the portal now, authenticated with the portal's own bearer token,
//! instead of only being reachable via the IDP's separate static web app
//! (`apps/idp/frontend/static/{profile,admin}.html`).
//!
//! The one thing that stays on the IDP's own origin is the actual
//! WebAuthn passkey ceremony (`navigator.credentials.create`) -- adding a
//! *new* passkey opens `${issuer_url}/profile` in a new tab for that one
//! step; everything else (viewing/deleting existing passkeys, sessions,
//! and all the admin actions) is a plain authenticated API call this panel
//! makes directly, the same way any other tool's backend would verify a
//! bearer token, except here the *caller* is a browser tab rather than a
//! server (see `apps/idp/backend/src/routes/oauth.rs::require_session`'s
//! Bearer-token acceptance path, added specifically to let this panel work
//! without a first-party session cookie on the IDP's own origin).
//!
//! This panel doesn't own a `LoginWidget` of its own -- unlike every other
//! tool's panel (which needs its own login scoped to its own client_id for
//! role claims), managing your own IDP account needs no role scoping at
//! all, just proof of who you are. So it reuses the portal's own top-bar
//! sign-in (`PortalApp::login`, client_id "portal") via [`IdpPanel::set_auth`],
//! called once per frame from `PortalApp::update` before this panel ticks.

use platform_config::JsonResource;
use platform_core::{Panel, PanelId};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct Me {
    id: String,
    username: String,
    display_name: String,
    is_admin: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Passkey {
    id: String,
    label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionInfo {
    id: String,
    last_seen_at: String,
    user_agent: Option<String>,
    ip_address: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AdminUser {
    id: String,
    username: String,
    display_name: String,
    is_admin: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct AdminClient {
    client_id: String,
    name: String,
    roles: Vec<String>,
    roles_claim: String,
    access_restricted: bool,
    managed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Invite {
    id: String,
    url: String,
    note: Option<String>,
    used: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RoleGrant {
    client_id: String,
    role: String,
}

pub struct IdpPanel {
    bearer_token: Option<String>,
    issuer_url: Option<String>,
    had_token: bool,

    me: JsonResource<Me>,
    passkeys: JsonResource<Vec<Passkey>>,
    sessions: JsonResource<Vec<SessionInfo>>,
    display_name_input: String,
    action_error: Option<String>,
    /// Set by `post_json`/`delete`'s `ehttp::fetch` callback on a failed or
    /// non-2xx response, and drained into `action_error` on the next
    /// `tick`. Every mutation in this panel used to be pure
    /// fire-and-forget (the response was never inspected at all), so a
    /// failed save/grant/delete -- a 403 from `require_admin`, a 400 from
    /// a validation error, a network blip -- looked identical to success:
    /// the JSON resource reset right after firing the request made the
    /// next `tick` refetch unchanged data, with nothing telling the admin
    /// *why* nothing changed. `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>`
    /// since `ehttp::fetch`'s callback must be `Send` even on wasm (see
    /// `crates/adapters/auth::frontend_web`'s identical reasoning).
    pending_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,

    admin_users: JsonResource<Vec<AdminUser>>,
    admin_clients: JsonResource<Vec<AdminClient>>,
    admin_invites: JsonResource<Vec<Invite>>,
    role_grants_for_user: JsonResource<Vec<RoleGrant>>,
    access_for_user: JsonResource<Vec<String>>,

    selected_user_id: String,
    selected_client_id: String,
    selected_role: String,
    invite_note_input: String,
    new_client_id: String,
    new_client_name: String,
    new_client_redirects: String,
    new_client_roles: String,
    new_client_claim: String,
    new_client_restricted: bool,
}

impl Default for IdpPanel {
    fn default() -> Self {
        Self {
            bearer_token: None,
            issuer_url: None,
            had_token: false,
            me: JsonResource::new(),
            passkeys: JsonResource::new(),
            sessions: JsonResource::new(),
            display_name_input: String::new(),
            action_error: None,
            pending_error: std::sync::Arc::new(std::sync::Mutex::new(None)),
            admin_users: JsonResource::new(),
            admin_clients: JsonResource::new(),
            admin_invites: JsonResource::new(),
            role_grants_for_user: JsonResource::new(),
            access_for_user: JsonResource::new(),
            selected_user_id: String::new(),
            selected_client_id: String::new(),
            selected_role: String::new(),
            invite_note_input: String::new(),
            new_client_id: String::new(),
            new_client_name: String::new(),
            new_client_redirects: String::new(),
            new_client_roles: String::new(),
            new_client_claim: "roles".to_string(),
            new_client_restricted: true,
        }
    }
}

impl IdpPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called once per frame by `PortalApp::update` with the portal's own
    /// top-bar login state -- see this module's doc comment on why this
    /// panel has no `LoginWidget` of its own.
    pub fn set_auth(&mut self, token: Option<String>, issuer_url: Option<String>) {
        let now_authenticated = token.is_some();
        if now_authenticated != self.had_token {
            *self = Self {
                bearer_token: None,
                issuer_url: None,
                had_token: false,
                ..Self::default()
            };
        }
        self.had_token = now_authenticated;
        self.bearer_token = token;
        self.issuer_url = issuer_url;
    }

    fn base(&self) -> Option<String> {
        self.issuer_url
            .as_deref()
            .map(|u| u.trim_end_matches('/').to_string())
    }

    fn post_json(&self, path: &str, body: &impl serde::Serialize) {
        let (Some(base), Some(token)) = (self.base(), self.bearer_token.clone()) else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(body) else {
            return;
        };
        let mut request = ehttp::Request::post(format!("{base}{path}"), bytes);
        request.headers = ehttp::Headers::new(&[
            ("Authorization", &format!("Bearer {token}")),
            ("Content-Type", "application/json"),
        ]);
        let pending_error = self.pending_error.clone();
        let path = path.to_string();
        ehttp::fetch(request, move |response| {
            report_mutation_failure(&pending_error, &path, response);
        });
    }

    fn delete(&self, path: &str) {
        let (Some(base), Some(token)) = (self.base(), self.bearer_token.clone()) else {
            return;
        };
        let mut request = ehttp::Request::post(format!("{base}{path}"), Vec::new());
        request.method = "DELETE".to_owned();
        request.headers = ehttp::Headers::new(&[("Authorization", &format!("Bearer {token}"))]);
        let pending_error = self.pending_error.clone();
        let path = path.to_string();
        ehttp::fetch(request, move |response| {
            report_mutation_failure(&pending_error, &path, response);
        });
    }

    fn profile_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profile");
        match self.me.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(me)) => {
                let me = me.clone();
                if self.display_name_input.is_empty() {
                    self.display_name_input = me.display_name.clone();
                }
                ui.label(format!("Username: {}", me.username));
                if me.is_admin {
                    ui.label(egui::RichText::new("Admin").strong());
                }
                ui.horizontal(|ui| {
                    ui.label("Display name:");
                    ui.text_edit_singleline(&mut self.display_name_input);
                    if ui.button("Save").clicked() {
                        self.post_json(
                            "/api/profile",
                            &serde_json::json!({ "display_name": self.display_name_input }),
                        );
                    }
                });
                if self.selected_user_id.is_empty() {
                    self.selected_user_id = me.id.clone();
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }
    }

    fn passkeys_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Passkeys");
        if let Some(base) = self.base() {
            ui.hyperlink_to(
                "Add a passkey (opens the IDP's own profile page)",
                format!("{base}/profile"),
            );
        }
        match self.passkeys.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(passkeys)) => {
                let passkeys = passkeys.clone();
                if passkeys.is_empty() {
                    ui.label("No passkeys registered.");
                }
                for pk in &passkeys {
                    ui.horizontal(|ui| {
                        ui.label(pk.label.as_deref().unwrap_or("(unlabeled)"));
                        if ui.small_button("Delete").clicked() {
                            self.delete(&format!("/api/passkeys/{}", pk.id));
                            self.passkeys = JsonResource::new();
                        }
                    });
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }
    }

    fn sessions_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sessions");
        match self.sessions.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(sessions)) => {
                let sessions = sessions.clone();
                for s in &sessions {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} -- last seen {} ({})",
                            s.user_agent.as_deref().unwrap_or("unknown device"),
                            s.last_seen_at,
                            s.ip_address.as_deref().unwrap_or("unknown ip"),
                        ));
                        if ui.small_button("Revoke").clicked() {
                            self.delete(&format!("/api/sessions/{}", s.id));
                            self.sessions = JsonResource::new();
                        }
                    });
                }
                if ui.button("Revoke all other sessions").clicked() {
                    self.delete("/api/sessions");
                    self.sessions = JsonResource::new();
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }
    }

    fn admin_users_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Admin: Users");
        match self.admin_users.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(users)) => {
                let users = users.clone();
                egui::Grid::new("idp_admin_users_grid")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("username");
                        ui.strong("display name");
                        ui.strong("admin");
                        ui.end_row();
                        for u in &users {
                            ui.label(&u.username);
                            ui.label(&u.display_name);
                            ui.label(if u.is_admin { "yes" } else { "" });
                            if ui.small_button("Delete").clicked() {
                                self.delete(&format!("/api/admin/users/{}", u.id));
                                self.admin_users = JsonResource::new();
                                // Otherwise the "Role grants & app login
                                // access" section below keeps showing this
                                // now-deleted user's stale cached grants
                                // until an admin happens to reselect
                                // someone in the dropdown.
                                if self.selected_user_id == u.id {
                                    self.selected_user_id.clear();
                                    self.role_grants_for_user = JsonResource::new();
                                    self.access_for_user = JsonResource::new();
                                }
                            }
                            ui.end_row();
                        }
                    });
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }
    }

    fn admin_clients_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Admin: Apps");
        match self.admin_clients.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(clients)) => {
                let clients = clients.clone();
                egui::Grid::new("idp_admin_clients_grid")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("client_id");
                        ui.strong("roles");
                        ui.strong("roles_claim");
                        ui.strong("restricted");
                        ui.strong("source");
                        ui.end_row();
                        for c in &clients {
                            ui.label(&c.client_id);
                            ui.label(c.roles.join(", "));
                            ui.label(&c.roles_claim);
                            ui.label(if c.access_restricted { "yes" } else { "" });
                            ui.label(if c.managed { "dynamic" } else { "static" });
                            if c.managed && ui.small_button("Delete").clicked() {
                                self.delete(&format!("/api/admin/clients/{}", c.client_id));
                                self.admin_clients = JsonResource::new();
                            }
                            ui.end_row();
                        }
                    });

                ui.collapsing("Register a new app", |ui| {
                    ui.label("For an external OAuth client (e.g. ArgoCD). No client_secret -- every client here is public/PKCE-only.");
                    ui.horizontal(|ui| {
                        ui.label("client_id:");
                        ui.text_edit_singleline(&mut self.new_client_id);
                    });
                    ui.horizontal(|ui| {
                        ui.label("name:");
                        ui.text_edit_singleline(&mut self.new_client_name);
                    });
                    ui.horizontal(|ui| {
                        ui.label("redirect_uris (comma-separated):");
                        ui.text_edit_singleline(&mut self.new_client_redirects);
                    });
                    ui.horizontal(|ui| {
                        ui.label("roles (comma-separated):");
                        ui.text_edit_singleline(&mut self.new_client_roles);
                    });
                    ui.horizontal(|ui| {
                        ui.label("roles_claim:");
                        ui.text_edit_singleline(&mut self.new_client_claim);
                    });
                    ui.checkbox(
                        &mut self.new_client_restricted,
                        "Restrict login to explicitly-granted users (opt-in by default)",
                    );
                    if ui.button("Create").clicked() {
                        let redirect_uris: Vec<String> = self
                            .new_client_redirects
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let roles: Vec<String> = self
                            .new_client_roles
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if self.new_client_id.trim().is_empty()
                            || self.new_client_name.trim().is_empty()
                            || redirect_uris.is_empty()
                        {
                            self.action_error = Some(
                                "client_id, name, and at least one redirect_uri are required"
                                    .to_string(),
                            );
                        } else {
                            self.post_json(
                                "/api/admin/clients",
                                &serde_json::json!({
                                    "client_id": self.new_client_id,
                                    "name": self.new_client_name,
                                    "redirect_uris": redirect_uris,
                                    "roles": roles,
                                    "roles_claim": self.new_client_claim,
                                    "access_restricted": self.new_client_restricted,
                                }),
                            );
                            self.new_client_id.clear();
                            self.new_client_name.clear();
                            self.new_client_redirects.clear();
                            self.new_client_roles.clear();
                            self.new_client_claim = "roles".to_string();
                            self.new_client_restricted = true;
                            self.admin_clients = JsonResource::new();
                        }
                    }
                });
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }
    }

    fn admin_invites_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Admin: Invites");
        ui.horizontal(|ui| {
            ui.label("note:");
            ui.text_edit_singleline(&mut self.invite_note_input);
            if ui.button("Create invite").clicked() {
                let note = self.invite_note_input.trim();
                self.post_json(
                    "/api/admin/invites",
                    &serde_json::json!({ "note": if note.is_empty() { None } else { Some(note) } }),
                );
                self.invite_note_input.clear();
                self.admin_invites = JsonResource::new();
            }
        });
        match self.admin_invites.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(invites)) => {
                let invites = invites.clone();
                for i in &invites {
                    ui.horizontal(|ui| {
                        if i.used {
                            ui.label(format!("(used) {}", i.note.as_deref().unwrap_or("")));
                        } else {
                            ui.hyperlink_to(i.note.as_deref().unwrap_or("invite link"), &i.url);
                            if ui.small_button("Delete").clicked() {
                                self.delete(&format!("/api/admin/invites/{}", i.id));
                                self.admin_invites = JsonResource::new();
                            }
                        }
                    });
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }
    }

    fn admin_roles_and_access_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Admin: Role grants & app login access");
        if let Some(Err(err)) = self.admin_users.ready() {
            ui.colored_label(egui::Color32::RED, format!("loading users: {err}"));
            return;
        }
        if let Some(Err(err)) = self.admin_clients.ready() {
            ui.colored_label(egui::Color32::RED, format!("loading apps: {err}"));
            return;
        }
        let (Some(Ok(users)), Some(Ok(clients))) =
            (self.admin_users.ready(), self.admin_clients.ready())
        else {
            ui.spinner();
            return;
        };
        let users = users.clone();
        let clients = clients.clone();

        ui.horizontal(|ui| {
            ui.label("User:");
            egui::ComboBox::from_id_salt("idp_admin_select_user")
                .selected_text(
                    users
                        .iter()
                        .find(|u| u.id == self.selected_user_id)
                        .map(|u| u.username.clone())
                        .unwrap_or_else(|| "(select)".to_string()),
                )
                .show_ui(ui, |ui| {
                    for u in &users {
                        if ui
                            .selectable_label(self.selected_user_id == u.id, &u.username)
                            .clicked()
                        {
                            self.selected_user_id = u.id.clone();
                            self.role_grants_for_user = JsonResource::new();
                            self.access_for_user = JsonResource::new();
                        }
                    }
                });
            ui.label("App:");
            egui::ComboBox::from_id_salt("idp_admin_select_client")
                .selected_text(if self.selected_client_id.is_empty() {
                    "(select)".to_string()
                } else {
                    self.selected_client_id.clone()
                })
                .show_ui(ui, |ui| {
                    for c in &clients {
                        if ui
                            .selectable_label(self.selected_client_id == c.client_id, &c.name)
                            .clicked()
                        {
                            self.selected_client_id = c.client_id.clone();
                            self.selected_role.clear();
                        }
                    }
                });
        });

        if self.selected_user_id.is_empty() || self.selected_client_id.is_empty() {
            return;
        }

        let selected_client = clients
            .iter()
            .find(|c| c.client_id == self.selected_client_id)
            .cloned();

        ui.horizontal(|ui| {
            ui.label("Role:");
            egui::ComboBox::from_id_salt("idp_admin_select_role")
                .selected_text(if self.selected_role.is_empty() {
                    "(select)".to_string()
                } else {
                    self.selected_role.clone()
                })
                .show_ui(ui, |ui| {
                    for r in selected_client.iter().flat_map(|c| c.roles.iter()) {
                        if ui.selectable_label(&self.selected_role == r, r).clicked() {
                            self.selected_role = r.clone();
                        }
                    }
                });
            if ui.button("Grant role").clicked() && !self.selected_role.is_empty() {
                self.post_json(
                    "/api/admin/roles",
                    &serde_json::json!({
                        "user_id": self.selected_user_id,
                        "client_id": self.selected_client_id,
                        "role": self.selected_role,
                    }),
                );
                self.role_grants_for_user = JsonResource::new();
            }
            if ui.button("Grant login access").clicked() {
                self.post_json(
                    "/api/admin/access",
                    &serde_json::json!({
                        "user_id": self.selected_user_id,
                        "client_id": self.selected_client_id,
                    }),
                );
                self.access_for_user = JsonResource::new();
            }
        });

        match self.role_grants_for_user.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(grants)) => {
                for g in grants.clone() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{}: {}", g.client_id, g.role));
                        if ui.small_button("Revoke role").clicked() {
                            self.delete(&format!(
                                "/api/admin/roles?user_id={}&client_id={}&role={}",
                                url_encode(&self.selected_user_id),
                                url_encode(&g.client_id),
                                url_encode(&g.role)
                            ));
                            self.role_grants_for_user = JsonResource::new();
                        }
                    });
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }

        match self.access_for_user.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(client_ids)) => {
                for cid in client_ids.clone() {
                    ui.horizontal(|ui| {
                        ui.label(format!("login access: {cid}"));
                        if ui.small_button("Revoke access").clicked() {
                            self.delete(&format!(
                                "/api/admin/access?user_id={}&client_id={}",
                                url_encode(&self.selected_user_id),
                                url_encode(&cid)
                            ));
                            self.access_for_user = JsonResource::new();
                        }
                    });
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, err);
            }
        }
    }
}

/// Free function (not a method) so a call site can hold `base`/`token` as
/// plain local values computed from `&self` *before* borrowing one of
/// `self`'s own `JsonResource` fields mutably -- `fn(&self, resource: &mut
/// JsonResource<T>)` can't work here since `&self` would overlap the very
/// field `resource` is a `&mut` borrow of.
fn fetch_authed<T: serde::de::DeserializeOwned + Send + 'static>(
    resource: &mut JsonResource<T>,
    base: &str,
    token: &str,
    path: &str,
) {
    let auth = format!("Bearer {token}");
    resource.fetch_with_headers(&format!("{base}{path}"), &[("Authorization", &auth)]);
}

/// Records a failed/non-2xx `ehttp` response from `post_json`/`delete` into
/// `pending_error` (drained into `IdpPanel::action_error` on the next
/// `tick`) -- see `IdpPanel::pending_error`'s doc comment on why this
/// panel's mutations need it at all.
fn report_mutation_failure(
    pending_error: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
    path: &str,
    response: ehttp::Result<ehttp::Response>,
) {
    let error = match response {
        Ok(resp) if resp.ok => return,
        Ok(resp) => Some(format!(
            "{path} failed: server returned {} {}",
            resp.status, resp.status_text
        )),
        Err(err) => Some(format!("{path} failed: {err}")),
    };
    *pending_error.lock().unwrap() = error;
}

fn url_encode(s: &str) -> String {
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

impl Panel for IdpPanel {
    fn id(&self) -> PanelId {
        "idp-account"
    }

    fn title(&self) -> &str {
        "Account"
    }

    fn tick(&mut self, _ctx: &egui::Context) {
        if let Some(err) = self.pending_error.lock().unwrap().take() {
            self.action_error = Some(err);
        }

        let (Some(base), Some(token)) = (self.base(), self.bearer_token.clone()) else {
            return;
        };

        if !self.me.has_requested() {
            fetch_authed(&mut self.me, &base, &token, "/api/me");
        }
        if !self.passkeys.has_requested() {
            fetch_authed(&mut self.passkeys, &base, &token, "/api/passkeys");
        }
        if !self.sessions.has_requested() {
            fetch_authed(&mut self.sessions, &base, &token, "/api/sessions");
        }

        let is_admin = matches!(self.me.ready(), Some(Ok(me)) if me.is_admin);
        if is_admin {
            if !self.admin_users.has_requested() {
                fetch_authed(&mut self.admin_users, &base, &token, "/api/admin/users");
            }
            if !self.admin_clients.has_requested() {
                fetch_authed(&mut self.admin_clients, &base, &token, "/api/admin/clients");
            }
            if !self.admin_invites.has_requested() {
                fetch_authed(&mut self.admin_invites, &base, &token, "/api/admin/invites");
            }
            if !self.selected_user_id.is_empty() && !self.role_grants_for_user.has_requested() {
                let path = format!("/api/admin/users/{}/roles", self.selected_user_id);
                fetch_authed(&mut self.role_grants_for_user, &base, &token, &path);
            }
            if !self.selected_user_id.is_empty() && !self.access_for_user.has_requested() {
                let path = format!("/api/admin/users/{}/access", self.selected_user_id);
                fetch_authed(&mut self.access_for_user, &base, &token, &path);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.bearer_token.is_none() {
            ui.label("Sign in above to manage your IDP account.");
            return;
        }
        if let Some(err) = self.action_error.clone() {
            ui.colored_label(egui::Color32::RED, &err);
            if ui.small_button("dismiss").clicked() {
                self.action_error = None;
            }
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.profile_ui(ui);
            ui.separator();
            self.passkeys_ui(ui);
            ui.separator();
            self.sessions_ui(ui);

            let is_admin = matches!(self.me.ready(), Some(Ok(me)) if me.is_admin);
            if is_admin {
                ui.separator();
                self.admin_users_ui(ui);
                ui.separator();
                self.admin_clients_ui(ui);
                ui.separator();
                self.admin_invites_ui(ui);
                ui.separator();
                self.admin_roles_and_access_ui(ui);
            }
        });
    }
}
