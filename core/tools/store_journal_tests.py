#!/usr/bin/env python3
"""S3 journal evaluator laws, real W2 cuts, named I/O failures and W4 schedules.

The journal tests themselves run with the rest of the store package under
`hirsel test core/programs/store` (store_tests.py).
"""
import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import tempfile

from std_native import ROOT, Evaluator
from store_tests import crash_inputs
from store_crash_tests import JOURNAL_SCENARIOS as SCENARIOS, checked, late_writes, run_crashes, sector_images, tail_subsets

# Evaluator instructions measured on the unchanged stress law, plus 25% rounded up.
INSTRUCTION_ALLOWANCES = {'frame_size_boundary': 1_892_798_149}  # measured 1_514_238_519

LAWS = ('end', 'payload', 'segment', 'apply', 'recovery', 'bootstrap', 'worker',
        'db', 'end_storage', 'recorded', 'options', 'call_vector', 'recorded_barrier', 'rotation', 'changes_checkpoint', 'frame_size_boundary', 'frame_oversize', 'drop', 'ack_channel', 'recorded_fork', 'streams', 'step_stop', 'place_idle')
TRIGGER_LAWS = ('trigger_revision',)
STEP_LAWS = (
    'steps_reexecution', 'steps_own_rows', 'steps_typed_error', 'steps_fingerprint',
    'steps_in_doubt', 'steps_sealed', 'steps_fence', 'steps_revision',
    'steps_transaction_pair', 'steps_transaction_child',
)
LAW_GROUPS = {
    'store': LAWS,
    'triggers': TRIGGER_LAWS,
    'runner': ('trigger_transact', 'trigger_granted', 'trigger_snapshot', 'trigger_cancel'),
    'steps': STEP_LAWS,
}
FORK_IMAGES = ('journal-fork-ack-cut', 'journal-fork-complete', 'journal-fork-torn', 'journal-fork-bad-end',
               'journal-fork-child-unwritten')
FAILURES = ('poison-segment', 'poison-end', 'poison-pack', 'poison-write', 'poison-cut',
            'orphan-end-recovery-sync', 'journal-place-short')
CADENCE_CUTS = ('journal-group-cadence', 'journal-rotate-cadence')


def provider_checks(executable, engine, env):
    rows = []
    for scenario in FAILURES:
        checked([str(executable), scenario, 'count'], env, engine=engine)
        rows.append(dict(scenario=scenario, passed=True))
    print(f'{engine}: {len(FAILURES)} named I/O failure laws passed', flush=True)
    # BL2: an interrupted Direct or Durable write recovers under the commit law
    # from each sector image it can leave.
    if sector_images(executable, ('journal', 'journal-rotate'), engine, env):
        raise RuntimeError(f'{engine}: sector images broke the commit law or were vacuous')
    if tail_subsets(executable, 'journal-tail', engine, env):
        raise RuntimeError(f'{engine}: a tail-block sector subset broke the commit law or none was wide')
    rows.append(dict(scenario='sector-images', passed=True))
    # Drift guard 4: syncs and END opens per §3.2, checked by store_crash.c.
    checked([str(executable), 'journal-sync-count', 'count'], env, engine=engine)
    rows.append(dict(scenario='journal-sync-count', passed=True))
    count = checked([str(executable), 'journal-cancel', 'count'], env, engine=engine)
    match = re.fullmatch(r'cuts ([0-9]+)\n', count.stdout)
    if match is None or int(match[1]) == 0:
        raise RuntimeError('cancellation inventory executed no wait entries')
    waits = int(match[1])
    for cut in range(waits):
        checked([str(executable), 'journal-cancel', str(cut)], env, engine=engine)
    rows.append(dict(cancellation_wait_entries=waits, passed=True))
    print(f'{engine}: {waits}/{waits} cancellation wait entries passed', flush=True)
    # The journal worker is cancelled while its issued sync is in flight; the
    # head must end the place as Broken, never unwrap the closed ack channel.
    count = checked([str(executable), 'journal-worker-cancel', 'count'], env, engine=engine)
    match = re.fullmatch(r'cuts ([0-9]+)\n', count.stdout)
    if match is None or int(match[1]) == 0:
        raise RuntimeError('journal worker issued no sync to cancel across')
    issued = int(match[1])
    for cut in range(issued):
        checked([str(executable), 'journal-worker-cancel', str(cut)], env, engine=engine)
    rows.append(dict(scenario='journal-worker-cancel', issued_syncs=issued, passed=True))
    print(f'{engine}: {issued}/{issued} journal worker cancels across an issued sync passed', flush=True)
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
        manifest.write_text(json.dumps(crash_inputs(root)))
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
                        result = evaluator.evaluate(manifest, f'{"store" if module == "store" else "runner"}@1/{module}/journal_law_{name}',
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
            try:
                # run_crashes builds with the same native toolchain, then executes
                # the immutable binary once at every W2 mutation cut.
                cuts = run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc,
                                   manifest, work, engine, entry='store@1/store/journal_crash_main',
                                   scenarios=(*SCENARIOS, 'journal-keyed', *CADENCE_CUTS), environment=env,
                                   count_only=('orphan-end', *FORK_IMAGES))
                rows.append(dict(engine=engine, passed=True, crashes=cuts))
                late = late_writes(work / (engine + '-crash') / 'store', engine, env)
                rows.append(dict(engine=engine, passed=True, late_writes=late))
                checks = provider_checks(work / (engine + '-crash') / 'store', engine, env)
                rows.append(dict(engine=engine, passed=True, providers=checks))
                step_work = work / 'steps'
                step_work.mkdir(exist_ok=True)
                step_cuts = run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc,
                                       manifest, step_work, engine, entry='runner@1/steps/journal_steps_main',
                                       scenarios=(), environment=env, count_only=('poison-segment', 'steps-park'))
                rows.append(dict(engine=engine, passed=True, step_cuts=step_cuts))
                # A platform host turn: answered implies durable at every cut, and a
                # failed sync answers nothing and leaves no decision after restart.
                platform_work = work / 'platform'
                platform_work.mkdir(exist_ok=True)
                platform_cuts = run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc,
                                           manifest, platform_work, engine, entry='hirsel.ai/store_crash@1/platform_crash/platform_crash_main',
                                           scenarios=('platform-turn',), environment=env, count_only=('poison-segment',))
                rows.append(dict(engine=engine, passed=True, platform_cuts=platform_cuts))
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
