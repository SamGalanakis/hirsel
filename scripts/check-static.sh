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
# second rhythm. ThreadAvatar is the one exception: a glyph centred in a box
# has no line to keep, so its `leading-none` is geometry, not type.
# `leading-none` included: a box centres its glyph with flex, not with a
# collapsed line. The ramp ends at `text-xl` and `text-display`; Tailwind's
# own `text-2xl`/`text-3xl` are steps the ramp does not have.
if grep -rn -E '\b(leading-|text-[23]xl)' app/src --include=*.tsx --include=*.ts --include=*.css \
  | grep -v -E '^app/src/styles\.css:'; then
  echo "leading-* and text-2xl/3xl utilities belong to the type ramp in app/src/styles.css, not to call sites" >&2
  exit 1
fi
# Every label over a group is `SectionLabel` (app/src/components/ui/section-label.tsx):
# sentence case at text-meta, per DESIGN.md. An `uppercase` utility at a call
# site is an eyebrow the ramp does not have.
if grep -rn -E '\buppercase\b' app/src --include=*.tsx --include=*.ts | grep -v -E '\.test\.tsx?:'; then
  echo "uppercase labels are not in the type vocabulary; use SectionLabel from app/src/components/ui/section-label.tsx" >&2
  exit 1
fi
node app/scripts/openui-prompt.mjs --check
