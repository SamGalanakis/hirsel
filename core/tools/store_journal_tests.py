#!/usr/bin/env python3
"""S3 journal laws, real W2 cuts, named I/O failures and W4 schedules."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import tempfile

from case_progress import run_case
from std_native import ROOT, Evaluator
from store_tests import inputs
from store_crash_tests import checked, run_crashes

# Evaluator instructions measured on the unchanged stress law, plus 25% rounded up.
INSTRUCTION_ALLOWANCES = {'frame_size_boundary': 1_892_798_149}  # measured 1_514_238_519

LAWS = ('end', 'payload', 'segment', 'apply', 'recovery', 'bootstrap', 'worker',
        'db', 'end_storage', 'recorded', 'options', 'call_vector', 'recorded_barrier', 'rotation', 'changes_checkpoint', 'frame_size_boundary', 'frame_oversize', 'drop', 'ack_channel', 'recorded_fork', 'streams', 'step_stop', 'place', 'place_idle')
TRIGGER_LAWS = ('trigger_revision',)
LAW_GROUPS = {'store': LAWS, 'triggers': TRIGGER_LAWS}
NATIVE_GROUPS = {
    module: {f'store@1/{module}/journal_test_{name}' for name in group
             if name not in ('frame_size_boundary', 'frame_oversize')}
    for module, group in LAW_GROUPS.items()
}
SCENARIOS = ('journal', 'journal-group', 'journal-rotate', 'journal-place', 'journal-place-split', 'journal-place-idle')
FAILURES = ('poison-segment', 'poison-end', 'poison-pack', 'poison-write', 'poison-cut',
            'orphan-end-recovery-sync')


def provider_checks(executable, engine, env):
    rows = []
    for scenario in FAILURES:
        checked([str(executable), scenario, 'count'], env, engine=engine)
        rows.append(dict(scenario=scenario, passed=True))
    print(f'{engine}: {len(FAILURES)} named I/O failure laws passed', flush=True)
    count = checked([str(executable), 'journal-cancel', 'count'], env, engine=engine)
    match = re.fullmatch(r'cuts ([0-9]+)\n', count.stdout)
    if match is None or int(match[1]) == 0:
        raise RuntimeError('cancellation inventory executed no wait entries')
    waits = int(match[1])
    for cut in range(waits):
        checked([str(executable), 'journal-cancel', str(cut)], env, engine=engine)
    rows.append(dict(cancellation_wait_entries=waits, passed=True))
    print(f'{engine}: {waits}/{waits} cancellation wait entries passed', flush=True)
    # L24 (iv)(a): syncs held for one writer turn; every schedule must finish.
    checked([str(executable), 'journal-ack', 'count'], env, engine=engine)
    for seed in range(1, 33):
        checked([str(executable), 'journal-ack', 'count', str(seed)], env, engine=engine)
    rows.append(dict(scenario='journal-ack', schedules=33, passed=True))
    print(f'{engine}: journal-ack held syncs passed 33 schedules', flush=True)
    for scenario in ('journal-group', 'journal-rotate'):
        hashes = set()
        for seed in range(1, 33):
            result = checked([str(executable), scenario, 'count', str(seed)], env, engine=engine)
            match = re.fullmatch(r'cuts [0-9]+\nevents ([0-9]+)\n', result.stdout)
            if match is None:
                raise RuntimeError(f'schedule {seed} emitted no event hash: {result.stdout!r}')
            hashes.add(match[1])
        if len(hashes) < 2:
            raise RuntimeError(f'{scenario}: all schedule seeds produced one event stream')
        rows.append(dict(scenario=scenario, schedules=32, distinct_event_streams=len(hashes), passed=True))
        print(f'{engine}: {scenario} 32 schedules passed, {len(hashes)} event streams', flush=True)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--evaluator', type=Path)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--bridge', type=Path, default=os.environ.get('HIRSEL_STORE_CRASH_BRIDGE'))
    parser.add_argument('--cc', type=Path)
    parser.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm', 'llvm-release'])
    parser.add_argument('--report', type=Path)
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    root = args.root.resolve()
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-store-journal-') as tmp, contextlib.ExitStack() as resources:
        work = Path(tmp)
        manifest = work / 'inputs.json'
        manifest.write_text(json.dumps(inputs(root)))
        env = dict(os.environ, XDG_DATA_HOME=str(work / 'data'), XDG_CACHE_HOME=str(work / 'cache'))
        libraries = [str(root / 'tools/buck2/toolchains/llvm-files/lib')]
        if args.bridge:
            libraries.insert(0, str(args.bridge.resolve().parent))
        env['LD_LIBRARY_PATH'] = ':'.join(libraries + [env.get('LD_LIBRARY_PATH', '')])
        evaluator = resources.enter_context(Evaluator(args.evaluator.resolve())) if args.evaluator else None
        for engine in args.backend or ['evaluator', 'cranelift', 'llvm', 'llvm-release']:
            if engine == 'evaluator':
                if args.evaluator is None:
                    parser.error('evaluator backend requires --evaluator')
                for module, group in LAW_GROUPS.items():
                    for name in group:
                        result = evaluator.evaluate(manifest, f'store@1/{module}/journal_law_{name}',
                                                    instructions=INSTRUCTION_ALLOWANCES.get(name))
                        try:
                            report = json.loads(result.stdout)
                        except ValueError:
                            report = dict(stdout=result.stdout.decode(errors='replace'), stderr=result.stderr.decode(errors='replace'))
                        good = result.returncode == 0 and report.get('outcome') == dict(kind='returned', values=[1])
                        rows.append(dict(engine=engine, law=name, passed=good, result=report))
                        print(f'{engine} {name}: {"PASS" if good else "FAIL"}', flush=True)
                continue
            if args.compiler is None or args.runtime is None or args.bridge is None:
                parser.error('native backends require compiler, runtime and bridge')
            host = work / (engine + '-host')
            host.mkdir()
            command = [str(args.compiler.resolve()), 'test', '--inputs', str(manifest),
                       '--source-core', '--source-std', '--backend', 'llvm' if engine == 'llvm-release' else engine,
                       '--runtime', str(args.runtime.resolve()), '--store', str(work / engine), '--json',
                       '--filter', 'store@1/store/journal_test_', '--dir', str(host), '--limit', str(64 * 1024 * 1024)]
            for grant in ('Clock', 'Random', 'Tasks'):
                command += ['--grant', grant]
            if engine == 'llvm-release':
                command.append('--release')
            if args.cc:
                command += ['--cc', str(args.cc.resolve())]
            for module, expected in NATIVE_GROUPS.items():
                prefix = f'store@1/{module}/journal_test_'
                selected = list(command)
                selected[selected.index('--filter') + 1] = prefix
                result = run_case(prefix, engine, selected, capture_output=True, text=True, env=env)
                try:
                    report = json.loads(result.stdout)
                except ValueError:
                    report = dict(stdout=result.stdout, stderr=result.stderr)
                tests = report.get('tests', [])
                good = (result.returncode == 0 and len(tests) == len(expected)
                        and {test['definition'] for test in tests} == expected
                        and all(test.get('passed') for test in tests))
                rows.append(dict(engine=engine, module=module, passed=good, result=report))
                print(f'{engine} {module}: {sum(bool(t.get("passed")) for t in tests)}/{len(expected)} passed', flush=True)
            large = list(command)
            large[large.index('--filter') + 1] = 'store@1/store/journal_frame_'
            # L29 simultaneously retains sixteen MiB of rows and the maximum
            # frame, plus replay buffers and the two reserved task roots.
            large[large.index('--limit') + 1] = str(512 * 1024 * 1024)
            result = run_case('store@1/store/journal_frame_', engine, large, capture_output=True, text=True, env=env)
            try:
                report = json.loads(result.stdout)
            except ValueError:
                report = dict(stdout=result.stdout, stderr=result.stderr)
            tests = report.get('tests', [])
            good = (result.returncode == 0 and len(tests) == 2
                    and {t.get('definition') for t in tests} == {'store@1/store/journal_frame_size_boundary', 'store@1/store/journal_frame_oversize'}
                    and all(t.get('passed') is True for t in tests))
            rows.append(dict(engine=engine, law='frame_size_boundary', passed=good, result=report))
            print(f'{engine} frame_size_boundary: {"PASS" if good else "FAIL"}', flush=True)
            try:
                # run_crashes builds with the same native toolchain, then executes
                # the immutable binary once at every W2 mutation cut.
                cuts = run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc,
                                   manifest, work, engine, entry='store@1/store/journal_crash_main',
                                   scenarios=SCENARIOS, environment=env, count_only=('orphan-end',))
                rows.append(dict(engine=engine, passed=True, crashes=cuts))
                checks = provider_checks(work / (engine + '-crash') / 'store', engine, env)
                rows.append(dict(engine=engine, passed=True, providers=checks))
            except RuntimeError as error:
                rows.append(dict(engine=engine, passed=False, provider_failure=str(error)))
        if args.report:
            args.report.write_text(json.dumps(rows, indent=2) + '\n')
    failed = [row for row in rows if not row['passed']]
    if failed:
        print(json.dumps(failed, indent=2))
    return int(bool(failed))


if __name__ == '__main__':
    raise SystemExit(main())
