# Local development

## Prerequisites

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk          # builds/serves the portal's wasm UI
cargo install cargo-generate  # only needed for scaffolding a new tool
```

Docker (or Podman) for the local Postgres/MinIO stand-ins below.

## The honest version

This is fresh scaffolding, not a battle-tested dev setup: there's no
hot-reload for the backends (no `cargo-watch` wired in -- add it yourself
if you want it: `cargo install cargo-watch && cargo watch -x 'run -p
hello-backend'`), and there are zero `#[test]`s in the repo yet, so `cargo
test` currently just proves everything compiles. What follows is the
actual loop that works today.

## 1. Local infra stand-ins

Nothing in the cluster is reachable from a laptop, so `docker-compose.yml`
at the repo root stands in for what a real deployment gets from rook-ceph
(S3) and the chart's single-pod Postgres:

```sh
docker compose up -d
```

This starts Postgres (`hello`/`hello`/`hello`) on `:5432` and MinIO
(S3-compatible) on `:9000` (API) / `:9001` (console), with a `hello`
bucket created automatically. `apps/hello/backend`'s adapters don't know
or care that it's MinIO instead of Ceph RGW -- same S3 API, same env
vars.

## 2. Run a tool's backend

```sh
cp .env.example .env   # matches docker-compose.yml exactly
set -a && source .env && set +a
cargo run -p hello-backend      # 0.0.0.0:8081
```

`/health` and `/api/status` should now work
(`curl localhost:8081/api/status`), and `/api/status`'s
`bucket_object_count`/Postgres row count prove both connections are real.

## 3. Run the portal backend

```sh
cargo run -p portal-backend     # 0.0.0.0:8080
```

No env vars needed for local dev: with `TOOLS_REGISTRY_JSON`/
`TOOLS_REGISTRY_FILE` unset and no `/etc/portal/tools.json`, it falls back
to the bundled `apps/portal/backend/src/dev_registry.json`, which already
points at `hello` on `localhost:8081`. The dashboard's HTTP check against
it will show healthy once step 2 is running; the k8s-readiness column will
show "unknown" (no cluster to check against locally, and the dev registry
leaves `k8s_namespace`/`k8s_deployment` unset on purpose).

## 4. Run the portal's wasm UI

```sh
cd apps/portal/frontend
trunk serve
```

Open `http://localhost:1420`. `Trunk.toml`'s `[[proxy]]` entries forward
`/api`, `/config`, and `/health` to `localhost:8080` (the portal backend),
so the wasm app can use the same relative URLs it would in production. If
you add a route to the portal backend that a frontend panel needs, add a
matching `[[proxy]]` entry.

From here: the Home panel lists `hello` (fetched from `/config/tools.json`
via the proxy), the Dashboard panel polls `/api/status`, and opening
"Hello" from the sidebar talks *directly* to `http://localhost:8081`
(hello-backend's `api_base_url` from the registry) -- not through the
proxy, which is why `hello-backend`'s CORS is wide open.

## 5. Run a tool standalone (no portal at all)

```sh
cargo run -p hello-frontend --bin hello-standalone
```

Opens a native window, defaulting to `http://localhost:8081`
(`HELLO_API_BASE_URL` overrides it).

## The usual checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --target wasm32-unknown-unknown -p portal-frontend --lib
```

`ci.yml` runs exactly these (plus `helm lint`/`helm template` on every
chart), so a clean run of the above is a good proxy for whether CI will
pass.

## Adding a new tool locally

`templates/new-tool/generate.sh my-tool` (see `docs/adding-a-tool.md`)
scaffolds the crates and chart. If it needs its own Postgres DB or S3
bucket for local dev, either point it at the existing `docker-compose.yml`
Postgres with a new database, or add its own `postgres`/`minio` service +
`.env` entries -- there's nothing tool-specific hard-coded in
`docker-compose.yml`, it's just enough to make `hello` runnable today.
