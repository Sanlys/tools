//! Spool-safe stats ingest: each item is idempotent (client-generated UUID)
//! and processed independently, so one bad row never poisons a batch.

use game_mgr_api_types::{EventDto, SessionDto};
use sqlx::PgPool;
use sqlx::error::ErrorKind;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Inserted,
    Duplicate,
    /// Constraint violation specific to this item (unknown game/machine/
    /// profile, check failure). The batch continues.
    Rejected(String),
}

fn constraint_outcome(err: sqlx::Error) -> Result<Outcome, sqlx::Error> {
    match &err {
        sqlx::Error::Database(db) => match db.kind() {
            ErrorKind::ForeignKeyViolation
            | ErrorKind::CheckViolation
            | ErrorKind::NotNullViolation
            | ErrorKind::UniqueViolation => Ok(Outcome::Rejected(db.message().to_string())),
            _ => Err(err),
        },
        _ => Err(err),
    }
}

pub async fn insert_session(pool: &PgPool, s: &SessionDto) -> Result<Outcome, sqlx::Error> {
    let result = sqlx::query(
        r#"
        INSERT INTO sessions
            (id, machine_id, profile_id, game_id, started_at, ended_at, duration_s,
             exit_code, end_reason)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(s.id)
    .bind(s.machine_id)
    .bind(s.profile_id)
    .bind(&s.game_id)
    .bind(s.started_at)
    .bind(s.ended_at)
    .bind(s.duration_s)
    .bind(s.exit_code)
    .bind(s.end_reason.as_str())
    .execute(pool)
    .await;

    match result {
        Ok(done) if done.rows_affected() == 1 => Ok(Outcome::Inserted),
        Ok(_) => Ok(Outcome::Duplicate),
        Err(err) => constraint_outcome(err),
    }
}

pub async fn insert_event(pool: &PgPool, e: &EventDto) -> Result<Outcome, sqlx::Error> {
    let payload = if e.payload.is_null() {
        serde_json::json!({})
    } else {
        e.payload.clone()
    };
    let result = sqlx::query(
        r#"
        INSERT INTO events (client_event_id, machine_id, profile_id, game_id, kind, payload, occurred_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (client_event_id) DO NOTHING
        "#,
    )
    .bind(e.client_event_id)
    .bind(e.machine_id)
    .bind(e.profile_id)
    .bind(&e.game_id)
    .bind(&e.kind)
    .bind(payload)
    .bind(e.occurred_at)
    .execute(pool)
    .await;

    match result {
        Ok(done) if done.rows_affected() == 1 => Ok(Outcome::Inserted),
        Ok(_) => Ok(Outcome::Duplicate),
        Err(err) => constraint_outcome(err),
    }
}
