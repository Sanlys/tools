use game_mgr_api_types::{MachineDto, RegisterMachineRequest};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Machine {
    pub id: Uuid,
    pub name: String,
    pub os: Option<String>,
    pub client_version: Option<String>,
    pub registered_by: Uuid,
    pub syncthing_device_id: Option<String>,
    pub created_at: OffsetDateTime,
    pub last_seen_at: Option<OffsetDateTime>,
}

impl From<Machine> for MachineDto {
    fn from(m: Machine) -> Self {
        MachineDto {
            id: m.id,
            name: m.name,
            os: m.os,
            client_version: m.client_version,
            syncthing_device_id: m.syncthing_device_id,
            last_seen_at: m.last_seen_at,
        }
    }
}

/// Idempotent register/heartbeat. `registered_by` is set on first contact
/// and deliberately never changed by later upserts.
pub async fn upsert(
    pool: &PgPool,
    id: Uuid,
    req: &RegisterMachineRequest,
    registered_by: Uuid,
) -> Result<Machine, sqlx::Error> {
    sqlx::query_as::<_, Machine>(
        r#"
        INSERT INTO machines (id, name, os, client_version, syncthing_device_id, registered_by, last_seen_at)
        VALUES ($1, $2, $3, $4, $5, $6, now())
        ON CONFLICT (id) DO UPDATE SET
            name = EXCLUDED.name,
            os = EXCLUDED.os,
            client_version = EXCLUDED.client_version,
            syncthing_device_id = EXCLUDED.syncthing_device_id,
            last_seen_at = now()
        RETURNING id, name, os, client_version, registered_by, syncthing_device_id, created_at, last_seen_at
        "#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.os)
    .bind(&req.client_version)
    .bind(&req.syncthing_device_id)
    .bind(registered_by)
    .fetch_one(pool)
    .await
}

pub async fn list(pool: &PgPool) -> Result<Vec<Machine>, sqlx::Error> {
    sqlx::query_as::<_, Machine>(
        "SELECT id, name, os, client_version, registered_by, syncthing_device_id, created_at, last_seen_at
         FROM machines ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
}
