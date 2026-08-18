#!/usr/bin/env bash
set -euo pipefail

# Hard line budgets for hand-written sources: Rust under crates/ and web
# sources under app/src/. A file over budget fails CI; the fix is to split it,
# not to raise the budget.
production_limit="${HIRSEL_PRODUCTION_LINE_LIMIT:-1000}"
test_limit="${HIRSEL_TEST_LINE_LIMIT:-2000}"

if (($#)); then
  roots=("$@")
else
  roots=("crates" "app/src")
fi

is_test_file() {
  local file="$1"
  case "$file" in
    # Rust: test trees plus the conventional test-module file names.
    */tests/* | */test/* | */testing/* | \
      */src/tests.rs | */src/test.rs | */src/*/tests.rs | */src/*/test.rs | \
      src/tests.rs | */tests.rs | *_tests.rs | */src/*_tests.rs)
      return 0
      ;;
    # Web: vitest specs and the shared test setup.
    *.test.ts | *.test.tsx | *.test.js | *.test.jsx | */vitest.setup.ts)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

line_limit_for() {
  local rel="$1"
  if is_test_file "$rel"; then
    printf '%s' "$test_limit"
  else
    printf '%s' "$production_limit"
  fi
}

# Pre-existing files that exceed the budget, exempt with a one-line reason.
# This is an explicit, closed list — a new file over budget still fails hard.
# The intended direction of travel is to shrink an entry below its limit and
# delete its line here, never to raise the global budget. Entries are
# self-expiring: this script fails when an allowlisted path no longer exists or
# has shrunk to its limit, so a stale exemption cannot sit here unnoticed.
declare -A allowlist=(
)

# Allowlist entries are measured before the tree walk so an exemption that no
# longer earns its place fails the gate instead of silently lingering. Paths are
# repository-relative; run this script from the repository root.
stale_exemptions=()
for rel in "${!allowlist[@]}"; do
  if [[ ! -f "$rel" ]]; then
    stale_exemptions+=("missing:$rel")
    continue
  fi
  limit="$(line_limit_for "$rel")"
  lines=$(wc -l < "$rel")
  if ((lines <= limit)); then
    stale_exemptions+=("within-budget:${lines}<=${limit}:$rel")
  fi
done

if ((${#stale_exemptions[@]})); then
  echo "Stale entries in the file-size allowlist (delete them):" >&2
  echo "  production limit: ${production_limit} lines" >&2
  echo "  test/support limit: ${test_limit} lines" >&2
  mapfile -t stale_exemptions < <(printf '%s\n' "${stale_exemptions[@]}" | sort)
  printf '  %s\n' "${stale_exemptions[@]}" >&2
  exit 1
fi

failures=()
while IFS= read -r -d '' file; do
  rel="${file#./}"
  if [[ -n "${allowlist[$rel]+set}" ]]; then
    continue
  fi
  limit="$(line_limit_for "$rel")"
  if is_test_file "$rel"; then
    kind="test"
  else
    kind="production"
  fi

  lines=$(wc -l < "$file")
  if ((lines > limit)); then
    failures+=("$kind:$lines:$rel")
  fi
done < <(
  find "${roots[@]}" \
    \( \
      -name '.git' -o \
      -name 'node_modules' -o \
      -name 'target' -o \
      -name 'dist' -o \
      -name 'build' -o \
      -name 'vendor' -o \
      -name 'vendored' -o \
      -name 'generated' -o \
      -path '*/app/tools' -o \
      -path './e2e' -o \
      -path './android' \
    \) -prune -o \
    -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.tsx' \) -print0
)

if ((${#failures[@]})); then
  echo "Files over line budget:" >&2
  echo "  production limit: ${production_limit} lines" >&2
  echo "  test/support limit: ${test_limit} lines" >&2
  mapfile -t failures < <(printf '%s\n' "${failures[@]}" | sort)
  printf '  %s\n' "${failures[@]}" >&2
  exit 1
fi
