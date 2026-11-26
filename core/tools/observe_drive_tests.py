#!/usr/bin/env python3
"""Run 64 bounded client histories through the real runner and the trace observers."""
import argparse
import json
from pathlib import Path
import sys
import tempfile

from std_native import ROOT
from runner_tests import evaluate, manifest
from observe_tests import Codec, CLAUSES, check_trace, shown_key, trace


MIXES = ('Correct', 'ReuseWhileLive', 'RecoverNewBytes', 'NoDirectory', 'AdvanceOnRefusal')
POSITIONS = ('H', 'I', 'M', 'HU', 'AH2')
SESSION = ('W3', 'W4', 'W5')


def seeded_inputs(root, work, seeds, mix=None):
    source = work / 'observe_seeds.hirsel'
    source.write_text('\n'.join(
        f'pub fn seed_{seed}(out: Stdout, err: Stderr, clock: Clock, random: Random, tasks: Tasks, net: Net) -> bool waits {{\n'
        f'    observe_drive.run(out, err, clock, random, tasks, net, {seed}u64, {mix or MIXES[seed % len(MIXES)]})\n}}\n'
        for seed in seeds))
    return manifest(root, work, 'drive', extra=[(source, 'observe_seeds')])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--seed', type=int, action='append')
    parser.add_argument('--mix', choices=MIXES)
    parser.add_argument('--artifacts', type=Path, default=Path('buck-out/observe-drive'))
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    root, evaluator = args.root.resolve(), args.evaluator.resolve()
    sys.path.insert(0, str(root / 'core/programs/runner/observe/models'))
    import handoffs
    seeds = args.seed if args.seed is not None else list(range(64))
    if any(seed < 0 or seed >= 64 for seed in seeds) or len(seeds) != len(set(seeds)):
        parser.error('use distinct seeds in 0..63')
    args.artifacts.mkdir(parents=True, exist_ok=True)
    coverage, unavailable, observed, exercised = {}, set(), {}, set()
    passed, controls, failures, injected = 0, 0, 0, None
    observer_bodies, fixture_bodies = 0, 0
    with tempfile.TemporaryDirectory(prefix='hirsel-observe-drive-') as tmp:
        work = Path(tmp)
        inputs = seeded_inputs(root, work, seeds, args.mix)
        value, report = evaluate(evaluator, inputs, 'runner@1/observe_fixture/law_digest')
        digest_good = value == 1
        fixture_bodies += report.get('outcome', {}).get('kind') == 'returned'
        failures += not digest_good
        print(f'OBSERVE DRIVE digest_vector passed={digest_good}', flush=True)
        if not digest_good:
            (args.artifacts / 'digest.log').write_text(json.dumps(report, indent=2) + '\n')
        for seed in seeds:
            mix = args.mix or MIXES[seed % len(MIXES)]
            value, report = evaluate(evaluator, inputs, 'runner@1/observe_seeds/seed_' + str(seed))
            t = trace(report)
            t['header'].update(has_digest=True, has_ran=True)
            codec = Codec(report.get('stderr', []))
            suites = {}
            if value == 1:
                for observer, clauses in (('positions', POSITIONS), ('joins', CLAUSES), ('session', SESSION)):
                    suites[observer] = codec.verdicts(clauses)
                observer_bodies += len(suites)
                if codec.at != len(codec.data):
                    raise ValueError('trailing drive verdict bytes')
            bad = []
            # STORE-SPEC §1.8: a V1 refusal leaves the stream without a row.
            # The control jumps to p1 (H), which the store refuses with
            # HistoryUnavailable despite there being no latest position (HU).
            expected_failures = {'H', 'HU'} if mix == 'AdvanceOnRefusal' else set()
            control_failures = {v['clause'] for v in suites.get('positions', ()) if v['status'] == 'Failed'}
            control_good = expected_failures <= control_failures
            if expected_failures:
                controls += control_good
                for clause in sorted(expected_failures - control_failures):
                    print(f'OBSERVER positions CONTROL-FAILED clause={clause} client={mix} seed={seed} expected=Failed actual=not-Failed', flush=True)
            for observer, verdicts in suites.items():
                for v in verdicts:
                    name = observer + '.' + v['clause']
                    if v['status'] in ('Held', 'Failed'):
                        exercised.add(name)
                    if v['status'] == 'Held':
                        coverage[name] = coverage.get(name, 0) + v['hits']
                    elif v['status'] == 'Unavailable':
                        unavailable.add((name, v['needs']))
                    elif v['status'] == 'Failed':
                        expected = observer == 'positions' and v['clause'] in expected_failures
                        if not expected:
                            bad.append((observer, v))
                        print(f'OBSERVER {observer} FAILED clause={v["clause"]} key={shown_key(v["key"])} '
                              f'obs #{v["start"]}..#{v["end"]} seed={seed} client={mix} expected={expected} '
                              f'trace={args.artifacts / ("drive-" + str(seed) + ".obs")}', flush=True)
                    elif v['status'] == 'Incomplete':
                        print(f'OBSERVER {observer} INCOMPLETE pending={v["clause"]}[{v["pending"]}] seed={seed}', flush=True)
                        bad.append((observer, v))
            good = value == 1 and t['header']['complete'] and control_good and not bad
            if not good or expected_failures:
                (args.artifacts / f'drive-{seed}.obs').write_text(json.dumps(t, indent=2) + '\n')
                (args.artifacts / f'drive-{seed}.log').write_text(json.dumps(report, indent=2) + '\n')
            handoffs.record_visible(t, observed)
            if good and injected is None and any(e['kind'] == 'Ran' for e in t['obs']):
                injected = json.loads(json.dumps(t))
            passed += good
            failures += not good
            print(f'OBSERVE DRIVE seed={seed} client={mix} events={len(t["obs"])} passed={good}', flush=True)
        identity_good = False
        if injected is not None:
            ran = next(e for e in reversed(injected['obs']) if e['kind'] == 'Ran')
            ran['digest'][0] ^= 1
            verdicts = check_trace(evaluator, root, work, injected, 'injected_i', observer='positions')
            fixture_bodies += 1
            identity_good = any(v['clause'] == 'I' and v['status'] == 'Failed' for v in verdicts)
            (args.artifacts / 'injected-i.obs').write_text(json.dumps(injected, indent=2) + '\n')
        failures += not identity_good
        print(f'OBSERVE DRIVE I_injected expected=Failed passed={identity_good}', flush=True)
    inventory = handoffs.compare(observed, ('admit', 'reply', 'join', 'wake', 'cancel', 'publication'))
    failures += not inventory
    names = ['positions.' + c for c in POSITIONS] + ['joins.' + c for c in CLAUSES] + ['session.' + c for c in SESSION]
    print('HELD: ' + ', '.join(f'{c}({n})' for c, n in sorted(coverage.items())))
    print('VACUOUS: ' + ', '.join(c for c in names if c not in exercised and not any(u[0] == c for u in unavailable)))
    print('UNAVAILABLE: ' + ', '.join(f'{c}({needs})' for c, needs in sorted(unavailable)))
    print(f'OBSERVE DRIVE seeds={len(seeds)} observer_bodies={observer_bodies} fixture_bodies={fixture_bodies} passed={passed} H_HU_negative_controls={controls} failed={failures}')
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
