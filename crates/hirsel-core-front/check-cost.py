#!/usr/bin/env python3
"""Fail a release check/lower cost regression on the fixed M1 and core/std corpora."""
import argparse
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
from measure import source_lines


def check_ceiling(metrics, lines, ceiling):
    if metrics.get('counter_refusal'):
        raise ValueError(metrics['counter_refusal'])
    stages = {s['name']: s['instructions'] for s in metrics['stages']}
    lower = stages.get('check/lower')
    total = metrics.get('instructions')
    if lower is None or total is None:
        raise ValueError('instruction counter produced no complete sample')
    if lines != ceiling['code_lines']:
        raise ValueError(f"corpus changed: {lines} code lines, recorded {ceiling['code_lines']}")
    failures = []
    for name, instructions, maximum in [
        ('check/lower', lower, ceiling['check_lower_per_line']),
        ('whole fresh request', total, ceiling['whole_request_per_line']),
    ]:
        if instructions > maximum * lines:
            failures.append(f'{name}: {instructions / lines:.2f} instructions/line exceeds {maximum}')
    return lower / lines, total / lines, failures


def run(compiler, output, ceilings):
    manifest = json.loads((ROOT / 'core/programs/m1/manifest.json').read_text())
    manifest.pop('entries', None)
    for package in manifest['packages']:
        for source in package['sources']:
            source['file'] = str((ROOT / source['file']).resolve())
    failures = []
    for name in ['m1', 'std']:
        inputs = copy.deepcopy(manifest)
        if name == 'std':
            inputs['packages'] = [p for p in inputs['packages'] if p['package']['name'] in ['core', 'std']]
        directory = output / name
        directory.mkdir()
        (directory / 'inputs.json').write_text(json.dumps(inputs))
        files = {Path(s['file']) for p in inputs['packages'] for s in p['sources']}
        lines = sum(source_lines(p) for p in sorted(files))
        env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1'}
        command = ['taskset', '-c', '16', str(ROOT / 'tools/capped'), str(compiler), 'check',
                   '--inputs', str(directory / 'inputs.json'), '--store', str(directory / 'store'),
                   '--metrics', str(directory / 'metrics.json'), '--json']
        with (directory / 'result.json').open('w') as stdout, (directory / 'stderr.log').open('w') as stderr:
            result = subprocess.run(command, cwd=ROOT, env=env, stdout=stdout, stderr=stderr)
        # Until the checker is complete, benchmark corpora legitimately contain GAPs.
        # A tool crash or missing counter never passes as a cheap check.
        if result.returncode not in (0, 1, 2):
            raise ValueError(f'{name}: driver exited {result.returncode}')
        product = json.loads((directory / 'result.json').read_text())
        if 'incomplete' not in product or 'diagnostics' not in product:
            raise ValueError(f'{name}: driver did not return a check result')
        metrics = json.loads((directory / 'metrics.json').read_text())
        lower, total, errors = check_ceiling(metrics, lines, ceilings['corpora'][name])
        print(f'{name}: {lines} code lines; check/lower {lower:.2f}; whole fresh request {total:.2f} instructions/line')
        failures.extend(f'{name}: {e}' for e in errors)
    if failures:
        raise ValueError('\n'.join(failures))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--out', type=Path, help='new evidence directory, otherwise a temporary directory')
    parser.add_argument('--ceilings', type=Path, default=Path(__file__).with_name('cost-ceilings.json'))
    args = parser.parse_args()
    ceilings = json.loads(args.ceilings.read_text())
    if args.out:
        args.out.mkdir(parents=True, exist_ok=False)
        run(args.compiler.resolve(), args.out.resolve(), ceilings)
    else:
        with tempfile.TemporaryDirectory(prefix='hirsel-check-cost-') as out:
            run(args.compiler.resolve(), Path(out), ceilings)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
