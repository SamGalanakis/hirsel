#!/usr/bin/env bash
# Build one pool side (session_u and its recording executable) for record.py.
# Usage: taskset -c 6-7 build_record.sh SCRATCH SIDE DRIVER POOL_RT
set -euo pipefail

scratch=$(realpath -m "$1")
side=$2
driver=$(realpath "$3")
pool_rt=$(realpath "$4")
cd "$(dirname "$0")/../.."
case "$(taskset -pc $$ | awk '{print $NF}')" in
    6,7|6-7) ;;
    *) echo "pin the builder with taskset -c 6-7" >&2; exit 1 ;;
esac
out="$scratch/$side"
mkdir -p "$out"/build "$scratch"/{store,tapes}
export UNITS_DIR="$scratch/store" UNITS_LIMIT=1073741824
python3 bench/units/build.py bench/units/session_u.hirsel "$out/build/live" "$driver" "$pool_rt"
cp "$out/build/live/session_u" "$out/"
# Zero sessions creates the recording executable without binding a port.
UNITS_RECORD="$scratch/tapes/session_u.tape" UNITS_RECORD_ARGS="0 0 8 1 0 0" \
    python3 bench/units/build.py bench/units/session_u.hirsel "$out/build/record" "$driver" "$pool_rt" || true
cp "$out/build/record/session_u" "$out/session_u_rec"
rm -f "$scratch"/tapes/*
sha256sum "$driver" "$pool_rt" "$out"/session_u "$out"/session_u_rec
