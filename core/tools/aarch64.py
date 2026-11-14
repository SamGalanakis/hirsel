#!/usr/bin/env python3
"""Execute ARM LLVM acceptance using declared compiler, runtime and emulator inputs."""
import argparse
import json
import os
from pathlib import Path
import tempfile

import front_native
import m1
import std_native

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--evaluator', type=Path)
    parser.add_argument('--phase', choices=['front', 'std', 'm1'], required=True)
    parser.add_argument('--facts', choices=['both', 'on', 'off'], default='both')
    parser.add_argument('--small', action='store_true')
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    compiler = args.compiler.resolve()
    runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVE']).resolve()
    cc = Path(os.environ['HIRSEL_AARCH64_CC']).resolve()
    emulator = Path(os.environ['HIRSEL_QEMU_AARCH64']).resolve()
    sysroot = Path(os.environ['HIRSEL_AARCH64_SYSROOT']).resolve()
    evaluator = args.evaluator.resolve() if args.evaluator else None
    target = 'aarch64-unknown-linux-gnu'
    modes = ['on', 'off'] if args.facts == 'both' else [args.facts]
    executed, passed = 0, True
    with tempfile.TemporaryDirectory(prefix='hirsel-arm-acceptance-') as temporary:
        for mode in modes:
            if mode == 'off':
                os.environ['HIRSEL_LLVM_NO_FACTS'] = '1'
            else:
                os.environ.pop('HIRSEL_LLVM_NO_FACTS', None)
            if args.phase == 'front':
                only = ['args-empty', 'stdout-cancel', 'string-growth-refusal', 'host-read'] if args.small else None
                rows = front_native.run(compiler, runtime, ROOT, cc, target, emulator, sysroot, only)
                executed += sum(r['ran'] for r in rows)
                passed &= all(r['passed'] for r in rows)
            elif args.phase == 'std':
                contracts = std_native.discover(ROOT, [])
                owners = json.loads((ROOT / 'core/tools/std_native_owners.json').read_text())
                rows = std_native.run(ROOT, compiler, runtime, contracts, ['llvm'], None, owners,
                                      cc, 30, evaluator, target, emulator, sysroot)
                executed += sum(r['ran'] for r in rows if r['engine'] == 'llvm')
                passed &= all(r['passed'] for r in rows if r['applicable'])
            else:
                rows = m1.run(compiler, runtime, ['llvm'], ROOT / 'core/programs/m1/manifest.json', ROOT,
                              Path(temporary) / mode, cc, target=target, emulator=emulator, sysroot=sysroot)
                print(m1.table(rows), flush=True)
                executed += sum(len(r.get('case_outcomes', [])) for r in rows)
                passed &= all(r['built'] and r['passed'] == r['cases'] for r in rows)
    print(f'AARCH64_EXECUTED {executed}', flush=True)
    return int(not passed or executed == 0)


if __name__ == '__main__':
    raise SystemExit(main())
