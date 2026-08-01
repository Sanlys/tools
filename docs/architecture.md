# Architecture

A Rust monorepo of shared adapters, UI patterns, and deploy templates for
internal homelab tools -- the goal is that porting or building a new tool
means writing its actual logic, not re-inventing S3 access, a Postgres
connection, a Helm chart, CI, or metrics every time.

## Layout

```
crates/
  platform-core     -- Panel trait (the egui UI contract every tool implements)
  platform-config    -- runtime config fetching for wasm/native frontends (JsonResource)
  api-types           -- wire DTOs shared between backends and frontends
  adapters/
    s3                -- S3 client wired from a rook-ceph ObjectBucketClaim's env vars
    postgres          -- Postgres pool wired from a single-pod Postgres's env vars
    k8s               -- read-only Deployment-readiness client (dashboard only)
    metrics           -- axum-prometheus /metrics wiring
    auth              -- AuthUser extractor (backend) + LoginWidget (frontend),
                          talking to apps/idp -- see "Auth" below

apps/
  portal/             -- the unified web app (see below)
    backend           -- axum: serves the wasm bundle, tool registry, dashboard status
    frontend          -- egui/eframe, compiles to wasm, hosts every tool's panel
  hello/              -- reference example tool exercising every adapter
    backend
    frontend
  idp/                -- the platform's own OIDC provider + WebAuthn passkey login
    backend           -- axum: OIDC endpoints, WebAuthn ceremonies, Postgres
    frontend          -- plain static HTML/CSS/JS (not egui -- see "Auth" below)

deploy/
  charts/tool-library -- shared Helm library chart (Deployment, Service, Ingress,
                          bucket claim, Postgres, ServiceMonitor, PrometheusRule,
                          GrafanaDashboard, dashboard RBAC)
  hello/, portal/     -- per-app charts + values.yaml + a standalone ArgoCD Application

templates/new-tool/   -- cargo-generate template scaffolding a new tool's app + chart

.github/workflows/    -- ci.yml (build/test/lint), release.yml (build+push+GitOps bump)
```

## The unified app

"The tools platform" is `apps/portal`: one egui app that compiles to a
single wasm binary and is also runnable natively. It hosts three kinds of
panel, all implementing the same `platform_core::Panel` trait:

- **Home** -- links out to every tool's standalone deployment (subdomain
  per tool).
- **Dashboard** -- per-tool health, combining an HTTP health-check hit and
  Kubernetes Deployment readiness.
- **One panel per tool** -- e.g. `hello_frontend::HelloPanel`. Multiple
  panels can be open at once, each in its own `egui::Window` -- that's what
  "tools can be opened simultaneously" means here.

There is **no runtime plugin loading**. The set of panels is a compile-time
`enum` (`ToolPanel` in `apps/portal/frontend/src/lib.rs`) with a
hand-written dispatch `match` -- not the `enum_dispatch` crate, whose
trait/enum linkage only works within a single crate and silently produces
nothing across the crate boundary between `platform-core` (trait) and
`apps/portal` (enum). See the comment on `ToolPanel` for details. Adding a
tool means adding a variant + a match arm, per `docs/adding-a-tool.md`.

Per-tool backend URLs are **not** baked into the wasm binary. The portal
backend serves `/config/tools.json` (a `platform_config::ToolRegistry`);
the wasm app fetches it at load time. One wasm build works unmodified
across dev/staging/prod and across whatever subdomains you deploy tools at.

## Standalone tools

Every tool also gets a native standalone binary for free:
`platform_core::standalone::run(YourPanel::new(...))` wraps any `Panel` in
its own eframe window. A tool can additionally ship a completely different
frontend stack (iced, a plain web UI, whatever) if egui doesn't fit -- the
platform doesn't require it, it just makes egui the path of least
resistance.

## Backends

Every tool's backend is a plain axum service. It's reachable from the
browser two ways: plain HTTPS (`ehttp`/`fetch`, used by `platform_config`)
or a websocket (`apps/hello/backend`'s `/ws` demonstrates the pattern;
`ewebsock` is the wasm+native-compatible client-side counterpart to
`ehttp` if a tool's frontend needs to consume one).

## What's cluster-specific here

This now targets the real homelab cluster (Talos + Argo CD, see the
`kubernetes` repo's `CLAUDE.md`) rather than generic placeholders:

- Ingress: `ingressClassName: internal` (nginx-ingress,
  `*.k8s.lysakermoen.com`) or `public` (`*.lysakermoen.com`), with the
  matching `cert-manager.io/cluster-issuer` annotation
  (`letsencrypt-internal`/`-public`) -- set per tool in its own
  `values.yaml`, see `deploy/hello/values.yaml`.
- Monitoring: this cluster's Prometheus Operator (kube-prometheus-stack)
  watches `ServiceMonitor`/`PrometheusRule` cluster-wide with an empty
  selector, so `monitoring.serviceMonitorLabels`/`prometheusRuleLabels` can
  stay empty. There's no grafana-operator/`GrafanaDashboard` CRD --
  dashboards are plain `ConfigMap`s labeled `grafana_dashboard: "1"` in the
  `monitoring` namespace (`monitoring.dashboardNamespace`), picked up by
  Grafana's sidecar. See `docs/observability.md`.
- `image.repository` in every `values.yaml` and `vars.REGISTRY_HOST` in
  `.github/workflows/release.yml` point at this cluster's Harbor
  (`harbor.k8s.lysakermoen.com`, project `tools` -- must be created there
  before first push).
- `deploy/*/app.yaml`'s `repoURL`/`project`/`destination` point at
  `github.com/Sanlys/tools`, the `default` ArgoCD project, and each tool's
  own `tools-<name>` namespace. These `app.yaml`s are auto-discovered via
  `cluster/prod/apps/tools/app.yaml` in the `kubernetes` repo (an
  app-of-apps "tools root" Application) -- see `docs/ci-cd.md`.
- Storage: `bucket.storageClassName: rook-ceph-bucket` and
  `postgres.storageClassName: rook-ceph-block-ssd` are this cluster's real
  StorageClasses -- neither has a cluster-default StorageClass to fall back
  on, so both must stay explicit.
- Dashboard RBAC (portal reading other tools' Deployment readiness) is
  namespace-scoped, not cluster-wide: each tool that wants to appear in the
  dashboard grants the portal's ServiceAccount a `Role`/`RoleBinding` in
  its *own* namespace only (`dashboardGrant` in that tool's `values.yaml`),
  rather than the portal holding a `ClusterRole`.

## Auth: `apps/idp` + `crates/adapters/auth`

Ported from the design proven out in `sanlys/manager`'s `idp/` (a
from-scratch OIDC provider + WebAuthn passkey login), as its own app here:

- **`apps/idp`** is a standalone tool like any other (own Postgres, own
  Helm chart), *not* a portal panel -- WebAuthn ceremonies have to run on
  the IDP's own origin anyway (RP ID/origin matching), so signing in,
  managing passkeys/sessions, and admin (invites, per-app role grants) all
  happen by visiting the IDP directly. Its frontend is plain static
  HTML/CSS/vanilla JS (`apps/idp/frontend/static`), deliberately *not*
  egui/wasm: any OAuth client redirecting a browser here -- including a
  hypothetical one outside this whole Rust workspace -- gets a small, fast
  login page regardless of what stack that client is built with.
- No consent screen and no dynamic OAuth client registration: every client
  (and the role vocabulary it declares) is listed once in the IDP's own
  `IDP_CLIENTS_JSON` (`deploy/idp/values.yaml`), the same "static,
  GitOps-declared registry" pattern the portal already uses for
  `TOOLS_REGISTRY_JSON`. Every client is a *public* client (PKCE only, no
  `client_secret`) -- safe because every client here is first-party and
  known in advance, and it means the IDP needs zero sops-managed secrets
  of its own (its RS256 signing key and cookie-encryption key are
  generated on first boot and persisted in its own Postgres, same idea as
  the original design).
- Per-app, per-user role grants (`user_app_roles`) sit on top of "is
  logged in": each client declares its own flat list of role-name strings,
  an admin grants specific users specific roles for a specific app from
  the IDP's `/admin` page, and that app's issued token carries only the
  roles granted for *that app's own* `client_id` (never another app's).
- **`crates/adapters/auth`** is the shared library every other tool
  depends on: a `backend` feature (`AuthUser` axum extractor + `AuthState`,
  verifying a Bearer JWT against the IDP's JWKS) for backends, and a
  `LoginWidget` egui component (redirect + PKCE on wasm via
  `frontend_web`, an RFC 8252 loopback-redirect flow + OS-keyring token
  storage on native via `frontend_native`) for frontends. See
  `apps/hello`'s wiring for the copy-paste pattern, and
  `docs/adding-a-tool.md`'s auth section.
- One important consequence of standard OIDC audience scoping: a token
  minted for one client_id can't carry roles for a different client_id.
  So a tool's own panel -- even when it's opened *inside* the portal --
  manages its own login independently, scoped to its own `client_id`, not
  the portal's. The portal's own top-bar sign-in only gates portal-native
  features (there are none yet); it can't gate other tools' panels, and
  doesn't try to. Silent SSO (`prompt=none`) is what keeps this from
  meaning repeated passkey prompts: once the IDP has a session cookie
  (from any prior login), each tool's own silent attempt picks it up with
  at most a brief invisible redirect, not a new ceremony.
