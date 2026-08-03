//! `/me`, `/users` and `/profiles` — users & profiles (PLAN.md §8.0).
//! Household-trust authz: any authenticated user reads everything; renames
//! and transfers require ownership (enforced atomically in the repo layer).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use game_mgr_api_types::{
    CreateProfileRequest, MeResponse, ProfileDto, RenameProfileRequest, TransferProfileRequest,
    UserDto,
};
use uuid::Uuid;

use super::{AppState, validate_name};
use crate::auth::Authed;
use crate::error::ApiError;
use crate::repo;
use crate::repo::profiles::{OwnedUpdate, TransferOutcome};

fn owned_update(update: OwnedUpdate) -> Result<ProfileDto, ApiError> {
    match update {
        OwnedUpdate::Updated(p) => Ok(p.into()),
        OwnedUpdate::NotFound => Err(ApiError::NotFound("profile not found".into())),
        OwnedUpdate::NotOwner => Err(ApiError::Forbidden(
            "only the profile owner may do this".into(),
        )),
    }
}

pub async fn me(
    State(state): State<AppState>,
    Authed(user): Authed,
) -> Result<Json<MeResponse>, ApiError> {
    let pool = state.db();
    let row = repo::users::get(pool, user.user_id)
        .await?
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("authenticated user row vanished")))?;
    let profiles = repo::profiles::list_owned(pool, user.user_id).await?;
    Ok(Json(MeResponse {
        user: row.into(),
        profiles: profiles.into_iter().map(Into::into).collect(),
    }))
}

pub async fn list_users(
    State(state): State<AppState>,
    Authed(_user): Authed,
) -> Result<Json<Vec<UserDto>>, ApiError> {
    let users = repo::users::list(state.db()).await?;
    Ok(Json(users.into_iter().map(Into::into).collect()))
}

pub async fn list(
    State(state): State<AppState>,
    Authed(_user): Authed,
) -> Result<Json<Vec<ProfileDto>>, ApiError> {
    let profiles = repo::profiles::list_all(state.db()).await?;
    Ok(Json(profiles.into_iter().map(Into::into).collect()))
}

pub async fn create(
    State(state): State<AppState>,
    Authed(user): Authed,
    Json(req): Json<CreateProfileRequest>,
) -> Result<(StatusCode, Json<ProfileDto>), ApiError> {
    let name = validate_name(&req.name)?;
    let profile = repo::profiles::create(state.db(), user.user_id, name).await?;
    Ok((StatusCode::CREATED, Json(profile.into())))
}

pub async fn rename(
    State(state): State<AppState>,
    Authed(user): Authed,
    Path(id): Path<Uuid>,
    Json(req): Json<RenameProfileRequest>,
) -> Result<Json<ProfileDto>, ApiError> {
    let name = validate_name(&req.name)?;
    let update = repo::profiles::rename(state.db(), id, user.user_id, name).await?;
    owned_update(update).map(Json)
}

/// Owner-only. Cascades the profile's sessions and transfer history
/// (events keep their rows with a nulled profile) — the client confirms
/// with the user before calling.
pub async fn delete(
    State(state): State<AppState>,
    Authed(user): Authed,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let update = repo::profiles::delete(state.db(), id, user.user_id).await?;
    owned_update(update).map(|_| StatusCode::NO_CONTENT)
}

pub async fn transfer(
    State(state): State<AppState>,
    Authed(user): Authed,
    Path(id): Path<Uuid>,
    Json(req): Json<TransferProfileRequest>,
) -> Result<Json<ProfileDto>, ApiError> {
    match repo::profiles::transfer(state.db(), id, user.user_id, req.to_user_id).await {
        Ok(TransferOutcome::Done(p)) => Ok(Json(p.into())),
        Ok(TransferOutcome::NotFound) => Err(ApiError::NotFound("profile not found".into())),
        Ok(TransferOutcome::NotOwner) => Err(ApiError::Forbidden(
            "only the profile owner may do this".into(),
        )),
        Ok(TransferOutcome::AlreadyOwner) => Err(ApiError::Unprocessable(
            "profile is already owned by the target user".into(),
        )),
        Err(err) if repo::is_fk_violation(&err) => {
            Err(ApiError::Unprocessable("target user does not exist".into()))
        }
        Err(err) => Err(err.into()),
    }
}
