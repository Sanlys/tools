# Local development

## Prerequisites

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk          # builds/serves the portal's wasm UI
cargo install cargo-generate  # only needed for scaffolding a new tool
```

Docker (or Podman) for the local Postgres/MinIO stand-ins below.

On Linux, building anything that depends on `auth-adapter`'s native login
flow (any tool's standalone binary, or `idp-backend` itself) needs dbus
headers for the `keyring` crate: `sudo apt install libdbus-1-dev
pkg-config` (Debian/Ubuntu) or the equivalent for your distro.

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

Native window:

```sh
HELLO_API_BASE_URL=http://localhost:8081 cargo run -p hello-frontend --bin hello-standalone
```

Opens a native window talking to your local `hello-backend`. The env var
isn't optional for local dev: `hello-standalone` defaults to the real
deployed `https://hello.k8s.lysakermoen.com` (what the shipped release
binary should point at out of the box) rather than localhost, so skipping
it here means you're not testing against what you just started in step 3.

Standalone wasm build (what `hello.k8s.lysakermoen.com` actually serves in
prod):

```sh
cd apps/hello/frontend
trunk serve
```

Open `http://localhost:1421`. `Trunk.toml`'s `[[proxy]]` entries forward
`/api`, `/health`, and `/ws` to `localhost:8081` (step 2's `hello-backend`),
so this build can use the same relative, same-origin URLs
(`HelloPanel::new("")`) it uses in production, where `hello-backend` serves
both the API and this compiled bundle from one container.

`apps/webhello` has no wasm build at all -- it's a static page. It shares
`hello`'s own Postgres table and S3 bucket rather than having its own (see
its module doc comment), so it needs step 1's `docker compose up -d` and
the same `.env` step 2 uses -- run it alongside `hello-backend`, not
instead of it. Run its backend (`cargo run -p webhello-backend`, defaulting
to `0.0.0.0:8082`) and open `http://localhost:8082` directly; `STATIC_DIR`
defaults to `./static` relative to the process's cwd, so run it from
`apps/webhello/backend/` (or set `STATIC_DIR` explicitly) rather than the
repo root.

## 6. Run the IDP locally (to test auth end to end)

```sh
cp .env.example .env   # DATABASE_URL below is for idp-backend, not hello
DATABASE_URL=postgres://idp:idp@localhost:5433/idp cargo run -p idp-backend   # 0.0.0.0:4000
```

With no `IDP_CLIENTS_JSON`/`IDP_CLIENTS_FILE` set, it falls back to the
bundled `apps/idp/backend/src/dev_clients.json`, which already declares
`portal`, `hello`, and `webhello` exactly as `deploy/idp/values.yaml` does
in production, just pointed at `localhost` -- and, unlike production, with
`access_restricted: false` so a fresh local account can sign into
everything immediately (production defaults every client to
`access_restricted: true`; see docs/architecture.md's "App login access"
section). Visit `http://localhost:4000` and register the first account
(becomes admin automatically, no invite needed) -- needs a real
WebAuthn-capable browser and a platform authenticator or security key;
there's no password fallback.

With `idp-backend` running, start `hello-backend`/`portal-backend`/
`webhello-backend` and `trunk serve` as above; all three default to
`IDP_ISSUER_URL=http://localhost:4000` if unset, so the "Sign in" button in
the portal's top bar, the Hello panel, and Webhello's page should all work
without any extra env vars. Grant yourself the `operator` role for `hello`
from the IDP's `/admin` page (or the portal's own "Account" panel, once
signed into the portal) to see the "Reset all greetings" button in the
Hello panel go from disabled to enabled.

## The usual checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --target wasm32-unknown-unknown -p portal-frontend --lib
cargo check --target wasm32-unknown-unknown -p hello-frontend --lib
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
