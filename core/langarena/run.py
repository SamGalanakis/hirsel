#!/usr/bin/env python3
"""Measure one LangArena configuration against two separately reported Rust twins.

Supply binaries built under core/MEASURE.md. The matched binary accepts
TASK WARMUP ITERATIONS EXPECTED key=value..., using rust-matched/support.rs.
Examples and build flags are in rust/README.md. No command is timed out or retried.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
from langarena import inputs
from std_native import wait_load

_spec = importlib.util.spec_from_file_location('core_measure', ROOT / 'core/tools/measure.py')
_measure = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_measure)

POPULATE = 'core/langarena/rust/vendor.sh'


def vendor_check(rust):
    """Use Cargo's package and per-file checksums, pinned by Cargo.lock."""
    vendor = rust / 'vendor'
    if not vendor.is_dir():
        raise ValueError(f'missing vendor directory; run: {POPULATE}')
    packages = tomllib.loads((rust / 'Cargo.lock').read_text())['package']
    directories = {}
    for path in vendor.iterdir():
        if not path.is_dir() or not (path / 'Cargo.toml').is_file():
            continue
        package = tomllib.loads((path / 'Cargo.toml').read_text()).get('package', {})
        directories[(package.get('name'), package.get('version'))] = path
    for package in packages:
        if not package.get('source', '').startswith('registry+'):
            continue
        key = package['name'], package['version']
        directory = directories.get(key)
        if directory is None:
            raise ValueError(f'missing vendored {key[0]} {key[1]}; run: {POPULATE}')
        try:
            checksum = json.loads((directory / '.cargo-checksum.json').read_text())
        except (OSError, ValueError) as error:
            raise ValueError(f'invalid checksum metadata for {key}; run: {POPULATE}') from error
        if checksum.get('package') != package['checksum']:
            raise ValueError(f'crate checksum differs from Cargo.lock: {key}; run: {POPULATE}')
        for relative, expected in checksum['files'].items():
            file = directory / relative
            if not file.resolve().is_relative_to(directory.resolve()) or not file.is_file():
                raise ValueError(f'missing or escaping vendor file {key}/{relative}; run: {POPULATE}')
            if hashlib.sha256(file.read_bytes()).hexdigest() != expected:
                raise ValueError(f'vendor file checksum mismatch: {key}/{relative}; run: {POPULATE}')


def ratio(numerator, denominator):
    """No incomplete sample, missing counter or zero denominator becomes a ratio."""
    if numerator.get('status') != 'ok' or denominator.get('status') != 'ok':
        return {'status': 'blocked', 'reason': 'both samples must be complete and correct'}
    result = {}
    for field in ['instructions', 'max_rss_kib', 'seconds']:
        try:
            left, right = float(numerator[field]), float(denominator[field])
        except (KeyError, ValueError, TypeError):
            return {'status': 'blocked', 'reason': f'missing {field}'}
        if left < 0 or right <= 0 or not (left < float('inf') and right < float('inf')):
            return {'status': 'blocked', 'reason': f'invalid {field}'}
        result[field] = left / right
    return {'status': 'ok', **result}


def expected_line(task, expected):
    return f'{task} checksum={expected} expected={expected} status=OK\n'


def observe(command, directory, task, expected, idiomatic=False):
    wait_load()
    sample = _measure.sample(command, directory)
    # /usr/bin/time records RSS; capped diagnostics live in stderr, not stdout.
    stdout = (directory / 'stdout').read_text()
    if idiomatic:
        valid = (re.search(r'^' + re.escape(task) + r': OK in [0-9.]+s$', stdout, re.M)
                 and re.search(r'^Summary: [0-9.]+s, 1, 1, 0$', stdout, re.M))
    else:
        valid = stdout == expected_line(task, expected)
    if not valid:
        sample['status'] = 'wrong-output'
    sample['directory'] = str(sample['directory'])
    return sample


def matched_command(binary, task, row):
    iterations = row['iterations']
    warmup = row.get('warmup_iterations', max(int(iterations * 0.2), 1))
    fields = [f'{key}={value}' for key, value in row.items()
              if key not in ('name', 'checksum', 'iterations', 'warmup_iterations')]
    return [str(binary.resolve()), task, str(warmup), str(iterations), str(row['checksum']), *fields]


def reviewed(source):
    # The other lane owns this line after reviewing the final matching source.
    return source.is_file() and re.search(r'^// Reviewed-by: (?!langs-numeric-920\b)\S+.*$',
                                         source.read_text(), re.M) is not None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--module', required=True)
    parser.add_argument('--task', required=True)
    parser.add_argument('--config', type=Path, default=ROOT / 'core/langarena/rust/run.js')
    parser.add_argument('--backend', choices=['cranelift', 'llvm'], required=True)
    parser.add_argument('--idiomatic', type=Path, required=True)
    parser.add_argument('--matched', type=Path)
    parser.add_argument('--matched-source', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    try:
        vendor_check(ROOT / 'core/langarena/rust')
    except (OSError, ValueError, KeyError) as error:
        parser.error(str(error))
    if args.out.exists() or args.out.resolve().is_relative_to(ROOT):
        parser.error('--out must be new and outside the repository')
    if 16 not in os.sched_getaffinity(0):
        parser.error('CPU 16 is required by core/MEASURE.md')
    config = json.loads(args.config.read_text())
    row = next(c for c in config if c['name'] == args.task) if isinstance(config, list) else config[args.task]
    args.out.mkdir(parents=True)
    os.sched_setaffinity(0, {16})
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    os.environ['HIRSEL_JOBS'] = '1'
    report = {'task': args.task, 'backend': args.backend, 'config': str(args.config.resolve()),
              'source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'cpu': 16, 'sample_policy': 'one sample per side; no retry; no system page-cache flush',
              'samples': {}, 'ratios': {}}
    try:
        with tempfile.TemporaryDirectory(prefix='langarena-host-', dir=args.out) as tmp:
            host = Path(tmp)
            config_file = host / 'config.json'
            config_file.write_bytes(args.config.read_bytes())
            manifest = args.out / 'inputs.json'
            manifest.write_text(json.dumps(inputs(ROOT, args.module)))
            binary = args.out / 'hirsel'
            build = [str(args.compiler.resolve()), 'build', '--inputs', str(manifest), '--entry',
                     f'langarena@1/{args.module}/main', '--backend', args.backend,
                     '--runtime', str(args.runtime.resolve()), '--store', str(args.out / 'store'),
                     '--dir', str(host), '--grant', 'Stdout', '--grant', 'Tasks', '--json', '-o', str(binary)]
            if args.backend == 'llvm':
                build.append('--release')
            wait_load()
            built = subprocess.run(build, capture_output=True)
            (args.out / 'build.stdout').write_bytes(built.stdout)
            (args.out / 'build.stderr').write_bytes(built.stderr)
            if built.returncode:
                report['samples']['hirsel'] = {'status': f'build-exit:{built.returncode}'}
            else:
                report['samples']['hirsel'] = observe([str(binary), 'config.json', args.task],
                    args.out / 'hirsel-run', args.task, row['checksum'])
            if args.idiomatic.is_file():
                report['samples']['idiomatic'] = observe([str(args.idiomatic.resolve()), str(config_file), args.task],
                    args.out / 'idiomatic-run', args.task, row['checksum'], idiomatic=True)
            else:
                report['samples']['idiomatic'] = {'status': 'missing-binary'}
            if args.matched and args.matched.is_file() and args.matched_source and reviewed(args.matched_source):
                report['samples']['matched'] = observe(matched_command(args.matched, args.task, row),
                    args.out / 'matched-run', args.task, row['checksum'])
            else:
                report['samples']['matched'] = {'status': 'blocked: missing binary or independent review'}
            for twin in ['idiomatic', 'matched']:
                report['ratios'][twin] = ratio(report['samples']['hirsel'], report['samples'][twin])
    except (OSError, subprocess.SubprocessError) as error:
        report['stop'] = str(error)
    (args.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return int('stop' in report or any(x['status'] != 'ok' for x in report['ratios'].values()))


if __name__ == '__main__':
    raise SystemExit(main())
