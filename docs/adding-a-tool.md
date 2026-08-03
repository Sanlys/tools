# Adding a new tool

The fast path is the generator:

```sh
templates/new-tool/generate.sh my-tool
```

This runs `cargo generate` twice (once for `apps/my-tool`, once for
`deploy/my-tool`) and prints the manual steps below. You can also invoke it
by hand for more control:

```sh
cargo generate --path . templates/new-tool/app    --name my-tool --destination apps
cargo generate --path . templates/new-tool/deploy --name my-tool --destination deploy \
  -d description="What it does" -d container_port=8090 -d needs_bucket=true -d needs_postgres=true
```

## What you get

- `apps/my-tool/backend` -- an axum service with `/health`, `/api/status`,
  a `/metrics` endpoint, S3 + Postgres wired up (delete what you don't
  need, and the matching `bucket`/`postgres` blocks in
  `deploy/my-tool/values.yaml`), and a `trunk`-built wasm UI served as a
  fallback for any path that isn't one of those API routes -- so this
  tool's own ingress host renders its panel directly, not just a bare API.
- `apps/my-tool/frontend` -- a `MyToolPanel` implementing
  `platform_core::Panel`, an `index.html`/`Trunk.toml` for the standalone
  wasm build above, and a `my-tool-standalone` *native* binary (a separate,
  desktop-only build -- see `docs/architecture.md`'s "Standalone tools").
- `deploy/my-tool` -- a Helm chart built on `deploy/charts/tool-library`,
  and a plain `app.yaml` for ArgoCD.

If egui doesn't fit and you'd rather ship a plain web frontend instead,
skip the generator's `frontend` half and see `apps/webhello` -- a
hand-written static HTML/JS page served straight from the backend, no
`Panel` impl, no wasm. It only needs steps 1, 3, and 4 below (no
`ToolPanel` to register), plus adding its id to `PortalApp::has_panel`'s
`matches!` in `apps/portal/frontend/src/lib.rs` is *not* needed -- leaving
an id out of that list is what makes it link-out-only in the first place.

## Manual steps (not automated, on purpose -- these touch shared files)

1. **Workspace member.** Add `apps/my-tool/backend` and
   `apps/my-tool/frontend` to the root `Cargo.toml`'s `[workspace]
   members`. Run `cargo check --workspace`.

2. **Register the panel.** In `apps/portal/frontend/src/lib.rs`:
   - add `MyTool(my_tool_frontend::MyToolPanel)` to the `ToolPanel` enum
   - add `ToolPanel::MyTool($panel) => $body,` to the `dispatch!` macro's
     match list
   - add a match arm in `PortalApp::open_tool`:
     `"my-tool" => ToolPanel::MyTool(my_tool_frontend::MyToolPanel::new(link.api_base_url.clone())),`
   - add `my-tool-frontend.workspace = true` to
     `apps/portal/frontend/Cargo.toml`, and to the root `Cargo.toml`'s
     `[workspace.dependencies]` add
     `my-tool-frontend = { path = "apps/my-tool/frontend", default-features = false }`
     -- the generated crate's `standalone` feature is on by default (its
     own `#[wasm_bindgen(start)]`), which must be off when embedded in the
     portal's own wasm bundle: two crates in the same wasm module each
     exporting a `start` symbol is a linker error, and the portal already
     has its own. See `hello-frontend`'s identical entry for reference.

3. **Tool registry.** Add an entry to the `TOOLS_REGISTRY_JSON` block in
   `deploy/portal/values.yaml` (id must match the `link.id.as_str()` match
   arm above and the panel's `Panel::id()`; `k8s_namespace`/`k8s_deployment`
   are what the dashboard uses for the readiness check).

4. **CI/CD.** Add a matrix entry for `my-tool` to
   `.github/workflows/release.yml` (image build+push+bump) --
   see `docs/ci-cd.md`.

Nothing needs bootstrapping in ArgoCD by hand: `deploy/my-tool/app.yaml`
is auto-discovered via `cluster/prod/apps/tools/app.yaml` in the
`kubernetes` repo (an app-of-apps "tools root" Application) the moment
this commit lands on `main` -- see `docs/ci-cd.md`. No image-pull Secret
is needed either, as long as the `tools` Harbor project allows anonymous
pull (see `docs/ci-cd.md`).

Steps 1-4 are exactly what `templates/new-tool/generate.sh` prints at the
end, so you don't have to remember this list.

## Adding auth to a tool

Optional -- a tool with no gated actions doesn't need any of this. If it
does:

1. **Declare the client.** Add an entry to `deploy/idp/values.yaml`'s
   `IDP_CLIENTS_JSON`: `client_id` (usually the tool's own id), its
   `redirect_uris`, and the flat list of role names it wants to be able to
   grant (e.g. `["operator"]`). Set `"native": true` too if the tool's
   standalone binary should support the loopback login flow.
2. **Backend.** Add `auth-adapter = { workspace = true, features =
   ["backend"] }`. Add an `AuthState` field to your `AppState` +
   `impl FromRef<AppState> for AuthState`) (same pattern as
   `axum_extra::extract::cookie::Key` elsewhere), construct it with
   `AuthState::from_env("your-client-id")`, merge
   `auth_adapter::backend::config_route(auth.public_config())` into your
   router, and take `user: AuthUser` as a handler parameter + call
   `user.require_role("...")?` in any route you want gated. See
   `apps/hello/backend`'s `reset_greetings` handler. Nothing here needs to
   change to support a panel embedded in the portal: `AuthUser`'s
   extraction transparently accepts either a token minted for your own
   `client_id` (the common case) or one minted for a different client_id
   (the portal's own, reused by your embedded panel), falling back to a
   live per-request roles lookup against the IDP only for the latter --
   see `docs/architecture.md`'s audience-scoping note.
3. **Frontend.** Add `auth-adapter.workspace = true` and take an
   `embedded: bool` constructor param (`true` only when the portal itself
   is the one constructing your panel -- see `docs/architecture.md`'s
   audience-scoping note on why embedded vs. standalone need genuinely
   different login handling, not just a different button style):
   - **Standalone** (`!embedded`: native, or your tool's own wasm bundle):
     add a `#[cfg(target_arch = "wasm32")] use
     auth_adapter::frontend_web::LoginWidget;` / `#[cfg(not(target_arch =
     "wasm32"))] use auth_adapter::frontend_native::LoginWidget;` pair, a
     `login: LoginWidget` field, call `login.tick(ctx)` every frame and
     `login.ui(ui)` to draw it, and use `login.bearer_token()` /
     `login.has_role("...")` for your own API calls/gating.
   - **Embedded** (`embedded: true`): don't construct or tick a
     `LoginWidget` at all -- there's no working OAuth flow to run from
     inside the portal's page (see the architecture doc). Instead, add a
     `set_portal_token(&mut self, token: Option<String>)` method the
     portal calls once per frame with its own bearer token (see
     `apps/portal/frontend/src/lib.rs`'s `PortalApp::update`, which
     special-cases each embeddable `ToolPanel` variant), store it, and use
     *that* token for your API calls instead. Draw no login UI at all in
     this case -- the portal's own top bar already shows who's signed in.
   
   See `apps/hello/frontend`'s `HelloPanel` for the full pattern (a
   `bearer_token()`/`is_authenticated()` helper method that branches on
   `embedded` keeps the rest of the panel's code the same either way).
4. **Env vars.** Add `IDP_ISSUER_URL`/`AUTH_CLIENT_ID` to the tool's
   `deploy/<tool>/values.yaml` (both default sensibly for local dev if
   unset -- see `docs/local-development.md`). New clients default to
   `access_restricted: true` (opt-in by default -- a user needs an
   explicit grant, from the IDP's `/admin` page or the portal's own
   "Account" panel, just to log in at all, independent of any role
   grants); set `false` explicitly if the tool should be open to every IDP
   user.

No egui/wasm? `apps/webhello` is the reference for a plain-JS frontend:
its `static/index.html` hand-rolls the same redirect+PKCE dance
`auth_adapter::frontend_web` does in Rust (same query params, same
`sessionStorage`-backed state/verifier, same JWT-payload decode for
`preferred_username`/`exp`), scoped to its own `client_id`. Backend-side
it's identical either way -- `AuthUser` doesn't care what produced the
bearer token.

One thing *not* to do: don't expect the portal's own sign-in to gate your
tool's panel. A token minted for the portal's `client_id` can't carry
roles for your tool's `client_id` (standard OIDC audience scoping) -- your
panel needs its own `LoginWidget`, even when hosted inside the portal. See
`docs/architecture.md`'s "Auth" section for why, and how silent SSO
(`prompt=none`) keeps that from meaning a second passkey prompt.

## Design constraints to keep in mind

- No runtime plugin loading -- the portal's `ToolPanel` enum is a closed,
  compile-time set. See `docs/architecture.md` for why this is a
  hand-written `match` rather than `enum_dispatch`.
- A tool's frontend crate must stay wasm-compatible: no native-only deps
  (no `tokio`, no `reqwest`) in the `frontend` crate -- use `ehttp` for
  HTTP and `ewebsock` for websockets, both of which work unmodified on
  native and wasm. The `backend` crate has no such constraint.
- Backend URLs are resolved at runtime from the tool registry, not
  compiled in -- don't hard-code a host in the frontend crate.
