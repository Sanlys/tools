//! `/sessions:batch` and `/events:batch` — spool-safe, idempotent ingest
//! with per-item outcomes (PLAN.md §8.2).

use axum::extract::{Query, State};
use axum::response::Json;
use game_mgr_api_types::{
    BatchItemError, BatchResponse, EventsBatchRequest, SessionDto, SessionsBatchRequest,
};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{AppState, validate_batch_len};
use crate::auth::Authed;
use crate::error::ApiError;
use crate::repo;
use crate::repo::ingest::Outcome;
use crate::repo::sessions::SessionFilter;

fn tally(response: &mut BatchResponse, id: uuid::Uuid, outcome: Outcome) {
    match outcome {
        Outcome::Inserted => response.inserted += 1,
        Outcome::Duplicate => response.duplicates += 1,
        Outcome::Rejected(reason) => response.errors.push(BatchItemError { id, reason }),
    }
}

pub async fn sessions_batch(
    State(state): State<AppState>,
    Authed(_user): Authed,
    Json(req): Json<SessionsBatchRequest>,
) -> Result<Json<BatchResponse>, ApiError> {
    validate_batch_len(req.sessions.len())?;
    let pool = state.db();
    let mut response = BatchResponse::default();
    for session in &req.sessions {
        let outcome = repo::ingest::insert_session(pool, session).await?;
        tally(&mut response, session.id, outcome);
    }
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    pub game_id: Option<String>,
    pub machine_id: Option<Uuid>,
    pub profile_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub before: Option<OffsetDateTime>,
    pub limit: Option<i64>,
}

const MAX_SESSION_PAGE: i64 = 500;

pub async fn list_sessions(
    State(state): State<AppState>,
    Authed(_user): Authed,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<Vec<SessionDto>>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, MAX_SESSION_PAGE);
    let filter = SessionFilter {
        game_id: query.game_id,
        machine_id: query.machine_id,
        profile_id: query.profile_id,
        before: query.before,
        limit,
    };
    Ok(Json(repo::sessions::list(state.db(), &filter).await?))
}

pub async fn events_batch(
    State(state): State<AppState>,
    Authed(_user): Authed,
    Json(req): Json<EventsBatchRequest>,
) -> Result<Json<BatchResponse>, ApiError> {
    validate_batch_len(req.events.len())?;
    let pool = state.db();
    let mut response = BatchResponse::default();
    for event in &req.events {
        let outcome = repo::ingest::insert_event(pool, event).await?;
        tally(&mut response, event.client_event_id, outcome);
    }
    Ok(Json(response))
}
