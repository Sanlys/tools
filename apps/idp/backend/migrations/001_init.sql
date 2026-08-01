-- Users. No password column at all -- passkeys (WebAuthn) are the only
-- first-factor login method. `is_admin` gates this IDP's own admin console
-- (invites, user list, per-app role grants) -- unrelated to the per-app
-- `user_app_roles` grants below, which are what gate *other tools*.
CREATE TABLE IF NOT EXISTS users (
    id           TEXT PRIMARY KEY,
    username     TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    is_admin     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Passkey credentials (one user may register several, e.g. one per device).
CREATE TABLE IF NOT EXISTS credentials (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    credential_id BYTEA NOT NULL UNIQUE,
    public_key    BYTEA NOT NULL,  -- serialised webauthn-rs Passkey JSON
    counter       BIGINT NOT NULL DEFAULT 0,
    label         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- OAuth 2.0 clients: reconciled at boot from IDP_CLIENTS_JSON (a static,
-- GitOps-declared registry -- see deploy/idp/values.yaml), not created via
-- an admin CRUD flow. `roles` is that app's own declared role vocabulary
-- (a flat JSON array of strings); `native` allows the RFC 8252
-- loopback-any-port redirect_uri exception for standalone eframe binaries.
-- Every client is a *public* client (PKCE only, no secret) -- see
-- docs/architecture.md's "Porting the IDP" note on why no client_secret is
-- needed here.
CREATE TABLE IF NOT EXISTS clients (
    client_id     TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    redirect_uris TEXT NOT NULL,  -- JSON array
    roles         TEXT NOT NULL,  -- JSON array of role name strings
    native        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Per-user, per-app role grants -- this is what "an app requires a role,
-- and only some users have it" actually means at the data level. Roles are
-- opaque strings scoped to one client_id; two different apps can reuse the
-- same role name with completely different meaning.
CREATE TABLE IF NOT EXISTS user_app_roles (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id  TEXT NOT NULL REFERENCES clients(client_id) ON DELETE CASCADE,
    role       TEXT NOT NULL,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, client_id, role)
);

-- Short-lived authorization codes (authorization_code + PKCE flow).
CREATE TABLE IF NOT EXISTS authorization_codes (
    code                  TEXT PRIMARY KEY,
    client_id             TEXT NOT NULL REFERENCES clients(client_id) ON DELETE CASCADE,
    user_id               TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    redirect_uri          TEXT NOT NULL,
    scope                 TEXT NOT NULL,
    nonce                 TEXT,
    code_challenge        TEXT,
    code_challenge_method TEXT,
    expires_at            TIMESTAMPTZ NOT NULL,
    used                  BOOLEAN NOT NULL DEFAULT FALSE,
    auth_time             BIGINT NOT NULL
);

-- Refresh tokens, stored as SHA-256(token) to avoid plaintext storage.
CREATE TABLE IF NOT EXISTS refresh_tokens (
    token_hash  TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id   TEXT NOT NULL REFERENCES clients(client_id) ON DELETE CASCADE,
    scope       TEXT NOT NULL,
    issued_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked_at  TIMESTAMPTZ,
    auth_time   BIGINT NOT NULL
);

-- The IDP's own first-party browser session (an opaque id in an encrypted
-- cookie) -- separate from the OAuth access/ID tokens issued to relying
-- parties. Used both to authenticate calls to the IDP's own /api/* routes
-- and to drive silent SSO (`prompt=none`) for a second tool's tab.
CREATE TABLE IF NOT EXISTS sessions (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at   TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    user_agent   TEXT,
    ip_address   TEXT
);

-- RS256 signing key pairs (private key stored as PKCS#8 PEM). Generated on
-- first boot and persisted here rather than as a sops secret -- see
-- docs/architecture.md: this and `config.cookie_secret` below are the only
-- two secrets the IDP needs, and both are self-generated.
CREATE TABLE IF NOT EXISTS signing_keys (
    kid             TEXT PRIMARY KEY,
    private_key_pem TEXT NOT NULL,
    public_key_pem  TEXT NOT NULL,
    n_b64url        TEXT NOT NULL,
    e_b64url        TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    retired_at      TIMESTAMPTZ
);

-- WebAuthn ceremony challenges (short-lived, deleted on use).
CREATE TABLE IF NOT EXISTS webauthn_registration_challenges (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL,
    username        TEXT NOT NULL,
    invite_id       TEXT,
    challenge_state TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at      TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS webauthn_auth_challenges (
    id              TEXT PRIMARY KEY,
    challenge_state TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at      TIMESTAMPTZ NOT NULL
);

-- Invite tokens for invite-only registration (the very first user to
-- register becomes admin with no invite needed).
CREATE TABLE IF NOT EXISTS invites (
    id          TEXT PRIMARY KEY,
    token       TEXT NOT NULL UNIQUE,
    created_by  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    note        TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at  TIMESTAMPTZ NOT NULL,
    used_at     TIMESTAMPTZ,
    used_by     TEXT REFERENCES users(id) ON DELETE SET NULL
);

-- Generic key-value config store (currently just the cookie-encryption key,
-- generated on first boot so it survives restarts without a sops secret).
CREATE TABLE IF NOT EXISTS config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
