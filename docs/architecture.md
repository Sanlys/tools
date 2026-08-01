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

apps/
  portal/             -- the unified web app (see below)
    backend           -- axum: serves the wasm bundle, tool registry, dashboard status
    frontend          -- egui/eframe, compiles to wasm, hosts every tool's panel
  hello/              -- reference example tool exercising every adapter,
                          egui panel + standalone wasm build
    backend
    frontend
  webhello/           -- reference tool with a plain static HTML/JS frontend
                          instead of egui -- see "Standalone tools" below
    backend

deploy/
  charts/tool-library -- shared Helm library chart (Deployment, Service, Ingress,
                          bucket claim, Postgres, ServiceMonitor, PrometheusRule,
                          dashboard ConfigMap, per-namespace dashboard RBAC grant)
  hello/, webhello/, portal/ -- per-app charts + values.yaml + an ArgoCD Application

templates/new-tool/   -- cargo-generate template scaffolding a new tool's app + chart

.github/workflows/    -- ci.yml (build/test/lint), release.yml (build+push+GitOps bump)
```

## The unified app

"The tools platform" is `apps/portal`: one egui app that compiles to a
single wasm binary and is also runnable natively. It hosts three kinds of
panel, all implementing the same `platform_core::Panel` trait:

- **Home** -- links out to every tool's standalone deployment (subdomain
  per tool). Every registered tool gets a working link here regardless of
  whether it has a compiled-in panel (see "Standalone tools" below) --
  `webhello` shows up this way, with no `ToolPanel` variant at all.
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

Every egui tool gets two standalone builds for free, both via
`platform_core::standalone`:

- **Native**: `standalone::run(YourPanel::new(...))` wraps any `Panel` in
  its own eframe window (`apps/hello/frontend/src/bin/standalone.rs`).
- **Wasm**: `standalone::run_web("the_canvas_id", YourPanel::new(""))`,
  called from a `#[wasm_bindgen(start)]` function in the tool's own
  `lib.rs`, mounts the same panel into a browser canvas -- this is what
  makes a tool's own ingress host (e.g. `hello.k8s.lysakermoen.com`) render
  its panel directly instead of exposing a bare API with nothing at `/`.
  Needs the tool's own `index.html`/`Trunk.toml` (mirroring the portal's)
  and a `trunk build` stage in its backend's Dockerfile that serves the
  compiled `dist/` as a fallback for unmatched routes -- see
  `apps/hello/frontend` and `apps/hello/backend`'s Dockerfile/`main.rs` for
  the full reference wiring. Uses an empty (`""`) `api_base_url` rather
  than an absolute one: since the tool's own backend serves this bundle,
  every API call resolves as a same-origin relative path.

A tool can also ship a completely different frontend stack (iced, a plain
web UI, whatever) if egui doesn't fit -- the platform doesn't require it,
it just makes egui the path of least resistance. `apps/webhello` is the
reference example: a hand-written static HTML page with plain `fetch()`
calls, no wasm/build step, no `Panel` impl, and correspondingly no
`ToolPanel` variant in the portal -- it only shows up via the portal's Home
link-out list, the same way a tool with an unreachable/broken standalone
build would (the difference is `webhello`'s actually works).

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

## Porting the IDP

Not done here, deliberately -- but nothing in this scaffolding should
conflict with it. When it happens, per the plan: the IDP moves into this
monorepo as `apps/idp`, using `crates/adapters/*` directly like any other
tool, and becomes both a panel in the portal and the auth layer other
tools integrate against (likely a future `crates/adapters/auth` built once
the IDP's OIDC token-exchange interface is stable, per the "adapters:
build as needed, not upfront" principle already in the Homelab notes).
