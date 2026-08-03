//! Persistence layer: every SQL query lives here, behind plain functions
//! taking a `&PgPool`. Runtime-checked queries; correctness is guaranteed by
//! the DB-backed test suite (PLAN.md §15).

pub mod games;
pub mod ingest;
pub mod machines;
pub mod profiles;
pub mod sessions;
pub mod users;

use sqlx::error::ErrorKind;

/// True when the error is a foreign-key violation (e.g. transfer target user
/// does not exist, session references an unknown game).
pub fn is_fk_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.kind() == ErrorKind::ForeignKeyViolation)
}
