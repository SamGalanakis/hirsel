#!/bin/bash
# Nightly goal board (board-1270). Build CPUs 8-11, measure CPUs 16-17.
set -euo pipefail
tools=$(dirname "$(readlink -f "$0")")
base=${HIRSEL_BOARD_BASE:-"$HOME/.local/share/hirsel-board"}
current=${HIRSEL_BOARD_CURRENT:-/workspace/code/hirsel-briefs/CURRENT-BOARD.md}
mkdir -p "$base/runs"
exec 9>"$base/baseline.lock"
flock 9
started=$(date +%s)
run=$(mktemp -d "$base/runs/$(date -u -d "@$started" +%Y%m%dT%H%M%SZ)-XXXXXX")
commit=$(git -C "$base/main" rev-parse --verify origin/main 2>/dev/null) || commit=
step="fetch origin/main"
record() {
  python3 "$tools/board-freshness.py" --base "$base" --current "$current" \
    --record "$run" --started "$started" --commit "$commit" --step "$step" "$@"
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
  baseline=(--baseline "$base/last-complete.json")
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
  printf '%s\n' "$run" > "$base/baseline-refused"
  echo "Comparison refused. Baseline retained until explicit rebaseline." >>"$run/run.log"
elif (( status == 3 )); then
  step="board comparison"
elif (( status == 0 )) && [[ ! -e "$base/baseline-refused" ]]; then
  step="promote baseline"
  ln -s "$run/board/board.json" "$run/complete.json"
  mv -Tf "$run/complete.json" "$base/last-complete.json"
elif (( status == 0 )); then
  echo "Promotion held by an earlier refusal. Run board.py rebaseline with a clean run." >>"$run/run.log"
fi
if (( status == 0 && boat_status != 0 )); then
  step="Boat screen and containment"
  status=$boat_status
fi
echo "$run"
exit "$status"
