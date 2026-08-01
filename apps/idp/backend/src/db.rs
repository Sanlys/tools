use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::AppError;

// ── Domain types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
}

fn map_user(r: &sqlx::postgres::PgRow) -> User {
    User {
        id: r.get("id"),
        username: r.get("username"),
        display_name: r.get("display_name"),
        is_admin: r.get("is_admin"),
        created_at: r.get("created_at"),
    }
}

#[derive(Debug, Clone)]
pub struct Credential {
    pub id: String,
    pub user_id: String,
    pub credential_id: Vec<u8>,
    pub public_key: Vec<u8>,
    pub counter: i64,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Client {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub roles: Vec<String>,
    pub native: bool,
}

fn map_client(r: &sqlx::postgres::PgRow) -> Client {
    let uris_json: String = r.get("redirect_uris");
    let roles_json: String = r.get("roles");
    Client {
        client_id: r.get("client_id"),
        name: r.get("name"),
        redirect_uris: serde_json::from_str(&uris_json).unwrap_or_default(),
        roles: serde_json::from_str(&roles_json).unwrap_or_default(),
        native: r.get("native"),
    }
}

#[derive(Debug, Clone)]
pub struct AuthorizationCode {
    pub client_id: String,
    pub user_id: String,
    pub scope: String,
    pub nonce: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub auth_time: i64,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SigningKey {
    pub kid: String,
    pub private_key_pem: String,
    pub public_key_pem: String,
    pub n_b64url: String,
    pub e_b64url: String,
}

#[derive(Debug, Clone)]
pub struct Invite {
    pub id: String,
    pub token: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct RefreshToken {
    pub user_id: String,
    pub client_id: String,
    pub scope: String,
    pub auth_time: i64,
}

// ── Users ─────────────────────────────────────────────────────────────────────

pub async fn create_user(
    pool: &PgPool,
    username: &str,
    display_name: &str,
    is_admin: bool,
) -> Result<User, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, is_admin, created_at) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(username)
    .bind(display_name)
    .bind(is_admin)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(User {
        id,
        username: username.to_string(),
        display_name: display_name.to_string(),
        is_admin,
        created_at: now,
    })
}

pub async fn get_user_by_username(pool: &PgPool, username: &str) -> Result<Option<User>, AppError> {
    let row = sqlx::query(
        "SELECT id, username, display_name, is_admin, created_at FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(map_user))
}

pub async fn get_user_by_id(pool: &PgPool, id: &str) -> Result<Option<User>, AppError> {
    let row = sqlx::query(
        "SELECT id, username, display_name, is_admin, created_at FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(map_user))
}

pub async fn count_users(pool: &PgPool) -> Result<i64, AppError> {
    let row = sqlx::query("SELECT COUNT(*) AS cnt FROM users")
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("cnt"))
}

pub async fn list_users(pool: &PgPool) -> Result<Vec<User>, AppError> {
    let rows = sqlx::query("SELECT id, username, display_name, is_admin, created_at FROM users ORDER BY created_at ASC")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(map_user).collect())
}

pub async fn delete_user(pool: &PgPool, user_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_display_name(
    pool: &PgPool,
    user_id: &str,
    display_name: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET display_name = $1 WHERE id = $2")
        .bind(display_name)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ── Clients (reconciled from IDP_CLIENTS_JSON, see clients.rs) ────────────────

pub async fn reconcile_clients(
    pool: &PgPool,
    configs: &[crate::clients::ClientConfig],
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;

    let ids: Vec<String> = configs.iter().map(|c| c.client_id.clone()).collect();
    sqlx::query("DELETE FROM clients WHERE NOT (client_id = ANY($1))")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;

    for c in configs {
        let uris_json = serde_json::to_string(&c.redirect_uris).unwrap_or_default();
        let roles_json = serde_json::to_string(&c.roles).unwrap_or_default();
        sqlx::query(
            "INSERT INTO clients (client_id, name, redirect_uris, roles, native) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (client_id) DO UPDATE SET name = EXCLUDED.name, redirect_uris = EXCLUDED.redirect_uris, \
             roles = EXCLUDED.roles, native = EXCLUDED.native",
        )
        .bind(&c.client_id)
        .bind(&c.name)
        .bind(&uris_json)
        .bind(&roles_json)
        .bind(c.native)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn get_client(pool: &PgPool, client_id: &str) -> Result<Option<Client>, AppError> {
    let row = sqlx::query(
        "SELECT client_id, name, redirect_uris, roles, native FROM clients WHERE client_id = $1",
    )
    .bind(client_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(map_client))
}

pub async fn list_clients(pool: &PgPool) -> Result<Vec<Client>, AppError> {
    let rows = sqlx::query(
        "SELECT client_id, name, redirect_uris, roles, native FROM clients ORDER BY name ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(map_client).collect())
}

// ── Per-app role grants ───────────────────────────────────────────────────────

pub async fn granted_roles(
    pool: &PgPool,
    user_id: &str,
    client_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query("SELECT role FROM user_app_roles WHERE user_id = $1 AND client_id = $2")
        .bind(user_id)
        .bind(client_id)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.get::<String, _>("role"))
        .collect())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoleGrant {
    pub user_id: String,
    pub client_id: String,
    pub role: String,
}

pub async fn list_roles_for_user(pool: &PgPool, user_id: &str) -> Result<Vec<RoleGrant>, AppError> {
    let rows = sqlx::query("SELECT user_id, client_id, role FROM user_app_roles WHERE user_id = $1 ORDER BY client_id, role")
        .bind(user_id)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| RoleGrant {
            user_id: r.get("user_id"),
            client_id: r.get("client_id"),
            role: r.get("role"),
        })
        .collect())
}

pub async fn grant_role(
    pool: &PgPool,
    user_id: &str,
    client_id: &str,
    role: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO user_app_roles (user_id, client_id, role) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(client_id)
    .bind(role)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn revoke_role(
    pool: &PgPool,
    user_id: &str,
    client_id: &str,
    role: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM user_app_roles WHERE user_id = $1 AND client_id = $2 AND role = $3")
        .bind(user_id)
        .bind(client_id)
        .bind(role)
        .execute(pool)
        .await?;
    Ok(())
}

// ── Credentials ───────────────────────────────────────────────────────────────

pub async fn save_credential(
    pool: &PgPool,
    user_id: &str,
    credential_id: &[u8],
    public_key: &[u8],
    counter: u32,
    label: Option<&str>,
) -> Result<(), AppError> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO credentials (id, user_id, credential_id, public_key, counter, label) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(credential_id)
    .bind(public_key)
    .bind(counter as i64)
    .bind(label)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_credentials_for_user(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<Credential>, AppError> {
    let rows = sqlx::query(
        "SELECT id, user_id, credential_id, public_key, counter, label, created_at FROM credentials WHERE user_id = $1 ORDER BY created_at ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Credential {
            id: r.get("id"),
            user_id: r.get("user_id"),
            credential_id: r.get("credential_id"),
            public_key: r.get("public_key"),
            counter: r.get("counter"),
            label: r.get("label"),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn update_credential(
    pool: &PgPool,
    credential_id: &[u8],
    public_key: &[u8],
    counter: u32,
) -> Result<(), AppError> {
    sqlx::query("UPDATE credentials SET public_key = $1, counter = $2 WHERE credential_id = $3")
        .bind(public_key)
        .bind(counter as i64)
        .bind(credential_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_credential(
    pool: &PgPool,
    credential_id: &str,
    user_id: &str,
) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM credentials WHERE id = $1 AND user_id = $2")
        .bind(credential_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_user_by_credential_id(
    pool: &PgPool,
    credential_id: &[u8],
) -> Result<Option<User>, AppError> {
    let row = sqlx::query(
        "SELECT u.id, u.username, u.display_name, u.is_admin, u.created_at \
         FROM users u JOIN credentials c ON c.user_id = u.id WHERE c.credential_id = $1",
    )
    .bind(credential_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(map_user))
}

// ── Sessions ──────────────────────────────────────────────────────────────────

pub async fn create_session(
    pool: &PgPool,
    user_id: &str,
    user_agent: Option<&str>,
    ip_address: Option<&str>,
) -> Result<Session, AppError> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let expires_at = now + chrono::Duration::hours(24);
    sqlx::query(
        "INSERT INTO sessions (id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(now)
    .bind(expires_at)
    .bind(now)
    .bind(user_agent)
    .bind(ip_address)
    .execute(pool)
    .await?;
    Ok(Session {
        id,
        user_id: user_id.to_string(),
        created_at: now,
        expires_at,
        last_seen_at: now,
        user_agent: user_agent.map(str::to_string),
        ip_address: ip_address.map(str::to_string),
    })
}

pub async fn get_session(pool: &PgPool, session_id: &str) -> Result<Option<Session>, AppError> {
    let row = sqlx::query(
        "SELECT id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address \
         FROM sessions WHERE id = $1 AND expires_at > NOW()",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Session {
        id: r.get("id"),
        user_id: r.get("user_id"),
        created_at: r.get("created_at"),
        expires_at: r.get("expires_at"),
        last_seen_at: r.get("last_seen_at"),
        user_agent: r.get("user_agent"),
        ip_address: r.get("ip_address"),
    }))
}

pub async fn touch_session(pool: &PgPool, session_id: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE sessions SET last_seen_at = NOW() WHERE id = $1")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_session_by_id(pool: &PgPool, session_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_session(
    pool: &PgPool,
    session_id: &str,
    user_id: &str,
) -> Result<bool, AppError> {
    let r = sqlx::query("DELETE FROM sessions WHERE id = $1 AND user_id = $2")
        .bind(session_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn delete_all_sessions(pool: &PgPool, user_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_sessions_for_user(
    pool: &PgPool,
    user_id: &str,
) -> Result<Vec<Session>, AppError> {
    let rows = sqlx::query(
        "SELECT id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address \
         FROM sessions WHERE user_id = $1 AND expires_at > NOW() ORDER BY last_seen_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Session {
            id: r.get("id"),
            user_id: r.get("user_id"),
            created_at: r.get("created_at"),
            expires_at: r.get("expires_at"),
            last_seen_at: r.get("last_seen_at"),
            user_agent: r.get("user_agent"),
            ip_address: r.get("ip_address"),
        })
        .collect())
}

// ── Authorization codes ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn create_authorization_code(
    pool: &PgPool,
    client_id: &str,
    user_id: &str,
    redirect_uri: &str,
    scope: &str,
    nonce: Option<&str>,
    code_challenge: Option<&str>,
    code_challenge_method: Option<&str>,
    auth_time: i64,
) -> Result<String, AppError> {
    let code = generate_token(32);
    let expires_at = Utc::now() + chrono::Duration::minutes(5);
    sqlx::query(
        "INSERT INTO authorization_codes \
         (code, client_id, user_id, redirect_uri, scope, nonce, code_challenge, code_challenge_method, expires_at, auth_time) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&code)
    .bind(client_id)
    .bind(user_id)
    .bind(redirect_uri)
    .bind(scope)
    .bind(nonce)
    .bind(code_challenge)
    .bind(code_challenge_method)
    .bind(expires_at)
    .bind(auth_time)
    .execute(pool)
    .await?;
    Ok(code)
}

pub async fn consume_authorization_code(
    pool: &PgPool,
    code: &str,
    client_id: &str,
    redirect_uri: &str,
) -> Result<Option<AuthorizationCode>, AppError> {
    let row = sqlx::query(
        "UPDATE authorization_codes SET used = TRUE \
         WHERE code = $1 AND client_id = $2 AND redirect_uri = $3 AND used = FALSE AND expires_at > NOW() \
         RETURNING client_id, user_id, scope, nonce, code_challenge, code_challenge_method, auth_time",
    )
    .bind(code)
    .bind(client_id)
    .bind(redirect_uri)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| AuthorizationCode {
        client_id: r.get("client_id"),
        user_id: r.get("user_id"),
        scope: r.get("scope"),
        nonce: r.get("nonce"),
        code_challenge: r.get("code_challenge"),
        code_challenge_method: r.get("code_challenge_method"),
        auth_time: r.get("auth_time"),
    }))
}

/// Returns `(user_id, client_id)` if the code exists but was already used --
/// indicates a replay attack (RFC 6749 §10.5).
pub async fn detect_code_reuse(
    pool: &PgPool,
    code: &str,
) -> Result<Option<(String, String)>, AppError> {
    let row = sqlx::query(
        "SELECT user_id, client_id FROM authorization_codes WHERE code = $1 AND used = TRUE",
    )
    .bind(code)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.get("user_id"), r.get("client_id"))))
}

// ── Refresh tokens ────────────────────────────────────────────────────────────

pub async fn create_refresh_token(
    pool: &PgPool,
    user_id: &str,
    client_id: &str,
    scope: &str,
    ttl_days: i64,
    auth_time: i64,
) -> Result<String, AppError> {
    let token = generate_token(48);
    let hash = hash_token(&token);
    let expires_at = Utc::now() + chrono::Duration::days(ttl_days);
    sqlx::query(
        "INSERT INTO refresh_tokens (token_hash, user_id, client_id, scope, expires_at, auth_time) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&hash)
    .bind(user_id)
    .bind(client_id)
    .bind(scope)
    .bind(expires_at)
    .bind(auth_time)
    .execute(pool)
    .await?;
    Ok(token)
}

pub async fn consume_refresh_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<RefreshToken>, AppError> {
    let hash = hash_token(token);
    let row = sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = NOW() \
         WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > NOW() \
         RETURNING user_id, client_id, scope, auth_time",
    )
    .bind(&hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| RefreshToken {
        user_id: r.get("user_id"),
        client_id: r.get("client_id"),
        scope: r.get("scope"),
        auth_time: r.get("auth_time"),
    }))
}

pub async fn revoke_refresh_token(pool: &PgPool, token: &str) -> Result<bool, AppError> {
    let hash = hash_token(token);
    let r = sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = NOW() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(&hash)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn revoke_all_refresh_tokens_for_user_client(
    pool: &PgPool,
    user_id: &str,
    client_id: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE refresh_tokens SET revoked_at = NOW() WHERE user_id = $1 AND client_id = $2 AND revoked_at IS NULL")
        .bind(user_id)
        .bind(client_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ── Signing keys ──────────────────────────────────────────────────────────────

pub async fn get_active_signing_key(pool: &PgPool) -> Result<Option<SigningKey>, AppError> {
    let row = sqlx::query(
        "SELECT kid, private_key_pem, public_key_pem, n_b64url, e_b64url FROM signing_keys WHERE retired_at IS NULL ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| SigningKey {
        kid: r.get("kid"),
        private_key_pem: r.get("private_key_pem"),
        public_key_pem: r.get("public_key_pem"),
        n_b64url: r.get("n_b64url"),
        e_b64url: r.get("e_b64url"),
    }))
}

pub async fn save_signing_key(pool: &PgPool, key: &SigningKey) -> Result<(), AppError> {
    sqlx::query("INSERT INTO signing_keys (kid, private_key_pem, public_key_pem, n_b64url, e_b64url) VALUES ($1, $2, $3, $4, $5)")
        .bind(&key.kid)
        .bind(&key.private_key_pem)
        .bind(&key.public_key_pem)
        .bind(&key.n_b64url)
        .bind(&key.e_b64url)
        .execute(pool)
        .await?;
    Ok(())
}

// ── Config (used to persist the cookie-encryption key) ────────────────────────

pub async fn get_config(pool: &PgPool, key: &str) -> Result<Option<String>, AppError> {
    let row = sqlx::query("SELECT value FROM config WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get("value")))
}

pub async fn set_config(pool: &PgPool, key: &str, value: &str) -> Result<(), AppError> {
    sqlx::query("INSERT INTO config (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

// ── WebAuthn challenges ───────────────────────────────────────────────────────

pub async fn save_webauthn_registration_challenge(
    pool: &PgPool,
    user_id: &str,
    username: &str,
    invite_id: Option<&str>,
    challenge_state: &str,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + chrono::Duration::minutes(5);
    sqlx::query(
        "INSERT INTO webauthn_registration_challenges (id, user_id, username, invite_id, challenge_state, expires_at) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(username)
    .bind(invite_id)
    .bind(challenge_state)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn get_and_delete_webauthn_registration_challenge(
    pool: &PgPool,
    id: &str,
) -> Result<Option<(String, String, Option<String>, String)>, AppError> {
    let row = sqlx::query(
        "DELETE FROM webauthn_registration_challenges WHERE id = $1 AND expires_at > NOW() \
         RETURNING user_id, username, invite_id, challenge_state",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| {
        (
            r.get("user_id"),
            r.get("username"),
            r.get("invite_id"),
            r.get("challenge_state"),
        )
    }))
}

pub async fn save_webauthn_auth_challenge(
    pool: &PgPool,
    challenge_state: &str,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + chrono::Duration::minutes(5);
    sqlx::query("INSERT INTO webauthn_auth_challenges (id, challenge_state, expires_at) VALUES ($1, $2, $3)")
        .bind(&id)
        .bind(challenge_state)
        .bind(expires_at)
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn get_and_delete_webauthn_auth_challenge(
    pool: &PgPool,
    id: &str,
) -> Result<Option<String>, AppError> {
    let row = sqlx::query("DELETE FROM webauthn_auth_challenges WHERE id = $1 AND expires_at > NOW() RETURNING challenge_state")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get("challenge_state")))
}

// ── Invites ───────────────────────────────────────────────────────────────────

pub async fn create_invite(
    pool: &PgPool,
    created_by: &str,
    note: Option<&str>,
    ttl_hours: i64,
) -> Result<Invite, AppError> {
    let id = Uuid::new_v4().to_string();
    let token = generate_token(32);
    let now = Utc::now();
    let expires_at = now + chrono::Duration::hours(ttl_hours);
    sqlx::query(
        "INSERT INTO invites (id, token, created_by, note, expires_at) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(&token)
    .bind(created_by)
    .bind(note)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(Invite {
        id,
        token,
        note: note.map(str::to_string),
        created_at: now,
        expires_at,
        used_at: None,
    })
}

pub async fn get_invite_by_token(pool: &PgPool, token: &str) -> Result<Option<Invite>, AppError> {
    let row = sqlx::query(
        "SELECT id, token, note, created_at, expires_at, used_at FROM invites WHERE token = $1",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Invite {
        id: r.get("id"),
        token: r.get("token"),
        note: r.get("note"),
        created_at: r.get("created_at"),
        expires_at: r.get("expires_at"),
        used_at: r.get("used_at"),
    }))
}

pub async fn consume_invite(
    pool: &PgPool,
    invite_id: &str,
    used_by: &str,
) -> Result<bool, AppError> {
    let r = sqlx::query("UPDATE invites SET used_at = NOW(), used_by = $1 WHERE id = $2 AND used_at IS NULL AND expires_at > NOW()")
        .bind(used_by)
        .bind(invite_id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn list_invites(pool: &PgPool) -> Result<Vec<Invite>, AppError> {
    let rows = sqlx::query("SELECT id, token, note, created_at, expires_at, used_at FROM invites ORDER BY created_at DESC")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Invite {
            id: r.get("id"),
            token: r.get("token"),
            note: r.get("note"),
            created_at: r.get("created_at"),
            expires_at: r.get("expires_at"),
            used_at: r.get("used_at"),
        })
        .collect())
}

pub async fn delete_invite(pool: &PgPool, invite_id: &str) -> Result<bool, AppError> {
    let r = sqlx::query("DELETE FROM invites WHERE id = $1 AND used_at IS NULL")
        .bind(invite_id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn generate_token(len: usize) -> String {
    use rand::Rng;
    let bytes: Vec<u8> = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(len)
        .collect();
    String::from_utf8(bytes).unwrap()
}

pub fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(token.as_bytes());
    hex::encode(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_length() {
        let t = generate_token(32);
        assert_eq!(t.len(), 32);
    }

    #[test]
    fn hash_token_is_deterministic() {
        assert_eq!(hash_token("hello"), hash_token("hello"));
    }

    #[test]
    fn hash_token_differs_for_different_inputs() {
        assert_ne!(hash_token("a"), hash_token("b"));
    }
}
