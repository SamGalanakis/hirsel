#!/usr/bin/env python3
"""Run the store's named S1 laws from checkout sources, on each selected engine."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile

from std_native import ROOT, wait_load


BYTE_LAWS = ('model', 'snapshots', 'reads_ranges', 'call_info', 'resident', 'foreign', 'frames', 'follow', 'node_ids',
             'typed_indexes', 'schema_numbers', 'catalog_invalid', 'index_refusal',
             'typed_foreign', 'drop_recreate', 'typed_resident', 'typed_model', 'close_follow', 'abort_budget')
NATIVE_TESTS = {*('test_' + name for name in BYTE_LAWS), 'test_reader_task', 'test_memory_identity', 'test_typed_snapshot_task'}


def inputs(root):
    packages = []
    for name in ('core', 'std'):
        packages.append({
            'package': {'name': name, 'version': '1'},
            'dependencies': [] if name == 'core' else [{'name': 'core', 'version': '1'}],
            'sources': [{'file': str(p), 'modules': [p.stem], 'signatures': name == 'core'}
                        for p in sorted((root / 'core' / name).glob('*.hirsel'))]})
    store = json.loads((root / 'core/programs/store/manifest.json').read_text())['packages'][0]
    for source in store['sources']:
        source['file'] = str(root / source['file'])
    return {'core': {'name': 'core', 'version': '1'}, 'packages': packages + [store]}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--evaluator', type=Path)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm'])
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = p.parse_args()
    engines = args.backend or ['evaluator', 'cranelift', 'llvm']
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-store-laws-') as tmp:
        work = Path(tmp)
        manifest = work / 'inputs.json'
        manifest.write_text(json.dumps(inputs(args.root.resolve())))
        for engine in engines:
            if engine == 'evaluator':
                if args.evaluator is None:
                    p.error('evaluator backend requires --evaluator')
                for name in BYTE_LAWS:
                    result = subprocess.run([str(args.evaluator.resolve()), str(manifest),
                                             'store@1/store/law_' + name], capture_output=True, text=True)
                    try:
                        report = json.loads(result.stdout)
                    except ValueError:
                        report = {'stdout': result.stdout, 'stderr': result.stderr}
                    good = result.returncode == 0 and report.get('outcome') == {'kind': 'returned', 'values': [1]}
                    # Orchestrator's 2026-11-16 ruling: these assembly failures
                    # belong to 914 and ride S1. Execute every eligible law;
                    # distinguish blocked assembly from an executed answer.
                    error = report.get('error', '')
                    blocked = (name in {'schema_numbers', 'drop_recreate', 'typed_resident', 'typed_model'}
                               and (error == 'generic source body is incomplete'
                                    or re.fullmatch(r'not well formed: f\d+: b\d+: direct call through another signature', error)))
                    rows.append(dict(engine=engine, law=name, passed=True if good else None if blocked else False,
                                     blocked='core-ops-914' if blocked else None, result=report))
                    status = 'PASS' if good else 'BLOCKED by evaluator assembly (914)' if blocked else 'FAIL'
                    print(f'{engine} {name}: {status}; admitted={report.get("used")}'
                          + (f'; {error}' if blocked else ''), flush=True)
            else:
                if args.compiler is None or args.runtime is None:
                    p.error('native backends require --compiler and --runtime')
                command = [str(args.compiler.resolve()), 'test', '--inputs', str(manifest),
                           '--source-core', '--source-std', '--backend', engine, '--runtime',
                           str(args.runtime.resolve()), '--store', str(work / engine), '--json',
                           '--filter', 'store@1/store/test_', '--limit', str(64 * 1024 * 1024),
                           '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random', '--grant', 'Stderr']
                if args.cc:
                    command += ['--cc', str(args.cc.resolve())]
                wait_load()
                result = subprocess.run(command, capture_output=True, text=True)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = {'stdout': result.stdout, 'stderr': result.stderr}
                tests = report.get('tests', [])
                selected = {t['definition'].removeprefix('store@1/store/') for t in tests}
                good = (result.returncode == 0 and selected == NATIVE_TESTS
                        and len(tests) == len(NATIVE_TESTS) and all(t.get('passed') for t in tests))
                rows.append(dict(engine=engine, passed=good, result=report))
                print(f'{engine}: {sum(bool(t.get("passed")) for t in tests)}/{len(NATIVE_TESTS)} passed', flush=True)
                # Only the concurrent law runs under seeded scheduling. Repeating
                # the serial laws would have no alternative event streams.
                cc = args.cc or (Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
                                 if 'LLVM_SYS_231_PREFIX' in os.environ else Path(shutil.which('cc')))
                support = work / 'schedule.o'
                subprocess.run([str(cc), '-c', '-fPIC', str(args.root / 'core/tools/std_schedule.c'),
                                '-I' + str(args.root / 'crates/hirsel-core-rt/include'), '-DSEEDS=32',
                                '-o', str(support)], check=True)
                wrapper = work / 'scheduled-cc'
                wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc)) + ' "$@" '
                                   + shlex.quote(str(support)) + ' -Wl,--wrap=hirsel_core_os_run_with_sources\n')
                wrapper.chmod(0o755)
                scheduled = list(command)
                if '--cc' in scheduled:
                    at = scheduled.index('--cc')
                    del scheduled[at:at + 2]
                scheduled[scheduled.index('--filter') + 1] = 'store@1/store/follow_schedules'
                scheduled += ['--cc', str(wrapper)]
                result = subprocess.run(scheduled, capture_output=True, text=True)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = {'stdout': result.stdout, 'stderr': result.stderr}
                tests = report.get('tests', [])
                good = (result.returncode == 0 and len(tests) == 1
                        and tests[0].get('definition') == 'store@1/store/follow_schedules'
                        and tests[0].get('passed'))
                rows.append(dict(engine=engine, schedules=32, passed=good, result=report))
                print(f'{engine}: follow across 32 schedules {"PASS" if good else "FAIL"}', flush=True)
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    failures = [row for row in rows if row['passed'] is False]
    if failures:
        print(json.dumps(failures, indent=2))
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
