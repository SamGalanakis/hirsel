#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bash scripts/check-production-file-size.sh
bash scripts/check-plugins-synced.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
(cd app && npm run lint)
(cd app && npm exec -- tsc --noEmit)
