#!/usr/bin/env python3
"""Observer tests use law fixtures adapted in this test's private manifest."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile

from case_progress import Case, run_case
from runner_tests import (CALLS, LAWS, MUTANTS, evaluate, manifest, p19_module, counted,
                          l12_module, L12, l12_holds, l12_values, revision_holds,
                          l10_values, l10_holds, c_compiler, dir_for, native_run, counting_cc,
                          COUNTS)
from std_native import ROOT


OBSERVE_MUTANTS = (
    ('digest_dropped_when_owed_zero', (
        ('digest: store.RequestDigest, forwarded: bool, queued: u64, owed: List[WireOwed]',
         'digest: store.RequestDigest, digest_live: bool, forwarded: bool, queued: u64, owed: List[WireOwed]'),
        ('digest: sent.digest, forwarded: false,',
         'digest: sent.digest, digest_live: true, forwarded: false,'),
        ('digest: digest, forwarded: true,',
         'digest: digest, digest_live: true, forwarded: true,'),
        ('(h.forwarded, h.digest == sent.digest, h.queued)',
         '(h.forwarded, !h.digest_live || h.digest == sent.digest, h.queued)'),
        ('Some(h) => if h.forwarded { Some(h.digest) }',
         'Some(h) => if h.forwarded && h.digest_live { Some(h.digest) }'),
        ('            let _ = list.remove(mut h.owed, index)\n',
         '            let _ = list.remove(mut h.owed, index)\n            if list.len(h.owed) == 0u64 { h.digest_live = false }\n'),
    )),
)
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
    text = replace(text, 'type Conn =', 'type Transport =')
    text = replace(text, 'type Drive: send {', 'type Drive: send { out: Stdout, err: Stderr,')
    text = text.replace('Drive(head:', 'Drive(out: out, err: err, head:')
    text = re.sub(r'(pub fn law_\w+\()', r'\1out: Stdout, err: Stderr, ', text)
    for name in ('served', 'served_in', 'served_on'):
        text = replace(text, f'fn {name}(', f'fn {name}(out: Stdout, err: Stderr, ')
        text = re.sub(rf'(?<!fn )\b{name}\(', f'{name}(out, err, ', text)
    text = replace(text, 'Some(Socket(take cn, take h))',
                   'Some(observed(d, Socket(take cn, take h)))')
    text = text.replace('Held(runner.fixture_owner(d.post))',
                        'observed(d, Held(runner.fixture_owner(d.post)))')
    text = text.replace('match cn {', 'match cn.link {')
    text = replace(text, 'fn ask(cn: Conn,', 'fn ask(mut cn: Conn,')
    text = replace(text, 'fn stall(b: Conn,', 'fn stall(mut b: Conn,')
    text = re.sub(r'(?<![\w.])stall\(b,', 'stall(mut b,', text)
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

fn observed(d: Drive, take link: Transport) -> Conn {
    let entries: Result[Log[observe.Obs], Capacity] = log.new()
    Conn(id: host.mono_ns(d.clock), clock: d.clock, out: d.out, err: d.err,
        log: result.unwrap(take entries), digests: map.new(), link: take link)
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
    emit(out, Closed(observe.Close(conn: id, code: code, at: host.mono_ns(clock))))
    observe.write_wire_verdicts(err, log.finish(take entries), Closed(observe.Close(conn: id, code: code, at: host.mono_ns(clock))))
    ended
}

pub fn law_observe_early_then_late(out: Stdout, err: Stderr, c: Clock, tasks: Tasks, n: Net, r: Random) -> bool waits {
    let made: Result[(Sender[runner.Req], Receiver[runner.Req]), Capacity] = channel.new(64u64)
    let (head, inbox) = result.unwrap(take made)
    let mail = result.unwrap(runner.mailbox(64u64))
    let (policy, take policy_handle) = result.unwrap(ingress.start(tasks, c, r, fixture_policy(), 1048576u64))
    let mut policy_task = take policy_handle
    let principal = option.unwrap(ingress.hello(policy, V4(0x7f000001u32), None))
    let d = Drive(out: out, err: err, head: take head, post: runner.post(mail), clock: c,
        tasks: tasks, net: n, random: r, policy: ingress.share(policy),
        token: result.unwrap(clone(ingress.principal(principal))), accepted: None)
    let mut cn = conn(d, 1u64)
    ask(mut cn, c, 0u64, 0u64, k_write(), u64_arg(1u64), 2000000u64)
    ask(mut cn, c, 0u64, 0u64, k_write(), u64_arg(1u64), 50000000u64)
    // A request sent after the first deadline supplies an ordered reply marker.
    host.sleep_until(c, host.mono_ns(c) + 10000000u64)
    ask(mut cn, c, 1u64, 0u64, k_write(), u64_arg(2u64), 0u64)
    let _ = frame(cn)
    let _ = frame(cn)
    hang(take cn)
    task.cancel(mut policy_task)
    let _ = task.join(take policy_task)
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

// Memoize the pure hash by its complete byte inputs, not the slot or deadline.
fn send_digest(mut cn: Conn, key: Bytes, arg: Bytes) -> Bytes {
    let mut cache_key: Bytes = list.new()
    result.unwrap(std.codec.put_u64(list.len(key), mut cache_key))
    result.unwrap(list.append(mut cache_key, key))
    result.unwrap(std.codec.put_u64(list.len(arg), mut cache_key))
    result.unwrap(list.append(mut cache_key, arg))
    if let Some(d) = map.get(cn.digests, cache_key) { return result.unwrap(clone(d)) }
    let d = observe_fixture.digest(key, arg)
    result.unwrap(map.insert(mut cn.digests, take cache_key, result.unwrap(clone(d))))
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
        cc = c_compiler(args)
        support = work / 'observe-events.o'
        run_case('observe_events:observer', 'cranelift', [str(cc), '-c', '-fPIC', str(root / 'core/tools/observe_events.c'),
                        '-I' + str(root / 'crates/hirsel-core-rt/include'), '-o', str(support)], check=True)
        self.cc = work / 'observe-cc'
        self.cc.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc)) + ' "$@" '
                           + shlex.quote(str(support)) + ' -Wl,--wrap=hirsel_core_os_run_with_sources\n')
        self.cc.chmod(0o755)

    def __call__(self, inputs_path, definition):
        if not definition.startswith('store@1/runner_calls/'):
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
            for law, parameters in re.findall(r'pub fn (law_\w+)\(([^)]*)\)', source.read_text()):
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
                       '--entry', 'store@1/observe_entry/main', '-o', str(binary),
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
                e.update(stream=self.blob(), latest=self.uint(), state=self.uint(4), digest=self.blob())
            elif tag == 5:
                e.update(event=self.uint(4), task=self.uint(), object=self.uint(), value=self.uint())
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
    codec = Codec(raw)
    conn, stream, p = codec.uint(), codec.blob(), codec.uint()
    name = str(int.from_bytes(bytes(stream), 'little')) if len(stream) == 8 else bytes(stream).hex()
    return f'(conn {conn}, s{name}, p{p})'


def trace(report):
    events = Codec(report.get('stdout', [])).events()
    events += [dict(kind='Task', event=e[0], task=e[1], object=e[2], value=e[3])
               for e in report.get('events', []) if e[0] in TASK_KINDS]
    opened = {e['conn'] for e in events if e['kind'] == 'Sent'}
    closed = {e['conn'] for e in events if e['kind'] == 'Closed'}
    complete = report.get('outcome', {}).get('kind') == 'returned' and opened <= closed
    sends = [e for e in events if e['kind'] == 'Sent']
    return dict(header=dict(has_clock=True,
                            has_digest=bool(sends) and all(len(e['digest']) == 32 for e in sends),
                            has_ran=any(e['kind'] == 'Ran' for e in events),
                            complete=complete), obs=events)


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


def join_manifest(root, work, name, runner=None):
    source = work / (name + '-joins.hirsel')
    source.write_text('\n'.join(
        f'pub fn law_{case}(out: Stdout, err: Stderr, clock: Clock, random: Random, tasks: Tasks, net: Net) -> bool waits {{\n'
        f'    observe_drive.run_join(out, err, clock, random, tasks, net, {case})\n}}\n'
        for case in JOIN_CASES))
    modules = ('observe', 'fixture', 'positions', 'session', 'observe_drive')
    return manifest(root, work, name, runner, extra=[
        *((root / f'core/programs/observe/{module}.hirsel',
           'observe_fixture' if module == 'fixture' else module) for module in modules),
        (source, 'observe_joins')])


def identity_cases(root, evaluator, work, artifacts, coverage, unavailable, calls):
    rows, bodies = [], 0
    selected = join_manifest(root, work, 'identity')
    for case in JOIN_CASES:
        value, report = evaluate(evaluator, selected, 'store@1/observe_joins/law_' + case)
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

    original = (root / 'core/programs/store/runner.hirsel').read_text()
    poison = work / 'identity-poison.hirsel'
    poison.write_text(replace(original, '    let initial: Registry[G, H] = registry()\n',
                             '    let initial: Registry[G, H] = @observe_syntax_poison\n'))
    poisoned = join_manifest(root, work, 'identity-poison', poison)
    value, report = evaluate(evaluator, poisoned, 'store@1/observe_joins/law_AfterAbandoned')
    bodies += bool(report.get('outcome'))
    good = value is None and bool(report.get('error'))
    (artifacts / 'identity-poison.log').write_text(json.dumps(report, indent=2) + '\n')
    print(f'OBSERVE control=identity-syntax-poison expected=build-refused passed={good}', flush=True)
    rows.append(good)

    for mutant, changes in OBSERVE_MUTANTS:
        altered = original
        for old, new in changes:
            altered = replace(altered, old, new)
        runner = work / (mutant + '.hirsel')
        runner.write_text(altered)
        selected = join_manifest(root, work, mutant, runner)
        value, report = evaluate(evaluator, selected, 'store@1/observe_joins/law_AfterAbandoned')
        bodies += bool(report.get('outcome'))
        t = trace(report)
        t['header'].update(has_digest=True, has_ran=True)
        codec = Codec(report.get('stderr', []))
        verdicts = codec.verdicts() if value == 1 else []
        good = (value == 1 and t['header']['complete']
                and any(v['clause'] == 'J1' and v['status'] == 'Failed' for v in verdicts)
                and not any(v['status'] == 'Failed' and v['clause'] != 'J1' for v in verdicts))
        (artifacts / (mutant + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
        (artifacts / (mutant + '.log')).write_text(json.dumps(report, indent=2) + '\n')
        print(f'OBSERVE control={mutant} expected=J1/Failed passed={good}', flush=True)
        rows.append(good)

        # The original laws use their unadapted fixture and unchanged assertions.
        legacy = manifest(root, work, mutant + '-laws', runner)
        for module, cases in (('runner_calls', CALLS), ('runner_laws', LAWS)):
            for case in cases:
                if module == 'runner_calls':
                    definition = f'store@1/{module}/test_{case}'
                    cc = counting_cc(calls.args, root, legacy, case) if case in COUNTS else None
                    code, report = native_run(calls.args, 'cranelift', legacy, work / 'identity-native-store', definition, cc)
                    tests = report.get('tests', [])
                    bodies += len(tests)
                    good = code == 0 and [t['definition'] for t in tests] == [definition] and all(t['passed'] for t in tests)
                else:
                    value, report = evaluate(evaluator, legacy, f'store@1/{module}/law_' + case)
                    bodies += bool(report.get('outcome'))
                    good = value == 1 and counted(case, report)
                if not good:
                    (artifacts / (mutant + '-' + case + '.log')).write_text(json.dumps(report, indent=2) + '\n')
                print(f'OBSERVE control={mutant} legacy={case} passed={good}', flush=True)
                rows.append(good)
        p19 = work / (mutant + '-p19.hirsel')
        p19_module(p19)
        pm = manifest(root, work, mutant + '-p19', runner, [(p19, 'runner_p19')])
        value, report = evaluate(evaluator, pm, 'store@1/runner_p19/law_p19_place_twice')
        bodies += bool(report.get('outcome'))
        good = value == 1
        print(f'OBSERVE control={mutant} legacy=p19_place_twice passed={good}', flush=True)
        rows.append(good)
        values = l12_values(evaluator, root, work, mutant, runner)
        revisions = {name: evaluate(evaluator, work / f'l12_{mutant}_{name}.json',
                                   'store@1/runner_l12/law_l12_revision')[0]
                     for name in values}
        l10 = l10_values(evaluator, root, work, mutant, runner)
        bodies += sum(v is not None for v in values.values())
        bodies += sum(v is not None for v in revisions.values())
        bodies += sum(v is not None for pair in l10.values() for v in pair)
        for case, good in (('l12', l12_holds(values)), ('l12_revision', revision_holds(revisions)),
                           ('l10_e_only', l10_holds(l10))):
            print(f'OBSERVE control={mutant} legacy={case} passed={good}', flush=True)
            rows.append(good)
    return rows, bodies


def deadline_timing(root, evaluator, work, artifacts, coverage, runner=None):
    source = work / ('deadline-timing' + ('-control' if runner else '') + '.hirsel')
    source.write_text('pub fn law_timing(out: Stdout, err: Stderr, c: Clock, r: Random, tasks: Tasks, net: Net) -> bool waits {\n'
                      '    observe_drive.deadline_join(out, err, c, r, tasks, net)\n}\n')
    modules = ('observe', 'fixture', 'positions', 'session', 'observe_drive')
    selected = manifest(root, work, source.stem, runner, extra=[
        *((root / f'core/programs/observe/{module}.hirsel',
           'observe_fixture' if module == 'fixture' else module) for module in modules),
        (source, 'observe_timing')])
    value, report = evaluate(evaluator, selected, 'store@1/observe_timing/law_timing')
    verdicts = Codec(report.get('stderr', [])).verdicts() if value == 1 else []
    expected = 'Failed' if runner else 'Held'
    good = value == 1 and any(v['clause'] == 'J5' and v['status'] == expected for v in verdicts)
    if not runner:
        for v in verdicts:
            if v['status'] == 'Held':
                coverage[v['clause']] += v['hits']
    if runner or not good:
        (artifacts / (source.stem + '.log')).write_text(json.dumps(report, indent=2) + '\n')
    print(f'OBSERVE J5=virtual-timing control={bool(runner)} expected={expected} passed={good}', flush=True)
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
        lines += [f'    result.unwrap(list.push(mut b, {x}u8))' for x in data]
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
    value, report = evaluate(evaluator, m, 'store@1/observe_input/check')
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
    p.add_argument('--case', action='append')
    p.add_argument('--control', action='store_true', help='run the latest-deadline observer control')
    p.add_argument('--identity-only', action='store_true', help='run the identity fixtures and their mutation control')
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
        cases = [] if args.identity_only else [(case, 'runner_calls', m) for case in (args.case or (*CALLS, 'observe_early_then_late'))]
        if not args.case and not args.identity_only:
            cases += [(case, 'runner_laws', m) for case in LAWS]
            p19 = work / 'runner_p19.hirsel'
            p19_module(p19)
            pm = json.loads(m.read_text())
            pm['packages'][-1]['sources'].append(dict(file=str(p19), modules=['runner_p19'], signatures=False))
            p19_inputs = work / 'p19.json'
            p19_inputs.write_text(json.dumps(pm))
            cases.append(('p19_place_twice', 'runner_p19', p19_inputs))
        for case, module, selected in cases:
            value, report = calls(selected, f'store@1/{module}/law_' + case)
            bodies += bool(report.get('outcome'))
            t = trace(report)
            handoffs.record_visible(t, inventories)
            verdicts = Codec(report.get('stderr', [])).assessments()
            if value != 1 and t['obs'] and not verdicts:
                verdicts = check_trace(evaluator, root, work, t, 'prefix_' + case, 'wire_joins' if module == 'runner_calls' else 'joins')
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
                print(f'OBSERVER joins FAILED clause={v["clause"]} key={shown_key(v["key"])} obs #{start}..#{end} case={case} trace={args.artifacts / (case + ".obs")}', flush=True)
            if not good:
                (args.artifacts / (case + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
                (args.artifacts / (case + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            print(f'OBSERVE case={case} trace={len(t["obs"])} complete={t["header"]["complete"]} passed={good}', flush=True)
            rows.append(good)
        if not args.identity_only and (args.control or not args.case):
            timing_good, timing_bodies = deadline_timing(root, evaluator, work, args.artifacts, coverage)
            rows.append(timing_good)
            bodies += timing_bodies
            changes = next(changes for name, changes, _, _ in MUTANTS if name == 'latest_deadline')
            runner = (root / 'core/programs/store/runner.hirsel').read_text()
            for old, new in changes:
                runner = replace(runner, old, new)
            altered = work / 'latest-deadline.hirsel'
            altered.write_text(runner)
            timing_good, timing_bodies = deadline_timing(root, evaluator, work, args.artifacts, coverage, altered)
            rows.append(timing_good)
            bodies += timing_bodies
            cm = json.loads(m.read_text())
            for source in cm['packages'][-1]['sources']:
                if source['modules'] == ['runner']:
                    source['file'] = str(altered)
            control = work / 'latest-deadline.json'
            control.write_text(json.dumps(cm))
            value, report = calls(control, 'store@1/runner_calls/law_observe_early_then_late')
            bodies += bool(report.get('outcome'))
            verdicts = Codec(report.get('stderr', [])).assessments()
            t = trace(report)
            good = value == 1 and any(v['clause'] == 'J5' and v['status'] == 'Failed' for v in verdicts)
            (args.artifacts / 'latest-deadline.obs').write_text(json.dumps(t, indent=2) + '\n')
            if not good:
                (args.artifacts / 'latest-deadline.log').write_text(json.dumps(report, indent=2) + '\n')
            print(f'OBSERVE control=latest_deadline J5=wire-order expected=J5/Failed passed={good}', flush=True)
            rows.append(good)
        if not args.case and not args.identity_only:
            values = {}
            for variant in L12 + (('base_again',) + L12[0][1:],):
                path = work / ('runner_l12_' + variant[0] + '.hirsel')
                l12_module(path, variant)
                lm = manifest(root, work, variant[0], extra=[(path, 'runner_l12')])
                value, report = evaluate(evaluator, lm, 'store@1/runner_l12/law_l12_describe')
                bodies += bool(report.get('outcome'))
                values[variant[0]] = value
                t = trace(report)
                good = value not in (None, 0) and bool(t['obs'])
                print(f'OBSERVE case=l12_{variant[0]} trace={len(t["obs"])} complete={t["header"]["complete"]} passed={good}', flush=True)
                if not good:
                    (args.artifacts / ('l12_' + variant[0] + '.obs')).write_text(json.dumps(t, indent=2) + '\n')
                rows.append(good)
            rows.append(l12_holds(values))
        fixtures = () if args.identity_only or args.case else ('observe/law_prefix', 'observe/law_second_reply', 'observe/law_complete_missing',
                     'observe/law_closed', 'observe/law_deadline_order', 'observe/law_refusal_order',
                     'observe/law_missing_clock', 'observe_fixture/law_record')
        for case in fixtures:
            value, report = evaluate(evaluator, m, 'store@1/' + case)
            bodies += bool(report.get('outcome'))
            good = value == 1
            if case == 'observe_fixture/law_record':
                good &= any(e['kind'] == 'Ran' for e in Codec(report.get('stdout', [])).events())
            print(f'OBSERVE case={case} passed={good}', flush=True)
            if not good:
                (args.artifacts / (case.split('/')[1] + '.log')).write_text(json.dumps(report, indent=2) + '\n')
            rows.append(good)
        if not args.case:
            identity_rows, identity_bodies = identity_cases(root, evaluator, work, args.artifacts, coverage, unavailable, calls)
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
