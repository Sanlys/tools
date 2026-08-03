//! `/machines` — registration/heartbeat and the peer list that drives
//! Syncthing mesh discovery (PLAN.md §5).

use axum::extract::{Path, State};
use axum::response::Json;
use game_mgr_api_types::{MachineDto, RegisterMachineRequest};
use uuid::Uuid;

use super::{AppState, validate_name};
use crate::auth::Authed;
use crate::error::ApiError;
use crate::repo;

pub async fn register(
    State(state): State<AppState>,
    Authed(user): Authed,
    Path(id): Path<Uuid>,
    Json(req): Json<RegisterMachineRequest>,
) -> Result<Json<MachineDto>, ApiError> {
    validate_name(&req.name)?;
    let machine = repo::machines::upsert(state.db(), id, &req, user.user_id).await?;
    Ok(Json(machine.into()))
}

pub async fn list(
    State(state): State<AppState>,
    Authed(_user): Authed,
) -> Result<Json<Vec<MachineDto>>, ApiError> {
    let machines = repo::machines::list(state.db()).await?;
    Ok(Json(machines.into_iter().map(Into::into).collect()))
}
