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

# One leading per type step: every step of the ramp in app/src/styles.css
# carries its own line height, so a `leading-*` utility at a call site is a
# second rhythm. ThreadAvatar (a glyph centred in a box) and RichLink (an
# inline chip) are other lanes' files at the time of writing and keep theirs.
if grep -rn -E '\bleading-' app/src --include=*.tsx --include=*.ts --include=*.css \
  | grep -v -E '^app/src/(styles\.css|threads/ThreadAvatar\.tsx|related/RichLink\.tsx):'; then
  echo "leading-* utilities belong to the type ramp in app/src/styles.css, not to call sites" >&2
  exit 1
fi
