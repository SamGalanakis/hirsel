#!/bin/bash
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")/.."
export PYTHONDONTWRITEBYTECODE=1
export LLVM_SYS_231_PREFIX="$PWD/tools/buck2/toolchains/llvm-files"
export LD_LIBRARY_PATH="$LLVM_SYS_231_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

results="${HIRSEL_BRIEFS:-/workspace/code/hirsel-briefs}/rtc10/nightly"
mkdir -p "$results"
run=$(mktemp -d "$results/$(date -u +%Y-%m-%dT%H%M%SZ)-XXXXXX")
trap 'rm -rf -- "$run/bundle" "$run/store" "$run/frames.hirsel" "$run/inputs.json"' EXIT

python3 tools/buck2/prepare.py
outputs=$(.buck2/bin/buck2 build -j4 --show-json-output \
  //:hirsel-core-driver-bin-hirsel //:core-driver-runtime-pic)
driver=$(python3 -c 'import json, sys; print(json.load(sys.stdin)["root//:hirsel-core-driver-bin-hirsel"])' <<<"$outputs")
runtime=$(python3 -c 'import json, sys; print(json.load(sys.stdin)["root//:core-driver-runtime-pic"])' <<<"$outputs")

runner=core/programs/webrtc_sfu/nightly/run.py
python3 "$runner" build --compiler "$driver" --runtime "$runtime" --scratch "$run"
python3 "$runner" run --scratch "$run"
