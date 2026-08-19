#!/usr/bin/env bash
set -euo pipefail

# Fail when the generated plugin aggregator does not match the folders under
# plugins/ — i.e. someone dropped a plugin in (or removed one) without running
# scripts/sync-plugins.sh. Discovery is generated rather than scanned at
# runtime, so this check is what makes "drop the folder in" reliable.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bash scripts/sync-plugins.sh > /dev/null

if ! git diff --quiet -- crates/hirsel-plugins; then
  echo "crates/hirsel-plugins is out of date with plugins/." >&2
  echo "Run: bash scripts/sync-plugins.sh (or: just sync-plugins) and commit the result." >&2
  git --no-pager diff -- crates/hirsel-plugins >&2
  exit 1
fi
