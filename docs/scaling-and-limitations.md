# Scaling and limitations

Nothing in this document is a bug -- it's what falls out of decisions the
platform's requirements deliberately made (a closed, compile-time panel
set; a simple per-tool Postgres/S3 default; a basic in-app dashboard).
Worth knowing the ceilings before you hit them, especially as the number
of tools grows past a handful. One item below (the dashboard's health
checks) is a real design smell worth fixing before it bites you; the rest
are trade-offs to be aware of, not necessarily to change.

## The dashboard's health checks are sequential, synchronous, and uncached

`apps/portal/backend`'s `GET /api/status` (`apps/portal/backend/src/main.rs`)
loops over every tool in the registry and `.await`s an HTTP health-check
hit **one at a time**, not concurrently, and computes it fresh on every
single request -- there's no caching layer. The HTTP client's timeout is
3 seconds.

Concretely: with 10 tools registered, if one of them is down or slow, that
request alone can add up to 3 seconds to `/api/status`'s response time,
serially -- **every other tool's status is delayed behind it**, even
though checking them concurrently would cost nothing extra. And because
there's no cache, every browser tab with the Dashboard panel open
re-triggers this whole fan-out every 10 seconds (`apps/portal/frontend/src/panels/dashboard.rs`'s
poll interval). A handful of tools and a handful of viewers is fine. A
platform with dozens of tools, or one flaky/down tool sitting at the front
of the registry list, will make the dashboard feel sluggish or make it
time out entirely for everyone -- not just for the tool that's actually
down.

This is the one item here worth fixing proactively rather than just
watching for: run the per-tool checks concurrently (e.g.
`futures::future::join_all`) and consider a short-TTL cache (a few
seconds) in front of the aggregate result so concurrent dashboard viewers
share one computation instead of each triggering their own fan-out.

## The unified wasm binary only grows

Every tool's frontend crate is a compile-time dependency of
`apps/portal/frontend` (that's the whole point of "no runtime plugin
loading" -- see `docs/architecture.md`). That means:

- The portal's wasm binary is the sum of every registered tool's UI code
  and its dependencies. It never shrinks, and there's no lazy-loading or
  code-splitting -- a user who only cares about one tool still downloads
  all of them. For reference, the reference `hello` tool alone (plus
  eframe/egui itself) already produces a ~3 MB wasm binary; expect this to
  grow roughly per-tool as more are added, with no ceiling built in.
- Adding, or even just touching, any one tool's frontend crate invalidates
  the build cache for the whole `portal-frontend` compilation unit --
  wasm build times in CI (`ci.yml`'s `rust` job) grow with the total
  number of tools' frontend code, not just the one that changed.
- Blast radius: all tools' UI code runs in the same process/wasm module.
  A panic in one tool's `Panel::ui`/`tick` takes down the whole portal UI,
  not just that tool's window. Similarly, since egui's render loop is
  single-threaded, one CPU-heavy panel (e.g. doing real work per frame
  instead of just drawing) can stall every other open panel's
  responsiveness too.

There's no clean fix that preserves "no runtime plugin loading" as a
hard constraint -- dynamic wasm module loading is possible in principle
but is a significant undertaking and was explicitly ruled out by that
constraint. If the tool count grows large enough for this to hurt, the
likely escape hatches are: let heavier/rarely-used tools skip portal
registration entirely and be standalone-only (every tool already gets a
standalone binary for free, see `docs/adding-a-tool.md`), or eventually
split into more than one portal binary (e.g. by team or domain) -- neither
is implemented today.

## Adding a tool touches several shared files

`docs/adding-a-tool.md` lists them plainly: the root `Cargo.toml`
workspace members, `apps/portal/frontend/src/lib.rs` (three separate
edits), `deploy/portal/values.yaml`'s hand-maintained `TOOLS_REGISTRY_JSON`
blob, and `.github/workflows/release.yml`'s build matrix. None of this is
hard, but all of it is manual, unvalidated (nothing checks that a
registry entry's `id` actually matches a `ToolPanel` variant until you run
it), and concentrated in a small number of files that every tool addition
touches -- which means merge conflicts and forgotten steps scale with
both tool count and contributor count. This is the direct cost of a
closed, compile-time-known panel set; it's not accidental complexity, but
it will get more annoying well before the wasm-size issue above does if
several tools are being added in parallel by different people.

## What this platform fits poorly

The adapters and `tool-library` Helm chart model one shape well: a
stateless-ish HTTP(S)/websocket service, optionally with its own Postgres
database and/or S3 bucket, exposing `/health` and `/metrics`. Some kinds
of tools don't fit that shape cleanly:

- **Background/batch workloads with no HTTP surface** -- a
  sync/aggregation job, a periodic scraper, anything shaped like a worker
  rather than a request/response service. `tool-library` only renders a
  `Deployment` + `Service`, and its liveness/readiness probes and
  `ServiceMonitor` all assume an HTTP endpoint exists. There's no
  `CronJob`/`Job` template. (Worth flagging since the Homelab notes'
  `game-mgr` reference -- save syncing, playtime tracking -- is exactly
  this shape, and it's not clear it would port onto this chart as-is.)
- **Tools needing bulk file/block storage** rather than an object-storage
  bucket -- e.g. a media library. Only S3 (via rook-ceph) and Postgres are
  modeled; there's no PVC/block-storage adapter or chart template.
- **Tools needing multi-replica coordination** -- `replicaCount` scales a
  stateless HTTP frontend fine, but there's no leader-election or
  distributed-lock primitive if a tool's own logic needs to coordinate
  across replicas.

## Per-tool Postgres has no HA/backup story

Deliberately simple (see `docs/postgres.md`): one pod, one PVC, no
operator, no automated backups, no point-in-time recovery. Fine for a
tool's incidental bookkeeping data. Not fine as the data grows large or
becomes something you can't afford to lose to a bad node -- there's no
signal in this platform for "this tool has outgrown the default," it's a
judgment call per tool.
