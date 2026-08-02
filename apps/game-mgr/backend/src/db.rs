//! Embedded migrations, run against `postgres_adapter::pool_from_env()`'s
//! pool at startup (`main.rs`) and against each integration test's own
//! per-test database (`tests/common`, PLAN.md §15).

use sqlx::migrate::Migrator;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
