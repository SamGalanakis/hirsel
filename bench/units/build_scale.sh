#!/usr/bin/env bash
# Build the session server on units and the Rust tools in a private measurement directory.
# Usage: taskset -c 8-11 build_scale.sh SCRATCH DRIVER RUNTIME [SIDE]
# session_u lands in SCRATCH/bin (scale.py's untied sides) and in SCRATCH/SIDE
# (light.py's side directory, default `lang`).
set -euo pipefail

if [[ ${UNITS_BUILD_UNDER_CAP:-0} != 1 ]]; then
    root=$(cd "$(dirname "$0")/../.." && pwd)
    export HIRSEL_JOBS=4 UNITS_BUILD_UNDER_CAP=1
    exec "$root/tools/capped" flock /run/user/1000/hirsel-mc-regimes.lock \
        timeout --kill-after=5s 220s "$0" "$@"
fi

scratch=$(realpath -m "$1")
driver=$(realpath "$2")
runtime=$(realpath "$3")
side=${4:-lang}
cd "$(dirname "$0")/../.."
case "$(taskset -pc $$ | awk '{print $NF}')" in
    8-11|8,9,10,11) ;;
    *) echo "pin the builder with taskset -c 8-11" >&2; exit 1 ;;
esac
mkdir -p "$scratch"/{bin,build,store,software,"$side"}

rustup run 1.99.0 cargo build -j4 --manifest-path bench/units/rust/Cargo.toml \
    --target-dir "$scratch/build/cargo" --locked --release --bin loadgen --bin toolsrv --bin twin
cp "$scratch/build/cargo/release/"{loadgen,toolsrv,twin} "$scratch/bin/"
for variant in drain detail churn; do
    ln -sf loadgen "$scratch/bin/loadgen_$variant"
done
rustup run 1.99.0 cargo build -j4 --manifest-path bench/units/rust/Cargo.toml \
    --target-dir "$scratch/build/cargo" --locked --release --features software-sha --bin twin
cp "$scratch/build/cargo/release/twin" "$scratch/software/"

# The partitions' log directory is the program's directory grant.
UNITS_DIR="$scratch/store" UNITS_LIMIT=1073741824 \
    python3 bench/units/build.py bench/units/session_u.hirsel "$scratch/build/live" "$driver" "$runtime"
cp "$scratch/build/live/session_u" "$scratch/bin/"
cp "$scratch/build/live/session_u" "$scratch/$side/"
sha256sum "$driver" "$runtime" "$scratch/bin/"* "$scratch/software/twin"
