#!/usr/bin/env python3
"""One CPU16 process-tree instruction/RSS sample per already-qualified side.

Uses core/tools/measure.py's cap, perf and GNU time accounting. Missing native
phases stay missing. This is not a fresh build or an extra correctness sweep.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'core/tools'))
from std_native import wait_load


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', action='append', required=True, help='phase=/absolute/qualified/binary')
    parser.add_argument('--twins', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--rust', type=Path, help='optional separate graph twin binary')
    args = parser.parse_args()
    os.environ["HIRSEL_MEMORY_LIMIT"] = "2G"
    for source in ['rust.rs', 'json_matched.rs']:
        if not any(line.startswith('// Reviewed-by:') for line in Path(__file__).with_name(source).read_text().splitlines()):
            parser.error(f'{source} requires independent review')
    args.out.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('measurement', ROOT / 'core/tools/measure.py')
    measurement = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(measurement)
    expected = json.loads(Path(__file__).with_name('inputs').joinpath('expected.json').read_text())['phases']
    rows = []
    for selection in args.native:
        phase, binary = selection.split('=', 1)
        sides = [('hirsel', [binary])]
        if phase.startswith('json_'):
            for twin in ['json_matched', 'json_serde']:
                sides.append((twin, [str(args.twins / twin), phase, str(Path(__file__).with_name('inputs').joinpath('documents.json'))]))
        else:
            sides.append(('rust', [str(args.rust or args.twins / 'rust'), phase]))
        for side, command in sides:
            wait_load()
            # Pin the process under perf, including its children. Both sides include
            # the same taskset/time startup floor and retain checked arithmetic.
            sample = measurement.sample(['taskset', '-c', '16', *command], args.out / f'{phase}-{side}')
            text = (args.out / f'{phase}-{side}' / 'stdout').read_text()
            if text != f'{expected[phase]}\n': sample['status'] = 'wrong-output'
            row = dict(phase=phase, side=side, **sample)
            rows.append(row)
            (args.out / 'results.json').write_text(json.dumps(rows, indent=2, default=str) + '\n')
            print(json.dumps(row, default=str), flush=True)
    return int(any(row['status'] != 'ok' for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
