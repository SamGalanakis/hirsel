#!/usr/bin/env python3
"""Compare executable outputs with the committed independent oracles."""
import argparse
import difflib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
TARGET = 'x86_64-unknown-linux-gnu'


def verify(binary_dir, selected=None):
    passed = failed = 0
    for directory in sorted((ROOT/'corpus/m1').iterdir()):
        if not directory.is_dir() or selected and directory.name not in selected:
            continue
        for case in json.loads((directory/'cases.json').read_text()):
            for layout in ('idiomatic', 'matched'):
                binary = binary_dir / f'{directory.name}-{layout}'
                result = subprocess.run([str(binary), str(directory/case['input']), case['variant']], capture_output=True)
                expected = directory / 'EXPECTED' / case['name']
                want_out = expected.with_suffix('.stdout').read_bytes()
                want_err = expected.with_suffix('.stderr').read_bytes()
                want_code = int(expected.with_suffix('.exit').read_text())
                if (result.stdout, result.stderr, result.returncode) == (want_out, want_err, want_code):
                    passed += 1
                else:
                    failed += 1
                    print(f'FAIL {directory.name}/{case["name"]}/{layout}: exit {result.returncode}, expected {want_code}', file=sys.stderr)
                    print(''.join(difflib.unified_diff(want_out.decode().splitlines(True), result.stdout.decode(errors='replace').splitlines(True), fromfile='EXPECTED', tofile='actual')), file=sys.stderr)
                    if result.stderr:
                        print(result.stderr.decode(errors='replace'), file=sys.stderr)
    print(f'{passed} passed, {failed} failed')
    return failed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--profile', choices=['dev', 'release'], default='dev')
    parser.add_argument('--only', nargs='+')
    args = parser.parse_args()
    sys.exit(bool(verify(args.target_dir.resolve()/TARGET/('debug' if args.profile == 'dev' else 'release'), args.only)))


if __name__ == '__main__':
    main()
