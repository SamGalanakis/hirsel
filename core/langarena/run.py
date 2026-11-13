#!/usr/bin/env python3
"""Measure one LangArena configuration against two separately reported Rust twins.

Supply binaries built under core/MEASURE.md. The matched binary accepts
TASK WARMUP ITERATIONS EXPECTED key=value..., using rust-matched/support.rs.
Examples and build flags are in rust/README.md. No command is timed out or retried.
"""
import argparse
import fcntl
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


def observe(command, directory, task, expected, idiomatic=False, numeric=False):
    # Serialize process-tree samples even when several lane runners are active.
    with open("/tmp/hirsel-langarena-measure.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        wait_load()
        sample = _measure.sample(command, directory)
    # /usr/bin/time records RSS; capped diagnostics live in stderr, not stdout.
    stdout = (directory / 'stdout').read_text()
    if idiomatic:
        valid = (re.search(r'^' + re.escape(task) + r': OK in [0-9.]+s$', stdout, re.M)
                 and re.search(r'^Summary: [0-9.]+s, 1, 1, 0$', stdout, re.M))
    elif numeric:
        valid = stdout == str(expected) + '\n'
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


def reviewed(source, author):
    # The other lane owns this line after reviewing the final matching source.
    if not source.is_file():
        return False
    match = re.search(r'^// Reviewed-by: (\S+)', source.read_text(), re.M)
    return bool(match and match[1] in {'langs-numeric-920', 'langs-text-921'} and match[1] != author)


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
    parser.add_argument('--matched-author', choices=['langs-numeric-920', 'langs-text-921'], default='langs-numeric-920')
    parser.add_argument('--matched-args', nargs='*', help='argument templates, e.g. {count} {warmup} {iterations}')
    parser.add_argument('--matched-numeric-output', action='store_true', help='twin prints only the computed checksum')
    parser.add_argument('--resume', type=Path, help='previous report; reuse its complete samples and run missing sides only')
    parser.add_argument('--profile', action='store_true', help='profile instructions at period 50003 when an instruction ratio exceeds 2')
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
    args.out = args.out.resolve()
    args.out.mkdir(parents=True)
    os.sched_setaffinity(0, {16})
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    os.environ['HIRSEL_JOBS'] = '1'
    report = {'task': args.task, 'backend': args.backend, 'config': str(args.config.resolve()),
              'source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'cpu': 16, 'sample_policy': 'one sample per side; no retry; no system page-cache flush',
              'configuration': row, 'samples': {}, 'ratios': {}}
    if args.resume:
        previous = json.loads(args.resume.read_text())
        if previous['task'] != args.task or previous['backend'] != args.backend or previous.get('configuration') != row:
            parser.error('--resume must name the same task, backend and config')
        report['resumed_from'] = str(args.resume.resolve())
        report['samples'] = {key: value for key, value in previous['samples'].items() if value.get('status') == 'ok'}
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
            if 'hirsel' not in report['samples']:
                wait_load()
                built = subprocess.run(build, capture_output=True)
                (args.out / 'build.stdout').write_bytes(built.stdout)
                (args.out / 'build.stderr').write_bytes(built.stderr)
                if built.returncode:
                    report['samples']['hirsel'] = {'status': f'build-exit:{built.returncode}'}
                else:
                    report['samples']['hirsel'] = observe([str(binary), 'config.json', args.task],
                        args.out / 'hirsel-run', args.task, row['checksum'])
            if 'idiomatic' in report['samples']:
                pass
            elif args.idiomatic.is_file():
                report['samples']['idiomatic'] = observe([str(args.idiomatic.resolve()), str(config_file), args.task],
                    args.out / 'idiomatic-run', args.task, row['checksum'], idiomatic=True)
            else:
                report['samples']['idiomatic'] = {'status': 'missing-binary'}
            if 'matched' in report['samples']:
                pass
            elif args.matched and args.matched.is_file() and args.matched_source and reviewed(args.matched_source, args.matched_author):
                command = matched_command(args.matched, args.task, row)
                if args.matched_args is not None:
                    fields = dict(row, warmup=row.get('warmup_iterations', max(int(row['iterations'] * 0.2), 1)))
                    command = [str(args.matched.resolve()), *[arg.format_map(fields) for arg in args.matched_args]]
                report['samples']['matched'] = observe(command, args.out / 'matched-run', args.task, row['checksum'], numeric=args.matched_numeric_output)
            else:
                report['samples']['matched'] = {'status': 'blocked: missing binary or independent review'}
            for twin in ['idiomatic', 'matched']:
                report['ratios'][twin] = ratio(report['samples']['hirsel'], report['samples'][twin])
            if args.profile and binary.is_file() and any(r.get('instructions', 0) > 2 for r in report['ratios'].values()):
                data = args.out / 'perf.data'
                with open('/tmp/hirsel-langarena-measure.lock', 'a') as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX)
                    wait_load()
                    with (args.out / 'profile.log').open('w') as log:
                        profiled = subprocess.run([str(ROOT / 'tools/capped'), 'perf', 'record', '-e', 'instructions:u', '-c', '50003', '-o', str(data), '--', str(binary), 'config.json', args.task], stdout=log, stderr=subprocess.STDOUT)
                report['profile_status'] = profiled.returncode
                if profiled.returncode == 0:
                    with (args.out / 'hot-symbols.txt').open('w') as output:
                        subprocess.run(['perf', 'report', '--stdio', '--no-children', '-i', str(data)], stdout=output, stderr=subprocess.STDOUT, check=True)
                if data.exists():
                    data.unlink()

    except (OSError, subprocess.SubprocessError) as error:
        report['stop'] = str(error)
    (args.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return int('stop' in report or any(x['status'] != 'ok' for x in report['ratios'].values()))


if __name__ == '__main__':
    raise SystemExit(main())
