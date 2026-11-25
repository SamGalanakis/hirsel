#!/bin/bash
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")/.."
checkout=$PWD
briefs=${HIRSEL_BRIEFS:-/workspace/code/hirsel-briefs}
mkdir -p "$briefs/vopr/nightly"
run=$(mktemp -d "$briefs/vopr/nightly/$(date -u +%Y-%m-%dT%H%M%SZ)-XXXXXX")
scratch=$(mktemp -d "$checkout/../prepare.XXXXXX")
TMPDIR=$(mktemp -d /dev/shm/hirsel-vopr.XXXXXX)
export TMPDIR HIRSEL_NET_REACTOR=epoll
trap 'rm -rf -- "$scratch" "$TMPDIR"' EXIT

taskset -c 8-11 python3 tools/buck2/prepare.py
driver=hirsel-core-driver-bin-hirsel
runtime=core-driver-runtime-pic
recording=core-driver-record-runtime-pic
outputs=$(taskset -c 8-11 .buck2/bin/buck2 build -j4 --show-json-output \
  "//:$driver" "//:$runtime" "//:$recording")
readarray -t paths < <(python3 -c '
import json, sys
outputs = json.load(sys.stdin)
for target in sys.argv[1:]:
    print(outputs["root//:" + target])
' "$driver" "$runtime" "$recording" <<<"$outputs")
export LLVM_SYS_231_PREFIX="$checkout/tools/buck2/toolchains/llvm-files"
export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
taskset -c 8-11 python3 core/tools/sim_campaign.py --compiler "${paths[0]}" \
  --runtime "${paths[1]}" --recording-runtime "${paths[2]}" --work "$scratch/prepared"

status=0
# Earlier nights' reds first; this allocates no seeds.
taskset -c 8-11 python3 tools/sim-campaign reds-first "$scratch/prepared/manifest.json" "$run" || status=$?

cd "$scratch"
taskset -c 8-11 python3 "$briefs/vopr-campaign-2092/sweep2.py" "$checkout/tools/sim-campaign" \
  1 24 --nightly "$run" || status=$?
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
taskset -c 8-11 python3 "$checkout/core/tools/vopr_failure_groups.py" "$run"
exit "$status"
