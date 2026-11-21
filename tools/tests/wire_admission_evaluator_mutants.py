#!/usr/bin/env python3
"""Run explicitly selected W3c admission controls against checkout source on the evaluator."""
import argparse
import json
from pathlib import Path
import re
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
from runner_tests import evaluate, manifest

CONTROLS = {
    'gone_refused_at_session': ('runner', 'store@1/runner/law_gone_slot_v5',
        'if !known && !map.contains(r.gone, want.key) { return Error(NoEntry) }',
        'if !known { return Error(NoEntry) }'),
    'commit_then_check': ('runner', 'store@1/runner/law_reply_over_done_max',
        'Ok(()) => if list.len(out) > DONE_MAX - 20u64 {',
        'Ok(()) => if false {'),
    'admit_without_digest': ('store', 'store@1/runner/law_request_mismatch_wire',
        'if at.digest != d {', 'if false {'),
    # Superseded was deleted. Returned models its erroneous claim of completion;
    # the law requires HistoryUnavailable and unchanged store version and data.
    'superseded_from_rejected': ('runner', 'store@1/runner/law_history_below_latest',
        'HistoryUnavailable => commitless(mut hd, take q, Refused(HistoryUnavailable)),',
        'HistoryUnavailable => commitless(mut hd, take q, Returned(list.new())),')
}


def selected(work, name, module, changed):
    runner = (ROOT / 'core/programs/store/runner.hirsel').read_text()
    if module == 'runner':
        runner = changed
    adapted = work / (name + '-runner.hirsel')
    adapted.write_text(runner + '\n' + (ROOT / 'core/programs/store/admission-laws.hirsel').read_text())
    path = manifest(ROOT, work, name, adapted)
    if module != 'runner':
        source = work / (name + '-' + module + '.hirsel')
        source.write_text(changed)
        description = json.loads(path.read_text())
        next(s for s in description['packages'][-1]['sources'] if s['modules'] == [module])['file'] = str(source)
        path.write_text(json.dumps(description))
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evaluator', required=True, type=Path)
    parser.add_argument('--mutant', action='append', choices=CONTROLS)
    parser.add_argument('--syntax-control', action='append', choices=['runner', 'store'])
    parser.add_argument('--report', required=True, type=Path)
    args = parser.parse_args()
    names = args.mutant or []
    if not names and not args.syntax_control:
        parser.error('select controls explicitly')
    if len(names) > 3 or len(set(names)) != len(names):
        parser.error('select at most three distinct controls per land')
    rows = []
    with tempfile.TemporaryDirectory(prefix='wire-w3c-controls-') as directory:
        work = Path(directory)
        for module in args.syntax_control or []:
            text = (ROOT / f'core/programs/store/{module}.hirsel').read_text()
            law = {'runner': 'store@1/runner/law_gone_slot_v5',
                   'store': 'store@1/runner/law_request_mismatch_wire'}[module]
            function = {'runner': 'prepare', 'ingress': 'bind', 'store': 'admit'}[module]
            header = re.search(r'\bfn ' + function + r'[^\n]*\{', text).group()
            assert text.count(header) == 1
            changed = text.replace(header, header[:-1] + '??? {')
            path = selected(work, 'syntax-' + module, module, changed)
            value, report = evaluate(args.evaluator.resolve(), path, law)
            refused = value is None and 'error' in report and 'Incomplete' in report['error']
            rows.append(dict(control='syntax-' + module, passed=refused, result=report))
            print('syntax-' + module + ': ' + ('REFUSED' if refused else 'NOT REFUSED'), flush=True)
        for name in names:
            module, law, old, new = CONTROLS[name]
            text = (ROOT / f'core/programs/store/{module}.hirsel').read_text()
            assert text.count(old) == 1, (name, old)
            path = selected(work, name, module, text.replace(old, new))
            value, report = evaluate(args.evaluator.resolve(), path, law)
            causes = (report.get('outcome') or {}).get('causes')
            caught = value == 0 or causes == ['Fail'] or (name == 'commit_then_check' and causes == ['Unwrap'])
            rows.append(dict(mutant=name, law=law, passed=caught, result=report))
            print(name + ': ' + ('CAUGHT' if caught else 'NOT CAUGHT'), flush=True)
    args.report.write_text(json.dumps(rows, indent=2) + '\n')
    return int(not all(row['passed'] for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
