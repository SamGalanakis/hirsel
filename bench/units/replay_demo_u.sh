#!/bin/bash
# mc-units-untied laws 2 and 3. Records the closed lock-step ring (x_ring,
# K=4 units) at W=1 and at W=8 with a fixed tape id, compares each unit's
# tape byte for byte, then replays units 0 and 1 alone (one thread, no peers,
# no grants) from the W=8 tapes. A negative control replays unit 1 against a
# program whose ring step differs; it must be refused at that operation.
# Usage: replay_demo_u.sh DRIVER RUNTIME_ARCHIVE WORKDIR   (runs on CPUs 6-7)
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DRIVER=$1; RT=$2; W=$3
L=$ROOT/tools/buck2/toolchains/llvm-files
mkdir -p $W/app $W/bin; cp $ROOT/bench/units/xbench_u.hirsel $W/app/main.hirsel
h() { local cmd=$1; shift; (cd $W && LD_LIBRARY_PATH=$L/lib HIRSEL_MEMORY_LIMIT=2G HIRSEL_JOBS=1 taskset -c 6-7 $DRIVER $cmd $W/app \
  --libraries $ROOT/core --source-core --source-std --store $W/bin/store --backend llvm --release \
  --runtime $RT --cc $L/bin/clang "$@"); }
NET='Net=Listen 127.77.0.0/16 0-65535;Connect 127.77.0.0/16 0-65535'
rm -f $W/ring.tape*
h run --grant Tasks --grant Clock --grant Stderr --grant "$NET" --grant Stdout --record $W/ring.tape -o $W/bin/xrec -- none 1 1 >/dev/null 2>&1
rm -f $W/ring.tape
for w in 1 8; do
  mkdir -p $W/w$w; rm -f $W/w$w/*
  (cd $W && HIRSEL_XGATE=1 HIRSEL_TAPE_ID_FIXED=1 HIRSEL_UNITS=4 HIRSEL_UNTIED=$w HIRSEL_XSTATS=1 taskset -c 6-7 $W/bin/xrec x_ring 3000 64 2>&1 | cut -c1-140)
  mv $W/ring.tape.u* $W/w$w/
done
for u in 0 1 2 3; do
  if cmp -s $W/w1/ring.tape.u$u $W/w8/ring.tape.u$u; then echo "law2 unit $u: W=1 and W=8 tapes identical ($(stat -c %s $W/w8/ring.tape.u$u) bytes, sha256 $(sha256sum < $W/w8/ring.tape.u$u | cut -c1-16))"
  else echo "law2 unit $u: DIFFER"; cmp $W/w1/ring.tape.u$u $W/w8/ring.tape.u$u; fi
done
for u in 0 1; do h replay --tape $W/w8/ring.tape.u$u >/dev/null 2>&1; echo "law3 replay unit $u alone from the W=8 tape: exit $?"; done
sed -i 's/kernel(wide + unit, 50)/kernel(wide + unit, 51)/' $W/app/main.hirsel
h replay --tape $W/w8/ring.tape.u1 --diagnose 2>&1 | grep -iE "diverg|refus|at " | head -5; echo "law3 negative control (ring step changed) exit ${PIPESTATUS[0]}"
cp $ROOT/bench/units/xbench_u.hirsel $W/app/main.hirsel
