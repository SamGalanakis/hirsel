#!/usr/bin/env bash
# Build both Hirsel sides and the Rust tools in a private measurement directory.
# Usage: taskset -c 4-5 build_scale.sh SCRATCH DRIVER POOL_RT CONTROL_RT
set -euo pipefail

scratch=$(realpath -m "$1")
driver=$(realpath "$2")
pool_rt=$(realpath "$3")
control_rt=$(realpath "$4")
cd "$(dirname "$0")/../.."
case "$(taskset -pc $$ | awk '{print $NF}')" in
    4,5|4-5) ;;
    *) echo "pin the builder with taskset -c 4-5" >&2; exit 1 ;;
esac
mkdir -p "$scratch"/{bin,build,store,tapes}

tools/capped rustup run 1.99.0 cargo build -j2 --manifest-path bench/units/rust/Cargo.toml \
    --target-dir "$scratch/build/cargo" --locked --release --bin loadgen --bin toolsrv --bin twin
cp "$scratch/build/cargo/release/"{loadgen,toolsrv,twin} "$scratch/bin/"
cp "$scratch/bin/loadgen" "$scratch/bin/loadgen_drain"

export UNITS_DIR="$scratch/store"
build_side() {
    local src=$1 rt=$2 limit=$3
    UNITS_LIMIT="$limit" python3 bench/units/build.py "bench/units/$src.hirsel" "$scratch/build/live" "$driver" "$rt"
    cp "$scratch/build/live/$src" "$scratch/bin/"
    # Zero sessions creates the recording executable without binding a port.
    UNITS_LIMIT="$limit" UNITS_RECORD="$scratch/tapes/$src.tape" UNITS_RECORD_ARGS="0 0 8 1 0 0" \
        python3 bench/units/build.py "bench/units/$src.hirsel" "$scratch/build/record" "$driver" "$rt"
    cp "$scratch/build/record/$src" "$scratch/bin/${src}_rec"
}

build_side session "$control_rt" 8589934592
build_side session_u "$pool_rt" 1073741824
sha256sum "$driver" "$pool_rt" "$control_rt" "$scratch/bin/"*
