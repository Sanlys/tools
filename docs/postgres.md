# Postgres

A tool opts in with `postgres.enabled: true` in its `values.yaml`. This
gets it a **single-instance** Postgres: a `Deployment` (strategy
`Recreate`, since the one `ReadWriteOnce` PVC can't be mounted by two pods
at once), a `PersistentVolumeClaim`, a `Service`, and a `Secret`.

This is deliberately not HA and not operator-managed (no CloudNativePG,
no Zalando operator) -- it's meant to be the simple default for internal
tools, not a production database platform. If a tool genuinely needs HA
Postgres, that's a case for a real Postgres operator, not this chart.

## Values

```yaml
postgres:
  enabled: true
  image: postgres:16-alpine
  storage: 1Gi
  storageClassName: rook-ceph-block-ssd   # no cluster-default SC exists; must be set explicitly
  database: my_tool
```

## Credentials

The chart generates a password once (`randAlphaNum 24`) and keeps it
stable across `helm upgrade` by reading it back out of the existing
Secret via Helm's `lookup` function, rather than regenerating (and
breaking the running database's credentials) on every release -- the same
trick Bitnami's charts use. On a `helm template`/dry-run there's nothing
to look up, so it just generates a fresh throwaway one; that's fine, it's
only meaningful for a real `helm upgrade --install`.

The Secret (`<fullname>-postgres`) carries both the split
`POSTGRES_USER`/`POSTGRES_PASSWORD`/`POSTGRES_DB`/`POSTGRES_HOST`/
`POSTGRES_PORT` and a precomputed `DATABASE_URL`, projected into the app's
container via `envFrom`. `crates/adapters/postgres::PgConfig::from_env()`
prefers `DATABASE_URL` if set, falling back to assembling one from the
parts -- either shape works.

## Local development

`docker-compose.yml` at the repo root runs a local Postgres with a
`hello`/`hello`/`hello` user/password/database -- `.env.example` has the
matching `DATABASE_URL`. See `docs/local-development.md`.
