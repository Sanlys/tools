//! `/games` — server-stored game definitions (created/edited from the
//! client UI, PLAN.md §4.1) plus session-derived aggregates.

use axum::extract::{Path, State};
use axum::response::Json;
use game_mgr_api_types::{GameDefinition, GameDto, UpsertGameRequest};

use super::{AppState, validate_name};
use crate::auth::Authed;
use crate::error::ApiError;
use crate::repo;

fn validate_game_id(id: &str) -> Result<(), ApiError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        return Err(ApiError::Unprocessable(
            "game id must be a slug: lowercase letters, digits and dashes (max 64)".into(),
        ));
    }
    Ok(())
}

pub async fn upsert_game(
    State(state): State<AppState>,
    Authed(_user): Authed,
    Path(id): Path<String>,
    Json(req): Json<UpsertGameRequest>,
) -> Result<Json<GameDefinition>, ApiError> {
    validate_game_id(&id)?;
    validate_name(&req.title)?;
    if req.class.trim().is_empty() {
        return Err(ApiError::Unprocessable("class must not be empty".into()));
    }
    if semver::Version::parse(&req.version).is_err() {
        return Err(ApiError::Unprocessable(format!(
            "version must be semver (e.g. 1.0.0), got '{}'",
            req.version
        )));
    }
    for artifact in &req.artifacts {
        if artifact.bucket_key.trim().is_empty() || artifact.sha256.len() != 64 {
            return Err(ApiError::Unprocessable(format!(
                "artifact '{}' needs a bucket key and a 64-char hex sha256",
                artifact.bucket_key
            )));
        }
    }
    let definition = repo::games::upsert(state.db(), &id, &req).await?;
    Ok(Json(definition))
}

pub async fn get_game(
    State(state): State<AppState>,
    Authed(_user): Authed,
    Path(id): Path<String>,
) -> Result<Json<GameDto>, ApiError> {
    repo::games::get(state.db(), &id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("game not found".into()))
}

pub async fn list_games(
    State(state): State<AppState>,
    Authed(_user): Authed,
) -> Result<Json<Vec<GameDto>>, ApiError> {
    Ok(Json(repo::games::list_with_stats(state.db()).await?))
}

#[cfg(test)]
mod tests {
    use super::validate_game_id;

    #[test]
    fn game_id_slug_rules() {
        assert!(validate_game_id("bg3").is_ok());
        assert!(validate_game_id("mario-kart-8").is_ok());
        assert!(validate_game_id("").is_err());
        assert!(validate_game_id("BG3").is_err());
        assert!(validate_game_id("has space").is_err());
        assert!(validate_game_id("under_score").is_err());
        assert!(validate_game_id(&"x".repeat(65)).is_err());
    }
}
