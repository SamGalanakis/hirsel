#!/usr/bin/env python3
"""Run the store package's tests and the store's S1 laws, on each selected engine.

Natively, `hirsel test core/programs/store` runs every test of the package under
the run configuration its manifest declares (PROG-4). What it cannot express
stays here: the evaluator laws, the checker's refusal controls, and
follow_schedules across 32 seeded schedules, which links a scheduling observer
through --cc.
"""
import argparse
import json
import os
import sys
from pathlib import Path
import shlex
import shutil
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/toolchain-dist'))
import hcfg

from case_progress import run_case
from std_native import ROOT, library_sources


BYTE_LAWS = ('refused_reply',
             'model', 'snapshots', 'reads_ranges', 'call_info', 'resident', 'foreign', 'frames', 'follow', 'node_ids',
             'typed_indexes', 'schema_numbers', 'catalog_invalid', 'index_refusal', 'put_validation',
             'typed_foreign', 'drop_recreate', 'typed_resident', 'typed_model', 'close_follow', 'abort_budget',
             'look', 'look_faults', 'changes', 'shared_changes', 'changes_identity', 'info_mut', 'changes_rewritten_equal',
             'assembly_indexed_recreate', 'read_liveness', 'overwrite_account', 'index_writes', 'read_cover', 'read_spans', 'range_prefix', 'published_value',
             'frame_size', 'stored_size', 'shared_payload_copy', 'branches', 'pin_settle',
             'fork_shared_head', 'fork_old_snapshot', 'fork_settle', 'pinned_delete', 'migrate', 'step_bounds',
             'streams', 'request_digest', 'keyset_pages')


# package_content::TOOLCHAIN in hirsel-core-driver, std aside.
TOOLCHAIN_PACKAGES = {'store', 'runner', 'media_bits', 'sframe', 'media_container', 'rtp', 'srtp', 'dtls',
                      'stun_ice', 'turn', 'webrtc_sfu', 'rtp_jitter', 'media_record', 'platform', 'search',
                      'mcp', 'cli'}


def inputs(root, tests=False, package='store', with_tests=()):
    """Core, std and a package of core/programs with the program packages it binds by path,
    each before the packages that bind it, the selected package last; `tests` adds the
    selected package's test sources, and `with_tests` names the bound packages whose test
    sources are added too. `package` names the directory (store, runner, platform,
    store-crash)."""
    packages = []
    for name in ('core', 'std'):
        packages.append({
            'package': {'name': name, 'version': '1'},
            'dependencies': [] if name == 'core' else [{'name': 'core', 'version': '1'}],
            'sources': library_sources(root / 'core' / name, name == 'core')})
    placed = set()

    def place(directory, selected):
        if directory in placed:
            return
        placed.add(directory)
        manifest = hcfg.package(directory)
        for dep in manifest['dependencies'].values():
            if 'path' in dep:
                place((directory / dep['path']).resolve(), False)
        listed = dict(manifest['sources'])
        if (tests and selected) or manifest['name'] in with_tests:
            listed.update(manifest.get('tests', {}).get('sources', {}))
        # A CorpusManifest binds a dependency by package, and its source names that
        # package by the last `/` component; it carries no hcfg aliases.
        for alias, dep in manifest['dependencies'].items():
            if alias != dep['package'].rsplit('/', 1)[-1]:
                raise ValueError(f"{manifest['name']}: --inputs cannot carry alias {alias} for {dep['package']}")
        packages.append({
            'package': {'name': manifest['name'], 'version': str(manifest['version'].split('.')[0])},
            'dependencies': [{'name': 'core', 'version': '1'}] +
                            [{'name': dep['package'], 'version': str(dep['major'])}
                             for dep in manifest['dependencies'].values()],
            'sources': [{'file': str(directory / file), 'modules': module.split('.'), 'signatures': False}
                        for file, module in listed.items()],
            # These are the toolchain's own package sources, which the installed toolchain
            # supplies: only a package so marked is the toolchain's runner (launch.md).
            'toolchain': manifest['name'] in TOOLCHAIN_PACKAGES,
        })

    place((root / 'core/programs' / package).resolve(), True)
    return {'core': {'name': 'core', 'version': '1'}, 'packages': packages}


def runner_inputs(root):
    """The runner package with its test sources, over the store package with its own:
    the runner laws bind store law helpers."""
    return inputs(root, tests=True, package='runner', with_tests=('store',))


def crash_inputs(root):
    """The store_crash package over the store and runner packages with their test sources,
    which hold the crash harnesses' laws and entries."""
    return inputs(root, package='store-crash', with_tests=('store', 'runner'))


DEPENDENT_CONTROLS = ('journal_input_refused', 'holder_table_refused', 'declare_state_refused',
                      'pack_disk_state_refused', 'pager_state_refused', 'disk_pager_state_refused',
                      'record_retention_refused', 'dir_journal_state_refused',
                      'history_register_refused', 'history_locator_refused', 'history_open_base_refused',
                      'history_pack_stream_refused', 'prepared_register_head_refused',
                      'prepared_capture_state_refused', 'capture_lock_refused', 'capture_packs_refused',
                      'stream_settle_view_refused', 'stream_follow_material_refused',
                      'stream_hold_guard_refused', 'stream_claim_machine_refused',
                      'issued_register_head_refused', 'object_writer_state_refused',
                      'object_worker_open_refused', 'object_cut_published_refused',
                      'object_journal_start_refused', 'object_journal_session_refused',
                      'index_owner_refused', 'index_bindings_refused', 'index_registry_refused',
                      'index_registration_refused', 'registered_index_refused',
                      'reader_state_refused', 'read_only_state_refused', 'entry_cursor_refused',
                      'i8_row_escape_refused', 'tx_state_refused', 'transfer_state_refused', 'index_walk_refused')
CHECKER_CONTROLS = ('look_put_refused', 'look_info_refused', 'index_rows_escape_refused',
                    'index_rows_write_refused', 'completion_unreachable',
                    'view_tx_write_refused', *DEPENDENT_CONTROLS)


def checker_controls(root, compiler, work, controls=CHECKER_CONTROLS):
    rows = []
    for control in controls:
        negative = inputs(root)
        source = dict(
            file=str(root / ('core/programs/store/' + control + '.hirsel')),
            modules=[control], signatures=False)
        if control in DEPENDENT_CONTROLS:
            dependencies = ['core', 'store']
            if control in ('history_register_refused', 'history_pack_stream_refused', 'stream_settle_view_refused',
                           'stream_hold_guard_refused', 'issued_register_head_refused'):
                dependencies.append('std')
            negative['packages'].append({
                'package': {'name': control, 'version': '1'},
                'dependencies': [{'name': name, 'version': '1'} for name in dependencies],
                'sources': [source],
            })
        else:
            negative['packages'][-1]['sources'].append(source)
        rejected = work / (control + '.json')
        rejected.write_text(json.dumps(negative))
        result = run_case(control, 'checker', [str(compiler), 'check', '--inputs', str(rejected),
                                 '--source-core', '--source-std', '--store', str(work / control),
                                 '--json'], capture_output=True, text=True)
        report = json.loads(result.stdout)
        ds = report.get('diagnostics', [])
        expected = {
            'completion_unreachable': [('PROG-2', '`store@1/store/Completion` is private or opaque')],
            'journal_input_refused': [('PROG-2', '`store@1/format/JournalInput` is private or opaque')],
            'holder_table_refused': [('PROG-2', '`store@1/pages/HolderTable` is private or opaque')],
            'pack_disk_state_refused': [('PROG-2', '`store@1/manifest/PackDiskState` is private or opaque')],
            'pager_state_refused': [('PROG-2', '`store@1/pager/PagerState` is private or opaque')],
            'disk_pager_state_refused': [('PROG-2', '`store@1/pager/DiskPagerState` is private or opaque')],
            'record_retention_refused': [('PROG-2', '`store@1/journal/RecordRetention` is private or opaque')],
            'dir_journal_state_refused': [('PROG-2', '`store@1/journal/DirJournalState` is private or opaque')],
            'history_register_refused': [('PROG-2', '`store@1/history/HistoryRegister` is private or opaque')],
            'history_locator_refused': [('PROG-2', '`store@1/history/HistoryLocator` is private or opaque')],
            'history_open_base_refused': [('PROG-2', '`store@1/history/HistoryOpenBase` is private or opaque')],
            'history_pack_stream_refused': [('PROG-2', '`store@1/history/HistoryPackStream` is private or opaque')],
            'prepared_register_head_refused': [('PROG-2', '`store@1/history/PreparedRegisterHead` is private or opaque')],
            'prepared_capture_state_refused': [('PROG-2', '`store@1/capture/PreparedState` is private or opaque')],
            'capture_lock_refused': [('PROG-2', '`store@1/capture/CaptureLock` is private or opaque')],
            'capture_packs_refused': [('PROG-2', '`store@1/capture/CapturePacks` is private or opaque')],
            'stream_settle_view_refused': [('PROG-2', '`store@1/host_stream/SettleView` is private or opaque')],
            'stream_follow_material_refused': [('PROG-2', '`store@1/host_stream/StreamFollowMaterial` is private or opaque')],
            'stream_hold_guard_refused': [('PROG-2', '`store@1/host_stream/HoldGuard` is private or opaque')],
            'stream_claim_machine_refused': [('PROG-2', '`store@1/host_stream/ClaimMachine` is private or opaque')],
            'issued_register_head_refused': [('PROG-2', '`store@1/history/IssuedRegisterHead` is private or opaque')],
            'object_writer_state_refused': [('PROG-2', '`store@1/object_writer/ObjectWriterState` is private or opaque')],
            'object_worker_open_refused': [('PROG-2', '`store@1/object_writer/ObjectWorkerOpen` is private or opaque')],
            'object_cut_published_refused': [('PROG-2', '`store@1/object_writer/PublishedCut` is private or opaque')],
            'object_journal_start_refused': [('PROG-2', '`store@1/object_writer/ObjectJournalStart` is private or opaque')],
            'object_journal_session_refused': [('PROG-2', '`store@1/object_writer/ObjectJournalSession` is private or opaque')],
            'index_owner_refused': [('PROG-2', '`store@1/indexes/IndexOwner` is private or opaque')],
            'index_bindings_refused': [('PROG-2', '`store@1/indexes/IndexBindings` is private or opaque')],
            'index_registry_refused': [('PROG-2', '`store@1/indexes/IndexRegistry` is private or opaque')],
            'index_registration_refused': [('PROG-2', '`store@1/indexes/IndexRegistration` is private or opaque')],
            'registered_index_refused': [('PROG-2', '`store@1/indexes/RegisteredIndex` is private or opaque')],
            'reader_state_refused': [('PROG-2', '`store@1/read/ReaderState` is private or opaque')],
            'tx_state_refused': [('PROG-2', '`store@1/txn/TxState` is private or opaque')],
            'transfer_state_refused': [('PROG-2', '`store@1/transfer/TransferState` is private or opaque')],
            'index_walk_refused': [('PROG-2', '`store@1/maintenance/IndexWalk` is private or opaque')],
            'read_only_state_refused': [('PROG-2', '`store@1/read/ReadOnlyState` is private or opaque')],
            'entry_cursor_refused': [('PROG-2', '`store@1/read/EntryCursor` is private or opaque')],
            'i8_row_escape_refused': [('LANG-13', 'scoped[view] fn results with anchors are reserved')],
            'declare_state_refused': [('PROG-2', '`store@1/catalog/DeclareState` is private or opaque')],
            'index_rows_escape_refused': [('LANG-13', 'scoped[view] fn results with anchors are reserved')],
            'index_rows_write_refused': [('OWN-10', '`tx` is viewed')],
            'view_tx_write_refused': [('OWN-10', '`tx` is viewed')],
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
        if (work / control).is_dir():
            shutil.rmtree(work / control)
        print(f'checker {control}: {"PASS" if good else "FAIL"}', flush=True)
    return rows


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--evaluator', type=Path)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--schedule-runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_SCRIPTED_ARCHIVE'))
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
        manifest.write_text(json.dumps(inputs(args.root.resolve(), tests=True)))
        if args.compiler is not None:
            rows.extend(checker_controls(args.root.resolve(), args.compiler.resolve(), work))
        for engine in engines:
            if engine == 'evaluator':
                if args.evaluator is None:
                    p.error('evaluator backend requires --evaluator')
                for name in BYTE_LAWS:
                    command = [str(args.evaluator.resolve()), str(manifest), 'store@1/store/law_' + name]
                    result = run_case(name, engine, command, capture_output=True, text=True)
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
                package = [str(args.compiler.resolve()), 'test', str(args.root.resolve() / 'core/programs/store'),
                           '--libraries', str(args.root.resolve() / 'core'), '--source-core', '--source-std',
                           '--backend', 'llvm' if engine == 'llvm-release' else engine,
                           '--store', str(work / engine), '--json']
                if engine == 'llvm-release':
                    package.append('--release')
                if args.cc:
                    package += ['--cc', str(args.cc.resolve())]
                result = run_case('store package', engine, package + ['--runtime', str(args.runtime.resolve())],
                                  capture_output=True, text=True)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = {'stdout': result.stdout, 'stderr': result.stderr}
                tests = report.get('tests', [])
                good = result.returncode == 0 and report.get('passed') is True
                rows.append(dict(engine=engine, passed=good, result=report))
                print(f'{engine}: {sum(bool(t.get("passed")) for t in tests)}/{len(tests)} passed', flush=True)
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
                if args.schedule_runtime is None:
                    p.error('follow_schedules requires --schedule-runtime')
                scheduled = [*package, '--runtime', str(args.schedule_runtime.resolve()),
                             '--filter', 'store@1/store/follow_schedules']
                if '--cc' in scheduled:
                    at = scheduled.index('--cc')
                    del scheduled[at:at + 2]
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
                if (work / engine).is_dir():
                    shutil.rmtree(work / engine)
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    failures = [row for row in rows if row['passed'] is False]
    if failures:
        print(json.dumps(failures, indent=2))
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
