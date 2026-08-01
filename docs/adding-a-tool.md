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
  a `/metrics` endpoint, and S3 + Postgres wired up (delete what you don't
  need, and the matching `bucket`/`postgres` blocks in
  `deploy/my-tool/values.yaml`).
- `apps/my-tool/frontend` -- a `MyToolPanel` implementing
  `platform_core::Panel`, plus a `my-tool-standalone` native binary.
- `deploy/my-tool` -- a Helm chart built on `deploy/charts/tool-library`,
  and a plain `app.yaml` for ArgoCD.

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
     `apps/portal/frontend/Cargo.toml`, and the same to the root
     `Cargo.toml`'s `[workspace.dependencies]`.

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
