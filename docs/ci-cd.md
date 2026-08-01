# CI/CD

## CI (`.github/workflows/ci.yml`)

Three jobs on every push/PR:

- **rust** -- `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test --workspace`, and a wasm build of `portal-frontend` (which
  transitively covers every tool's frontend crate, since they're all
  dependencies of it).
- **helm** -- `helm dependency build` + `helm lint` + `helm template` for
  every chart under `deploy/*`.
- **docker** -- builds (doesn't push) the `hello` and `portal` backend
  images, to catch Dockerfile breakage early. Add new tools to this
  matrix as they're added (see `docs/adding-a-tool.md`).

## CD (`.github/workflows/release.yml`)

On push to `main`:

1. Build + push each app's backend image to your registry, tagged with
   the commit SHA and `latest`.
2. Bump that app's `deploy/<app>/values.yaml` `image.tag` to the SHA and
   commit it back to `main` (with `[skip ci]` to avoid retriggering
   itself).
3. **ArgoCD** (already watching `deploy/<app>` per its `app.yaml`,
   auto-discovered via the app-of-apps chain below) picks up the commit and
   syncs the cluster.

This is the classic GitOps split: CI's job ends at "build, push, bump the
manifest"; ArgoCD's job is "notice the manifest changed, reconcile the
cluster." CI never touches the cluster directly.

### Required secrets/variables

`release.yml` needs, at the repo (or org) level:

| Name | Kind | Purpose |
|---|---|---|
| `REGISTRY_HOST` | variable | `harbor.k8s.lysakermoen.com` -- not secret, just not hardcoded |
| `REGISTRY_USERNAME` | secret | registry push credential (a Harbor robot account scoped to the `tools` project) |
| `REGISTRY_PASSWORD` | secret | registry push credential |

Until these are set, `release.yml` will fail at the login step. The
`tools` Harbor project must exist before the first push -- create it (and
the robot account) in Harbor first.

### Bootstrapping ArgoCD

Unlike a lone `kubectl apply`, this is wired into the cluster's normal
GitOps flow via an app-of-apps chain, none of it applied by hand:

1. The `kubernetes` repo's root Application
   (`cluster/prod/app.yaml`) recursively discovers
   `cluster/prod/apps/tools/app.yaml` -- the "tools root" Application.
2. That Application's source is *this* repo's `deploy/` directory,
   recursively matching `**/app.yaml` -- the same discovery pattern one
   level up.
3. That picks up each individual tool's own `deploy/<app>/app.yaml` (e.g.
   `deploy/hello/app.yaml`), each a plain `Application` manifest (not part
   of the Helm chart it points to -- an `Application` can't sensibly live
   inside the release it manages) targeting that tool's own
   `tools-<app>` namespace.

So adding a new tool's `deploy/<name>/app.yaml` (per `docs/adding-a-tool.md`)
is enough on its own -- no manual `kubectl apply`, no editing the
`kubernetes` repo. After the initial three-link chain is in place,
`release.yml`'s commits (which only touch `deploy/<app>/values.yaml`) are
all ArgoCD needs to see to roll out a new image.

Each tool also needs a `harbor-pull` image-pull Secret created in its own
`tools-<app>` namespace before first deploy (Harbor is a private registry;
see `deploy/hello/values.yaml`'s comment) -- this repo has no sops setup of
its own, so that's a manual, out-of-band step per tool, same as the
`REGISTRY_USERNAME`/`PASSWORD` secrets above.

### What's still a placeholder

- Each `app.yaml`'s `repoURL`/`project` assume
  `https://github.com/Sanlys/tools.git` and the `default` ArgoCD project --
  adjust if either changes.
- No image signing/scanning is wired in. Add it to `release.yml` if your
  registry/policy requires it.
