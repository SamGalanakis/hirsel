#!/bin/bash
set -euo pipefail

law_filter=()
refresh=0
existing_run=
if [[ ${1:-} == --refresh ]]; then
  refresh=1
  shift
elif [[ ${1:-} == --resume && $# -ge 2 ]]; then
  existing_run=$2
  shift 2
fi
while (( $# )); do
  if [[ $1 != --law || $# -lt 2 ]]; then
    echo "Usage: $0 [--refresh] [--law DEFINITION]..." >&2
    exit 2
  fi
  law_filter+=(--law "$2")
  shift 2
done

cd "$(dirname "$(readlink -f "$0")")/.."
checkout=$PWD
commit=$(git rev-parse HEAD)
briefs=$(realpath -m -- "${HIRSEL_BRIEFS:-/workspace/code/hirsel-briefs}")
mkdir -p "$briefs/vopr/nightly"
if [[ -n $existing_run ]]; then
  run=$existing_run
else
  run=$(mktemp -d "$briefs/vopr/nightly/$(date -u +%Y-%m-%dT%H%M%SZ)-XXXXXX")
fi
manifest="$run/no-manifest"
scratch=
phase=checkout
run_tmp=
finish() {
  local status=$?
  python3 "$checkout/core/tools/vopr_nightly_receipt.py" "$run" "$manifest" "$commit" "$phase" "$status" \
    || status=$?
  rm -rf -- "${scratch:-}" "${run_tmp:-}"
  exit "$status"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if (( ${#law_filter[@]} )); then
  touch "$run/focused"
fi
if (( refresh )); then
  git fetch --quiet origin main
  git switch --quiet --detach origin/main
  commit=$(git rev-parse HEAD)
  exec "$checkout/tools/nightly-vopr.sh" --resume "$run" "${law_filter[@]}"
fi
phase=inventory
python3 core/tools/sim_campaign.py --list --nightly-results "$run" "${law_filter[@]}" > "$run/inventory"
phase=build
scratch=$(mktemp -d "$checkout/../prepare.XXXXXX")
manifest="$scratch/prepared/manifest.json"
run_tmp=$(mktemp -d /dev/shm/hirsel-vopr.XXXXXX)
TMPDIR=$run_tmp
export TMPDIR HIRSEL_NET_REACTOR=epoll

resolve_outputs() {
  python3 -c '
import json, sys
from pathlib import Path
outputs = json.load(sys.stdin)
for target in sys.argv[1:]:
    print(Path(outputs["root//:" + target]).resolve(strict=True))
' "$@"
}
python3 tools/buck2/prepare.py
driver=hirsel-core-driver-bin-hirsel
runtime=core-driver-runtime-pic
recording=core-driver-record-runtime-pic
outputs=$(.buck2/bin/buck2 build -j4 --show-json-output \
  "//:$driver" "//:$runtime" "//:$recording")
resolved=$(resolve_outputs "$driver" "$runtime" "$recording" <<<"$outputs")
readarray -t paths <<<"$resolved"
export LLVM_SYS_231_PREFIX="$checkout/tools/buck2/toolchains/llvm-files"
export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
status=0
seed_budget=()
if [[ -n ${HIRSEL_VOPR_SEEDS:-} ]]; then
  seed_budget=(--seeds "$HIRSEL_VOPR_SEEDS")
fi
phase=prepare
python3 core/tools/sim_campaign.py --compiler "${paths[0]}" \
  --runtime "${paths[1]}" --recording-runtime "${paths[2]}" --work "$scratch/prepared" \
  --nightly-results "$run" "${seed_budget[@]}" "${law_filter[@]}" || status=$?
# Earlier nights' reds first; this allocates no seeds.
phase=reds-first
python3 tools/sim-campaign reds-first "$manifest" "$run" || status=$?

if (( ${#law_filter[@]} == 0 )); then
  phase=negative
  bridge_target='core-store-crash-provider[shared]'
  crash_runtime=core-scripted-runtime-pic
  crash_flags=()
  if crash_outputs=$(.buck2/bin/buck2 build -j4 --show-json-output "//:$bridge_target" "//:$crash_runtime") &&
     crash_paths=$(resolve_outputs "$bridge_target" "$crash_runtime" <<<"$crash_outputs"); then
    readarray -t crash <<<"$crash_paths"
    crash_flags=(--bridge "${crash[0]}" --crash-runtime "${crash[1]}")
  fi
  python3 core/tools/vopr_nightly_negative.py "${paths[0]}" "${paths[1]}" \
    "$manifest" "$scratch/negative" "$run/negative.jsonl" "${crash_flags[@]}" || status=$?
fi

cd "$scratch"
phase=sweep
sweep_status=0
python3 "$briefs/vopr-campaign-2092/sweep2.py" "$checkout/tools/sim-campaign" \
  1 "${HIRSEL_VOPR_MINUTES:-24}" --nightly "$run" || sweep_status=$?
# Each batch merged one coverage profile per law group; fold them into the night's.
# Coverage never changes the night's status.
shopt -s nullglob
declare -A groups=()
for profile in "$scratch"/*.coverage/*.json; do
  groups[$(basename "$profile")]+=" $profile"
done
mkdir -p "$run/coverage"
for name in "${!groups[@]}"; do
  # shellcheck disable=SC2086 # one word per batch profile
  "${paths[0]}" coverage merge ${groups[$name]} -o "$run/coverage/$name" >/dev/null \
    || echo "coverage ${name%.json}: merge failed" >&2
done
if (( sweep_status != 0 )); then
  status=$sweep_status
  phase=sweep-failed
else
  phase=finished
fi
exit "$status"
