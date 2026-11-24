#!/bin/bash
# Nightly goal board (board-1270). Build CPUs 8-11, measure CPUs 16-17.
set -euo pipefail
tools=$(dirname "$(readlink -f "$0")")
base=${HIRSEL_BOARD_BASE:-"$HOME/.local/share/hirsel-board"}
current=${HIRSEL_BOARD_CURRENT:-/workspace/code/hirsel-briefs/CURRENT-BOARD.md}
mkdir -p "$base/runs"
exec 8>"$base/nightly.lock"
flock 8
exec 9>"$base/baseline.lock"
flock 9
started=$(date +%s)
run=$(mktemp -d "$base/runs/$(date -u -d "@$started" +%Y%m%dT%H%M%SZ)-XXXXXX")
commit=$(git -C "$base/main" rev-parse --verify origin/main 2>/dev/null) || commit=
step="fetch origin/main"
boat_status=
vopr_status=
law_status=
record() {
  local auxiliary=()
  if [[ -n "$boat_status" ]]; then auxiliary+=(--boat-status "$boat_status"); fi
  if [[ -n "$vopr_status" ]]; then auxiliary+=(--vopr-status "$vopr_status"); fi
  if [[ -n "$law_status" ]]; then auxiliary+=(--law-status "$law_status"); fi
  python3 "$tools/board-freshness.py" --base "$base" --current "$current" \
    --record "$run" --started "$started" --commit "$commit" --step "$step" "${auxiliary[@]}" "$@"
}
# shellcheck disable=SC2317 # Invoked by the EXIT trap.
finish() {
  local status=$?
  if (( status != 0 )); then
    printf 'Nightly board failed at %s (exit %s). See %s\n' "$step" "$status" "$run" >&2
  fi
  record --status "$status" || {
    if (( status == 0 )); then
      exit 1
    fi
  }
}
trap finish EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
record
taskset -c 8-11 git -C "$base/main" fetch -q origin main
step="check out origin/main"
cd "$base/main"
commit=$(git rev-parse --verify origin/main)
taskset -c 8-11 git switch -q --detach "$commit"
checkout=$PWD
boat_status=0
step="Boat screen and containment"
record
screen=()
candidate="$run/screen-candidate"
if taskset -c 8-11 python3 bench/awfy/screen.py prepare "$candidate" \
    --awfy "$base/peers/awfy" --langarena "$base/peers/langarena" >>"$run/run.log" 2>&1; then
  if [[ -d "$base/last-screen-inputs" ]]; then
    if taskset -c 8-11 python3 bench/awfy/screen.py manifest "$run/screen-bundle" \
        --base "$base/last-screen-inputs" --candidate "$candidate" --awfy "$base/peers/awfy" \
        >"$run/boat-jobs.json" 2>>"$run/run.log"; then
      screen=(--manifest "$run/boat-jobs.json")
    else
      boat_status=$?
    fi
  fi
  ln -s "$candidate" "$run/prepared-screen"
  mv -Tf "$run/prepared-screen" "$base/last-screen-inputs"
else
  boat_status=$?
fi
(
  cd "$run"
  taskset -c 8-11 "$checkout/tools/boat-dispatch" "${screen[@]}" \
    --contain-worktree "$checkout" --contain-record "$base/last-contain"
) >>"$run/run.log" 2>&1 || boat_status=$?
# Warm peer Rust caches before board.py invokes cargo --offline.
for manifest in "$base/peers/langarena/rust/Cargo.toml" "$base/peers/awfy/rust/Cargo.toml"; do
  if [[ -f "$manifest" ]]; then
    step="cargo +1.99.0 fetch ${manifest#"$base/peers/"}"
    taskset -c 8-11 cargo +1.99.0 fetch --locked --manifest-path "$manifest" 2>&1 | tee -a "$run/run.log"
  fi
done
baseline=()
if [[ -e "$base/last-complete.json" || -L "$base/last-complete.json" ]]; then
  baseline=(--baseline "$(realpath -m "$base/last-complete.json")")
fi
law_baseline=()
if previous=$(python3 "$tools/board-freshness.py" --base "$base" --last-complete-commit); then
  if [[ -n "$previous" ]]; then law_baseline=(--commit "$previous"); fi
else
  law_status=$?
fi
step=board
record
status=0
taskset -c 8-11,16-17 python3 bench/awfy/board.py "$run/board" \
  --build-cpus 8-11 --measure-cpus 16-17 --awfy "$base/peers/awfy" \
  --resid "$base/peers/resid" --goose "$base/peers/goose" --langarena "$base/peers/langarena" \
  "${baseline[@]}" >>"$run/run.log" 2>&1 || status=$?
if (( status == 2 )); then
  step="board comparison"
  flock -u 9
  if [[ -n ${baseline[1]:-} ]] && python3 bench/awfy/board.py rebaseline "$run" \
      --base "$base" --expected-baseline "${baseline[1]}" >>"$run/run.log" 2>&1; then
    step="rebaseline"
    status=0
  else
    echo "Comparison refused. Baseline retained." >>"$run/run.log"
  fi
elif (( status == 3 )); then
  step="board comparison"
elif (( status == 0 )); then
  step="promote baseline"
  ln -s "$run/board/board.json" "$run/complete.json"
  mv -Tf "$run/complete.json" "$base/last-complete.json"
fi

build_toolchain() {
  local output
  taskset -c 8-11 python3 tools/buck2/prepare.py >&2 || return
  output=$(taskset -c 8-11 .buck2/bin/buck2 build -j4 --show-json-output //:hirsel-toolchain) || return
  python3 -c 'import json, sys; print(json.load(sys.stdin)["root//:hirsel-toolchain"])' <<<"$output"
}

run_vopr() (
  local ledger vopr
  vopr="$run/vopr"
  ledger=${HIRSEL_VOPR_LEDGER:-/workspace/code/hirsel-briefs/vopr/seeds.sqlite}
  mkdir -p "$vopr" "$(dirname "$ledger")" || return
  taskset -c 8-11 python3 core/tools/sim_campaign.py --compiler "$checkout/$compiler/hirsel" \
    --work "$vopr/prepared" || return
  cd "$vopr" || return
  exec taskset -c 8-11 "$checkout/tools/sim-campaign" batch "$(basename "$run")" prepared/manifest.json \
    --ledger "$ledger" --results results.jsonl
)

toolchain_status=0
compiler=$(build_toolchain 2>>"$run/run.log") || toolchain_status=$?
if [[ -z "$law_status" ]]; then
  law_status=$toolchain_status
  if (( law_status == 0 )); then
    taskset -c 8-11 python3 core/tools/law_board.py nightly --compiler "$checkout/$compiler/hirsel" \
      "${law_baseline[@]}" --logs "$run/laws" >>"$run/run.log" 2>&1 || law_status=$?
  fi
fi
if (( law_status != 0 )); then
  printf 'Law board exited %s. See %s/run.log.\n' "$law_status" "$run" >>"$run/run.log"
fi
vopr_status=$toolchain_status
if (( vopr_status == 0 )); then
  run_vopr >>"$run/run.log" 2>&1 || vopr_status=$?
fi
if (( vopr_status == 75 )); then
  echo "VOPR deferred until a spare Boat start is available." >>"$run/run.log"
elif (( vopr_status != 0 )); then
  printf 'VOPR campaign exited %s. See %s/vopr.\n' "$vopr_status" "$run" >>"$run/run.log"
fi
printf 'Boat exit %s · VOPR exit %s · Laws exit %s\n' "$boat_status" "$vopr_status" "$law_status" |
  tee -a "$run/run.log" >&2

run_w2() (
  local output compiler runtime bridge
  output=$(taskset -c 8-11 .buck2/bin/buck2 build -j4 --show-json-output \
    //:hirsel-core-driver-bin-hirsel //:core-scripted-runtime-pic '//:core-store-crash-provider[shared]') || return
  read -r compiler runtime bridge < <(python3 -c 'import json, sys; j = json.load(sys.stdin); print(*(j["root//:" + target] for target in ("hirsel-core-driver-bin-hirsel", "core-scripted-runtime-pic", "core-store-crash-provider[shared]")))' <<<"$output") || return
  export LLVM_SYS_231_PREFIX="$checkout/tools/buck2/toolchains/llvm-files"
  export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  taskset -c 8-11 python3 core/tools/store_crash_tests.py prepare \
    --compiler "$compiler" --runtime "$runtime" --bridge "$bridge" --bundle "$run/w2" || return
  exec taskset -c 8-11 tools/boat-dispatch --job-class vopr w2-damage "$run/w2" \
    "python3 store_crash_tests.py sweep $((started * 4))"
)

w2_status=0
run_w2 >>"$run/w2.log" 2>&1 || w2_status=$?
if (( w2_status == 75 )); then
  echo "W2 damage sweep deferred until a spare Boat start is available." >>"$run/run.log"
elif (( w2_status != 0 )); then
  printf 'W2 damage sweep exited %s. See %s/w2.log.\n' "$w2_status" "$run" >>"$run/run.log"
fi
echo "$run"
exit "$status"
