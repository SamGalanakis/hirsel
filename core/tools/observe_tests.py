#!/usr/bin/env python3
"""Observer tests use law fixtures adapted in this test's private manifest."""
import argparse
import json
import os
from pathlib import Path
import re
import sys
import tempfile

from case_progress import Case
from runner_tests import CALLS, NATIVE_ONLY_CALLS, c_compiler, dir_for, evaluate, manifest
from std_native import ROOT


# OBSERVERS-SPEC §6 requires a client log. R1 also exercises startup,
# reserves and direct head requests outside the traced client session.
UNOBSERVED_CALLS = NATIVE_ONLY_CALLS | {
    'atomic_migration', 'drain_answers_once', 'duplicate_own_reply',
    'ready_after_activation', 'release_digest', 'serve_bounded_and_settled',
}
LIVE_CASES = (*(case for case in CALLS if case not in UNOBSERVED_CALLS), 'observe_early_then_late')
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
    text = without_tests((root / 'core/programs/runner/runner_calls.hirsel').read_text())
    text = replace(text, 'type Conn =', 'type Transport =')
    text = replace(text, 'type Drive: send {', 'type Drive: send { out: Stdout, err: Stderr,')
    text = text.replace('let d = Drive(', 'let d = Drive(out: out, err: err, ')
    text = re.sub(r'(pub fn law_\w+\()', r'\1out: Stdout, err: Stderr, ', text)
    for name in ('served', 'served_in', 'served_on'):
        text = replace(text, f'fn {name}(', f'fn {name}(out: Stdout, err: Stderr, ')
        text = re.sub(rf'(?<!fn )\b{name}\(', f'{name}(out, err, ', text)
    text = replace(text, 'Some(Socket(take cn, take h))',
                   'Some(observed(d, Socket(take cn, take h)))')
    text = re.sub(r'Held\(runner\.fixture_owner\(d\.post, (\w+\(\w+\))\)\)',
                  r'observed(d, Held(runner.fixture_owner(d.post, \1)))', text)
    text = text.replace('match cn {', 'match cn.link {')
    text = replace(text, 'fn ask(cn: Conn,', 'fn ask(mut cn: Conn,')
    text = replace(text, 'fn stall(b: Conn,', 'fn stall(mut b: Conn,')
    text = re.sub(r'(?<![\w.])stall\(b,', 'stall(mut b,', text)
    text = replace(text, 'fn put_at(w: Conn,', 'fn put_at(mut w: Conn,')
    text = re.sub(r'(?<![\w.])put_at\(w,', 'put_at(mut w,', text)
    text = re.sub(r'(?<![\w.])ask\((\w+),', r'ask(mut \1,', text)
    text = re.sub(r'let (\w+)(: Conn)? = (conn\(|try_conn\(|observed\()', r'let mut \1\2 = \3', text)
    text = replace(text, 'let Some(take cn) = try_conn', 'let Some(take available) = try_conn')
    text = replace(text, '    let stream: u32 = int.narrow(s)',
                   '    let digest = send_digest(mut cn, key, arg)\n'
                   '    record(cn, Sent(observe.Send(conn: cn.id, stream: sid(s), p: p, digest: take digest, deadline: host.mono_ns(clock) + after, at: host.mono_ns(clock))))\n'
                   '    let stream: u32 = int.narrow(s)')
    text = replace(text, '    let bad = string_arg(4096u64, true)', '    let mut cn = take available\n    let bad = string_arg(4096u64, true)')
    # Record all decoded batches, including malformed-frame probes and try_down.
    text = replace(text, '        Ok(take d) => take d,',
                   '        Ok(take d) => { replies(cn, d); take d },')
    text = replace(text, 'fn try_down(cn: Conn) -> Result[runner.Down, ReceiveError] waits {\n    match cn.link {',
                   'fn try_down(cn: Conn) -> Result[runner.Down, ReceiveError] waits {\n    let got: Result[runner.Down, ReceiveError] = match cn.link {')
    text = replace(text, '        Held(owner) => runner.fixture_try_receive(owner),\n    }',
                   '        Held(owner) => runner.fixture_try_receive(owner),\n    }\n'
                   '    if let Ok(d) = got { replies(cn, d) }\n    take got')
    text = replace(text, '    match take cn {\n        Socket(take wire, take s) => { let _ = wire_client.close(take wire); let _ = task.join(take s) },\n        Held(take owner) => runner.fixture_close(take owner),\n    }',
                   '    let _ = finish(take cn, 1000u64)')
    text = replace(text, '    match take cn {\n        Socket(take wire, take s) => { let _ = wire_client.close(take wire); match task.join(take s) { Ok(Ok(())) => all, _ => false } },\n        _ => false,\n    }',
                   '    finish(take cn, 1000u64) && all')
    text = replace(text, '    match take cn {\n        Socket(take wire, take s) => { let _ = wire_client.close(take wire); match task.join(take s) { Ok(Ok(())) => true, _ => false } },\n        _ => false,\n    }',
                   '    finish(take cn, 1008u64)')
    text += '''
type Conn { id: u64, clock: Clock, out: Stdout, err: Stderr, log: Log[observe.Obs], digests: Map[Bytes, Bytes], link: Transport }

fn observed(d: Drive, take link: Transport) -> Conn waits {
    let entries: Log[observe.Obs] = log.new()
    let id = host.mono_ns(d.clock)
    emit(d.out, Opened(id))
    Conn(id: id, clock: d.clock, out: d.out, err: d.err,
        log: take entries, digests: map.new(), link: take link)
}

fn replies(cn: Conn, d: runner.Down) -> () waits {
    if let Batched(b) = d {
        if b.version != NEVER {
            for t in b.outcomes {
                record(cn, Got(observe.Reply(conn: cn.id, stream: observe.own(t.stream), p: t.p,
                    version: b.version, outcome: observe.from_runner(t.outcome), at: host.mono_ns(cn.clock))))
            }
        }
    }
}

fn finish(take cn: Conn, code: u64) -> bool waits {
    let Conn(id: id, clock: clock, out: out, err: err, log: take entries, digests: _, link: take link) = take cn
    let ended = match take link {
        Socket(take wire, take session) => {
            let _ = wire_client.close(take wire)
            match task.join(take session) { Ok(Ok(())) => true, _ => false }
        },
        Held(take owner) => { runner.fixture_close(take owner); true },
    }
    if !ended { return false }
    let at = host.mono_ns(clock)
    emit(out, Closed(observe.Close(conn: id, code: code, at: at)))
    observe.write_wire_verdicts(err, log.finish(take entries), Closed(observe.Close(conn: id, code: code, at: at)))
    true
}

pub fn law_observe_early_then_late(out: Stdout, err: Stderr, c: Clock, tasks: Tasks, n: Net, r: Random) -> bool waits {
    let (head, inbox): (Sender[runner.Req], Receiver[runner.Req]) = channel.new(64u64)
    let mail = result.unwrap(runner.mailbox(64u64))
    let db = store.memory(c, r, result.unwrap(store.sized(Memory, 1074790400u64)))
    let d = Drive(out: out, err: err, head: take head, post: runner.post(mail), clock: c,
        tasks: tasks, net: n, random: r,
        token: ingress.credential(r), accepted: None)
    let mut cn = conn(d, 1u64)
    ask(mut cn, c, 0u64, 0u64, k_write(), u64_arg(1u64), 2000000u64)
    ask(mut cn, c, 0u64, 0u64, k_write(), u64_arg(1u64), 50000000u64)
    // A request sent after the first deadline supplies an ordered reply marker.
    host.sleep_until(c, host.mono_ns(c) + 10000000u64)
    ask(mut cn, c, 1u64, 0u64, k_write(), u64_arg(2u64), 0u64)
    let _ = frame(cn)
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
    log.append(cn.log, take e)
}

// Memoize the pure hash by its complete byte inputs, not the slot or deadline.
fn send_digest(mut cn: Conn, key: Bytes, arg: Bytes) -> Bytes {
    let mut cache_key: Bytes = list.new()
    std.codec.put_u64(list.len(key), mut cache_key)
    list.append(mut cache_key, key)
    std.codec.put_u64(list.len(arg), mut cache_key)
    list.append(mut cache_key, arg)
    if let Some(d) = map.get(cn.digests, cache_key) { return clone(d) }
    let d = observe_fixture.digest(key, arg)
    map.insert(mut cn.digests, take cache_key, clone(d))
    d
}
'''
    path = work / 'runner_calls.hirsel'
    path.write_text(text)
    return path


class CallEvaluator:
    """Wire laws run natively; pure observers and model fixtures use the evaluator."""
    def __init__(self, args, root, work):
        self.args, self.root, self.work = args, root, work
        self.binaries = {}
        self.cc = c_compiler(args)

    def __call__(self, inputs_path, definition):
        if not definition.startswith('runner@1/runner_calls/'):
            return evaluate(self.args.evaluator, inputs_path, definition)
        with Case(f'{definition} [{inputs_path.stem}]', 'cranelift') as progress:
            return self.native(inputs_path, definition, progress)

    def native(self, inputs_path, definition, progress):
        if inputs_path not in self.binaries:
            m = json.loads(inputs_path.read_text())
            sources = m['packages'][-1]['sources']
            source = Path(next(s['file'] for s in sources if s['modules'] == ['runner_calls']))
            dispatch = ['pub fn main(out: Stdout, err: Stderr, c: Clock, r: Random, tasks: Tasks, n: Net, d: Dir, argv: Args) -> Result[(), Exit] waits {',
                        '    let name = option.unwrap(args.get(argv, 0))', '    let good = false']
            names = dict(Stdout='out', Stderr='err', Clock='c', Random='r', Tasks='tasks', Net='n', Dir='d')
            selected_cases = {f'law_{case}' for case in (self.args.case or LIVE_CASES)}
            for law, parameters in re.findall(r'pub fn (law_\w+)\(([^)]*)\)', source.read_text()):
                if law not in selected_cases:
                    continue
                arguments = ', '.join(names[p.split(':')[1].strip()] for p in parameters.split(',') if p.strip())
                dispatch.append(f'        || (name == "{law}" && runner_calls.{law}({arguments}))')
            dispatch += [ '    if good { Ok(()) } else { Error(Exit(status: 1)) }', '}']
            entry = self.work / (inputs_path.stem + '-entry.hirsel')
            entry.write_text('\n'.join(dispatch) + '\n')
            sources.append(dict(file=str(entry), modules=['observe_entry'], signatures=False))
            selected = entry.with_suffix('.json')
            selected.write_text(json.dumps(m))
            binary = entry.with_suffix('')
            store = self.work / 'native-store'
            command = [str(self.args.compiler.resolve()), 'build', '--inputs', str(selected),
                       '--source-core', '--source-std', '--backend', 'cranelift',
                       '--runtime', str(self.args.runtime.resolve()), '--cc', str(self.cc),
                       '--store', str(store), '--limit', str(64 * 1024 * 1024),
                       '--entry', 'runner@1/observe_entry/main', '-o', str(binary),
                       '--grant', 'Stdout', '--grant', 'Stderr', '--grant', 'Clock', '--grant', 'Random', '--grant', 'Tasks',
                       '--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535',
                       '--dir', str(dir_for(store))]
            built = progress.run(command, capture_output=True, text=True)
            if built.returncode:
                return None, dict(error=built.stderr, command=command)
            self.binaries[inputs_path] = binary
        run = progress.run([str(self.binaries[inputs_path]), definition.rsplit('/', 1)[1]], capture_output=True)
        return (1 if run.returncode == 0 else 0 if run.returncode == 1 else None,
                dict(outcome=dict(kind='returned' if run.returncode in (0, 1) else 'failed'),
                     stdout=list(run.stdout), stderr=list(run.stderr) if run.returncode in (0, 1) else [],
                     status=run.returncode, diagnostic=run.stderr.hex() if run.returncode not in (0, 1) else ''))


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
        name = ('Returned', 'Failed', 'Refused', 'Abandoned', 'InDoubt')[tag]
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
            e = dict(kind=('Sent', 'Got', 'Closed', 'Ran', 'Pos', 'Task', 'Tx', 'Rx', 'Opened')[tag])
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
                e.update(stream=self.blob(), latest=self.uint(), state=self.uint(4), digest=self.blob())
            elif tag == 5:
                e.update(event=self.uint(4), task=self.uint(), object=self.uint(), value=self.uint())
            elif tag == 8:
                e.update(conn=self.uint())
            else:
                e.update(conn=self.uint(), frame=self.blob())
            events.append(e)
        return events

    def verdicts(self, clauses=CLAUSES):
        results = []
        n = self.uint(4)
        if n != len(clauses):
            raise ValueError(f'expected {len(clauses)} clause verdicts, got {n}')
        for clause in clauses:
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


def shown_key(raw):
    if not raw:
        return '(no key)'
    codec = Codec(raw)
    conn, stream, p = codec.uint(), codec.blob(), codec.uint()
    name = str(int.from_bytes(bytes(stream), 'little')) if len(stream) == 8 else bytes(stream).hex()
    return f'(conn {conn}, s{name}, p{p})'


def trace(report, *, live=False):
    events = Codec(report.get('stdout', [])).events()
    opens = [e['conn'] for e in events if e['kind'] == ('Opened' if live else 'Sent')]
    closes = [e['conn'] for e in events if e['kind'] == 'Closed']
    opened, closed = set(opens), set(closes)
    complete = report.get('outcome', {}).get('kind') == 'returned' and opened <= closed
    if live:
        complete = (complete and bool(opened) and opened == closed
                    and len(opens) == len(closes) == len(opened)
                    and all(e['conn'] in opened for e in events if e['kind'] in ('Sent', 'Got')))
    sends = [e for e in events if e['kind'] == 'Sent']
    return dict(header=dict(has_clock=True,
                            has_digest=bool(sends) and all(len(e['digest']) == 32 for e in sends),
                            has_ran=any(e['kind'] == 'Ran' for e in events),
                            complete=complete), obs=events)


def live_evidence(case, t, verdicts):
    opened = {e['conn'] for e in t['obs'] if e['kind'] == 'Opened'}
    assessed = {v['conn'] for v in verdicts}
    complete = (t['header']['complete'] and opened == assessed
                and len(verdicts) == len(opened) * len(CLAUSES))
    held = {v['clause'] for v in verdicts if v['status'] == 'Held' and v['hits'] > 0}
    return (complete and all(v['status'] not in ('Failed', 'Incomplete') for v in verdicts)
            and (case != 'calls_two_streams' or {'J2', 'J3'} <= held))


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


JOIN_CASES = ('CrossSession', 'AfterAbandoned', 'AfterAbandonedSame')


def join_manifest(root, work, name):
    source = work / (name + '-joins.hirsel')
    source.write_text('\n'.join(
        f'pub fn law_{case}(out: Stdout, err: Stderr, clock: Clock, random: Random, tasks: Tasks, net: Net) -> bool waits {{\n'
        f'    observe_drive.run_join(out, err, clock, random, tasks, net, {case})\n}}\n'
        for case in JOIN_CASES))
    modules = ('observe', 'fixture', 'positions', 'session', 'observe_drive')
    return manifest(root, work, name, extra=[
        *((root / f'core/programs/observe/{module}.hirsel',
           'observe_fixture' if module == 'fixture' else module) for module in modules),
        (source, 'observe_joins')])


def identity_cases(root, evaluator, work, artifacts, coverage, unavailable):
    rows, bodies = [], 0
    selected = join_manifest(root, work, 'identity')
    for case in JOIN_CASES:
        value, report = evaluate(evaluator, selected, 'runner@1/observe_joins/law_' + case)
        bodies += bool(report.get('outcome'))
        t = trace(report)
        t['header'].update(has_digest=True, has_ran=True)
        codec = Codec(report.get('stderr', []))
        verdicts = codec.verdicts() if value == 1 else []
        failed = [v for v in verdicts if v['status'] == 'Failed']
        required = ('J1', 'J6') if case == 'CrossSession' else ('J1', 'J4', 'J5') if case == 'AfterAbandoned' else ('J1', 'J5')
        good = (value == 1 and t['header']['complete'] and codec.at == len(codec.data)
                and not failed and all(any(v['clause'] == c and v['status'] == 'Held' for v in verdicts)
                                       for c in required))
        for v in verdicts:
            if v['status'] == 'Held':
                coverage[v['clause']] += v['hits']
            elif v['status'] == 'Unavailable':
                unavailable.add((v['clause'], v['needs']))
        if not good:
            (artifacts / (case + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
            (artifacts / (case + '.log')).write_text(json.dumps(report, indent=2) + '\n')
        print(f'OBSERVE identity={case} events={len(t["obs"])} required={",".join(required)} passed={good}', flush=True)
        rows.append(good)

    return rows, bodies


def deadline_timing(root, evaluator, work, artifacts, coverage):
    source = work / 'deadline-timing.hirsel'
    source.write_text('pub fn law_timing(out: Stdout, err: Stderr, c: Clock, r: Random, tasks: Tasks, net: Net) -> bool waits {\n'
                      '    observe_drive.deadline_join(out, err, c, r, tasks, net)\n}\n')
    modules = ('observe', 'fixture', 'positions', 'session', 'observe_drive')
    selected = manifest(root, work, source.stem, extra=[
        *((root / f'core/programs/observe/{module}.hirsel',
           'observe_fixture' if module == 'fixture' else module) for module in modules),
        (source, 'observe_timing')])
    value, report = evaluate(evaluator, selected, 'runner@1/observe_timing/law_timing')
    verdicts = Codec(report.get('stderr', [])).verdicts() if value == 1 else []
    good = value == 1 and any(v['clause'] == 'J5' and v['status'] == 'Held' for v in verdicts)
    for v in verdicts:
        if v['status'] == 'Held':
            coverage[v['clause']] += v['hits']
    if not good:
        (artifacts / (source.stem + '.log')).write_text(json.dumps(report, indent=2) + '\n')
    print(f'OBSERVE J5=virtual-timing expected=Held passed={good}', flush=True)
    return good, bool(report.get('outcome'))


def trace_source(t, name, observer):
    """Construct a test entry from live observations, never a .obs artifact."""
    observer_module = 'observe' if observer in ('joins', 'wire_joins') else observer
    clauses = (CLAUSES if observer in ('joins', 'wire_joins') else ('V',) if observer == 'versions'
               else ('W3', 'W4', 'W5') if observer == 'session' else ('H', 'I', 'M', 'HU', 'AH2'))
    blobs = {}

    def blob(value):
        key = tuple(value)
        if key not in blobs:
            blobs[key] = name + '_b' + str(len(blobs))
        return blobs[key] + '()'

    def outcome(o):
        kind, p = o['kind'], o.get('payload', [])
        if kind == 'Returned':
            return 'ObsReturned(' + blob(p) + ')'
        if kind == 'Failed':
            payload = (f'Raised({p[1]}u32)' if p[0] == 0 else
                       f'OutOfBytes(runner.Room(requested: {p[1]}u64, available: {p[2]}u64))' if p[0] == 1 else
                       f'OutOfTime({p[1]}u64)' if p[0] == 2 else 'Gone')
            return f'ObsFailed({payload})'
        if kind == 'Refused':
            payload = ('NoEntry' if p[0] == 0 else
                       f'Malformed(runner.Bad(kind: {p[1]}u32, at: {p[2]}u64))' if p[0] == 1 else
                       'Overloaded' if p[0] == 2 else 'Deadline' if p[0] == 3 else
                       f'Budget(runner.Room(requested: {p[1]}u64, available: {p[2]}u64))' if p[0] == 4 else 'NotAllowed' if p[0] == 5 else 'RequestMismatch' if p[0] == 6 else 'HistoryUnavailable')
            return f'ObsRefused(Obs{payload})'
        return 'Obs' + kind

    lines = [f'fn {name}(out: Stdout) -> bool waits {{', f'    let mut s = {observer_module}.wire_state()' if observer == 'wire_joins' else f'    let mut s = {observer_module}.state()']
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
        elif kind == 'Pos':
            value = f'Pos(observe.Position(stream: {blob(e["stream"])}, latest: {e["latest"]}u64, state: {e["state"]}u32, digest: {blob(e["digest"])}))'
        elif kind == 'Task':
            value = f'Task(observe.TaskEvent(kind: {e["event"]}u32, task: {e["task"]}u64, object: {e["object"]}u64, value: {e["value"]}u64))'
        elif kind == 'Opened':
            value = f'Opened({e["conn"]}u64)'
        elif kind in ('Tx', 'Rx'):
            value = f'{kind}(observe.Wire(conn: {e["conn"]}u64, frame: {blob(e["frame"])}))'
        else:
            raise ValueError(f'unsupported trace observation {kind}')
        lines.append(f'    {observer_module}.step(mut s, {value})')
    fields = ', '.join(f'{k}: {str(v).lower()}' for k, v in t['header'].items())
    lines += [f'    let h = observe.Header({fields})', f'    let v = {observer_module}.finish(s, h)',
              '    let c: std.codec.Codec[List[observe.Verdict]] = std.codec.of()',
              '    let mut b: Bytes = list.new()', '    result.unwrap(std.codec.encode(c, v, mut b))',
              '    result.unwrap(stdout.write_bytes(out, b))', '    true', '}']
    for data, fn in blobs.items():
        lines += [f'fn {fn}() -> Bytes {{', '    let mut b: Bytes = list.new()']
        lines += [f'    list.push(mut b, {x}u8)' for x in data]
        lines += ['    b', '}']
    return '\n'.join(lines) + '\n', clauses


def check_traces(evaluator, root, work, plans, name):
    sources, selections = [], []
    main = ['pub fn check(out: Stdout) -> bool waits {']
    for index, (observer, t) in enumerate(plans):
        function = 'case_' + str(index)
        text, clauses = trace_source(t, function, observer)
        sources.append(text)
        selections.append(clauses)
        main.append(f'    if !{function}(out) {{ return false }}')
    main += ['    true', '}']
    source = work / (name + '.hirsel')
    source.write_text('\n'.join(sources + main) + '\n')
    modules = ('observe', 'positions', 'versions', 'session')
    m = manifest(root, work, name, extra=[
        *((root / f'core/programs/observe/{module}.hirsel', module) for module in modules),
        (source, 'observe_input')])
    value, report = evaluate(evaluator, m, 'runner@1/observe_input/check')
    if value != 1:
        (work / (name + '-error.json')).write_text(json.dumps(report, indent=2) + '\n')
        raise ValueError(f'observer evaluation failed; see {work / (name + "-error.json")}: {report.get("error", report.get("outcome"))}')
    codec = Codec(report['stdout'])
    results = [codec.verdicts(clauses) for clauses in selections]
    if codec.at != len(codec.data):
        raise ValueError('trailing verdict bytes')
    return results


def check_trace(evaluator, root, work, t, name, observer='joins'):
    return check_traces(evaluator, root, work, [(observer, t)], name)[0]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--evaluator', type=Path, required=True)
    p.add_argument('--compiler', type=Path, required=True)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--case', action='append', choices=LIVE_CASES)
    p.add_argument('--identity-only', action='store_true', help='run the identity fixtures')
    p.add_argument('--artifacts', type=Path, default=Path('buck-out/observe'))
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = p.parse_args()
    root, evaluator = args.root.resolve(), args.evaluator.resolve()
    sys.path.insert(0, str(root / 'core/programs/observe/models'))
    import handoffs
    args.artifacts.mkdir(parents=True, exist_ok=True)
    rows = []
    bodies = 0
    coverage = dict.fromkeys(CLAUSES, 0)
    unavailable = set()
    inventories = {}
    with tempfile.TemporaryDirectory(prefix='hirsel-observe-') as tmp:
        work = Path(tmp)
        calls = CallEvaluator(args, root, work)
        m = observed_manifest(root, work) if not args.identity_only else manifest(root, work, 'identity-inputs')
        cases = () if args.identity_only else (args.case or LIVE_CASES)
        for case in cases:
            value, report = calls(m, 'runner@1/runner_calls/law_' + case)
            bodies += bool(report.get('outcome'))
            try:
                t = trace(report, live=True)
                verdicts = Codec(report.get('stderr', [])).assessments()
            except (ValueError, IndexError) as error:
                report['decode_error'] = str(error)
                (args.artifacts / (case + '.log')).write_text(json.dumps(report, indent=2) + '\n')
                print(f'OBSERVE case={case} passed=False decode={error}', flush=True)
                rows.append(False)
                continue
            handoffs.record_visible(t, inventories)
            failures = [v for v in verdicts if v['status'] == 'Failed']
            for v in verdicts:
                if v['status'] == 'Held':
                    coverage[v['clause']] += v['hits']
                elif v['status'] == 'Unavailable':
                    unavailable.add((v['clause'], v['needs']))
                elif v['status'] == 'Incomplete':
                    print(f'OBSERVER joins INCOMPLETE pending={v["clause"]}[{v["pending"]}] case={case}', flush=True)
            good = value == 1 and live_evidence(case, t, verdicts)
            for v in failures:
                indices = [i for i, e in enumerate(t['obs']) if e.get('conn') == v['conn']] if 'conn' in v else []
                start, end = (indices[v[k]] if v[k] < len(indices) else v[k] for k in ('start', 'end'))
                print(f'OBSERVER joins FAILED clause={v["clause"]} key={shown_key(v["key"])} obs #{start}..#{end} case={case} trace={args.artifacts / (case + ".obs")}', flush=True)
            if not good:
                (args.artifacts / (case + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
                (args.artifacts / (case + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            print(f'OBSERVE case={case} trace={len(t["obs"])} complete={t["header"]["complete"]} passed={good}', flush=True)
            rows.append(good)
        if not args.identity_only and not args.case:
            timing_good, timing_bodies = deadline_timing(root, evaluator, work, args.artifacts, coverage)
            rows.append(timing_good)
            bodies += timing_bodies
        fixtures = () if args.identity_only or args.case else ('observe/law_prefix', 'observe/law_second_reply', 'observe/law_complete_missing',
                     'observe/law_closed', 'observe/law_deadline_order', 'observe/law_refusal_order',
                     'observe/law_missing_clock', 'observe_fixture/law_record')
        for case in fixtures:
            value, report = evaluate(evaluator, m, 'runner@1/' + case)
            bodies += bool(report.get('outcome'))
            good = value == 1
            if case == 'observe_fixture/law_record':
                good &= any(e['kind'] == 'Ran' for e in Codec(report.get('stdout', [])).events())
            print(f'OBSERVE case={case} passed={good}', flush=True)
            if not good:
                (args.artifacts / (case.split('/')[1] + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            rows.append(good)
        if not args.case:
            identity_rows, identity_bodies = identity_cases(root, evaluator, work, args.artifacts, coverage, unavailable)
            rows += identity_rows
            bodies += identity_bodies
    rows.append(handoffs.compare(inventories, ('join', 'wake', 'cancel')))
    if not args.case:
        exercised = all(coverage[c] for c in ('J2', 'J3', 'J5'))
        rows.append(exercised)
        if not exercised:
            print('OBSERVER joins coverage gap: ' + ', '.join(c for c in ('J2', 'J3', 'J5') if not coverage[c]))
    for clause, needs in sorted(unavailable):
        print(f'OBSERVER joins UNAVAILABLE clause={clause} needs={needs}')
    print('VACUOUS: ' + ', '.join(c for c in CLAUSES if not coverage[c] and not any(u[0] == c for u in unavailable)))
    print('HELD: ' + ', '.join(f'{c}({n})' for c, n in coverage.items() if n))
    print(f'OBSERVE bodies={bodies} checks={len(rows)} passed={sum(rows)} failed={len(rows) - sum(rows)}')
    return int(not all(rows))


if __name__ == '__main__':
    raise SystemExit(main())
