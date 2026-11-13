#!/usr/bin/env python3
"""Separate diagnostic peaks; never replace shipping instruction/RSS samples.

Hirsel uses the optional MEM-19 observer, including startup reservations. Rust
uses Massif's requested live heap, with stacks/admin bytes excluded and zero
peak inaccuracy. Neither diagnostic run supplies a performance counter.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'core/tools'))
import measurement_artifacts
import std_native
from run import PHASES, manifest


def massif_peak(source):
    """Massif's zero-inaccuracy peak is retained when ordinary snapshots thin."""
    values = re.findall(r'^mem_heap_B=(\d+)$', source, re.M)
    if not values or not re.search(r'^heap_tree=peak$', source, re.M):
        raise ValueError('Massif did not record a heap peak')
    return max(map(int, values))


def execute(command, stem):
    # Use the same shared serial slot as performance runs, without collecting I/RSS.
    with open('/tmp/hirsel-langarena-measure.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        std_native.wait_load()
        result = subprocess.run([str(ROOT / 'tools/capped'), 'taskset', '-c', '16',
                                 *map(str, command)], capture_output=True)
    stem.with_suffix('.stdout').write_bytes(result.stdout)
    stem.with_suffix('.stderr').write_bytes(result.stderr)
    return result


def observed(args, inputs, entry, label, backend, expected, directory=None):
    stem = args.out / f'{label}-{backend}'
    source = stem.with_suffix('.inputs.json')
    source.write_text(json.dumps(inputs))
    report = stem.with_suffix('.charge.json')
    command = [args.compiler, 'build', '--inputs', str(source), '--entry', entry,
               '--backend', backend, '--runtime', str(args.observed_runtime.resolve()),
               '--logical-charge-report', str(report), '--store', str(args.out / 'store'),
               '--json', '-o', str(stem)]
    if backend == 'llvm':
        command.append('--release')
    if label.startswith('layout-'):
        command += ['--grant', 'Stdout']
    if directory:
        command += ['--dir', str(directory)]
    std_native.wait_load()
    built = subprocess.run(command, capture_output=True)
    stem.with_suffix('.build.stdout').write_bytes(built.stdout)
    stem.with_suffix('.build.stderr').write_bytes(built.stderr)
    row = dict(test=label, backend=backend, status='not-run', **args.identity)
    try:
        product = json.loads(built.stdout)
    except ValueError:
        product = {'stopped': built.stderr.decode(errors='replace')}
    if built.returncode or not product.get('native'):
        row['cause'] = std_native.first_stop(product)
        return row
    result = execute([stem], stem)
    program_stderr = b'\n'.join(line for line in result.stderr.splitlines()
                                if not line.startswith(b'capped:'))
    row.update(status='pass' if result.returncode == 0 and result.stdout == expected
               and not program_stderr else 'fail', exit=result.returncode,
               output=result.stdout.decode(errors='replace'))
    if report.exists():
        charge = json.loads(report.read_text())
        row['peak_root_reserved_bytes'] = charge['peak_root_reserved_bytes']
        row['observation_mode'] = charge['observation_mode']
    else:
        row.update(status='no-peak', cause='observer did not produce its report')
    return row


def rust_heap(args, phase, side):
    binary = args.twins / ('rust' if side == 'rust' else side)
    stem = args.out / f'{phase}-{side}'
    report = stem.with_suffix('.massif')
    command = ['valgrind', '--tool=massif', '--heap-admin=0', '--stacks=no',
               '--peak-inaccuracy=0', '--time-unit=B', '--max-snapshots=100',
               '--massif-out-file=' + str(report), str(binary), phase]
    if phase.startswith('json_'):
        command.append(str(Path(__file__).with_name('inputs') / 'documents.json'))
    result = execute(command, stem)
    expected = json.loads(Path(__file__).with_name('inputs').joinpath('expected.json').read_text())['phases'][phase]
    row = dict(test='layout-' + phase, side=side, **args.identity,
               status='pass' if result.returncode == 0 and result.stdout == f'{expected}\n'.encode() else 'fail',
               exit=result.returncode, output=result.stdout.decode(errors='replace'))
    if report.exists():
        row['peak_heap_bytes'] = massif_peak(report.read_text())
    else:
        row.update(status='no-peak', cause='Massif did not produce its report')
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True, help='measurement_artifacts.py JSON')
    parser.add_argument('--observed-runtime', type=Path, required=True)
    parser.add_argument('--twins', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--group', action='append', choices=['layout', 'arena', 'rust'])
    parser.add_argument('--phase', action='append', choices=PHASES)
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    args.twins = args.twins.resolve()
    artifacts = json.loads(args.artifacts.read_text())
    args.compiler = artifacts['compiler']
    identity = measurement_artifacts.identity(Path(args.compiler), args.observed_runtime,
                                              artifacts['compiler_build_profile'],
                                              'charge-observation', diagnostic=True)
    args.identity = identity
    (args.out / 'artifacts.json').write_text(json.dumps(identity, indent=2) + '\n')
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    rows = []

    def save(row):
        rows.append(row)
        (args.out / 'results.json').write_text(json.dumps(rows, indent=2) + '\n')
        print(json.dumps(row), flush=True)

    groups = args.group or ['layout', 'arena', 'rust']
    if 'layout' in groups:
        expected = json.loads(Path(__file__).with_name('inputs').joinpath('expected.json').read_text())['phases']
        for phase in args.phase or PHASES:
            module = PHASES[phase]
            directory = Path(__file__).with_name('inputs').resolve() if module == 'json' else None
            save(observed(args, manifest(module), f'layout@1/{module}/{phase}_main',
                          'layout-' + phase, 'llvm', f'{expected[phase]}\n'.encode(), directory))
    if 'arena' in groups:
        for case in std_native.discover(ROOT, ['arena:']):
            adapted = args.out / ('arena-' + case.file.name)
            adapted.write_text(case.file.read_text() + std_native.wrapper(case))
            inputs = std_native.manifest(ROOT, case, adapted)
            for backend in ['cranelift', 'llvm']:
                save(observed(args, inputs, 'std@1/tests/arena/native_contract',
                              case.name.replace(':', '-'), backend, case.stdout))
    if 'rust' in groups:
        for phase in args.phase or PHASES:
            if phase in ['charges', 'json_reject', 'json_capacity']:
                continue  # These contracts have no Rust-equivalent workload.
            for side in ['json_matched', 'json_serde'] if phase.startswith('json_') else ['rust']:
                save(rust_heap(args, phase, side))
    return int(any(row['status'] != 'pass' for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
