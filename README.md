# tools

Internal homelab tools platform: a Rust monorepo of shared adapters
(S3, Postgres, Kubernetes, metrics), a unified egui/wasm UI, and Helm/CI
templates, so a new tool starts from working infrastructure instead of
rebuilding it each time.

Start here: **[`docs/architecture.md`](docs/architecture.md)**.

## Quick reference

| I want to... | See |
|---|---|
| Understand the overall design | `docs/architecture.md` |
| Add a new tool | `docs/adding-a-tool.md` (or run `templates/new-tool/generate.sh <name>`) |
| Give a tool an S3 bucket | `docs/s3-buckets.md` |
| Give a tool a Postgres database | `docs/postgres.md` |
| Wire up metrics/alerts/a dashboard | `docs/observability.md` |
| Understand the CI/CD pipeline | `docs/ci-cd.md` |

## Local development

```sh
# Backends
cargo run -p hello-backend      # 0.0.0.0:8081, needs DATABASE_URL + S3 env vars (see docs/)
cargo run -p portal-backend     # 0.0.0.0:8080

# Portal UI (wasm, hot-reloading)
cargo install trunk
cd apps/portal/frontend && trunk serve    # http://localhost:1420, proxies /api,/config,/health to :8080

# Any tool's standalone native UI
cargo run -p hello-frontend --bin hello-standalone

cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Repo layout

```
crates/            shared adapters + the egui Panel contract
apps/               portal (unified app) + hello (reference example tool)
deploy/            Helm charts (shared library + one per app) + ArgoCD Applications
templates/new-tool/ cargo-generate template for scaffolding a new tool
.github/workflows/  CI (build/test/lint) + CD (build, push, GitOps bump)
docs/               everything above, in depth
```
