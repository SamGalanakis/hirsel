#!/usr/bin/env python3
"""Post-push observers. Gate fixtures are adapted only in this test's private manifest."""
import argparse
import json
from pathlib import Path
import re
import tempfile

from runner_tests import CALLS, LAWS, MUTANTS, evaluate, manifest, p19_module, l12_module, L12, l12_holds
from std_native import ROOT


OBSERVE_MUTANTS = ()
TASK_KINDS = {0, 1, 2, 3, 4}
CLAUSES = ('J1', 'J2', 'J3', 'J4', 'J5', 'J6')


def replace(text, old, new):
    if text.count(old) != 1:
        raise ValueError(f'observe fixture seam changed: {old!r}')
    return text.replace(old, new)


def without_tests(text):
    while match := re.search(r'@test\s+fn test_.*?\{', text):
        end, depth = match.end(), 1
        while depth:
            depth += (text[end] == '{') - (text[end] == '}')
            end += 1
        text = text[:match.start()] + text[end:]
    return text


def traced_calls(root, work):
    text = without_tests((root / 'core/programs/store/runner_calls.hirsel').read_text())
    text = replace(text, 'type Conn {', 'type Conn { id: u64, clock: Clock, out: Stdout, err: Stderr, log: Log[observe.Obs],')
    text = replace(text, 'type Drive: send {', 'type Drive: send { out: Stdout, err: Stderr,')
    text = replace(text, 'let d = Drive(head:', 'let d = Drive(out: out, err: err, head:')
    for name in ('conn', 'conn_in', 'served', 'served_in'):
        text = replace(text, f'fn {name}(', f'fn {name}(out: Stdout, err: Stderr, ' + ('id: u64, ' if name.startswith('conn') else ''))
    text = replace(text, '    conn_in(tasks, head, clock, q,', '    conn_in(out, err, id, tasks, head, clock, q,')
    text = re.sub(r'(pub fn law_\w+\()', r'\1out: Stdout, err: Stderr, ', text)
    text = re.sub(r'(?m)^(\s*)served(_in)?\(c,', r'\1served\2(out, err, c,', text)
    next_id = 0

    def connection(m):
        nonlocal next_id
        next_id += 1
        out = 'd.out' if m[3].startswith('d.tasks') else 'out'
        err = 'd.err' if m[3].startswith('d.tasks') else 'err'
        return f'{m[1]}{m[2]}({out}, {err}, {next_id}u64, {m[3]}'

    text = re.sub(r'(let \w+ = )(conn(?:_in)?)\(([^\n]+)', connection, text)
    text = replace(text, '    Conn(to: take to,',
                   '    let recorded: Result[Log[observe.Obs], Capacity] = log.new()\n'
                   '    Conn(id: host.mono_ns(clock), clock: clock, out: out, err: err, log: result.unwrap(take recorded), to: take to,')
    text = replace(text, '    if let Error(_) = channel.send(cn.to, runner.ask(take c)) {',
                   '    record(cn, Sent(observe.Send(conn: cn.id, stream: observe.own(c.stream), p: c.p, digest: list.new(), deadline: c.deadline, at: host.mono_ns(clock))))\n'
                   '    if let Error(_) = channel.send(cn.to, runner.ask(take c)) {')
    text = replace(text, '            want(f.version != NEVER,',
                   '            if f.version != NEVER { record(cn, Got(observe.Reply(conn: cn.id, stream: observe.own(f.stream), p: f.p, version: f.version, outcome: observe.own(f.outcome), at: host.mono_ns(cn.clock)))) }\n'
                   '            want(f.version != NEVER,')
    text = replace(text, '            Ok(f) => f.version == 0u64 && match f.outcome {',
                   '            Ok(f) => {\n'
                   '                if f.version != NEVER { record(cn, Got(observe.Reply(conn: cn.id, stream: observe.own(f.stream), p: f.p, version: f.version, outcome: observe.own(f.outcome), at: host.mono_ns(cn.clock)))) }\n'
                   '                let valid = f.version == 0u64 && match f.outcome {')
    text = replace(text, '            Error(_) => false,\n        }\n        i += 1u64',
                   '                valid\n            },\n            Error(_) => false,\n        }\n        i += 1u64')
    text = replace(text, '                _ => false,\n            },\n                valid',
                   '                _ => false,\n            }\n                valid')
    text = replace(text, '    let Conn(to: _, from: _, dog: _, session: take s) = take cn\n    match task.join(take s) {\n        Ok(Ok(())) => all,',
                   '    let Conn(id: id, clock: clock, out: out, err: err, log: take entries, to: _, from: _, dog: _, session: take s) = take cn\n'
                   '    let joined = task.join(take s)\n'
                   '    emit(out, Closed(observe.Close(conn: id, code: 1000u64, at: host.mono_ns(clock))))\n'
                   '    observe.write_verdicts(err, log.finish(take entries), Closed(observe.Close(conn: id, code: 1000u64, at: host.mono_ns(clock))))\n'
                   '    match joined {\n        Ok(Ok(())) => all,')
    text = replace(text, '    let Conn(to: _, from: _, dog: _, session: take s) = take cn\n    let _ = task.join(take s)',
                   '    let Conn(id: id, clock: clock, out: out, err: err, log: take entries, to: _, from: _, dog: _, session: take s) = take cn\n'
                   '    let _ = task.join(take s)\n'
                   '    emit(out, Closed(observe.Close(conn: id, code: 1000u64, at: host.mono_ns(clock))))\n'
                   '    observe.write_verdicts(err, log.finish(take entries), Closed(observe.Close(conn: id, code: 1000u64, at: host.mono_ns(clock))))')
    text = text.replace('let Conn(to: _, from: _, dog: _, session: take s) = take cn',
                        'let Conn(to: _, from: _, dog: _, session: take s, ..) = take cn')
    text += '''
pub fn law_observe_early_then_late(out: Stdout, err: Stderr, c: Clock, tasks: Tasks) -> bool waits {
    let made: Result[(Sender[runner.Req], Receiver[runner.Req]), Capacity] = channel.new(64u64)
    let (head, inbox) = result.unwrap(take made)
    let cn = conn(out, err, 99u64, tasks, head, c, 1u64)
    ask(cn, c, 0u64, 0u64, k_write(), u64_arg(1u64), 2000000u64)
    ask(cn, c, 0u64, 0u64, k_write(), u64_arg(1u64), 50000000u64)
    let _ = frame(cn)
    hang(take cn)
    true
}

fn emit(out: Stdout, e: observe.Obs) -> () waits {
    let c: std.codec.Codec[observe.Obs] = std.codec.of()
    let mut b: Bytes = list.new()
    result.unwrap(std.codec.encode(c, e, mut b))
    result.unwrap(stdout.write_bytes(out, b))
}

fn record(cn: Conn, take e: observe.Obs) -> () waits {
    emit(cn.out, e)
    result.unwrap(log.append(cn.log, take e))
}
'''
    path = work / 'runner_calls.hirsel'
    path.write_text(text)
    return path


class Codec:
    """The fixed-width std.codec representation, not a second observer."""
    def __init__(self, data):
        self.data = bytes(data)
        self.at = 0

    def uint(self, size=8):
        end = self.at + size
        if end > len(self.data):
            raise ValueError('truncated observer codec')
        value = int.from_bytes(self.data[self.at:end], 'little')
        self.at = end
        return value

    def blob(self):
        n = self.uint(4)
        return [self.uint(1) for _ in range(n)]

    def string(self):
        return bytes(self.blob()).decode('utf-8')

    def outcome(self):
        tag = self.uint(4)
        name = ('Returned', 'Failed', 'Refused', 'Superseded', 'Abandoned', 'InDoubt')[tag]
        payload = []
        if tag == 0:
            payload = self.blob()
        if tag in (1, 2):
            inner = self.uint(4)
            payload = [inner]
            if (tag, inner) in ((1, 0), (2, 1)):
                payload += [self.uint(4)]
                if tag == 2:
                    payload += [self.uint()]
            elif (tag, inner) in ((1, 1), (2, 4)):
                payload += [self.uint(), self.uint()]
            elif (tag, inner) == (1, 2):
                payload += [self.uint()]
        return dict(kind=name, payload=payload)

    def events(self):
        events = []
        while self.at < len(self.data):
            tag = self.uint(4)
            e = dict(kind=('Sent', 'Got', 'Closed', 'Ran', 'Pos', 'Task', 'Tx', 'Rx')[tag])
            if tag in (0, 1):
                e.update(conn=self.uint(), stream=self.blob(), p=self.uint())
                if tag == 0:
                    e.update(digest=self.blob(), deadline=self.uint(), at=self.uint())
                else:
                    e.update(version=self.uint(), outcome=self.outcome(), at=self.uint())
            elif tag == 2:
                e.update(conn=self.uint(), code=self.uint(), at=self.uint())
            elif tag == 3:
                e.update(stream=self.blob(), p=self.uint(), digest=self.blob(), version=self.uint())
            elif tag == 4:
                e.update(stream=self.blob(), latest=self.uint(), state=self.uint(4))
            elif tag == 5:
                e.update(event=self.uint(4), task=self.uint(), object=self.uint(), value=self.uint())
            else:
                e.update(conn=self.uint(), frame=self.blob())
            events.append(e)
        return events

    def verdicts(self):
        results = []
        n = self.uint(4)
        if n != len(CLAUSES):
            raise ValueError(f'expected {len(CLAUSES)} clause verdicts, got {n}')
        for clause in CLAUSES:
            tag = self.uint(4)
            v = dict(clause=clause, status=('Held', 'Vacuous', 'Failed', 'Incomplete', 'Unavailable')[tag])
            if tag == 0:
                v['hits'] = self.uint()
            elif tag == 2:
                name = self.string()
                if name != clause:
                    raise ValueError(f'clause mismatch: {clause}, {name}')
                v.update(key=self.blob(), start=self.uint(), end=self.uint())
            elif tag == 3:
                v['pending'] = self.uint()
            elif tag == 4:
                v['needs'] = self.string()
            results.append(v)
        return results

    def assessments(self):
        results = []
        while self.at < len(self.data):
            conn = self.uint()
            results += [dict(v, conn=conn) for v in self.verdicts()]
        return results


def trace(report):
    events = Codec(report.get('stdout', [])).events()
    events += [dict(kind='Task', event=e[0], task=e[1], object=e[2], value=e[3])
               for e in report.get('events', []) if e[0] in TASK_KINDS]
    opened = {e['conn'] for e in events if e['kind'] == 'Sent'}
    closed = {e['conn'] for e in events if e['kind'] == 'Closed'}
    complete = report.get('outcome', {}).get('kind') == 'returned' and opened <= closed
    return dict(header=dict(has_clock=True, has_digest=False, has_ran=False, complete=complete), obs=events)


def observed_manifest(root, work):
    path = manifest(root, work, 'observe-inputs', extra=[
        (root / 'core/programs/observe/observe.hirsel', 'observe'),
        (root / 'core/programs/observe/fixture.hirsel', 'observe_fixture')])
    m = json.loads(path.read_text())
    for source in m['packages'][-1]['sources']:
        if source['modules'] == ['runner_calls']:
            source['file'] = str(traced_calls(root, work))
    path.write_text(json.dumps(m))
    return path


def check_trace(evaluator, root, work, t, name):
    """Feed live test observations to the independent Hirsel machine, never a .obs artifact."""
    blobs = {}

    def blob(value):
        key = tuple(value)
        if key not in blobs:
            blobs[key] = 'b' + str(len(blobs))
        return blobs[key] + '()'

    def outcome(o):
        kind, p = o['kind'], o.get('payload', [])
        if kind == 'Returned':
            return 'Returned(' + blob(p) + ')'
        if kind == 'Failed':
            payload = (f'Raised({p[1]}u32)' if p[0] == 0 else
                       f'OutOfBytes(runner.Room(requested: {p[1]}u64, available: {p[2]}u64))' if p[0] == 1 else
                       f'OutOfTime({p[1]}u64)' if p[0] == 2 else 'Gone')
            return f'Failed({payload})'
        if kind == 'Refused':
            payload = ('NoEntry' if p[0] == 0 else
                       f'Malformed(runner.Bad(kind: {p[1]}u32, at: {p[2]}u64))' if p[0] == 1 else
                       'Overloaded' if p[0] == 2 else 'Deadline' if p[0] == 3 else
                       f'Budget(runner.Room(requested: {p[1]}u64, available: {p[2]}u64))' if p[0] == 4 else 'NotAllowed')
            return f'Refused({payload})'
        return kind

    lines = ['pub fn check(out: Stdout) -> bool waits {', '    let mut s = observe.state()']
    for e in t['obs']:
        kind = e['kind']
        if kind == 'Sent':
            value = f'Sent(observe.Send(conn: {e["conn"]}u64, stream: {blob(e["stream"])}, p: {e["p"]}u64, digest: {blob(e["digest"])}, deadline: {e["deadline"]}u64, at: {e["at"]}u64))'
        elif kind == 'Got':
            value = f'Got(observe.Reply(conn: {e["conn"]}u64, stream: {blob(e["stream"])}, p: {e["p"]}u64, version: {e["version"]}u64, outcome: {outcome(e["outcome"])}, at: {e["at"]}u64))'
        elif kind == 'Closed':
            value = f'Closed(observe.Close(conn: {e["conn"]}u64, code: {e["code"]}u64, at: {e["at"]}u64))'
        elif kind == 'Ran':
            value = f'Ran(observe.Run(stream: {blob(e["stream"])}, p: {e["p"]}u64, digest: {blob(e["digest"])}, version: {e["version"]}u64))'
        elif kind == 'Task':
            value = f'Task(observe.TaskEvent(kind: {e["event"]}u32, task: {e["task"]}u64, object: {e["object"]}u64, value: {e["value"]}u64))'
        else:
            raise ValueError(f'unsupported trace observation {kind}')
        lines.append(f'    observe.step(mut s, {value})')
    fields = ', '.join(f'{k}: {str(v).lower()}' for k, v in t['header'].items())
    lines += [f'    let h = observe.Header({fields})', '    let v = observe.finish(s, h)',
              '    let c: std.codec.Codec[List[observe.Verdict]] = std.codec.of()',
              '    let mut b: Bytes = list.new()', '    result.unwrap(std.codec.encode(c, v, mut b))',
              '    result.unwrap(stdout.write_bytes(out, b))', '    true', '}']
    for data, fn in blobs.items():
        lines += [f'fn {fn}() -> Bytes {{', '    let mut b: Bytes = list.new()']
        lines += [f'    result.unwrap(list.push(mut b, {x}u8))' for x in data]
        lines += ['    b', '}']
    source = work / (name + '.hirsel')
    source.write_text('\n'.join(lines) + '\n')
    m = manifest(root, work, name, extra=[(root / 'core/programs/observe/observe.hirsel', 'observe'),
                                         (source, 'observe_input')])
    value, report = evaluate(evaluator, m, 'store@1/observe_input/check')
    if value != 1:
        raise ValueError(f'observer evaluation failed: {report}')
    c = Codec(report['stdout'])
    results = c.verdicts()
    if c.at != len(c.data):
        raise ValueError('trailing verdict bytes')
    return results


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--evaluator', type=Path, required=True)
    p.add_argument('--case', action='append')
    p.add_argument('--control', action='store_true', help='run the latest-deadline observer control')
    p.add_argument('--artifacts', type=Path, default=Path('buck-out/observe'))
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = p.parse_args()
    root, evaluator = args.root.resolve(), args.evaluator.resolve()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    rows = []
    coverage = dict.fromkeys(CLAUSES, 0)
    unavailable = set()
    with tempfile.TemporaryDirectory(prefix='hirsel-observe-') as tmp:
        work = Path(tmp)
        m = observed_manifest(root, work)
        cases = [(case, 'runner_calls', m) for case in (args.case or (*CALLS, 'observe_early_then_late'))]
        if not args.case:
            cases += [(case, 'runner_laws', m) for case in LAWS]
            p19 = work / 'runner_p19.hirsel'
            p19_module(p19)
            pm = json.loads(m.read_text())
            pm['packages'][-1]['sources'].append(dict(file=str(p19), modules=['runner_p19'], signatures=False))
            p19_inputs = work / 'p19.json'
            p19_inputs.write_text(json.dumps(pm))
            cases.append(('p19_place_twice', 'runner_p19', p19_inputs))
        for case, module, selected in cases:
            value, report = evaluate(evaluator, selected, f'store@1/{module}/law_' + case)
            t = trace(report)
            verdicts = Codec(report.get('stderr', [])).assessments()
            if value != 1 and t['obs'] and not verdicts:
                verdicts = check_trace(evaluator, root, work, t, 'prefix_' + case)
            failures = [v for v in verdicts if v['status'] == 'Failed']
            for v in verdicts:
                if v['status'] == 'Held':
                    coverage[v['clause']] += v['hits']
                elif v['status'] == 'Unavailable':
                    unavailable.add((v['clause'], v['needs']))
                elif v['status'] == 'Incomplete':
                    print(f'OBSERVER joins INCOMPLETE pending={v["clause"]}[{v["pending"]}] case={case}', flush=True)
            good = value == 1 and bool(t['obs']) and not failures
            for v in failures:
                indices = [i for i, e in enumerate(t['obs']) if e.get('conn') == v['conn']] if 'conn' in v else []
                start, end = (indices[v[k]] if v[k] < len(indices) else v[k] for k in ('start', 'end'))
                print(f'OBSERVER joins FAILED clause={v["clause"]} key={v["key"]} obs #{start}..#{end} case={case} trace={args.artifacts / (case + ".obs")}', flush=True)
            if not good:
                (args.artifacts / (case + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
                (args.artifacts / (case + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            print(f'OBSERVE case={case} trace={len(t["obs"])} complete={t["header"]["complete"]} passed={good}', flush=True)
            rows.append(good)
        if args.control or not args.case:
            changes = next(changes for name, changes, _, _ in MUTANTS if name == 'latest_deadline')
            runner = (root / 'core/programs/store/runner.hirsel').read_text()
            for old, new in changes:
                runner = replace(runner, old, new)
            altered = work / 'latest-deadline.hirsel'
            altered.write_text(runner)
            cm = json.loads(m.read_text())
            for source in cm['packages'][-1]['sources']:
                if source['modules'] == ['runner']:
                    source['file'] = str(altered)
            control = work / 'latest-deadline.json'
            control.write_text(json.dumps(cm))
            value, report = evaluate(evaluator, control, 'store@1/runner_calls/law_observe_early_then_late')
            verdicts = Codec(report.get('stderr', [])).assessments()
            t = trace(report)
            good = value == 1 and any(v['clause'] == 'J5' and v['status'] == 'Failed' for v in verdicts)
            (args.artifacts / 'latest-deadline.obs').write_text(json.dumps(t, indent=2) + '\n')
            if not good:
                (args.artifacts / 'latest-deadline.log').write_text(json.dumps(report, indent=2) + '\n')
            print(f'OBSERVE control=latest_deadline expected=J5/Failed passed={good}', flush=True)
            rows.append(good)
        if not args.case:
            values = {}
            for variant in L12 + (('base_again',) + L12[0][1:],):
                path = work / ('runner_l12_' + variant[0] + '.hirsel')
                l12_module(path, variant)
                lm = manifest(root, work, variant[0], extra=[(path, 'runner_l12')])
                value, report = evaluate(evaluator, lm, 'store@1/runner_l12/law_l12_describe')
                values[variant[0]] = value
                t = trace(report)
                good = value not in (None, 0) and bool(t['obs'])
                print(f'OBSERVE case=l12_{variant[0]} trace={len(t["obs"])} complete={t["header"]["complete"]} passed={good}', flush=True)
                if not good:
                    (args.artifacts / ('l12_' + variant[0] + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
                rows.append(good)
            rows.append(l12_holds(values))
        for case in ('observe/law_prefix', 'observe/law_second_reply', 'observe/law_complete_missing',
                     'observe/law_closed', 'observe/law_deadline_order', 'observe/law_refusal_order',
                     'observe/law_missing_clock', 'observe_fixture/law_record'):
            value, report = evaluate(evaluator, m, 'store@1/' + case)
            good = value == 1
            if case == 'observe_fixture/law_record':
                good &= any(e['kind'] == 'Ran' for e in Codec(report.get('stdout', [])).events())
            print(f'OBSERVE case={case} passed={good}', flush=True)
            if not good:
                (args.artifacts / (case.split('/')[1] + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            rows.append(good)
    bodies = len(rows) - (0 if args.case else 1)  # l12's cross-run comparison is not another body.
    for clause, needs in sorted(unavailable):
        print(f'OBSERVER joins UNAVAILABLE clause={clause} needs={needs}')
    print('VACUOUS: ' + ', '.join(c for c in CLAUSES if not coverage[c] and not any(u[0] == c for u in unavailable)))
    print('HELD: ' + ', '.join(f'{c}({n})' for c, n in coverage.items() if n))
    print(f'OBSERVE bodies={bodies} checks={len(rows)} passed={sum(rows)} failed={len(rows) - sum(rows)}')
    return int(not all(rows))


if __name__ == '__main__':
    raise SystemExit(main())
