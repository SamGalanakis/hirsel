#!/usr/bin/env python3
"""LAYOUT correctness and one-sample measurements through the native product path."""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'core/tools'))
from std_native import wait_load, first_stop


def toolchain_identity(compiler, runtime, compiler_profile):
    spec = importlib.util.spec_from_file_location('langarena_runner', ROOT / 'core/langarena/run.py')
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    return runner.toolchain_identity(compiler, runtime, compiler_profile)


PHASES = {'ordinary': 'documents', 'sized': 'documents', 'streaming': 'documents',
          'exclusive': 'graph', 'churn': 'graph', 'rc': 'graph', 'arc': 'graph', 'json_parse': 'json', 'json_print': 'json', 'json_reject': 'json', 'json_capacity': 'json', 'charges': 'graph', 'lifetime': 'graph'}


def manifest(module):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    app = {'name': 'layout', 'version': '1'}
    def sources(where, signatures):
        return [{'file': str(p.resolve()), 'modules': [p.stem], 'signatures': signatures}
                for p in sorted(where.glob('*.hirsel'))]
    return {'core': core, 'packages': [
        {'package': core, 'sources': sources(ROOT / 'core/core', True), 'dependencies': []},
        {'package': std, 'sources': sources(ROOT / 'core/std', False), 'dependencies': [core]},
        {'package': app, 'sources': [{'file': str(Path(__file__).with_name(module + '.hirsel')),
                                    'modules': [module], 'signatures': False}], 'dependencies': [core, std]}]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--phase', action='append', choices=PHASES)
    parser.add_argument('--backend', choices=['both', 'llvm', 'cranelift'], default='both')
    parser.add_argument('--measure', action='store_true')
    parser.add_argument('--compiler-profile', choices=['release', 'checked', 'unknown'], default='unknown')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    expected = json.loads(Path(__file__).with_name('inputs').joinpath('expected.json').read_text())['phases']
    rows = []
    measure = None
    if args.measure:
        try:
            identity = toolchain_identity(args.compiler, args.runtime, args.compiler_profile)
        except ValueError as error:
            parser.error(str(error))
        (args.out / 'toolchain.json').write_text(json.dumps(identity, indent=2) + '\n')
        os.environ["HIRSEL_MEMORY_LIMIT"] = "2G"
        spec = importlib.util.spec_from_file_location('layout_measure', ROOT / 'core/tools/measure.py')
        measure = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(measure)
        if 'Reviewed-by:' not in Path(__file__).with_name('rust.rs').read_text():
            parser.error('matched Rust twin needs independent review before ratios')
    for phase in args.phase or PHASES:
        module = PHASES[phase]
        for backend in ['cranelift', 'llvm'] if args.backend == 'both' else [args.backend]:
            stem = args.out / f'{phase}-{backend}'
            source = stem.with_suffix('.inputs.json')
            source.write_text(json.dumps(manifest(module)))
            command = [str(args.compiler.resolve()), 'build', '--inputs', str(source), '--entry',
                       f'layout@1/{module}/{phase}_main', '--backend', backend, '--runtime', str(args.runtime.resolve()),
                       '--store', str(args.out / 'store'), '--grant', 'Stdout', '--json', '-o', str(stem)]
            if module == 'json': command.extend(['--dir', str(Path(__file__).with_name('inputs'))])
            if backend == 'llvm': command.append('--release')
            wait_load()
            built = subprocess.run(command, capture_output=True)
            stem.with_suffix('.build.stdout').write_bytes(built.stdout)
            stem.with_suffix('.build.stderr').write_bytes(built.stderr)
            try: product = json.loads(built.stdout)
            except ValueError: product = {'stopped': built.stderr.decode(errors='replace')}
            row = dict(phase=phase, backend=backend, status='not-run', cause=None)
            if built.returncode or not product.get('native'):
                row['cause'] = first_stop(product)
            else:
                wait_load()
                run = subprocess.run([str(stem)], capture_output=True)
                row.update(status='pass' if run.returncode == 0 and run.stdout == f'{expected[phase]}\n'.encode() and not run.stderr else 'fail',
                           output=run.stdout.decode(errors='replace'), stderr=run.stderr.decode(errors='replace'), exit=run.returncode)
                if measure and row['status'] == 'pass':
                    wait_load()
                    with open('/tmp/hirsel-langarena-measure.lock', 'a') as lock:
                        fcntl.flock(lock, fcntl.LOCK_EX)
                        row['measurement'] = measure.sample(['taskset', '-c', '16', str(stem)], args.out / f'{phase}-{backend}-sample')
                    sample = row['measurement']
                    if (Path(sample['directory']) / 'stdout').read_bytes() != f'{expected[phase]}\n'.encode():
                        sample['status'] = 'wrong-output'
                    if sample['status'] != 'ok':
                        row['status'] = 'measurement-failed'
            rows.append(row)
            (args.out / 'results.json').write_text(json.dumps(rows, indent=2, default=str) + '\n')
            print(json.dumps(row, default=str), flush=True)
    return int(any(r['status'] != 'pass' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
