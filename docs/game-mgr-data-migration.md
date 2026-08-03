# game-mgr: migrating local dev data to the deployed instance

A one-time runbook for moving the Postgres data (and, separately, the
bucket contents) from the standalone `game-mgr` local dev stack
(`deploy/compose.yaml` in the old repo) into the deployed `tools-game-mgr`
instance. Plain `pg_dump`/`pg_restore` is sufficient here — the schema is
byte-identical between the old standalone repo and this port, and nothing
in the database stores a bucket name, host, or URL (see
`docs/game-mgr-buckets.md`), so this is a bounded three-part job: dump,
restore, fix one stale user id.

Run all of this by hand, in order, against a stopped app on both ends —
none of it should run while `game-mgr-backend` (old or new) is live.

## 0. What you need

- `pg_dump` / `pg_restore` / `psql` — any Postgres 16 client works, since
  both the old dev stack and the deployed instance run `postgres:16`.
- `kubectl`, pointed at the cluster.
- `jq`.

## 1. Stop everything that talks to either database

```sh
# old local dev stack (in the game-mgr repo)
docker compose -f deploy/compose.yaml stop server

# new deployed instance -- one Deployment serves both the API and the
# compiled wasm UI (apps/game-mgr/backend's main.rs), so this is the only
# thing to scale down
kubectl -n tools-game-mgr scale deployment/tools-game-mgr --replicas=0
```

Leave both Postgres containers/pods themselves running — you need them up
to dump from and restore into.

## 2. Reach the new cluster's Postgres

Its Service is ClusterIP-only, so forward it to a local port first (leave
this running in its own terminal):

```sh
kubectl -n tools-game-mgr port-forward svc/tools-game-mgr-postgres 5433:5432
```

Grab its credentials (same pattern `docs/game-mgr-buckets.md` uses for the
bucket secret):

```sh
kubectl -n tools-game-mgr get secret tools-game-mgr-postgres -o json \
  | jq -r '.data | map_values(@base64d)'
# -> POSTGRES_USER, POSTGRES_PASSWORD, POSTGRES_DB (gamemgr), ...
```

## 3. Dump the old (local dev) database

The old stack's `postgres` service publishes 5432 to the host directly, with
the fixed dev credentials from `deploy/compose.yaml`:

```sh
pg_dump -Fc --no-owner --no-acl \
  "postgres://gamemgr:gamemgr@localhost:5432/gamemgr" \
  -f gamemgr.dump
```

## 4. Restore into the new database

The target database already has the schema in it (the backend's embedded
`sqlx` migrations ran the first time it ever started) — `--clean --if-exists`
drops that first so the restore is a clean full replacement rather than a
merge:

```sh
PGPASSWORD='<POSTGRES_PASSWORD from step 2>' \
pg_restore -h localhost -p 5433 \
  -U <POSTGRES_USER from step 2> \
  -d gamemgr \
  --clean --if-exists --no-owner --no-acl \
  gamemgr.dump
```

## 5. Fix the one stale user id

The old dev stack authenticates with `GM_AUTH_MODE=fake` /
`GM_AUTH_FAKE_SUB=dev-user` (`deploy/compose.yaml`) — every row in the dump
is owned by the literal string `dev-user`, not a real identity. The
deployed instance always validates against the real IDP
(`crates/adapters/auth`), whose `sub` claim is that IDP's own internal
`users.id` (`apps/idp/backend/src/routes/oauth.rs`) — a random UUID assigned
at account creation, unrelated to `dev-user`. Fix this by repointing the
existing row rather than logging in and reconciling two rows: game-mgr's
other tables (`profiles.owner_user_id`, `machines.registered_by`, ...) all
reference game-mgr's own internal `users.id`, not `sub`, so a single
`UPDATE` is enough — no FK repointing needed.

**5a. Look up your real idp user id** — directly against idp's own
database, no login or running idp backend required:

```sh
kubectl -n tools-idp port-forward svc/tools-idp-postgres 5434:5432 &

kubectl -n tools-idp get secret tools-idp-postgres -o json \
  | jq -r '.data | map_values(@base64d)'
# -> note POSTGRES_USER / POSTGRES_PASSWORD

PGPASSWORD='<idp POSTGRES_PASSWORD>' \
psql -h localhost -p 5434 -U <idp POSTGRES_USER> -d idp \
  -c "SELECT id, username FROM users;"
```

Copy the `id` (a UUID) for your account.

**5b. Repoint game-mgr's `dev-user` row at it:**

```sh
PGPASSWORD='<gamemgr POSTGRES_PASSWORD>' \
psql -h localhost -p 5433 -U <gamemgr POSTGRES_USER> -d gamemgr \
  -c "UPDATE users SET sub = '<idp-user-id-from-5a>' WHERE sub = 'dev-user';"
```

If you ever used the `x-fake-sub` header to test as other identities
locally, repeat 5a/5b for each of those subs too, or drop the throwaway
ones instead (cascades to their profiles/sessions):

```sh
psql ... -c "DELETE FROM users WHERE sub = '<other-fake-sub>';"
```

## 6. Verify, then bring the new instance back up

```sh
PGPASSWORD='...' psql -h localhost -p 5433 -U <user> -d gamemgr \
  -c "SELECT id, sub FROM users;"
# should show your account(s), sub = the real idp uuid(s) -- no 'dev-user' left

kubectl -n tools-game-mgr scale deployment/tools-game-mgr --replicas=1
```

## 7. Bucket data (separate, independent of the above)

Nothing in the database needs to change for this — the DB only ever stores
relative object keys, never a bucket name or host. Mirror the objects
straight across with `mc` (credentials for the new bucket per
`docs/game-mgr-buckets.md`):

```sh
mc alias set gm-old <old-endpoint> <old-access-key> <old-secret-key>
mc alias set gm-new http://<BUCKET_HOST>:<BUCKET_PORT> <AWS_ACCESS_KEY_ID> <AWS_SECRET_ACCESS_KEY>
mc mirror gm-old/<old-bucket> gm-new/tools-game-mgr
```
