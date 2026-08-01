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
3. **ArgoCD** (already watching `deploy/<app>` per its `application.yaml`,
   applied once out of band) picks up the commit and syncs the cluster.

This is the classic GitOps split: CI's job ends at "build, push, bump the
manifest"; ArgoCD's job is "notice the manifest changed, reconcile the
cluster." CI never touches the cluster directly.

### Required secrets/variables

`release.yml` needs, at the repo (or org) level:

| Name | Kind | Purpose |
|---|---|---|
| `REGISTRY_HOST` | variable | e.g. `harbor.example.internal` -- not secret, just not hardcoded |
| `REGISTRY_USERNAME` | secret | registry push credential |
| `REGISTRY_PASSWORD` | secret | registry push credential |

Until these are set, `release.yml` will fail at the login step -- expected
for a fresh clone of this scaffolding, not a bug.

### Bootstrapping ArgoCD

Each app ships a plain `deploy/<app>/application.yaml` (not part of the
Helm chart it points to -- an `Application` can't sensibly live inside the
release it manages). Apply these once:

```sh
kubectl apply -f deploy/hello/application.yaml
kubectl apply -f deploy/portal/application.yaml
```

or fold them into whatever "app of apps" root `Application` your ArgoCD
setup already uses. After that, `release.yml`'s commits are all ArgoCD
needs to see to roll out a new image.

### What's still a placeholder

- `vars.REGISTRY_HOST` and every chart's `image.repository` assume Harbor
  at `harbor.example.internal` -- point these at your real registry.
- `application.yaml`'s `repoURL`/`project`/`destination.namespace` assume
  `https://github.com/Sanlys/tools.git`, the `default` ArgoCD project, and
  a `tools` namespace -- adjust to match your actual ArgoCD setup.
- No image signing/scanning is wired in. Add it to `release.yml` if your
  registry/policy requires it.
