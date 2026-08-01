//! Postgres pool wired up from the env vars the platform's Helm library
//! chart generates for a tool that opts into `postgres.enabled: true`.
//!
//! Every app that needs Postgres gets its **own** single-instance Postgres
//! `Deployment` + `PersistentVolumeClaim` + `Service` (see
//! `deploy/charts/tool-library/templates/postgres.yaml`) -- this is not a
//! shared cluster or an operator-managed HA setup, deliberately: it's meant
//! to be the simple default for internal tools, not a production database
//! platform. Reach for something heavier only if a tool genuinely needs it.
//!
//! The chart writes a Secret with `POSTGRES_USER`, `POSTGRES_PASSWORD`,
//! `POSTGRES_DB`, `POSTGRES_HOST`, `POSTGRES_PORT`, and a precomputed
//! `DATABASE_URL`, all projected into the pod via `envFrom`.

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::env;

#[derive(Debug, thiserror::Error)]
pub enum PgConfigError {
    #[error("missing required env var {0} (expected from the tool's postgres Secret)")]
    MissingEnv(&'static str),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
pub struct PgConfig {
    pub database_url: String,
}

impl PgConfig {
    /// Prefers `DATABASE_URL` if set; otherwise assembles one from the
    /// individual `POSTGRES_*` parts, so either shape works.
    pub fn from_env() -> Result<Self, PgConfigError> {
        if let Ok(database_url) = env::var("DATABASE_URL") {
            return Ok(Self { database_url });
        }

        let host = require_env("POSTGRES_HOST")?;
        let port = env::var("POSTGRES_PORT").unwrap_or_else(|_| "5432".to_string());
        let db = require_env("POSTGRES_DB")?;
        let user = require_env("POSTGRES_USER")?;
        let password = require_env("POSTGRES_PASSWORD")?;

        Ok(Self {
            database_url: format!("postgres://{user}:{password}@{host}:{port}/{db}"),
        })
    }
}

fn require_env(key: &'static str) -> Result<String, PgConfigError> {
    env::var(key).map_err(|_| PgConfigError::MissingEnv(key))
}

/// Builds a small connection pool. A single-instance homelab Postgres pod
/// doesn't need a large pool; five connections is a deliberately low
/// default. Override by constructing your own `PgPoolOptions` if a tool
/// needs more.
pub async fn pool_from_env() -> Result<PgPool, PgConfigError> {
    let cfg = PgConfig::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&cfg.database_url)
        .await?;
    Ok(pool)
}
