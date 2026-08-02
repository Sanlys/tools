//! Game definitions: server-stored titles created from the client UI
//! (PLAN.md §4.1). The server treats `config` as opaque class data.

use game_mgr_api_types::{ArtifactDto, GameDefinition, GameDto, UpsertGameRequest};
use sqlx::PgPool;
use time::OffsetDateTime;

#[derive(Debug, Clone, sqlx::FromRow)]
struct GameRow {
    id: String,
    title: String,
    class: String,
    version: String,
    config: serde_json::Value,
    artifacts: serde_json::Value,
    total_playtime_s: i64,
    session_count: i64,
    last_played: Option<OffsetDateTime>,
}

impl From<GameRow> for GameDto {
    fn from(r: GameRow) -> Self {
        let artifacts: Vec<ArtifactDto> = serde_json::from_value(r.artifacts).unwrap_or_default();
        GameDto {
            definition: GameDefinition {
                id: r.id,
                title: r.title,
                class: r.class,
                version: r.version,
                config: r.config,
                artifacts,
            },
            total_playtime_s: r.total_playtime_s,
            session_count: r.session_count,
            last_played: r.last_played,
        }
    }
}

/// Full upsert: the definition is replaced as submitted (the server is the
/// single source of truth — no version-skew arbitration needed).
pub async fn upsert(
    pool: &PgPool,
    id: &str,
    req: &UpsertGameRequest,
) -> Result<GameDefinition, sqlx::Error> {
    let artifacts =
        serde_json::to_value(&req.artifacts).unwrap_or(serde_json::Value::Array(vec![]));
    sqlx::query(
        r#"
        INSERT INTO games (id, title, class, version, config, artifacts, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, now())
        ON CONFLICT (id) DO UPDATE SET
            title = EXCLUDED.title,
            class = EXCLUDED.class,
            version = EXCLUDED.version,
            config = EXCLUDED.config,
            artifacts = EXCLUDED.artifacts,
            updated_at = now()
        "#,
    )
    .bind(id)
    .bind(&req.title)
    .bind(&req.class)
    .bind(&req.version)
    .bind(&req.config)
    .bind(&artifacts)
    .execute(pool)
    .await?;

    Ok(GameDefinition {
        id: id.to_string(),
        title: req.title.clone(),
        class: req.class.clone(),
        version: req.version.clone(),
        config: req.config.clone(),
        artifacts: req.artifacts.clone(),
    })
}

const LIST_SQL: &str = r#"
    SELECT g.id, g.title, g.class, g.version, g.config, g.artifacts,
           COALESCE(SUM(s.duration_s), 0)::bigint AS total_playtime_s,
           COUNT(s.id)::bigint AS session_count,
           MAX(s.ended_at) AS last_played
    FROM games g
    LEFT JOIN sessions s ON s.game_id = g.id
"#;

pub async fn list_with_stats(pool: &PgPool) -> Result<Vec<GameDto>, sqlx::Error> {
    let rows = sqlx::query_as::<_, GameRow>(&format!(
        "{LIST_SQL} GROUP BY g.id, g.title, g.class, g.version, g.config, g.artifacts ORDER BY g.title"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn get(pool: &PgPool, id: &str) -> Result<Option<GameDto>, sqlx::Error> {
    let row = sqlx::query_as::<_, GameRow>(&format!(
        "{LIST_SQL} WHERE g.id = $1 GROUP BY g.id, g.title, g.class, g.version, g.config, g.artifacts"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}
