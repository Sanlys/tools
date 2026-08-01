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
  hello/              -- reference example tool exercising every adapter
    backend
    frontend

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

## What's ingress-controller-agnostic vs. cluster-specific

This scaffolding was built without direct access to the real cluster
config, so several things are deliberately generic placeholders you should
adjust:

- `ingress.className`/`ingress.annotations` in every chart's `values.yaml`
  -- currently empty/no controller-specific annotations. Set these to match
  whatever's actually fronting the cluster (Traefik, nginx-ingress, ...).
- `monitoring.grafanaInstanceSelector` and
  `monitoring.serviceMonitorLabels`/`prometheusRuleLabels` -- must match
  your actual Grafana CR's `instanceSelector` and Prometheus Operator's
  `serviceMonitorSelector`/`ruleSelector` labels.
- `image.repository` in every `values.yaml` and `vars.REGISTRY_HOST` in
  `.github/workflows/release.yml` -- placeholder Harbor hostnames.
- `deploy/*/application.yaml`'s `repoURL`/`project`/`destination` -- adjust
  to your actual ArgoCD project and repo URL.
- The Kubernetes distribution itself (k3s/kubeadm/etc) -- nothing here
  assumes a specific one; the k8s API usage (Deployments, RBAC,
  ObjectBucketClaim, ServiceMonitor, PrometheusRule, GrafanaDashboard) is
  distro-agnostic, but the Operators providing those CRDs
  (rook-ceph, Prometheus Operator, grafana-operator, ArgoCD) must be
  installed for the corresponding `values.yaml` toggles to do anything.

## Porting the IDP

Not done here, deliberately -- but nothing in this scaffolding should
conflict with it. When it happens, per the plan: the IDP moves into this
monorepo as `apps/idp`, using `crates/adapters/*` directly like any other
tool, and becomes both a panel in the portal and the auth layer other
tools integrate against (likely a future `crates/adapters/auth` built once
the IDP's OIDC token-exchange interface is stable, per the "adapters:
build as needed, not upfront" principle already in the Homelab notes).
