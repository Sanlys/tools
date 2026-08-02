-- Flip the default posture: every app (built-in or external, GitOps-declared
-- or admin-created) now requires an explicit user_app_access grant to log in
-- at all, unless an admin deliberately opts a client back out. Before this,
-- access_restricted defaulted to FALSE (any IDP user could log into any
-- app); the new default is TRUE.
ALTER TABLE clients ALTER COLUMN access_restricted SET DEFAULT TRUE;

-- Grandfather every existing user into every client that already exists at
-- the moment this migration runs, so nobody who could already log into
-- portal/hello gets locked out by this deploy -- the new "opt-in by
-- default" posture only bites going forward, for clients that don't exist
-- yet (e.g. a newly-added webhello client, or any future one).
INSERT INTO user_app_access (user_id, client_id)
SELECT u.id, c.client_id FROM users u CROSS JOIN clients c
ON CONFLICT DO NOTHING;

-- Existing rows keep whatever value reconcile_clients / the admin API gave
-- them (portal/hello are about to be reconciled with access_restricted =
-- true explicitly from IDP_CLIENTS_JSON on this same deploy anyway).
