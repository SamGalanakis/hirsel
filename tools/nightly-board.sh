#!/bin/bash
# Nightly goal board (board-1270). Build CPUs 8-11, measure CPUs 16-17 (off checkbox 20-23 and SMT build siblings).
set -eu
base="$HOME/.local/share/hirsel-board"
mkdir -p "$base/runs"
exec 9>"$base/baseline.lock"
flock 9
cd "$base/main"
taskset -c 8-11 git fetch -q origin main
taskset -c 8-11 git switch -q --detach origin/main
# Warm peer Rust caches before board.py invokes cargo --offline (prevents silent exit-101 incomplete boards).
for manifest in "$base/peers/langarena/rust/Cargo.toml" "$base/peers/awfy/rust/Cargo.toml"; do
  if [[ -f "$manifest" ]]; then
    taskset -c 8-11 cargo +1.99.0 fetch --locked --manifest-path "$manifest"
  fi
done
run=$(mktemp -d "$base/runs/$(date -u +%Y%m%dT%H%M%SZ)-XXXXXX")
baseline=()
if [[ -e "$base/last-complete.json" || -L "$base/last-complete.json" ]]; then
  baseline=(--baseline "$base/last-complete.json")
fi
status=0
taskset -c 8-11,16-17 python3 bench/awfy/board.py "$run/board" \
  --build-cpus 8-11 --measure-cpus 16-17 --awfy "$base/peers/awfy" \
  --resid "$base/peers/resid" --goose "$base/peers/goose" --langarena "$base/peers/langarena" \
  "${baseline[@]}" >"$run/run.log" 2>&1 || status=$?
if [[ -f "$run/board/board.md" ]]; then
  ln -sfn "$run/board/board.md" /workspace/code/hirsel-briefs/CURRENT-BOARD.md
fi
if (( status == 2 )); then
  printf '%s\n' "$run" > "$base/baseline-refused"
  echo "Comparison refused. Baseline retained until explicit rebaseline." >>"$run/run.log"
elif (( status == 0 )) && [[ ! -e "$base/baseline-refused" ]]; then
  ln -s "$run/board/board.json" "$run/complete.json"
  mv -Tf "$run/complete.json" "$base/last-complete.json"
elif (( status == 0 )); then
  echo "Promotion held by an earlier refusal. Run board.py rebaseline with a clean run." >>"$run/run.log"
fi
echo "$run"
exit "$status"
