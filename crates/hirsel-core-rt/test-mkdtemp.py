#!/usr/bin/env python3
"""Run mkdtemp prefix contracts on both engines through the std native walker."""
import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
import std_native


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', required=True, type=Path)
    parser.add_argument('--runtime', required=True, type=Path)
    parser.add_argument('--mismatch-runtime', type=Path,
                        help='old prefix/suffix mismatch mutant; must fail create_temp_names')
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent / 'fixtures/mkdtemp.hirsel'
    contracts = []
    for line, text in enumerate(fixture.read_text().splitlines(), 1):
        match = re.fullmatch(r'\s*// expect: run (.*?) = (.*?)\s*', text)
        if match:
            contracts.append(std_native.Contract(fixture, line, *match.groups(), True))
    if not contracts:
        raise ValueError('mkdtemp fixture has no contracts')
    owners = json.loads((ROOT / 'core/tools/std_native_owners.json').read_text())
    rows = std_native.run(ROOT, args.compiler.resolve(), args.runtime.resolve(), contracts,
                          ['cranelift', 'llvm'], args.out / 'cases.json', owners)
    if any(not row['passed'] for row in rows):
        return 1
    if args.mismatch_runtime:
        rows = std_native.run(ROOT, args.compiler.resolve(), args.mismatch_runtime.resolve(),
                              std_native.discover(ROOT, ['dir:create_temp_names']),
                              ['cranelift', 'llvm'], args.out / 'mutant.json', owners)
        if not all(row['ran'] and not row['passed'] and 'Unwrap' in row['cause'] for row in rows):
            return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
