//! Passkey (WebAuthn) registration and authentication -- the *only*
//! first-factor login method this IDP supports; there is no password at
//! all. Ported from the design in `sanlys/manager`'s `idp/`, trimmed to
//! this IDP's smaller user model (no profile-claim fields, `is_admin`
//! instead of a role string).

use axum::{
    extract::{Json, State},
    http::HeaderMap,
    response::IntoResponse,
};
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::*;

use super::oauth::require_session;
use crate::{db, error::AppError, metrics, state::AppState};

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum StoredAuthState {
    Passkey {
        inner: PasskeyAuthentication,
    },
    /// Usernameless/discoverable flow: created with an empty
    /// `allowCredentials`; the user's actual credentials get patched in on
    /// finish (see [`patch_discoverable_state`]).
    Discoverable {
        inner: PasskeyAuthentication,
    },
}

/// Inject the user's passkeys into a `PasskeyAuthentication` created with an
/// empty `allowCredentials` (discoverable/resident-key flow) -- without
/// this, webauthn-rs can't find the credential's public key in the empty
/// state and rejects the assertion.
fn patch_discoverable_state(
    state: PasskeyAuthentication,
    passkeys: &[Passkey],
) -> Result<PasskeyAuthentication, String> {
    let mut v = serde_json::to_value(&state).map_err(|e| e.to_string())?;
    let creds: Vec<serde_json::Value> = passkeys
        .iter()
        .filter_map(|pk| serde_json::to_value(pk).ok())
        .filter_map(|v| v.get("cred").cloned())
        .collect();
    v["ast"]["credentials"] = serde_json::Value::Array(creds);
    serde_json::from_value(v).map_err(|e| e.to_string())
}

fn extract_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-real-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_ua(headers: &HeaderMap) -> Option<String> {
    headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

// ── Registration ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterStartRequest {
    pub username: String,
    pub display_name: Option<String>,
    /// Required unless this is the first-ever user (bootstrap admin).
    pub invite_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterStartResponse {
    pub challenge_id: String,
    pub options: serde_json::Value,
}

pub async fn register_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, AppError> {
    let ip = extract_ip(&headers);
    if !state.auth_limiter.check(&format!("register:{ip}")) {
        metrics::rate_limited("register");
        return Err(AppError::RateLimited);
    }

    let username = req.username.trim().to_lowercase();
    if username.is_empty() || username.len() > 64 {
        return Err(AppError::BadRequest("invalid username".into()));
    }

    let is_bootstrap = db::count_users(&state.db).await? == 0;
    let invite_id: Option<String> = if is_bootstrap {
        None
    } else {
        let token = req
            .invite_token
            .ok_or_else(|| AppError::BadRequest("invite_token required".into()))?;
        let invite = db::get_invite_by_token(&state.db, &token)
            .await?
            .ok_or_else(|| AppError::BadRequest("invalid or expired invite".into()))?;
        if invite.used_at.is_some() {
            return Err(AppError::BadRequest("invite already used".into()));
        }
        if invite.expires_at < chrono::Utc::now() {
            return Err(AppError::BadRequest("invite has expired".into()));
        }
        Some(invite.id)
    };

    let display_name = req.display_name.unwrap_or_else(|| username.clone());

    let user_id = match db::get_user_by_username(&state.db, &username).await? {
        Some(u) => u.id,
        None => {
            db::create_user(&state.db, &username, &display_name, is_bootstrap)
                .await?
                .id
        }
    };

    let existing_creds = db::get_credentials_for_user(&state.db, &user_id)
        .await?
        .into_iter()
        .map(|c| CredentialID::from(c.credential_id))
        .collect::<Vec<_>>();

    let user_uuid = Uuid::parse_str(&user_id).map_err(|e| AppError::Internal(e.to_string()))?;

    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(user_uuid, &username, &display_name, Some(existing_creds))
        .map_err(|e| AppError::WebAuthn(e.to_string()))?;

    let state_json =
        serde_json::to_string(&reg_state).map_err(|e| AppError::Internal(e.to_string()))?;
    let challenge_id = db::save_webauthn_registration_challenge(
        &state.db,
        &user_id,
        &username,
        invite_id.as_deref(),
        &state_json,
    )
    .await?;
    let options = serde_json::to_value(&ccr).map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(RegisterStartResponse {
        challenge_id,
        options,
    }))
}

#[derive(Debug, Deserialize)]
pub struct RegisterFinishRequest {
    pub challenge_id: String,
    pub credential: serde_json::Value,
    pub label: Option<String>,
}

pub async fn register_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: PrivateCookieJar,
    Json(req): Json<RegisterFinishRequest>,
) -> Result<(PrivateCookieJar, impl IntoResponse), AppError> {
    let (user_id, _username, invite_id, state_json) =
        db::get_and_delete_webauthn_registration_challenge(&state.db, &req.challenge_id)
            .await?
            .ok_or_else(|| AppError::BadRequest("challenge not found or expired".into()))?;

    let reg_state: PasskeyRegistration =
        serde_json::from_str(&state_json).map_err(|e| AppError::Internal(e.to_string()))?;
    let register_public_key_credential: RegisterPublicKeyCredential =
        serde_json::from_value(req.credential)
            .map_err(|e| AppError::BadRequest(format!("invalid credential: {e}")))?;

    let passkey = state
        .webauthn
        .finish_passkey_registration(&register_public_key_credential, &reg_state)
        .map_err(|e| AppError::WebAuthn(e.to_string()))?;

    let cred_id: Vec<u8> = passkey.cred_id().to_vec();
    let cred_json = serde_json::to_vec(&passkey).map_err(|e| AppError::Internal(e.to_string()))?;
    db::save_credential(
        &state.db,
        &user_id,
        &cred_id,
        &cred_json,
        0,
        req.label.as_deref(),
    )
    .await?;

    if let Some(iid) = invite_id {
        let _ = db::consume_invite(&state.db, &iid, &user_id).await;
    }

    let ua = extract_ua(&headers);
    let ip = extract_ip(&headers);
    let session = db::create_session(&state.db, &user_id, ua.as_deref(), Some(&ip)).await?;
    let user_count = db::count_users(&state.db).await.unwrap_or(0);
    metrics::registered_users_gauge(user_count as f64);
    metrics::active_sessions(1.0);

    let cookie = Cookie::build(("session", session.id))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .build();
    Ok((jar.add(cookie), Json(serde_json::json!({ "ok": true }))))
}

// ── Authentication ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AuthStartRequest {
    pub username: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthStartResponse {
    pub challenge_id: String,
    pub options: serde_json::Value,
}

pub async fn auth_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AuthStartRequest>,
) -> Result<Json<AuthStartResponse>, AppError> {
    let ip = extract_ip(&headers);
    if !state.auth_limiter.check(&format!("auth:{ip}")) {
        metrics::rate_limited("auth");
        return Err(AppError::RateLimited);
    }

    if let Some(username) = &req.username {
        let username = username.trim().to_lowercase();
        let user = db::get_user_by_username(&state.db, &username)
            .await?
            .ok_or_else(|| AppError::NotFound("user not found".into()))?;
        let creds = db::get_credentials_for_user(&state.db, &user.id).await?;
        if creds.is_empty() {
            return Err(AppError::BadRequest(
                "no passkeys registered for this user".into(),
            ));
        }
        let passkeys: Vec<Passkey> = creds
            .into_iter()
            .filter_map(|c| serde_json::from_slice(&c.public_key).ok())
            .collect();
        let (rcr, auth_state) = state
            .webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(|e| AppError::WebAuthn(e.to_string()))?;
        let stored = StoredAuthState::Passkey { inner: auth_state };
        let state_json =
            serde_json::to_string(&stored).map_err(|e| AppError::Internal(e.to_string()))?;
        let challenge_id = db::save_webauthn_auth_challenge(&state.db, &state_json).await?;
        let options = serde_json::to_value(&rcr).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(Json(AuthStartResponse {
            challenge_id,
            options,
        }))
    } else {
        let (rcr, auth_state) = state
            .webauthn
            .start_passkey_authentication(&[])
            .map_err(|e| AppError::WebAuthn(e.to_string()))?;
        let stored = StoredAuthState::Discoverable { inner: auth_state };
        let state_json =
            serde_json::to_string(&stored).map_err(|e| AppError::Internal(e.to_string()))?;
        let challenge_id = db::save_webauthn_auth_challenge(&state.db, &state_json).await?;
        let options = serde_json::to_value(&rcr).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(Json(AuthStartResponse {
            challenge_id,
            options,
        }))
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthFinishRequest {
    pub challenge_id: String,
    pub credential: serde_json::Value,
}

pub async fn auth_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: PrivateCookieJar,
    Json(req): Json<AuthFinishRequest>,
) -> Result<(PrivateCookieJar, impl IntoResponse), AppError> {
    let ip = extract_ip(&headers);
    if !state.auth_limiter.check(&format!("auth_finish:{ip}")) {
        metrics::rate_limited("auth_finish");
        return Err(AppError::RateLimited);
    }

    let state_json = db::get_and_delete_webauthn_auth_challenge(&state.db, &req.challenge_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("challenge not found or expired".into()))?;
    let stored_state: StoredAuthState =
        serde_json::from_str(&state_json).map_err(|e| AppError::Internal(e.to_string()))?;
    let auth_credential: PublicKeyCredential = serde_json::from_value(req.credential)
        .map_err(|e| AppError::BadRequest(format!("invalid credential: {e}")))?;

    let raw_id: Vec<u8> = auth_credential.raw_id.to_vec();
    let user = db::get_user_by_credential_id(&state.db, &raw_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("credential not found".into()))?;

    let db_creds = db::get_credentials_for_user(&state.db, &user.id).await?;
    let mut passkeys: Vec<Passkey> = db_creds
        .iter()
        .filter_map(|c| serde_json::from_slice(&c.public_key).ok())
        .collect();

    let auth_result = match stored_state {
        StoredAuthState::Passkey { inner: auth_state } => state
            .webauthn
            .finish_passkey_authentication(&auth_credential, &auth_state)
            .map_err(|e| {
                metrics::auth_attempt("failure");
                AppError::WebAuthn(e.to_string())
            })?,
        StoredAuthState::Discoverable { inner: auth_state } => {
            let patched =
                patch_discoverable_state(auth_state, &passkeys).map_err(AppError::Internal)?;
            state
                .webauthn
                .finish_passkey_authentication(&auth_credential, &patched)
                .map_err(|e| {
                    metrics::auth_attempt("failure");
                    AppError::WebAuthn(e.to_string())
                })?
        }
    };

    if auth_result.needs_update() {
        for pk in passkeys.iter_mut() {
            if pk.update_credential(&auth_result).unwrap_or(false) {
                let cred_json =
                    serde_json::to_vec(pk).map_err(|e| AppError::Internal(e.to_string()))?;
                db::update_credential(
                    &state.db,
                    pk.cred_id().as_ref(),
                    &cred_json,
                    auth_result.counter(),
                )
                .await?;
                break;
            }
        }
    }

    let ua = extract_ua(&headers);
    let session = db::create_session(&state.db, &user.id, ua.as_deref(), Some(&ip)).await?;
    metrics::auth_attempt("success");
    metrics::active_sessions(1.0);

    let cookie = Cookie::build(("session", session.id))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .build();
    Ok((
        jar.add(cookie),
        Json(serde_json::json!({ "ok": true, "username": user.username })),
    ))
}

pub async fn logout(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> (PrivateCookieJar, impl IntoResponse) {
    if let Some(sid) = jar.get("session").map(|c| c.value().to_string()) {
        let _ = db::delete_session_by_id(&state.db, &sid).await;
        metrics::active_sessions(-1.0);
    }
    (
        jar.remove(Cookie::from("session")),
        Json(serde_json::json!({ "ok": true })),
    )
}

// ── Add passkey (authenticated users only) ────────────────────────────────────

pub async fn add_passkey_start(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<RegisterStartResponse>, AppError> {
    let user = require_session(&state, &jar).await?;
    let existing_creds = db::get_credentials_for_user(&state.db, &user.id)
        .await?
        .into_iter()
        .map(|c| CredentialID::from(c.credential_id))
        .collect::<Vec<_>>();
    let user_uuid = Uuid::parse_str(&user.id).map_err(|e| AppError::Internal(e.to_string()))?;

    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(
            user_uuid,
            &user.username,
            &user.display_name,
            Some(existing_creds),
        )
        .map_err(|e| AppError::WebAuthn(e.to_string()))?;
    let state_json =
        serde_json::to_string(&reg_state).map_err(|e| AppError::Internal(e.to_string()))?;
    let challenge_id = db::save_webauthn_registration_challenge(
        &state.db,
        &user.id,
        &user.username,
        None,
        &state_json,
    )
    .await?;
    let options = serde_json::to_value(&ccr).map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(RegisterStartResponse {
        challenge_id,
        options,
    }))
}

#[derive(Debug, Deserialize)]
pub struct AddPasskeyFinishRequest {
    pub challenge_id: String,
    pub credential: serde_json::Value,
    pub label: Option<String>,
}

pub async fn add_passkey_finish(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(req): Json<AddPasskeyFinishRequest>,
) -> Result<impl IntoResponse, AppError> {
    let user = require_session(&state, &jar).await?;
    let (challenge_user_id, _username, _invite_id, state_json) =
        db::get_and_delete_webauthn_registration_challenge(&state.db, &req.challenge_id)
            .await?
            .ok_or_else(|| AppError::BadRequest("challenge not found or expired".into()))?;
    if challenge_user_id != user.id {
        return Err(AppError::Forbidden(
            "challenge belongs to a different user".into(),
        ));
    }

    let reg_state: PasskeyRegistration =
        serde_json::from_str(&state_json).map_err(|e| AppError::Internal(e.to_string()))?;
    let rpkc: RegisterPublicKeyCredential = serde_json::from_value(req.credential)
        .map_err(|e| AppError::BadRequest(format!("invalid credential: {e}")))?;
    let passkey = state
        .webauthn
        .finish_passkey_registration(&rpkc, &reg_state)
        .map_err(|e| AppError::WebAuthn(e.to_string()))?;

    let cred_id: Vec<u8> = passkey.cred_id().to_vec();
    let cred_json = serde_json::to_vec(&passkey).map_err(|e| AppError::Internal(e.to_string()))?;
    db::save_credential(
        &state.db,
        &user.id,
        &cred_id,
        &cred_json,
        0,
        req.label.as_deref(),
    )
    .await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn setup_status(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let needs_setup = db::count_users(&state.db).await? == 0;
    Ok(Json(serde_json::json!({ "needs_setup": needs_setup })))
}

pub async fn me(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<impl IntoResponse, AppError> {
    let session_id = jar
        .get("session")
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::Unauthorized("not logged in".into()))?;
    let session = db::get_session(&state.db, &session_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("session expired".into()))?;
    let _ = db::touch_session(&state.db, &session_id).await;
    let user = db::get_user_by_id(&state.db, &session.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;
    let apps = db::list_roles_for_user(&state.db, &user.id).await?;

    Ok(Json(serde_json::json!({
        "id": user.id,
        "username": user.username,
        "display_name": user.display_name,
        "is_admin": user.is_admin,
        "apps": apps,
    })))
}

// ── Profile / sessions / passkeys management ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub display_name: String,
}

pub async fn update_profile(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(req): Json<UpdateProfileRequest>,
) -> Result<impl IntoResponse, AppError> {
    let user = require_session(&state, &jar).await?;
    let display_name = req.display_name.trim();
    if display_name.is_empty() {
        return Err(AppError::BadRequest("display_name cannot be empty".into()));
    }
    db::update_display_name(&state.db, &user.id, display_name).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list_passkeys(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = require_session(&state, &jar).await?;
    let creds = db::get_credentials_for_user(&state.db, &user.id).await?;
    let list: Vec<_> = creds
        .iter()
        .map(|c| serde_json::json!({ "id": c.id, "label": c.label, "created_at": c.created_at }))
        .collect();
    Ok(Json(serde_json::json!(list)))
}

pub async fn delete_passkey(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let user = require_session(&state, &jar).await?;
    let deleted = db::delete_credential(&state.db, &id, &user.id).await?;
    if deleted {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound("passkey not found".into()))
    }
}

pub async fn list_sessions(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = require_session(&state, &jar).await?;
    let sessions = db::list_sessions_for_user(&state.db, &user.id).await?;
    let list: Vec<_> = sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id, "created_at": s.created_at, "last_seen_at": s.last_seen_at,
                "user_agent": s.user_agent, "ip_address": s.ip_address,
            })
        })
        .collect();
    Ok(Json(serde_json::json!(list)))
}

pub async fn revoke_session(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let user = require_session(&state, &jar).await?;
    let deleted = db::delete_session(&state.db, &id, &user.id).await?;
    if deleted {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound("session not found".into()))
    }
}

pub async fn revoke_all_sessions(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<impl IntoResponse, AppError> {
    let user = require_session(&state, &jar).await?;
    db::delete_all_sessions(&state.db, &user.id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
