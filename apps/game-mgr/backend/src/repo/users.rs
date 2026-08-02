use game_mgr_api_types::UserDto;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub sub: String,
    pub display_name: Option<String>,
    pub created_at: OffsetDateTime,
}

impl From<User> for UserDto {
    fn from(u: User) -> Self {
        UserDto {
            id: u.id,
            sub: u.sub,
            display_name: u.display_name,
        }
    }
}

/// Auto-provisioning (PLAN.md §8.0): one row per OIDC subject. The no-op
/// `DO UPDATE` makes `RETURNING` work on the conflict path too.
pub async fn upsert_by_sub(pool: &PgPool, sub: &str) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (sub) VALUES ($1)
        ON CONFLICT (sub) DO UPDATE SET sub = EXCLUDED.sub
        RETURNING id, sub, display_name, created_at
        "#,
    )
    .bind(sub)
    .fetch_one(pool)
    .await
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("SELECT id, sub, display_name, created_at FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn list(pool: &PgPool) -> Result<Vec<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "SELECT id, sub, display_name, created_at FROM users ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
}
