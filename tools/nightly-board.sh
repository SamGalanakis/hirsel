#!/bin/bash
# Nightly goal board (board-1270). Build CPUs 8-11, measure CPUs 16-17.
set -euo pipefail
script=$(readlink -f "$0")
tools=$(dirname "$script")
if [[ ${1:-} == --self-check ]]; then
  exec python3 "$tools/tests/test_board_freshness.py"
fi
base=$(realpath -m -- "${HIRSEL_BOARD_BASE:-"$HOME/.local/share/hirsel-board"}")
current=${HIRSEL_BOARD_CURRENT:-/workspace/code/hirsel-briefs/CURRENT-BOARD.md}
mkdir -p "$base/runs"
# The lock belongs to flock alone, outside the board's process tree.
if [[ ${1:-} != --locked ]]; then
  exec flock --close "$base/nightly.lock" "$script" --locked
fi
started=$(date +%s)
run=$(mktemp -d "$base/runs/$(date -u -d "@$started" +%Y%m%dT%H%M%SZ)-XXXXXX")
commit=$(git -C "$base/main" rev-parse --verify origin/main 2>/dev/null) || commit=
step="fetch origin/main"
boat_status=
vopr_status=
law_status=
media_status=
record() {
  local auxiliary=()
  if [[ -n "$boat_status" ]]; then auxiliary+=(--boat-status "$boat_status"); fi
  if [[ -n "$vopr_status" ]]; then auxiliary+=(--vopr-status "$vopr_status"); fi
  if [[ -n "$law_status" ]]; then auxiliary+=(--law-status "$law_status"); fi
  if [[ -n "$media_status" ]]; then auxiliary+=(--media-status "$media_status"); fi
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
  python3 "$tools/board-history.py" append --base "$base" "$run" >>"$run/run.log" 2>&1 ||
    printf 'Board history append failed. See %s/run.log.\n' "$run" >&2
  if [[ -f "$run/bisect/alert.txt" ]]; then
    { mkdir -p "$base/alerts" && cat "$run/bisect/alert.txt" >>"$base/alerts/$(date -u +%F).txt"; } ||
      printf 'Board bisect alert could not be copied from %s/bisect/alert.txt.\n' "$run" >&2
  fi
}
trap finish EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
record
if [[ ${2:-} != --fetched ]]; then
  taskset -c 8-11 git -C "$base/main" fetch -q origin main
fi
step="check out origin/main"
cd "$base/main"
if [[ ${2:-} == --fetched ]]; then
  commit=$(git rev-parse --verify HEAD)
else
  commit=$(git rev-parse --verify origin/main)
  if ! taskset -c 8-11 git switch -q --detach "$commit"; then
    git status --porcelain | tee -a "$run/run.log" >&2
    exit 2
  fi
fi
step="check clean checkout"
checkout_status=$(git status --porcelain)
if [[ -n "$checkout_status" ]]; then
  printf '%s\n' "$checkout_status" | tee -a "$run/run.log" >&2
  exit 2
fi
checkout=$PWD
boat_status=0
step="Boat screen"
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
if (( ${#screen[@]} )); then
  (
    cd "$run"
    taskset -c 8-11 "$checkout/tools/boat-dispatch" --log-dir "$run/boat-logs" "${screen[@]}"
  ) >>"$run/run.log" 2>&1 || boat_status=$?
fi
# Warm peer Rust caches before board.py invokes cargo --offline.
for manifest in "$base/peers/langarena/rust/Cargo.toml" "$base/peers/awfy/rust/Cargo.toml"; do
  if [[ -f "$manifest" ]]; then
    step="cargo +1.99.0 fetch ${manifest#"$base/peers/"}"
    taskset -c 8-11 cargo +1.99.0 fetch --locked --manifest-path "$manifest" 2>&1 | tee -a "$run/run.log"
  fi
done
baseline=()
# Pass the pointer as a positional argument to the shell inside flock.
# shellcheck disable=SC2016
baseline_state=$(flock --close "$base/baseline.lock" bash -c '
  if [[ -e "$1" || -L "$1" ]]; then
    realpath -m -- "$1" && stat -Lc "%d:%i" -- "$1"
  fi
' _ "$base/last-complete.json")
baseline_path=${baseline_state%%$'\n'*}
baseline_inode=${baseline_state#*$'\n'}
if [[ -n "$baseline_path" ]]; then
  baseline=(--baseline "$baseline_path")
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
taskset -c 8-11,16-19 python3 bench/awfy/board.py "$run/board" \
  --build-cpus 8-11 --measure-cpus 16-19 --awfy "$base/peers/awfy" \
  --resid "$base/peers/resid" --goose "$base/peers/goose" --langarena "$base/peers/langarena" \
  "${baseline[@]}" >>"$run/run.log" 2>&1 || status=$?
if (( status == 0 )); then
  step="promote baseline"
  taskset -c 8-11 python3 bench/awfy/board.py rebaseline "$run" \
    --base "$base" --expected-baseline "$baseline_path" --expected-baseline-inode "$baseline_inode" \
    >>"$run/run.log" 2>&1 || status=$?
elif (( status == 2 )); then
  step="board comparison"
  echo "Comparison refused. Baseline retained." >>"$run/run.log"
elif (( status == 3 )); then
  step="board comparison"
fi
record

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
  # Build stores and caches go to disk under the run; each law's working files stay on
  # tmpfs. With both on the shared /dev/shm, a night's stores filled it beside other lanes
  # and stopped 197 laws with ENOSPC or SIGBUS (run 20261126T031505Z-YivHTQ).
  if (( law_status == 0 )); then
    # The full lock check: catches a lock moved by a compiler change the land lint does not select.
    taskset -c 8-11 python3 core/tools/law_board.py check --compiler "$checkout/$compiler/hirsel" \
      --compiler-scratch "$run" >>"$run/run.log" 2>&1 || law_status=$?
  fi
  if (( law_status == 0 )); then
    taskset -c 8-11 python3 core/tools/law_board.py nightly --compiler "$checkout/$compiler/hirsel" \
      --compiler-scratch "$run" "${law_baseline[@]}" --logs "$run/laws" >>"$run/run.log" 2>&1 || law_status=$?
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
toolchain_hirsel="$checkout/$compiler/hirsel"

run_media_vectors() (
  if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
    echo "ffmpeg or ffprobe is missing; media_record vectors skipped."
    return 0
  fi
  HIRSEL="$toolchain_hirsel" timeout 300 core/programs/media_record/test-vectors.sh \
    "$run/media-record/work" --libraries "$checkout/core"
)

media_status=$toolchain_status
if (( media_status == 0 )); then
  run_media_vectors >>"$run/media-vectors.log" 2>&1 || media_status=$?
fi
if (( media_status != 0 )); then
  printf 'media_record vectors exited %s. See %s/media-vectors.log.\n' "$media_status" "$run" >>"$run/run.log"
fi
printf 'Boat exit %s · VOPR exit %s · Laws exit %s · Media vectors exit %s\n' \
  "$boat_status" "$vopr_status" "$law_status" "$media_status" | tee -a "$run/run.log" >&2

run_w2() (
  local output compiler runtime bridge
  output=$(taskset -c 8-11 .buck2/bin/buck2 build -j4 --show-json-output \
    //:hirsel-core-driver-bin-hirsel //:core-scripted-runtime-pic '//:core-store-crash-provider[shared]') || return
  read -r compiler runtime bridge < <(python3 -c 'import json, sys; j = json.load(sys.stdin); print(*(j["root//:" + target] for target in ("hirsel-core-driver-bin-hirsel", "core-scripted-runtime-pic", "core-store-crash-provider[shared]")))' <<<"$output") || return
  export LLVM_SYS_231_PREFIX="$checkout/tools/buck2/toolchains/llvm-files"
  export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  taskset -c 8-11 python3 core/tools/store_crash_tests.py prepare \
    --compiler "$compiler" --runtime "$runtime" --bridge "$bridge" --bundle "$run/w2" || return
  exec taskset -c 8-11 tools/boat-dispatch --log-dir "$run/boat-logs" --job-class vopr \
    w2-damage "$run/w2" "python3 store_crash_tests.py sweep $((started * 4))"
)

w2_status=0
run_w2 >>"$run/w2.log" 2>&1 || w2_status=$?
if (( w2_status == 75 )); then
  echo "W2 damage sweep deferred until a spare Boat start is available." >>"$run/run.log"
elif (( w2_status != 0 )); then
  printf 'W2 damage sweep exited %s. See %s/w2.log.\n' "$w2_status" "$run" >>"$run/run.log"
fi
# N1 browser_room (webrtc.md): headless Chromium and Firefox against the SFU deployment on
# 127.0.0.1 (ports 7800, 7810-7811 and 3478). Playwright installs into ~/.cache/hirsel/n1.
run_n1() (
  local work="$run/n1"
  python3 core/programs/webrtc_sfu/service/run.py \
    --hirsel "$toolchain_hirsel" --work "$work" --build-only || return
  TMPDIR="$work" exec timeout 900 node core/programs/webrtc_sfu/service/n1.mjs --sfu "$work/sfu"
)

n1_status=$toolchain_status
if (( n1_status == 0 )); then
  run_n1 >>"$run/n1.log" 2>&1 || n1_status=$?
fi
if (( n1_status != 0 )); then
  printf 'N1 browser_room exited %s. See %s/n1.log.\n' "$n1_status" "$run" >>"$run/run.log"
fi
if (( status == 3 )) && [[ -n "$baseline_path" ]]; then
  python3 "$checkout/bench/awfy/board_bisect.py" "$run/bisect" \
    --baseline "$baseline_path" --board "$run/board/board.json" --alert "$run/bisect/alert.txt" \
    --awfy "$base/peers/awfy" --langarena "$base/peers/langarena" >>"$run/run.log" 2>&1 ||
    printf 'Board bisect unresolved. See %s/run.log.\n' "$run" >&2
fi
echo "$run"
exit "$status"
