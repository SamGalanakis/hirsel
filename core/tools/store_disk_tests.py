#!/usr/bin/env python3
"""Run the store's S2 laws from checkout sources with exact body selection."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from std_native import ROOT, wait_load
from store_tests import inputs
from store_crash_tests import run_crashes

LAWS = ('frames', 'manifest', 'nodes', 'graph', 'packs', 'load', 'compact',
        'draft_refusal', 'worker_refusal', 'opening', 'db', 'db_refusal', 'schema_refusal', 'publish_refusal')
NATIVE_TESTS = {'store@1/store/disk_test_' + name for name in LAWS}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--evaluator', type=Path)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--bridge', type=Path, default=os.environ.get('HIRSEL_STORE_CRASH_BRIDGE'))
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm', 'llvm-release'])
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = p.parse_args()
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-store-disk-laws-') as tmp:
        work = Path(tmp)
        manifest = work / 'inputs.json'
        manifest.write_text(json.dumps(inputs(args.root.resolve())))
        for engine in args.backend or ['evaluator', 'cranelift', 'llvm', 'llvm-release']:
            if engine == 'evaluator':
                if args.evaluator is None:
                    p.error('evaluator backend requires --evaluator')
                for name in LAWS:
                    result = subprocess.run([str(args.evaluator.resolve()), str(manifest),
                                             'store@1/store/disk_law_' + name], capture_output=True, text=True)
                    try:
                        report = json.loads(result.stdout)
                    except ValueError:
                        report = {'stdout': result.stdout, 'stderr': result.stderr}
                    good = result.returncode == 0 and report.get('outcome') == {'kind': 'returned', 'values': [1]}
                    rows.append(dict(engine=engine, law=name, passed=good, result=report))
                    print(f'{engine} {name}: {"PASS" if good else "FAIL"}', flush=True)
            else:
                if args.compiler is None or args.runtime is None or args.bridge is None:
                    p.error('native backends require --compiler, --runtime and --bridge')
                host = work / (engine + '-host')
                host.mkdir()
                command = [str(args.compiler.resolve()), 'test', '--inputs', str(manifest),
                           '--source-core', '--source-std', '--backend', 'llvm' if engine == 'llvm-release' else engine,
                           '--runtime', str(args.runtime.resolve()), '--store', str(work / engine), '--json',
                           '--filter', 'store@1/store/disk_test_', '--dir', str(host), '--limit', str(64 * 1024 * 1024),
                           '--grant', 'Clock', '--grant', 'Random', '--grant', 'Tasks', '--grant', 'Stderr']
                if engine == 'llvm-release':
                    command.append('--release')
                if args.cc:
                    command += ['--cc', str(args.cc.resolve())]
                wait_load()
                result = subprocess.run(command, capture_output=True, text=True)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = {'stdout': result.stdout, 'stderr': result.stderr}
                tests = report.get('tests', [])
                good = (result.returncode == 0 and len(tests) == len(NATIVE_TESTS)
                        and {t['definition'] for t in tests} == NATIVE_TESTS and all(t.get('passed') for t in tests))
                rows.append(dict(engine=engine, passed=good, result=report))
                print(f'{engine}: {sum(bool(t.get("passed")) for t in tests)}/{len(NATIVE_TESTS)} passed', flush=True)
                try:
                    cuts = run_crashes(args.root.resolve(), args.compiler, args.runtime, args.bridge,
                                       args.cc, manifest, work, engine)
                    rows.append(dict(engine=engine, passed=True, crashes=cuts))
                except RuntimeError as error:
                    rows.append(dict(engine=engine, passed=False, crash_failure=str(error)))
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    failures = [row for row in rows if not row['passed']]
    if failures:
        print(json.dumps(failures, indent=2))
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
