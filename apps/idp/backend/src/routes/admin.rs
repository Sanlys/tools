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
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &jar).await?;
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
    Path(user_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let me = require_admin(&app, &jar).await?;
    if me.id == user_id {
        return Err(AppError::BadRequest(
            "cannot delete your own account".into(),
        ));
    }
    db::delete_user(&app.db, &user_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ── Clients (read-only -- the registry itself is static, see clients.rs) ──────

pub async fn list_clients(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &jar).await?;
    let clients = db::list_clients(&app.db).await?;
    let list: Vec<_> = clients
        .iter()
        .map(|c| serde_json::json!({ "client_id": c.client_id, "name": c.name, "roles": c.roles, "native": c.native }))
        .collect();
    Ok(Json(serde_json::json!(list)))
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
    Json(req): Json<RoleGrantRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &jar).await?;
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
    Query(q): Query<RoleRevokeQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &jar).await?;
    db::revoke_role(&app.db, &q.user_id, &q.client_id, &q.role).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list_roles_for_user(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    Path(user_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &jar).await?;
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
    Json(req): Json<CreateInviteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let me = require_admin(&app, &jar).await?;
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
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&app, &jar).await?;
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
    Path(invite_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_admin(&app, &jar).await?;
    let deleted = db::delete_invite(&app.db, &invite_id).await?;
    if deleted {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(
            "invite not found or already used".into(),
        ))
    }
}
