#!/bin/bash
set -euo pipefail

law_filter=()
while (( $# )); do
  if [[ $1 != --law || $# -lt 2 ]]; then
    echo "Usage: $0 [--law DEFINITION]..." >&2
    exit 2
  fi
  law_filter+=(--law "$2")
  shift 2
done

cd "$(dirname "$(readlink -f "$0")")/.."
checkout=$PWD
briefs=$(realpath -m -- "${HIRSEL_BRIEFS:-/workspace/code/hirsel-briefs}")
mkdir -p "$briefs/vopr/nightly"
run=$(mktemp -d "$briefs/vopr/nightly/$(date -u +%Y-%m-%dT%H%M%SZ)-XXXXXX")
scratch=$(mktemp -d "$checkout/../prepare.XXXXXX")
TMPDIR=$(mktemp -d /dev/shm/hirsel-vopr.XXXXXX)
export TMPDIR HIRSEL_NET_REACTOR=epoll
trap 'rm -rf -- "$scratch" "$TMPDIR"' EXIT

python3 tools/buck2/prepare.py
driver=hirsel-core-driver-bin-hirsel
runtime=core-driver-runtime-pic
recording=core-driver-record-runtime-pic
outputs=$(.buck2/bin/buck2 build -j4 --show-json-output \
  "//:$driver" "//:$runtime" "//:$recording")
readarray -t paths < <(python3 -c '
import json, sys
outputs = json.load(sys.stdin)
for target in sys.argv[1:]:
    print(outputs["root//:" + target])
' "$driver" "$runtime" "$recording" <<<"$outputs")
export LLVM_SYS_231_PREFIX="$checkout/tools/buck2/toolchains/llvm-files"
export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
status=0
seed_budget=()
if [[ -n ${HIRSEL_VOPR_SEEDS:-} ]]; then
  seed_budget=(--seeds "$HIRSEL_VOPR_SEEDS")
fi
python3 core/tools/sim_campaign.py --compiler "${paths[0]}" \
  --runtime "${paths[1]}" --recording-runtime "${paths[2]}" --work "$scratch/prepared" \
  --nightly-results "$run" "${seed_budget[@]}" "${law_filter[@]}" || status=$?
# Earlier nights' reds first; this allocates no seeds.
python3 tools/sim-campaign reds-first "$scratch/prepared/manifest.json" "$run" || status=$?

cd "$scratch"
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
group_status=0
python3 "$checkout/core/tools/vopr_failure_groups.py" "$run" || group_status=$?
if (( sweep_status == 0 && (group_status == 0 || group_status == 3) )); then
  if (( ${#law_filter[@]} )); then
    touch "$run/focused-complete"
  else
    touch "$run/complete"
  fi
fi
if (( sweep_status != 0 )); then
  status=$sweep_status
elif (( group_status != 0 )); then
  status=$group_status
fi
exit "$status"
