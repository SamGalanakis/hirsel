#!/usr/bin/env python3
"""Post-push observers. Gate fixtures are adapted only in this test's private manifest."""
import argparse
import json
from pathlib import Path
import re
import tempfile

from runner_tests import CALLS, LAWS, evaluate, manifest, p19_module, l12_module, L12, l12_holds
from std_native import ROOT


OBSERVE_MUTANTS = ()
TASK_KINDS = {0, 1, 2, 3, 4}


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
    text = replace(text, 'type Conn {', 'type Conn { id: u64, clock: Clock, out: Stdout, log: Log[observe.Obs],')
    text = replace(text, 'type Drive: send {', 'type Drive: send { out: Stdout,')
    text = replace(text, 'let d = Drive(head:', 'let d = Drive(out: out, head:')
    for name in ('conn', 'conn_in', 'served', 'served_in'):
        text = replace(text, f'fn {name}(', f'fn {name}(out: Stdout, ' + ('id: u64, ' if name.startswith('conn') else ''))
    text = replace(text, '    conn_in(tasks, head, clock, q,', '    conn_in(out, id, tasks, head, clock, q,')
    text = re.sub(r'(pub fn law_\w+\()', r'\1out: Stdout, ', text)
    text = re.sub(r'(?m)^(\s*)served(_in)?\(c,', r'\1served\2(out, c,', text)
    next_id = 0

    def connection(m):
        nonlocal next_id
        next_id += 1
        out = 'd.out' if m[3].startswith('d.tasks') else 'out'
        return f'{m[1]}{m[2]}({out}, {next_id}u64, {m[3]}'

    text = re.sub(r'(let \w+ = )(conn(?:_in)?)\(([^\n]+)', connection, text)
    text = replace(text, '    Conn(to: take to,',
                   '    let recorded: Result[Log[observe.Obs], Capacity] = log.new()\n'
                   '    Conn(id: id, clock: clock, out: out, log: result.unwrap(take recorded), to: take to,')
    text = replace(text, '    if let Error(_) = channel.send(cn.to, runner.ask(take c)) {',
                   '    record(cn, Sent(observe.Send(conn: cn.id, stream: observe.own(c.stream), p: c.p, digest: list.new(), deadline: c.deadline, at: host.mono_ns(clock))))\n'
                   '    if let Error(_) = channel.send(cn.to, runner.ask(take c)) {')
    text = replace(text, '            want(f.version != NEVER,',
                   '            record(cn, Got(observe.Reply(conn: cn.id, stream: observe.own(f.stream), p: f.p, version: f.version, outcome: observe.own(f.outcome), at: host.mono_ns(cn.clock))))\n'
                   '            want(f.version != NEVER,')
    text = replace(text, '            Ok(f) => f.version == 0u64 && match f.outcome {',
                   '            Ok(f) => {\n'
                   '                record(cn, Got(observe.Reply(conn: cn.id, stream: observe.own(f.stream), p: f.p, version: f.version, outcome: observe.own(f.outcome), at: host.mono_ns(cn.clock))))\n'
                   '                let valid = f.version == 0u64 && match f.outcome {')
    text = replace(text, '            Error(_) => false,\n        }\n        i += 1u64',
                   '                valid\n            },\n            Error(_) => false,\n        }\n        i += 1u64')
    text = replace(text, '                _ => false,\n            },\n                valid',
                   '                _ => false,\n            }\n                valid')
    text = replace(text, '    let Conn(to: _, from: _, dog: _, session: take s) = take cn\n    match task.join(take s) {\n        Ok(Ok(())) => all,',
                   '    let Conn(id: id, clock: clock, out: out, log: take entries, to: _, from: _, dog: _, session: take s) = take cn\n'
                   '    let joined = task.join(take s)\n'
                   '    emit(out, Closed(observe.Close(conn: id, code: 1000u64, at: host.mono_ns(clock))))\n'
                   '    let _ = log.finish(take entries)\n'
                   '    match joined {\n        Ok(Ok(())) => all,')
    text = replace(text, '    let Conn(to: _, from: _, dog: _, session: take s) = take cn\n    let _ = task.join(take s)',
                   '    let Conn(id: id, clock: clock, out: out, log: take entries, to: _, from: _, dog: _, session: take s) = take cn\n'
                   '    let _ = task.join(take s)\n'
                   '    emit(out, Closed(observe.Close(conn: id, code: 1000u64, at: host.mono_ns(clock))))\n'
                   '    let _ = log.finish(take entries)')
    text = text.replace('let Conn(to: _, from: _, dog: _, session: take s) = take cn',
                        'let Conn(to: _, from: _, dog: _, session: take s, ..) = take cn')
    text += '''
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


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--evaluator', type=Path, required=True)
    p.add_argument('--case', action='append')
    p.add_argument('--artifacts', type=Path, default=Path('buck-out/observe'))
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = p.parse_args()
    root, evaluator = args.root.resolve(), args.evaluator.resolve()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-observe-') as tmp:
        work = Path(tmp)
        m = observed_manifest(root, work)
        cases = [(case, 'runner_calls', m) for case in (args.case or CALLS)]
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
            good = value == 1 and bool(t['obs'])
            if not good:
                (args.artifacts / (case + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
                (args.artifacts / (case + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            print(f'OBSERVE case={case} trace={len(t["obs"])} complete={t["header"]["complete"]} passed={good}', flush=True)
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
        for case in ('observe/law_prefix', 'observe_fixture/law_record'):
            value, report = evaluate(evaluator, m, 'store@1/' + case)
            good = value == 1
            if case == 'observe_fixture/law_record':
                good &= any(e['kind'] == 'Ran' for e in Codec(report.get('stdout', [])).events())
            print(f'OBSERVE case={case} passed={good}', flush=True)
            if not good:
                (args.artifacts / (case.split('/')[1] + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            rows.append(good)
    bodies = len(rows) - (0 if args.case else 1)  # l12's cross-run comparison is not another body.
    print(f'OBSERVE bodies={bodies} checks={len(rows)} passed={sum(rows)} failed={len(rows) - sum(rows)}')
    return int(not all(rows))


if __name__ == '__main__':
    raise SystemExit(main())
