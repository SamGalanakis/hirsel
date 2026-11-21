#!/usr/bin/env python3
"""The standing provider-cost row: 10,000 small writes through a forwarding wrapper.

Run from the repository with the runtime archive without recording:
  python3 crates/hirsel-core-rt/measure-provider.py --runtime <libhirsel_core_rt.a>
Builds measure/provider-wrap-writes.c against it with the repository's clang, then
prints the median of five `perf stat -e instructions:u` runs on CPU 4 for the OS
provider and the wrapper. Exits 1 when the wrapper is over its line in
MEASUREMENTS.md ("Provider operations"): every other provider calls the OS
provider's callbacks through pointers, as the wrapper does.
"""
import argparse
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / 'crates/hirsel-core-rt'
LLVM = ROOT / 'tools/buck2/toolchains/llvm-files'
# 3,758,110 on a0e660e5 (release, 2026-11-16), plus 0.5%.
WRAPPER_LINE = 3_776_900


def instructions(exe, mode):
    q = subprocess.run(['perf', 'stat', '-x,', '-e', 'instructions:u', '--', exe, mode],
                       capture_output=True, text=True, check=True)
    return int([l for l in q.stderr.splitlines() if 'instructions' in l][-1].split(',')[0])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--runtime', required=True, type=Path)
    args = parser.parse_args()
    while os.getloadavg()[0] > 32:
        time.sleep(10)
    env = dict(os.environ, LD_LIBRARY_PATH=str(LLVM / 'lib'))
    with tempfile.TemporaryDirectory() as tmp:
        exe = str(Path(tmp) / 'provider-wrap-writes')
        subprocess.run([str(LLVM / 'bin/clang'), '-O2', '-I', str(CRATE / 'include'),
                        str(CRATE / 'measure/provider-wrap-writes.c'), str(args.runtime),
                        '-lpthread', '-ldl', '-lm', '-o', exe], env=env, check=True)
        os.sched_setaffinity(0, {4})
        medians = {mode: statistics.median(instructions(exe, mode) for _ in range(5))
                   for mode in ['os', 'wrap']}
    print(f"os {medians['os']:,} wrap {medians['wrap']:,} line {WRAPPER_LINE:,}")
    if medians['wrap'] > WRAPPER_LINE:
        print('the forwarding wrapper is over its line', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
