#!/usr/bin/env bash
# Model-check cross-worker storage return (crates/hirsel-rt/src/remote.rs): the loom models over
# the runtime's own source. The pass run must find no violation; each mutation removes one
# protocol ingredient and must fail with its named panic, which shows the passing run is not
# vacuous.
#
# Usage: check.sh [name-filter]   (a grep -E pattern over run names)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
filter="${1:-.}"
. "$here/../loom.sh"

# name | features | preemptions | test-args | bound | panic | expected (proofs/loom.sh)
loom_runs=(
  "loom||||||pass"
  "mutation-loom-direct-dequeue|--features direct-dequeue||||Causality violation: Concurrent|fail"
  "mutation-loom-blind-trim|--features blind-trim||||Causality violation: Concurrent read and write accesses to \`UnsafeCell\`.|fail"
)
failed=0
total=0
printf '%-44s %-8s %s\n' run expected result
loom_run_rows "$here/loom" "$filter"

if (( total == 0 )); then
  echo "check.sh: no run matches '$filter'" >&2
  exit 1
fi
if (( failed > 0 )); then
  echo "check.sh: $failed of $total runs did not have their expected result" >&2
  exit 1
fi
echo "check.sh: all $total runs have their expected result"
