#!/usr/bin/env bash
# Scaffolds a new tool: apps/<name> (backend + frontend) and deploy/<name>
# (Helm chart). Run from the repo root. See docs/adding-a-tool.md for the
# manual steps still needed afterwards (workspace member, portal panel,
# tools registry entry).
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <tool-name>" >&2
  exit 1
fi

name="$1"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

# The subfolder is a positional arg, not a --subfolder flag (that flag
# doesn't exist in cargo-generate) -- see docs/adding-a-tool.md's manual
# invocation, which this mirrors.
cargo generate --path . templates/new-tool/app --name "$name" --destination apps
cargo generate --path . templates/new-tool/deploy --name "$name" --destination deploy

cat <<EOF

Generated apps/$name and deploy/$name. Remaining steps (see docs/adding-a-tool.md):
  1. Add "apps/$name/backend" and "apps/$name/frontend" to the root Cargo.toml [workspace] members.
  2. Add a ToolPanel variant + match arm for it in apps/portal/frontend/src/lib.rs.
  3. Add an entry for it to deploy/portal/values.yaml's TOOLS_REGISTRY_JSON.
  4. cargo check --workspace
EOF
