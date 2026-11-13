#!/usr/bin/env python3
"""Single lifecycle instruction samples for tasks/providers under core/MEASURE.md.

Pinned local reference sources only, no dependency downloads. Builds one source
version with opt3, checked arithmetic, one codegen unit, no LTO/debug/incremental.
Only the runtime's charge dependency is needed by this ABI benchmark; compile
that exact production module without the unused layout/serde compiler machinery.
"""
import argparse
import csv
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / 'crates/hirsel-core-rt'
FLAGS = ['--target', 'x86_64-unknown-linux-gnu', '-C', 'target-cpu=x86-64',
         '-C', 'opt-level=3', '-C', 'overflow-checks=on', '-C', 'debug-assertions=no',
         '-C', 'codegen-units=1', '-C', 'debuginfo=0', '-C', 'lto=off']
VERSIONS = ['libc-0.2.191', 'pin-project-lite-0.2.17', 'bytes-1.12.0',
            'tokio-1.53.2', 'crossbeam-utils-0.8.23', 'crossbeam-channel-0.5.15']
MEASURE_ENV = dict(os.environ, HIRSEL_MEMORY_LIMIT='2G', HIRSEL_JOBS='1')
CASES = [('spawn', 10_000, ['core', 'threads', 'tokio']),
         ('roundtrip', 100_000, ['core', 'mpsc', 'crossbeam', 'tokio']),
         ('pipeline', 1_000_000, ['core', 'mpsc', 'crossbeam', 'tokio']),
         ('read', 128, ['core', 'std', 'confined', 'tokio']),
         ('write', 128, ['core', 'std', 'confined', 'tokio'])]


def low_load():
    while os.getloadavg()[0] > 32:
        print(f'build deferred: load {os.getloadavg()[0]:.2f} > 32', flush=True)
        time.sleep(15)


def source(version):
    matches = list((Path.home() / '.cargo/registry/src').glob(f'*/{version}/src/lib.rs'))
    if len(matches) != 1:
        raise RuntimeError(f'need exactly one installed {version} source: {matches}')
    return matches[0]


def build(out):
    rustc = subprocess.check_output(['rustup', 'which', '--toolchain', '1.99.0', 'rustc'], text=True).strip()
    libs = out / 'lib'
    libs.mkdir(exist_ok=True)
    charge = libs / 'charge-root.rs'
    charge.write_text('#[path=' + json.dumps(str(ROOT / 'crates/hirsel-core-layout/src/charge.rs')) + ']\npub mod charge;\n')
    targets = [
        ('hirsel_core_layout', charge, '2024', [], []),
        ('hirsel_stack', ROOT / 'crates/hirsel-stack/src/lib.rs', '2024', [], []),
        ('libc', source(VERSIONS[0]), '2021', [], []),
        ('pin_project_lite', source(VERSIONS[1]), '2018', [], []),
        ('bytes', source(VERSIONS[2]), '2021', ['std'], []),
        ('tokio', source(VERSIONS[3]), '2021', ['rt', 'sync', 'fs', 'io-util'], ['pin_project_lite', 'bytes']),
        ('crossbeam_utils', source(VERSIONS[4]), '2021', ['std'], []),
        ('crossbeam_channel', source(VERSIONS[5]), '2021', ['std'], ['crossbeam_utils']),
        ('hirsel_core_rt', CRATE / 'src/lib.rs', '2024', [], ['hirsel_core_layout', 'hirsel_stack', 'libc']),
    ]
    environment = dict(rustc=subprocess.check_output([rustc, '-vV'], text=True),
                       source_base=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                       flags=FLAGS, references=VERSIONS, platform=platform.platform(),
                       cpu=next(s.split(':', 1)[1].strip() for s in Path('/proc/cpuinfo').read_text().splitlines() if s.startswith('model name')),
                       cpu_affinity=[16], jobs=1, memory_limit='2 GiB per process tree',
                       cache='first lifecycle sample per configuration; shared OS page cache is not flushed',
                       scope='native runtime C ABI, not checked Hirsel sources or either emitter',
                       layout='exact production charge.rs; unused compiler metadata/serde not linked')
    (out / 'environment.json').write_text(json.dumps(environment, indent=2) + '\n')
    for name, path, edition, features, dependencies in targets:
        artifact = libs / f'lib{name}.rlib'
        if artifact.exists():
            continue
        low_load()
        args = [str(ROOT / 'tools/capped'), 'taskset', '-c', '16', rustc, str(path),
                '--crate-name', name, '--crate-type', 'rlib', '--edition', edition,
                *FLAGS, '-L', f'dependency={libs}', '-o', str(artifact)]
        for feature in features:
            args += ['--cfg', f'feature="{feature}"']
        for dep in dependencies:
            args += ['--extern', f'{dep}={libs}/lib{dep}.rlib']
        print('build', name, flush=True)
        with (out / f'build-{name}.log').open('w') as log:
            subprocess.run(args, cwd=ROOT, env=MEASURE_ENV, stdout=log, stderr=subprocess.STDOUT, check=True)
    binary = out / 'tasks-cost'
    if not binary.exists():
        low_load()
        args = [str(ROOT / 'tools/capped'), 'taskset', '-c', '16', rustc,
                str(CRATE / 'measure/tasks.rs'), '--edition', '2024', *FLAGS,
                '-L', f'dependency={libs}', '-o', str(binary)]
        for dep in ['hirsel_core_rt', 'tokio', 'crossbeam_channel', 'libc']:
            args += ['--extern', f'{dep}={libs}/lib{dep}.rlib']
        with (out / 'build-harness.log').open('w') as log:
            subprocess.run(args, cwd=ROOT, env=MEASURE_ENV, stdout=log, stderr=subprocess.STDOUT, check=True)
    return binary


def sample(out, binary, workload, model, count):
    directory = out / f'{workload}-{model}'
    directory.mkdir()
    perf, stats = directory / 'perf.csv', directory / 'time.tsv'
    command = [str(ROOT / 'tools/capped'), 'taskset', '-c', '16', 'perf', 'stat', '-x', ';',
               '-e', 'instructions:u,duration_time', '-o', str(perf), '--',
               '/usr/bin/time', '-f', '%e\t%M', '-o', str(stats), str(binary), model, workload, str(count)]
    started = time.monotonic()
    result = subprocess.run(command, cwd=ROOT, env=MEASURE_ENV, capture_output=True, text=True)
    (directory / 'stdout').write_text(result.stdout)
    (directory / 'stderr').write_text(result.stderr)
    expected = count if workload == 'spawn' else count * (count - 1) // 2 if workload in ('roundtrip', 'pipeline') else count * 1_000_000
    row = dict(workload=workload, model=model, operations=count, status='ok', instructions='', instructions_per_op='',
               seconds='', max_rss_kib='', tree_peak_mib='', wrapper_seconds=f'{time.monotonic()-started:.6f}')
    if result.returncode or result.stdout.strip() != str(expected):
        row['status'] = f'exit {result.returncode}' if result.returncode else 'wrong output'
    if perf.exists():
        for line in perf.read_text().splitlines():
            parts = line.split(';')
            if len(parts) > 2 and parts[0].strip().isdigit():
                if parts[2] == 'instructions:u':
                    row['instructions'] = int(parts[0]); row['instructions_per_op'] = f'{int(parts[0])/count:.2f}'
                elif parts[2] == 'duration_time':
                    row['seconds'] = f'{int(parts[0])/1e9:.6f}'
    if stats.exists():
        fields = stats.read_text().strip().split('\t')
        if len(fields) == 2:
            row['max_rss_kib'] = fields[1]
    peak = re.search(r'capped: peak (\d+) MiB of', result.stderr)
    row['tree_peak_mib'] = peak[1] if peak else ''
    if row['status'] == 'ok' and not row['instructions']:
        row['status'] = 'counter unavailable'
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--models', nargs='+', help='sample only these models; keep existing rows')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    binary = build(args.out)
    rows_path = args.out / 'results.tsv'
    rows = list(csv.DictReader(rows_path.open(), delimiter='\t')) if rows_path.exists() else []
    done = {(r['workload'], r['model']) for r in rows}
    for workload, count, models in CASES:
        for model in models:
            if args.models and model not in args.models:
                continue
            if (workload, model) in done:
                continue
            row = sample(args.out, binary, workload, model, count)
            rows.append(row)
            with rows_path.open('w') as target:
                writer = csv.DictWriter(target, fieldnames=list(row), delimiter='\t'); writer.writeheader(); writer.writerows(rows)
            print(workload, model, row['status'], row['instructions_per_op'], flush=True)
    return int(any(r['status'] != 'ok' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
