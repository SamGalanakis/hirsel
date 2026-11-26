#!/usr/bin/env python3
"""Model/observer differentials and outcome inventories, only after a push."""
import argparse
import json
from pathlib import Path
import sys

from std_native import ROOT
from observe_tests import check_traces, shown_key


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--model', choices=('calls', 'store', 'subs'), required=True)
    parser.add_argument('--case', help='one named sample-N, mutant, or boundary case')
    parser.add_argument('--artifacts', type=Path, default=Path('buck-out/observe-models'))
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    root, evaluator = args.root.resolve(), args.evaluator.resolve()
    sys.path.insert(0, str(root / 'core/programs/runner/observe/models'))
    import export
    import handoffs
    modules = dict(calls=export.calls, store=export.store, subs=export.subs)
    work = args.artifacts / args.model
    work.mkdir(parents=True, exist_ok=True)
    data = modules[args.model].export_obs(work / 'model-traces.json')
    if len(data['samples']) != 200:
        raise ValueError('the correct model must export 200 maximal histories')
    observers = dict(calls=('joins', 'versions'), store=('positions',), subs=('joins', 'versions', 'session'))[args.model]
    selected, plans, expected = [], [], []

    def add(name, t, clause, observer, kind):
        if args.case and name != args.case:
            return
        selected.append((name, t, observer, kind))
        plans.append((observer, t))
        expected.append(clause)

    for sample in data['samples']:
        for observer in observers:
            add('sample-' + str(sample['seed']), sample['trace'], None, observer, 'D2')
    internal = 0
    for ce in data['counterexamples']:
        if ce['clause'] is None:
            print(f'D1: internal mutant={ce["mutant"]} law={ce["why"]} reason={ce["internal"]}', flush=True)
            internal += 1
            continue
        clause = ce['clause']
        observer = ('joins' if clause.startswith('J') else 'versions' if clause == 'V'
                    else 'session' if clause.startswith('W') else 'positions')
        add(ce['mutant'], ce['trace'], clause, observer, 'D1')
    for boundary in data['boundaries']:
        add(boundary['name'], boundary['trace'], boundary['clause'], boundary['observer'], 'control')
    if not plans:
        parser.error('the selection has no differential bodies')
    passed, failures, clocks = 0, 0, 0
    coverage, unavailable, all_clauses, exercised = {}, set(), set(), set()
    for start in range(0, len(plans), 64):
        results = check_traces(evaluator, root, work, plans[start:start + 64], 'batch-' + str(start))
        for offset, verdicts in enumerate(results):
            i = start + offset
            name, t, observer, kind = selected[i]
            clause = expected[i]
            if clause:
                good = any(v['clause'] == clause and v['status'] == 'Failed' for v in verdicts)
            else:
                good = all(v['status'] in ('Held', 'Vacuous') or v['status'] == 'Unavailable'
                           and not t['header'].get('has_' + v['needs'], False) for v in verdicts)
            for v in verdicts:
                all_clauses.add(v['clause'])
                if v['status'] in ('Held', 'Failed'):
                    exercised.add(v['clause'])
                if v['status'] == 'Held':
                    coverage[v['clause']] = coverage.get(v['clause'], 0) + v['hits']
                elif v['status'] == 'Unavailable':
                    unavailable.add((v['clause'], v['needs']))
                if v['clause'] == 'J5':
                    clock_ok = v['status'] == 'Unavailable' and v.get('needs') == 'clock'
                    good &= clock_ok
                    clocks += clock_ok
            passed += good
            failures += not good
            actual_failures = [v for v in verdicts if v['status'] == 'Failed']
            if actual_failures or not good:
                trace = work / (name + '-' + observer + '.obs')
                trace.write_text(json.dumps(t, indent=2) + '\n')
                if not actual_failures:
                    print(f'{kind} model={args.model} case={name} observer={observer} expected={clause or "Held/Vacuous"} passed=False trace={trace} verdicts={verdicts}', flush=True)
                for v in actual_failures:
                    print(f'OBSERVER {observer} FAILED clause={v["clause"]} key={shown_key(v["key"])} '
                          f'obs #{v["start"]}..#{v["end"]} case={name} trace={trace} expected={bool(clause)} matched={good}', flush=True)
    names = dict(calls=('join', 'wake', 'cancel'), store=('admit', 'reply'), subs=('publication',))[args.model]
    inventory = handoffs.check(names=names)
    failures += not inventory
    print('HELD: ' + ', '.join(f'{c}({n})' for c, n in sorted(coverage.items())))
    print('VACUOUS: ' + ', '.join(sorted(c for c in all_clauses if c not in exercised and not any(u[0] == c for u in unavailable))))
    print('UNAVAILABLE: ' + ', '.join(f'{c}({needs})' for c, needs in sorted(unavailable)))
    sampled = len({name for name, _, _, kind in selected if kind == 'D2'})
    print(f'OBSERVE MODEL {args.model} exported_histories=200 sampled_histories={sampled} differential_bodies={len(plans)} passed={passed} '
          f'D1_internal={internal} D3_clock_unavailable={clocks} inventories={len(names)} failed={failures}')
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
