//! Session browsing: every session is a first-class record (PLAN.md §8.2).

use game_mgr_api_types::{SessionDto, SessionEndReason};
use sqlx::{PgPool, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    machine_id: Uuid,
    profile_id: Uuid,
    game_id: String,
    started_at: OffsetDateTime,
    ended_at: OffsetDateTime,
    duration_s: i32,
    exit_code: Option<i32>,
    end_reason: String,
}

impl From<SessionRow> for SessionDto {
    fn from(r: SessionRow) -> Self {
        SessionDto {
            id: r.id,
            machine_id: r.machine_id,
            profile_id: r.profile_id,
            game_id: r.game_id,
            started_at: r.started_at,
            ended_at: r.ended_at,
            duration_s: r.duration_s,
            exit_code: r.exit_code,
            end_reason: SessionEndReason::parse(&r.end_reason),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    pub game_id: Option<String>,
    pub machine_id: Option<Uuid>,
    pub profile_id: Option<Uuid>,
    /// Only sessions started strictly before this instant (pagination).
    pub before: Option<OffsetDateTime>,
    pub limit: i64,
}

/// Newest-first session list with optional filters.
pub async fn list(pool: &PgPool, filter: &SessionFilter) -> Result<Vec<SessionDto>, sqlx::Error> {
    let mut qb = QueryBuilder::new(
        "SELECT id, machine_id, profile_id, game_id, started_at, ended_at, \
         duration_s, exit_code, end_reason FROM sessions WHERE TRUE",
    );
    if let Some(game_id) = &filter.game_id {
        qb.push(" AND game_id = ").push_bind(game_id);
    }
    if let Some(machine_id) = filter.machine_id {
        qb.push(" AND machine_id = ").push_bind(machine_id);
    }
    if let Some(profile_id) = filter.profile_id {
        qb.push(" AND profile_id = ").push_bind(profile_id);
    }
    if let Some(before) = filter.before {
        qb.push(" AND started_at < ").push_bind(before);
    }
    qb.push(" ORDER BY started_at DESC LIMIT ")
        .push_bind(filter.limit);

    let rows: Vec<SessionRow> = qb.build_query_as().fetch_all(pool).await?;
    Ok(rows.into_iter().map(Into::into).collect())
}
