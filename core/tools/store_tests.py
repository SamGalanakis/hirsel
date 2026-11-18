#!/usr/bin/env python3
"""Run the store's named S1 laws from checkout sources, on each selected engine."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import tempfile

from case_progress import run_case
from std_native import ROOT


BYTE_LAWS = ('refused_reply',
             'model', 'snapshots', 'reads_ranges', 'call_info', 'resident', 'foreign', 'frames', 'follow', 'node_ids',
             'typed_indexes', 'schema_numbers', 'catalog_invalid', 'index_refusal',
             'typed_foreign', 'drop_recreate', 'typed_resident', 'typed_model', 'close_follow', 'abort_budget',
             'look', 'look_faults', 'changes', 'changes_identity', 'info_mut', 'changes_rewritten_equal',
             'assembly_indexed_recreate', 'read_liveness', 'overwrite_account', 'index_writes', 'read_cover', 'read_spans', 'published_value',
             'frame_size', 'frame_size_boundary', 'stored_size', 'shared_payload_copy', 'branches', 'pin_settle', 'pinned_delete', 'migrate', 'step_bounds', 'streams', 'scopes', 'request_digest', 'keyset_pages')
NATIVE_TESTS = {*('test_' + name for name in BYTE_LAWS if name != 'frame_size_boundary'),
                'test_reader_task', 'test_memory_identity', 'test_typed_snapshot_task', 'test_look_task'}


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
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm', 'llvm-release'])
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = p.parse_args()
    engines = args.backend or ['evaluator', 'cranelift', 'llvm', 'llvm-release']
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-store-laws-') as tmp:
        work = Path(tmp)
        manifest = work / 'inputs.json'
        corpus = inputs(args.root.resolve())
        # Regression laws are ordinary source in this test build. They do not
        # enter the published store module or its measurement manifest.
        library = corpus['packages'][-1]['sources'][0]
        adapted = work / 'store-with-assembly-laws.hirsel'
        adapted.write_text(Path(library['file']).read_text() + '\n' +
                           (args.root / 'core/programs/store/assembly_laws.hirsel').read_text())
        library['file'] = str(adapted)
        manifest.write_text(json.dumps(corpus))
        if args.compiler is not None:
            for control in ('look_put_refused', 'look_info_refused', 'index_rows_escape_refused', 'index_rows_write_refused'):
                negative = inputs(args.root.resolve())
                if control == 'look_info_refused':
                    # Check the fields of an opaque Tx before that change ships.
                    store = negative['packages'][-1]['sources'][0]
                    opaque_tx = work / 'store-with-opaque-tx.hirsel'
                    source = Path(store['file']).read_text()
                    opaque_tx.write_text(source.replace('pub scoped type Tx {', 'pub opaque scoped type Tx {', 1))
                    store['file'] = str(opaque_tx)
                negative['packages'][-1]['sources'].append(dict(
                    file=str(args.root.resolve() / ('core/programs/store/' + control + '.hirsel')),
                    modules=[control], signatures=False))
                rejected = work / (control + '.json')
                rejected.write_text(json.dumps(negative))
                result = run_case(control, 'checker', [str(args.compiler.resolve()), 'check', '--inputs', str(rejected),
                                         '--source-core', '--source-std', '--store', str(work / control),
                                         '--json'], capture_output=True, text=True)
                report = json.loads(result.stdout)
                ds = report.get('diagnostics', [])
                expected = {
                    'index_rows_escape_refused': [('LANG-13', 'scoped[view] fn results with anchors are reserved')],
                    'index_rows_write_refused': [('OWN-10', 'a place of `tx` is viewed')],
                    'look_info_refused': [('OWN-5', '`tx` is not writable'),
                                          ('PROG-2', '`Tx` is opaque outside its module'),
                                          ('PROG-2', '`RequestDigest` is opaque outside its module'),
                                          ('PROG-2', '`RequestDigest` is opaque outside its module')],
                }.get(control, [('OWN-5', '`tx` is not writable')])
                actual = [(d.get('rule'), d.get('message')) for d in ds]
                good = (result.returncode == 1 and report.get('accepted') is False
                        and sorted(actual) == sorted(expected)
                        and not report.get('incomplete') and not report.get('resources'))
                rows.append(dict(engine='checker', law=control, passed=good, result=report))
                print(f'checker {control}: {"PASS" if good else "FAIL"}', flush=True)
        for engine in engines:
            if engine == 'evaluator':
                if args.evaluator is None:
                    p.error('evaluator backend requires --evaluator')
                for name in BYTE_LAWS:
                    result = run_case(name, engine, [str(args.evaluator.resolve()), str(manifest),
                                             'store@1/store/law_' + name], capture_output=True, text=True)
                    try:
                        report = json.loads(result.stdout)
                    except ValueError:
                        report = {'stdout': result.stdout, 'stderr': result.stderr}
                    good = result.returncode == 0 and report.get('outcome') == {'kind': 'returned', 'values': [1]}
                    rows.append(dict(engine=engine, law=name, passed=good, result=report))
                    print(f'{engine} {name}: {"PASS" if good else "FAIL"}; admitted={report.get("used")}'
                          + (f'; {report.get("error")}' if not good else ''), flush=True)
            else:
                if args.compiler is None or args.runtime is None:
                    p.error('native backends require --compiler and --runtime')
                command = [str(args.compiler.resolve()), 'test', '--inputs', str(manifest),
                           '--source-core', '--source-std', '--backend', 'llvm' if engine == 'llvm-release' else engine, '--runtime',
                           str(args.runtime.resolve()), '--store', str(work / engine), '--json',
                           '--filter', 'store@1/store/test_', '--limit', str(64 * 1024 * 1024),
                           '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random', '--grant', 'Stderr']
                if engine == 'llvm-release':
                    command.append('--release')
                if args.cc:
                    command += ['--cc', str(args.cc.resolve())]
                result = run_case('store@1/store/test_', engine, command, capture_output=True, text=True)
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
                # L29's new16MiB vectors keep the stored rows, op list and direct
                # encoder buffers live together. Its128MiB root is separate
                # from the existing laws' unchanged64MiB root.
                boundary = list(command)
                boundary[boundary.index('--filter') + 1] = 'store@1/store/frame_size_boundary'
                boundary[boundary.index('--limit') + 1] = str(128 * 1024 * 1024)
                result = run_case('frame_size_boundary', engine, boundary, capture_output=True, text=True)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = {'stdout': result.stdout, 'stderr': result.stderr}
                tests = report.get('tests', [])
                good = (result.returncode == 0 and len(tests) == 1
                        and tests[0].get('definition') == 'store@1/store/frame_size_boundary'
                        and tests[0].get('passed'))
                rows.append(dict(engine=engine, law='frame_size_boundary', passed=good, result=report))
                print(f'{engine} frame_size_boundary: {"PASS" if good else "FAIL"}', flush=True)
                # Only the concurrent law runs under seeded scheduling. Repeating
                # the serial laws would have no alternative event streams.
                cc = args.cc or (Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
                                 if 'LLVM_SYS_231_PREFIX' in os.environ else Path(shutil.which('cc')))
                support = work / 'schedule.o'
                run_case('follow_schedules:observer', engine, [str(cc), '-c', '-fPIC', str(args.root / 'core/tools/std_schedule.c'),
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
                result = run_case('follow_schedules', engine, scheduled, capture_output=True, text=True)
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
