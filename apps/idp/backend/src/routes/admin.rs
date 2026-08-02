use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Json,
};
use axum_extra::extract::cookie::PrivateCookieJar;
use serde::Deserialize;

use super::oauth::require_admin;
use crate::{db, error::AppError, state::AppState};

// ── Users ─────────────────────────────────────────────────────────────────────

pub async fn list_users(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &headers, &jar).await?;
    let users = db::list_users(&app.db).await?;
    let list: Vec<_> = users
        .iter()
        .map(|u| serde_json::json!({ "id": u.id, "username": u.username, "display_name": u.display_name, "is_admin": u.is_admin, "created_at": u.created_at }))
        .collect();
    Ok(Json(serde_json::json!(list)))
}

pub async fn delete_user(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let me = require_admin(&app, &headers, &jar).await?;
    if me.id == user_id {
        return Err(AppError::BadRequest(
            "cannot delete your own account".into(),
        ));
    }
    db::delete_user(&app.db, &user_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ── Clients ────────────────────────────────────────────────────────────────────
//
// GitOps-declared clients (`managed: false`, from IDP_CLIENTS_JSON) are
// read-only here. Admin-created clients (`managed: true`) can be created,
// edited, and deleted through this API -- see docs/architecture.md's
// "Registering an external OAuth app" section. Every client, either way,
// stays a public PKCE-only client -- there is no `client_secret` field
// anywhere in this API.

fn client_json(c: &db::Client) -> serde_json::Value {
    serde_json::json!({
        "client_id": c.client_id,
        "name": c.name,
        "redirect_uris": c.redirect_uris,
        "roles": c.roles,
        "native": c.native,
        "managed": c.managed,
        "roles_claim": c.roles_claim,
        "access_restricted": c.access_restricted,
    })
}

pub async fn list_clients(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &headers, &jar).await?;
    let clients = db::list_clients(&app.db).await?;
    let list: Vec<_> = clients.iter().map(client_json).collect();
    Ok(Json(serde_json::json!(list)))
}

#[derive(Debug, Deserialize)]
pub struct ClientRequest {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub native: bool,
    #[serde(default = "default_roles_claim")]
    pub roles_claim: String,
    #[serde(default = "default_access_restricted")]
    pub access_restricted: bool,
}

fn default_roles_claim() -> String {
    "roles".to_string()
}

fn default_access_restricted() -> bool {
    true
}

/// Claim names the token issuer already needs for itself -- rejected as a
/// `roles_claim` choice since renaming over one would corrupt the token.
const RESERVED_CLAIMS: &[&str] = &[
    "iss",
    "sub",
    "aud",
    "exp",
    "iat",
    "nonce",
    "scope",
    "preferred_username",
    "name",
    "auth_time",
];

fn validate_client_request(req: &ClientRequest) -> Result<(), AppError> {
    if req.client_id.trim().is_empty() || req.name.trim().is_empty() {
        return Err(AppError::BadRequest(
            "client_id and name are required".into(),
        ));
    }
    if req.redirect_uris.is_empty() {
        return Err(AppError::BadRequest(
            "at least one redirect_uri is required".into(),
        ));
    }
    for uri in &req.redirect_uris {
        if url::Url::parse(uri).is_err() {
            return Err(AppError::BadRequest(format!("invalid redirect_uri: {uri}")));
        }
    }
    if req.roles_claim.trim().is_empty() || RESERVED_CLAIMS.contains(&req.roles_claim.as_str()) {
        return Err(AppError::BadRequest(format!(
            "`{}` is not a valid roles_claim name",
            req.roles_claim
        )));
    }
    Ok(())
}

pub async fn create_client(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Json(req): Json<ClientRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    validate_client_request(&req)?;
    let input = db::ClientInput {
        name: req.name,
        redirect_uris: req.redirect_uris,
        roles: req.roles,
        native: req.native,
        roles_claim: req.roles_claim,
        access_restricted: req.access_restricted,
    };
    db::create_client(&app.db, &req.client_id, &input).await?;
    Ok(axum::http::StatusCode::CREATED)
}

pub async fn update_client(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Path(client_id): Path<String>,
    Json(req): Json<ClientRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    validate_client_request(&req)?;
    let input = db::ClientInput {
        name: req.name,
        redirect_uris: req.redirect_uris,
        roles: req.roles,
        native: req.native,
        roles_claim: req.roles_claim,
        access_restricted: req.access_restricted,
    };
    if db::update_client(&app.db, &client_id, &input).await? {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(
            "no such admin-managed client (GitOps-declared clients can't be edited here)".into(),
        ))
    }
}

pub async fn delete_client(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Path(client_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    if db::delete_client(&app.db, &client_id).await? {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(
            "no such admin-managed client (GitOps-declared clients can't be deleted here)".into(),
        ))
    }
}

// ── Per-app login access grants (independent of role grants) ─────────────────

#[derive(Debug, Deserialize)]
pub struct AccessGrantRequest {
    pub user_id: String,
    pub client_id: String,
}

pub async fn grant_access(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Json(req): Json<AccessGrantRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    db::get_client(&app.db, &req.client_id)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown client_id".into()))?;
    db::grant_access(&app.db, &req.user_id, &req.client_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct AccessRevokeQuery {
    pub user_id: String,
    pub client_id: String,
}

pub async fn revoke_access(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Query(q): Query<AccessRevokeQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    db::revoke_access(&app.db, &q.user_id, &q.client_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list_access_for_user(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &headers, &jar).await?;
    let client_ids = db::list_access_for_user(&app.db, &user_id).await?;
    Ok(Json(serde_json::json!(client_ids)))
}

// ── Per-app role grants ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RoleGrantRequest {
    pub user_id: String,
    pub client_id: String,
    pub role: String,
}

pub async fn grant_role(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Json(req): Json<RoleGrantRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    let client = db::get_client(&app.db, &req.client_id)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown client_id".into()))?;
    if !client.roles.iter().any(|r| r == &req.role) {
        return Err(AppError::BadRequest(format!(
            "`{}` does not declare role `{}`",
            req.client_id, req.role
        )));
    }
    db::grant_role(&app.db, &req.user_id, &req.client_id, &req.role).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct RoleRevokeQuery {
    pub user_id: String,
    pub client_id: String,
    pub role: String,
}

pub async fn revoke_role(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Query(q): Query<RoleRevokeQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    db::revoke_role(&app.db, &q.user_id, &q.client_id, &q.role).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list_roles_for_user(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &headers, &jar).await?;
    let roles = db::list_roles_for_user(&app.db, &user_id).await?;
    Ok(Json(serde_json::json!(roles)))
}

// ── Invites ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub note: Option<String>,
    /// Hours until the invite expires (default 72).
    pub ttl_hours: Option<i64>,
}

pub async fn create_invite(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateInviteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let me = require_admin(&app, &headers, &jar).await?;
    let ttl = req.ttl_hours.unwrap_or(72).clamp(1, 720); // 1h-30d
    let invite = db::create_invite(&app.db, &me.id, req.note.as_deref(), ttl).await?;
    let invite_url = format!("{}/register?invite={}", app.base_url, invite.token);
    Ok(Json(serde_json::json!({
        "id": invite.id, "token": invite.token, "url": invite_url,
        "note": invite.note, "expires_at": invite.expires_at,
    })))
}

pub async fn list_invites(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &headers, &jar).await?;
    let invites = db::list_invites(&app.db).await?;
    let base = &app.base_url;
    let list: Vec<_> = invites
        .iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id, "url": format!("{}/register?invite={}", base, i.token),
                "note": i.note, "expires_at": i.expires_at, "used": i.used_at.is_some(), "used_at": i.used_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!(list)))
}

pub async fn delete_invite(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Path(invite_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &headers, &jar).await?;
    let deleted = db::delete_invite(&app.db, &invite_id).await?;
    if deleted {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(
            "invite not found or already used".into(),
        ))
    }
}
