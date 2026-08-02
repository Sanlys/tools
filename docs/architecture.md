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
  hello/              -- reference example tool exercising every adapter,
                          egui panel + standalone wasm build
    backend
    frontend
  webhello/           -- reference tool with a plain static HTML/JS frontend
                          instead of egui -- see "Standalone tools" below
    backend
  idp/                -- the platform's own OIDC provider + WebAuthn passkey login
    backend           -- axum: OIDC endpoints, WebAuthn ceremonies, Postgres
    frontend          -- plain static HTML/CSS/JS (not egui -- see "Auth" below)

deploy/
  charts/tool-library -- shared Helm library chart (Deployment, Service, Ingress,
                          bucket claim, Postgres, ServiceMonitor, PrometheusRule,
                          dashboard ConfigMap, per-namespace dashboard RBAC grant)
  hello/, webhello/, portal/, idp/ -- per-app charts + values.yaml + an ArgoCD Application

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

## Auth: `apps/idp` + `crates/adapters/auth`

Ported from the design proven out in `sanlys/manager`'s `idp/` (a
from-scratch OIDC provider + WebAuthn passkey login), as its own app here:

- **`apps/idp`** is a standalone tool like any other (own Postgres, own
  Helm chart). Only the actual sign-in redirect -- an OAuth client
  bouncing a browser through `/oauth/authorize` and the WebAuthn passkey
  ceremony itself -- has to happen on the IDP's own origin (RP ID/origin
  matching, plus `navigator.credentials.create/get` needing a real page to
  call it from). That part's frontend is plain static HTML/CSS/vanilla JS
  (`apps/idp/frontend/static`), deliberately *not* egui/wasm: any OAuth
  client redirecting a browser here -- including a hypothetical one
  outside this whole Rust workspace -- gets a small, fast login page
  regardless of what stack that client is built with.
- Everything *else* about managing an IDP account -- profile, passkeys,
  sessions, and (for admins) users/invites/clients/role-grants/access-grants
  -- lives in the portal's own egui "Account" panel
  (`apps/portal/frontend/src/panels/idp.rs`), not only the IDP's static
  `profile.html`/`admin.html` pages (which still exist and still work,
  since the IDP has to serve them to non-portal clients anyway). This
  works because proving *who's signed in* doesn't need a role scoped to
  any particular client: `apps/idp/backend/src/routes/oauth.rs::require_session`
  accepts *either* the IDP's own first-party session cookie (what
  `static/*.html` uses) *or* a valid bearer token issued for *any*
  client_id (what the portal's panel sends, using its own "portal" login)
  -- a signed, unexpired token's `sub` is enough to resolve the user, and
  `is_admin` is re-checked fresh from the database either way. Adding a
  *new* passkey is the one action the Account panel can't do itself (still
  needs the WebAuthn ceremony on the IDP's own origin) -- it links out to
  `${issuer_url}/profile` for that one step.
- No consent screen, and every client is a *public* client (PKCE only, no
  `client_secret` -- there is no such field anywhere in this IDP, for
  either kind of client below). Safe because every client here is
  first-party or admin-registered, never a third party doing dynamic
  self-registration; and it means the IDP needs zero sops-managed secrets
  of its own (its RS256 signing key and cookie-encryption key are
  generated on first boot and persisted in its own Postgres, same idea as
  the original design). Two ways a client gets registered:
  - **This repo's own tools** (portal, hello, webhello, ...): listed once
    in `IDP_CLIENTS_JSON` (`deploy/idp/values.yaml`), the same "static,
    GitOps-declared registry" pattern the portal already uses for
    `TOOLS_REGISTRY_JSON`. Reconciled into the `clients` table at boot
    (`db::reconcile_clients`); read-only from the admin UI.
  - **Everything else** (an external app outside this whole Rust
    workspace, e.g. ArgoCD): registered ad hoc through the IDP's own
    `/admin` page ("Register a new app"), stored directly in the DB
    (`clients.managed = true`). Never touched by a redeploy's JSON
    reconciliation. See "Registering an external OAuth app" below.
- Per-app, per-user role grants (`user_app_roles`) sit on top of "is
  logged in": each client declares its own flat list of role-name strings,
  an admin grants specific users specific roles for a specific app from
  the IDP's `/admin` page, and that app's issued token carries only the
  roles granted for *that app's own* `client_id` (never another app's). A
  client can override which JWT claim name that list is emitted under
  (`roles_claim`, defaults to `"roles"`) -- useful for an external relying
  party with its own claim-name expectations (see below).
- Every client is `access_restricted` **by default** (opt-in, even for
  this repo's own built-in tools): a user needs an *explicit*
  `user_app_access` grant just to complete login for that client at all,
  independent of role grants (which are about what a logged-in user can
  *do*, not whether they can log in in the first place).
  `/oauth/authorize` returns `error=access_denied` for anyone without that
  grant. Set `access_restricted: false` explicitly for a client that
  should stay open to every IDP user with no per-user grant needed.

### Registering an external OAuth app (e.g. ArgoCD)

From the IDP's `/admin` page, "Register a new app": pick a `client_id`,
list the app's real redirect URI(s), and (optionally) a role vocabulary. No
`client_secret` is issued or needed -- ArgoCD (v2.4+) supports OIDC login as
a public client with PKCE (`oidc.config.enablePKCEAuthentication: true` in
its `argocd-cm`), the same mechanism this IDP's own tools already use.

ArgoCD's RBAC maps policies from group membership under a `groups` claim by
default, not this IDP's own `roles` claim -- set the new client's
`roles_claim` to `groups` so ArgoCD's `policy.csv` sees the same per-app
role grants under the name it expects, with no extra logic on the IDP side.

To restrict who's even allowed to authenticate as that client (e.g. only
you, even though other IDP users exist): check "Restrict login" when
registering it, then grant yourself explicit access in the "App login
access" section of `/admin`. Everyone else gets `access_denied` at
`/oauth/authorize` before a code is ever issued.
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
