//! Profiles: N per user, transferable with an audit trail (PLAN.md §8.0).

use game_mgr_api_types::ProfileDto;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Profile {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub created_at: OffsetDateTime,
}

impl From<Profile> for ProfileDto {
    fn from(p: Profile) -> Self {
        ProfileDto {
            id: p.id,
            owner_user_id: p.owner_user_id,
            name: p.name,
            created_at: p.created_at,
        }
    }
}

/// Result of an ownership-guarded mutation. The guard lives in the SQL
/// (`WHERE id = … AND owner_user_id = …`) so check + mutate is atomic.
#[derive(Debug)]
pub enum OwnedUpdate {
    Updated(Profile),
    NotFound,
    NotOwner,
}

pub async fn create(pool: &PgPool, owner: Uuid, name: &str) -> Result<Profile, sqlx::Error> {
    sqlx::query_as::<_, Profile>(
        "INSERT INTO profiles (owner_user_id, name) VALUES ($1, $2)
         RETURNING id, owner_user_id, name, created_at",
    )
    .bind(owner)
    .bind(name)
    .fetch_one(pool)
    .await
}

pub async fn list_all(pool: &PgPool) -> Result<Vec<Profile>, sqlx::Error> {
    sqlx::query_as::<_, Profile>(
        "SELECT id, owner_user_id, name, created_at FROM profiles ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
}

pub async fn list_owned(pool: &PgPool, owner: Uuid) -> Result<Vec<Profile>, sqlx::Error> {
    sqlx::query_as::<_, Profile>(
        "SELECT id, owner_user_id, name, created_at FROM profiles
         WHERE owner_user_id = $1 ORDER BY created_at",
    )
    .bind(owner)
    .fetch_all(pool)
    .await
}

async fn exists(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM profiles WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?
            > 0,
    )
}

pub async fn rename(
    pool: &PgPool,
    id: Uuid,
    caller: Uuid,
    name: &str,
) -> Result<OwnedUpdate, sqlx::Error> {
    let updated = sqlx::query_as::<_, Profile>(
        "UPDATE profiles SET name = $3 WHERE id = $1 AND owner_user_id = $2
         RETURNING id, owner_user_id, name, created_at",
    )
    .bind(id)
    .bind(caller)
    .bind(name)
    .fetch_optional(pool)
    .await?;

    match updated {
        Some(p) => Ok(OwnedUpdate::Updated(p)),
        None if exists(pool, id).await? => Ok(OwnedUpdate::NotOwner),
        None => Ok(OwnedUpdate::NotFound),
    }
}

/// Delete a profile (owner only). Cascades: its sessions and transfer
/// history go with it; events keep their rows with `profile_id` nulled
/// (migration 0003). The UI warns before calling this.
pub async fn delete(pool: &PgPool, id: Uuid, caller: Uuid) -> Result<OwnedUpdate, sqlx::Error> {
    let deleted = sqlx::query_as::<_, Profile>(
        "DELETE FROM profiles WHERE id = $1 AND owner_user_id = $2
         RETURNING id, owner_user_id, name, created_at",
    )
    .bind(id)
    .bind(caller)
    .fetch_optional(pool)
    .await?;

    match deleted {
        Some(p) => Ok(OwnedUpdate::Updated(p)),
        None if exists(pool, id).await? => Ok(OwnedUpdate::NotOwner),
        None => Ok(OwnedUpdate::NotFound),
    }
}

/// Outcome of a transfer attempt. Ownership is checked *before* the
/// self-transfer case so a non-owner always gets `NotOwner`.
#[derive(Debug)]
pub enum TransferOutcome {
    Done(Profile),
    NotFound,
    NotOwner,
    AlreadyOwner,
}

/// Transfer ownership and write the immutable audit row in one transaction
/// (`FOR UPDATE` lock makes check + mutate atomic). A foreign-key violation
/// means the target user does not exist.
pub async fn transfer(
    pool: &PgPool,
    id: Uuid,
    from_user: Uuid,
    to_user: Uuid,
) -> Result<TransferOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let current: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_user_id FROM profiles WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;

    match current {
        None => Ok(TransferOutcome::NotFound),
        Some(owner) if owner != from_user => Ok(TransferOutcome::NotOwner),
        Some(_) if from_user == to_user => Ok(TransferOutcome::AlreadyOwner),
        Some(_) => {
            let profile = sqlx::query_as::<_, Profile>(
                "UPDATE profiles SET owner_user_id = $2 WHERE id = $1
                 RETURNING id, owner_user_id, name, created_at",
            )
            .bind(id)
            .bind(to_user)
            .fetch_one(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO profile_transfers (profile_id, from_user_id, to_user_id)
                 VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(from_user)
            .bind(to_user)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(TransferOutcome::Done(profile))
        }
    }
}
