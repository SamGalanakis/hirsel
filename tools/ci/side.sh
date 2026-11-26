#!/bin/bash
# Link the S12 corpus with a fresh build cache per package and mode.
set -euo pipefail
hirsel=$1
out=$2
mkdir "$out"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/hirsel-corpus.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

for pkg in toml media_bits jwtbind xml yaml tz; do
  for mode in dev llvm release; do
    case "$mode" in
      dev) flags=(--backend cranelift) ;;
      llvm) flags=(--backend llvm) ;;
      release) flags=(--release) ;;
    esac
    cache="$out/$pkg-$mode"
    mkdir "$cache"
    if ! XDG_CACHE_HOME="$cache" TMPDIR="$scratch" \
      "$hirsel" test "core/programs/$pkg" --libraries core --target aarch64-apple-darwin \
      "${flags[@]}" --link-only --json > "$cache/tests.json"; then
      cat "$cache/tests.json"
      exit 1
    fi
    python3 - "$cache/tests.json" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
report = json.loads(path.read_text())
if not report["tests"] or not report["linked"]:
    raise SystemExit(f"{path}: no successful corpus links")
print(f"{path}: {len(report['tests'])} laws linked for Darwin")
PY
  done
done
