#!/usr/bin/env python3
"""The standing isolate-call row: one local entry call, a decode, a call and an
encode inside a `bytes n` bound and `isolate` (RUNNER-SPEC section 8, p03 mode 2).

Run from the repository with the release compiler and runtime archive:
  python3 crates/hirsel-core-rt/measure-isolate-call.py --hirsel <hirsel> --runtime <libhirsel_core_rt.a>
Builds measure/isolate-call on both engines (LLVM with --release), then prints the
instructions per call: (median at 2n - median at n) / n, medians of five
`perf stat -e instructions:u` runs on CPU 4. Exits 1 when an engine is over its
line in MEASUREMENTS.md ("Isolate and bound calls").
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
N = 20_000
# 1,532 (LLVM) and 3,018.5 (Cranelift) per call on 2026-11-16, plus 0.5%.
LINES = {'llvm': 1_540, 'cranelift': 3_034}


def instructions(exe, n, env):
    q = subprocess.run(['perf', 'stat', '-x,', '-e', 'instructions:u', '--', exe, '2', str(n)],
                       capture_output=True, text=True, env=env, check=True)
    return int([l for l in q.stderr.splitlines() if 'instructions' in l][-1].split(',')[0])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--hirsel', required=True, type=Path)
    parser.add_argument('--runtime', required=True, type=Path)
    args = parser.parse_args()
    while os.getloadavg()[0] > 32:
        time.sleep(10)
    over = []
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, LD_LIBRARY_PATH=str(LLVM / 'lib'), HIRSEL_MEMORY_LIMIT='2G',
                   XDG_CACHE_HOME=str(Path(tmp) / 'cache'), XDG_DATA_HOME=str(Path(tmp) / 'data'))
        os.sched_setaffinity(0, {4})
        for engine, line in LINES.items():
            exe = str(Path(tmp) / f'isolate-call-{engine}')
            subprocess.run([str(args.hirsel), 'build', str(CRATE / 'measure/isolate-call'),
                            '--libraries', str(ROOT / 'core'), '--runtime', str(args.runtime),
                            '--cc', str(LLVM / 'bin/clang'), '--limit', '1048576',
                            '--backend', engine, '--grant', 'Tasks', '-o', exe]
                           + (['--release'] if engine == 'llvm' else []), env=env, check=True)
            median = {n: statistics.median(instructions(exe, n, env) for _ in range(5))
                      for n in (N, 2 * N)}
            per_call = (median[2 * N] - median[N]) / N
            print(f'{engine} {per_call:,.1f} per call, line {line:,}')
            if per_call > line:
                over.append(engine)
    if over:
        print(f'an isolate call is over its line on {", ".join(over)}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
