#!/bin/bash
# mc-units-proto replay demo: record three K=3 runs of x_svc, one runtime
# tape per unit, then replay unit 0 alone (no peers, no grants) from each.
# A negative control replays a program that reports state ^ 1; it must
# diverge (Arg) at the report.
# Usage: replay_demo.sh DRIVER RUNTIME_ARCHIVE WORKDIR   (runs on CPUs 6-7)
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DRIVER=$1; RT=$2; W=$3
L=$ROOT/tools/buck2/toolchains/llvm-files
mkdir -p $W/app $W/bin; cp $ROOT/bench/units/xbench.hirsel $W/app/main.hirsel
h() { local cmd=$1; shift; (cd $W && LD_LIBRARY_PATH=$L/lib HIRSEL_MEMORY_LIMIT=2G HIRSEL_JOBS=1 taskset -c 6-7 $DRIVER $cmd $W/app \
  --libraries $ROOT/core --source-core --source-std --store $W/bin/store --backend llvm --release \
  --runtime $RT --cc $L/bin/clang "$@"); }
NET='Net=Listen 127.77.0.0/16 0-65535;Connect 127.77.0.0/16 0-65535'
rm -f $W/svc.tape*
h run --grant Tasks --grant Clock --grant Stderr --grant "$NET" --grant Stdout --record $W/svc.tape -o $W/bin/xrec -- none 1 1 >/dev/null 2>&1
rm -f $W/svc.tape
for i in 1 2 3; do
  mkdir -p $W/run$i; rm -f $W/run$i/*
  (cd $W && HIRSEL_UNITS=3 HIRSEL_UNIT_CPUS=6,7,7 taskset -c 6-7 $W/bin/xrec x_svc 20000 0)
  mv $W/svc.tape.u* $W/run$i/
done
for i in 1 2 3; do h replay --tape $W/run$i/svc.tape.u0 >/dev/null 2>&1; echo "replay run$i unit0 exit $?"; done
sed -i 's/report(err, "x_svc", 2 \* n, 0, state)/report(err, "x_svc", 2 * n, 0, state ^ 1)/' $W/app/main.hirsel
h replay --tape $W/run1/svc.tape.u0 --diagnose 2>&1 | grep -i diverg; echo "mutated replay exit ${PIPESTATUS[0]}"
cp $ROOT/bench/units/xbench.hirsel $W/app/main.hirsel
