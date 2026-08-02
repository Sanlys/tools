-- Dynamic OAuth client registration (admin-managed, DB-only), alongside the
-- existing GitOps-declared IDP_CLIENTS_JSON registry from 001. `managed`
-- says which is which: `reconcile_clients` (boot-time JSON sync) only ever
-- inserts/updates/deletes rows with `managed = FALSE`, so an admin-created
-- client is never touched or wiped out by a redeploy, and the admin API
-- only ever touches rows with `managed = TRUE`, so it can't edit a
-- GitOps-declared client out from under version control. Every client
-- stays a *public* client (PKCE only, no client_secret) even when
-- admin-created -- see docs/architecture.md.
ALTER TABLE clients ADD COLUMN IF NOT EXISTS managed BOOLEAN NOT NULL DEFAULT FALSE;

-- Which JWT claim name this client's role list is emitted under (default
-- "roles", matching our own tools' expectations). Lets an external relying
-- party that looks for a different claim name (e.g. ArgoCD's RBAC, which
-- expects group membership under "groups") get the same data without the
-- IDP needing to know about that app specifically.
ALTER TABLE clients ADD COLUMN IF NOT EXISTS roles_claim TEXT NOT NULL DEFAULT 'roles';

-- When TRUE, a user needs an explicit `user_app_access` grant below to
-- complete login for this client at all -- independent of `user_app_roles`,
-- which is about what a logged-in user can *do*, not whether they can log
-- in in the first place. Lets e.g. an ArgoCD client be locked down to one
-- specific user while everyone else in the IDP is denied at /oauth/authorize.
ALTER TABLE clients ADD COLUMN IF NOT EXISTS access_restricted BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS user_app_access (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id  TEXT NOT NULL REFERENCES clients(client_id) ON DELETE CASCADE,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, client_id)
);
