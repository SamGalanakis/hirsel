#!/usr/bin/env python3
"""Run the runner's named laws and cases (RUNNER-SPEC v2.5 §8) from checkout sources.

Every law runs on the evaluator (a law_ function) and natively on each selected backend
(its test_ twin): R0's in core/programs/store/runner_laws.hirsel, R1's in
core/programs/store/runner_calls.hirsel, and two generated modules, runner_p19 (one
`place` of 1, 100, 1,000 and 10,000 entries, run in two tasks) and runner_l12 (describe across permuted
registrations, an added A field and a changed R; and ea's code revision, which every
engine must give alike, PROG-9). Each mutant applies its text changes
to a copy of runner.hirsel and must turn its law red on the evaluator. L4's high-water
case builds core/programs/store/runner_rss.hirsel per native backend and compares
the runtime's exact peak of live backing bytes across its two modes.

--part r1 runs R1's calls (transactions at durable versions, runs-once, store refusal)
and their native twins: the land gate's part. --part r0 runs the rest, which runs after
the push: R0's laws, p19, l12, L4, R1b's L1, the wire frame vectors, and every mutant
(a mutant qualifies a law, not the runner a change lands). --part wire selects only
frame_vectors and its evaluator mutants. Without --part both R0 and R1 run.

R1b's L1 builds core/programs/store/runner_crash.hirsel per native backend under the
store's W2 crash adapter (core/tools/store_crash_tests.py): every cut of each scenario,
and one failed journal sync. Its two mutants need that adapter, so they run natively, on
Cranelift only, and are red when the L1 floor check fails them.
"""
import argparse
import json
import shlex
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from std_native import ROOT, wait_load
from store_tests import inputs
from store_crash_tests import checked, run_crashes


LAWS = ('p01_call', 'p02_start_finish', 'p09_describe', 'p13_duplicate_refused', 'p15_types_roundtrip',
        'p19_place_bound', 'l4_malformed_flood', 'step_bounds')
SIZES = (1, 100, 1000, 10000)
# place_n registers in chunks of CHUNK calls, one function each, so no one function's
# frame grows with n (N13(b): a frame holds about 3,000 calls).
CHUNK = 500
# The bound one `place` and its describe run in: 10,000 entries take about 12 MiB on the
# evaluator (the registry's map grows to 2 MiB while its old table is live).
N_PLACE = 16 * 1024 * 1024

CALLS = ('calls_two_streams', 'calls_group_version', 'calls_joiners', 'calls_duplicate', 'calls_refusals',
         'calls_runs_once', 'calls_session_charge', 'calls_store_refusal', 'calls_one_outcome_per_send', 'calls_deadline_held', 'calls_deadline_hung', 'calls_held_head', 'l14_handles', 'calls_session_mismatch')

# L3 (RUNNER-SPEC v2.6): execution counts read from CONC-10 events, (event kind, count).
# L3's tally handler is the only caller of try_send (TRY_SEND is kind 7 in
# crates/hirsel-core-layout/src/event.rs), and calls_runs_once runs it for one position:
# exactly once, however often that position is re-sent. The evaluator's count comes from
# its event log; natively, core/tools/runner_count.c is linked into the case's test
# executable as its event sink and fails the run on any other count.
# L7 adds32664 sends by the hung body and one marker by the other session's
# compute body; the marker precedes the head's exhaustion slice (kind21).
COUNTS = {'calls_runs_once': (7, 1), 'calls_deadline_hung': (7, 32665)}

# R2's publisher laws, in runner.hirsel itself (RUNNER-SPEC §8.2).
SUBS = ('subs_main',)


def counted(name, report):
    if name not in COUNTS:
        return True
    kind, want = COUNTS[name]
    events = report.get('events', [])
    if sum(1 for e in events if e[0] == kind) != want:
        return False
    if name == 'calls_deadline_hung':
        sends = {}
        for at, e in enumerate(events):
            if e[0] == 7:
                sends.setdefault(e[2], []).append((at, e[1]))
        if sorted(map(len, sends.values())) != [1, 32664]:
            return False
        marker = next(v[0] for v in sends.values() if len(v) == 1)
        body = next(v for v in sends.values() if len(v) == 32664)
        slices = [(at, e[1]) for at, e in enumerate(events) if e[0] == 21]
        return (len(slices) == 1 and marker[0] < slices[0][0]
                and marker[1] != slices[0][1] == body[0][1])
    return True


# L4 (RUNNER-SPEC v2.5 §8): runner_rss.hirsel's program, built per native backend against
# the charge-observation runtime and run in two modes, one valid call (0) and 10,000
# malformed frames before it (1). Both exit 0 and their --logical-charge-report
# peak_retained_bytes (the runtime's exact peak of live backing requests) are equal: a
# refused frame that leaves any bytes behind raises mode 1's peak. ru_maxrss, the
# earlier figure, moves by 256 KB steps from run to run and cannot see a small leak.
def l4_retained(args, root, work, engine):
    inputs_path = manifest(root, work, 'rss', None, [(root / 'core/programs/store/runner_rss.hirsel', 'runner_rss')])
    binary = work / f'runner_rss_{engine}'
    charges = work / f'runner_rss_{engine}.json'
    command = [str(args.compiler.resolve()), 'build', '--inputs', str(inputs_path), '--source-core',
               '--source-std', '--backend', engine, '--runtime', str(args.observed.resolve()),
               '--store', str(work / f'store-rss-{engine}'), '--grant', 'Clock', '--grant', 'Tasks',
               '--grant', 'Random', '--entry', 'store@1/runner_rss/main',
               '--logical-charge-report', str(charges), '-o', str(binary)]
    if args.cc:
        command += ['--cc', str(args.cc.resolve())]
    wait_load()
    built = subprocess.run(command, capture_output=True, text=True)
    if built.returncode != 0:
        return False, {'build': built.stderr[-2000:]}
    figures = {}
    for mode, name in (('0', 'valid'), ('1', 'flood')):
        run = subprocess.run([str(binary), mode], capture_output=True)
        figures[name] = (run.returncode, json.loads(charges.read_text())['peak_retained_bytes'])
    held = figures['valid'][0] == 0 and figures['flood'][0] == 0 and figures['flood'][1] == figures['valid'][1]
    return held, figures


# (name, [(old text, new text)], module, the law that must turn red). Each is a wrong
# implementation the law names:
# - duplicate_unchecked: a registry that accepts a key twice;
# - decode_unbounded: an adapter that decodes the argument under a bound that ignores n;
# - encode_outside_bound (v2.3): an adapter that encodes R after leaving the bound;
# - key_without_e: a transact Key over R alone, so a change to E alone keeps the Key;
# - describe_without_schemas: a describe without the argument and result schemas;
# - direct_commitless: a head that answers a commitless outcome straight to the session;
# - release_at_commit_version: a Release at its own commit version, not the group's
#   durable version;
# - queue_outside_account: a session that frees a slot when it answers Abandoned.
# - join_without_digest (L21): a head that joins any request for the running position,
#   whatever its digest, so other argument bytes get that run's reply.
# - session_join_without_digest (L21): a session that joins any send of a held position,
#   so a send with other bytes gets the first send's answer.
# - mismatch_row_in_session (L21): a session that answers other bytes at a held position
#   with a Refused(RequestMismatch) row instead of ending (Close 1008).
# - late_completion_resent: a session that sends the head's answer to a position whose
#   sends were all answered (Abandoned, not re-sent), a second Outcome for one send.
# - one_for_joined: one row for a head answer however many sends of the position are owed.
# - latest_deadline: a joined send answered only when the latest send's deadline fires.
# - refused_frames_kept (L2): a session that keeps each refused frame's bytes, so its
#   charge grows with the flood.
# - join_outside_account: a joined send takes no slot, so per-send timers are unbounded.
# - rerun_on_duplicate (L3, C10): a head that runs the body again for a Duplicate, discards the
#   run and answers the stored reply (red by its execution count, COUNTS).
MUTANTS = (
    ('rerun_on_duplicate',
     [('                Ok(Ok(take o)) => commitless(mut hd, take q, take o),',
       '                Ok(Ok(take o)) => {\n'
       '                    let _ = effect(mut hd.db, hd.r, hd.h, q)\n'
       '                    commitless(mut hd, take q, take o)\n'
       '                },')],
     'runner_calls', 'calls_runs_once'),
    ('duplicate_unchecked',
     [('    if map.contains(r.slots, key) {\n        return Error(AlreadyRegistered(take key))\n    }\n', '')],
     'runner_laws', 'p13_duplicate_refused'),
    # R2 (RUNNER-SPEC §8.2): a Subscribe answered at the newest snapshot, not the base;
    # one session's sub found by another session's same sub id; a rerun that drops a key
    # without retracting it.
    ('subscribe_at_latest',
     [('    let version = store.version(b.base)\n    let mut o = Owe(',
       '    if let Some(take n) = replace(mut b.newest, None) {\n        b.base = take n\n    }\n'
       '    let version = store.version(b.base)\n    let mut o = Owe(')],
     'runner', 'subs_main'),
    ('shared_cache',
     [('if subs[i].session == who && subs[i].sub == sub {', 'if subs[i].sub == sub {')],
     'runner', 'subs_main'),
    ('rerun_without_retract',
     [('            result.unwrap(list.push(mut out, Row(sub: sub, key: result.unwrap(clone(before[i].tree_key)), value: None)))\n', '')],
     'runner', 'subs_main'),
    ('decode_unbounded',
     [('    let ran = bound bytes n {\n', '    let ran = bound bytes 4611686018427387904u64 {\n')],
     'runner_laws', 'l4_malformed_flood'),
    ('encode_outside_bound',
     [('        let run = scoped fn() -> Result[Result[Outcome, DecodeFail], OutOfTime] {',
       '        let run = scoped fn() -> Result[Result[Result[R, Outcome], DecodeFail], OutOfTime] {'),
      ('                    Ok(take r) => Ok(encoded(cr, r)),\n'
       '                    Error(c) => Ok(Failed(OutOfBytes(room(c)))),\n'
       '                },\n'
       '                Ok(Error(e)) => Error(DecodeFail(e: e)),\n'
       '                Error(c) => Ok(Failed(OutOfBytes(room(c)))),',
       '                    Ok(take r) => Ok(Ok(take r)),\n'
       '                    Error(c) => Ok(Error(Failed(OutOfBytes(room(c))))),\n'
       '                },\n'
       '                Ok(Error(e)) => Error(DecodeFail(e: e)),\n'
       '                Error(c) => Ok(Error(Failed(OutOfBytes(room(c))))),'),
      ('        Ok(Ok(Ok(Ok(Ok(take o))))) => take o,',
       '        Ok(Ok(Ok(Ok(Ok(Ok(take r)))))) => encoded(cr, r),\n        Ok(Ok(Ok(Ok(Ok(Error(take o)))))) => take o,')],
     'runner_laws', 'l4_malformed_flood'),
    # v2.11.2 L10: the Key over R alone, as before v2.11.2, so a change to E alone keeps it.
    ('key_without_e',
     [('    let carried: std.codec.Codec[Result[R, E]] = std.codec.of()\n'
       '    let (take key, take d) = match described(code.identity(f), Transact, ca, carried) {',
       '    let (take key, take d) = match described(code.identity(f), Transact, ca, cr) {')],
     'l10', 'l10_e_only'),
    ('describe_without_schemas',
     [('args: take args, result: take result)',
       'args: list.new(), result: list.new())')],
     'l12', 'l12'),
    ('direct_commitless',
     [('    let mut answers: List[Answer] = list.new()\n'
       '    result.unwrap(list.push(mut answers, Answer(to: take to, stream: take stream, p: p, outcome: take o)))\n'
       '    let _ = channel.send(hd.out, Release(version: 0u64, snapshot: None, answers: take answers))',
       '    let _ = channel.send(to, Event(e: Answered(Told(version: 0u64, stream: take stream, p: p, outcome: take o))))')],
     'runner_calls', 'calls_duplicate'),
    ('release_at_commit_version',
     [('let Worker(p: p, digest: _, version: _, outcome', 'let Worker(p: p, digest: _, version: wv, outcome'),
      ('Release(version: d, snapshot', 'Release(version: wv, snapshot')],
     'runner_calls', 'calls_group_version'),
    ('queue_outside_account',
     [('                    result.unwrap(map.insert(mut held, take k, take h))\n                    if hit {',
       '                    if !hit || list.len(h.owed) > 0u64 {\n'
       '                        result.unwrap(map.insert(mut held, take k, take h))\n'
       '                    } else {\n'
       '                        used -= 1u64\n'
       '                    }\n'
       '                    if hit {')],
     'runner_calls', 'calls_held_head'),
    ('join_without_digest',
     [('Some(w) => Some((w.p, w.digest == q.digest)),', 'Some(w) => Some((w.p, true)),')],
     'runner_calls', 'calls_joiners'),
    ('session_join_without_digest',
     [('Some(h) => (false, list.len(h.owed), h.digest == d),', 'Some(h) => (false, list.len(h.owed), true),')],
     'runner_calls', 'calls_session_mismatch'),
    ('mismatch_row_in_session',
     [('                if !same {\n                    return Ok(())\n                }\n',
       '                if !same {\n'
       '                    let Call(stream: take stream, p: p, key: _, arg: _, deadline: _) = take c\n'
       '                    tell(s.client, last, take stream, p, Refused(RequestMismatch));\n'
       '                    continue\n'
       '                }\n')],
     'runner_calls', 'calls_session_mismatch'),
    ('late_completion_resent',
     [('                if owed > 0u64 {\n                    last = f.version',
       '                if true {\n                    last = f.version')],
     'runner_calls', 'calls_one_outcome_per_send'),
    ('one_for_joined',
     [('                    for _ in 1u64 .. owed {', '                    for _ in 1u64 .. 1u64 {')],
     'runner_calls', 'calls_one_outcome_per_send'),
    ('latest_deadline',
     [('                        if h.owed[i].generation == t.generation {',
       '                        if h.owed[list.len(h.owed) - 1u64].generation == t.generation {')],
     'runner_calls', 'calls_held_head'),
    ('refused_frames_kept',
     [('    let mut used = 0u64\n', '    let mut used = 0u64\n    let mut kept: List[Bytes] = list.new()\n'),
      ('                    let Call(stream: take stream, p: p, key: _, arg: _, deadline: _) = take c\n'
       '                    tell(s.client, last, take stream, p, take o);',
       '                    let Call(stream: take stream, p: p, key: _, arg: take arg, deadline: _) = take c\n'
       '                    let _ = list.push(mut kept, take arg)\n'
       '                    tell(s.client, last, take stream, p, take o);')],
     'runner_calls', 'calls_session_charge'),
    ('join_outside_account',
     [('                let grows = fresh || owing > 0u64', '                let grows = fresh')],
     'runner_calls', 'calls_held_head'),
)


def p19_module(path):
    lines = ['// Generated by core/tools/runner_tests.py: RUNNER-SPEC p19_place_twice.',
             'type Reg = runner.Registry[(), ()]', '']
    for i in range(max(SIZES)):
        lines.append(f'fn e{i}(take x: u64) -> Result[u64, Capacity] {{ Ok(x + {i}u64) }}')
    for n in SIZES:
        chunks = range(0, n, CHUNK)
        for c in chunks:
            lines += ['', f'fn place_{n}_{c}(mut r: Reg) -> Result[(), runner.RegisterError] {{']
            lines += [f'    runner.compute(mut r, code.describe(e{i}), 65536u64, runner.DEFAULT_STEPS)?' for i in range(c, min(n, c + CHUNK))]
            lines += ['    Ok(())', '}']
        lines += ['', f'fn place_{n}(mut r: Reg) -> Result[(), runner.RegisterError] {{']
        lines += [f'    place_{n}_{c}(mut r)?' for c in chunks]
        lines += ['    Ok(())', '}']
    lines += ['', 'fn described(n: u64, entries: u64) -> Bytes {', '    let made = bound bytes n {',
              '        let mut r: Reg = runner.registry()', '        let placed = match entries {']
    lines += [f'            {n}u64 => place_{n}(mut r),' for n in SIZES]
    lines += ['            _ => Ok(()),', '        }', '        let mut b: Bytes = list.new()',
              '        match placed {', '            Ok(()) => result.unwrap(runner.describe(r, mut b)),',
              '            Error(_) => (),', '        }', '        b', '    }', '    match take made {',
              '        Ok(take b) => take b,', '        Error(_) => list.new(),', '    }', '}', '',
              'type Ask: send { entries: u64 }', '',
              f'fn in_task(take a: Ask) -> Bytes {{ described({N_PLACE}u64, a.entries) }}', '',
              '// Two `place` runs, in two tasks, give equal describe bytes holding every entry.',
              'fn twice(tasks: Tasks, entries: u64) -> bool waits {',
              f'    let mine = described({N_PLACE}u64, entries)',
              f'    let t = match task.spawn(tasks, bytes {2 * N_PLACE}u64, Ask(entries: entries), in_task) {{',
              '        Ok(take t) => take t,', '        Error(_) => return false,', '    }',
              '    let theirs = match task.join(take t) {', '        Ok(take b) => take b,',
              '        Error(_) => return false,', '    }',
              '    let c: std.codec.Codec[runner.Described] = std.codec.of()', '    let mut at = 0u64',
              '    match std.codec.take_list(mine, mut at, c) {',
              '        Ok(ds) => list.len(ds) == entries && at == list.len(mine) && mine == theirs,',
              '        Error(_) => false,', '    }', '}', '',
              'pub fn law_p19_place_twice(tasks: Tasks) -> bool waits {']
    lines += [line for n in SIZES for line in (f'    if !twice(tasks, {n}u64) {{', '        return false', '    }')]
    lines += ['    true', '}', '', '@test', 'fn test_p19_place_twice(tasks: Tasks) -> Result[(), ()] waits {',
              '    if law_p19_place_twice(tasks) { Ok(()) } else { Error(()) }', '}', '']
    path.write_text('\n'.join(lines))


# L12 variants: (name, A's fields, R's fields, registration order).
L12 = (('base', 'a: u64', 'x: u64', ('ea', 'eb')),
       ('permuted', 'a: u64', 'x: u64', ('eb', 'ea')),
       ('a_field', 'a: u64, b: u64', 'x: u64', ('ea', 'eb')),
       ('r_changed', 'a: u64', 'x: u64, y: u32', ('ea', 'eb')))


def l12_module(path, variant, expect=None, revision=None):
    _, a, rr, order = variant
    q = ', '.join(f'{f.split(":")[0].strip()}: {"p.a" if f.startswith("x") else "0u32"}' for f in rr.split(', '))
    lines = ['// Generated by core/tools/runner_tests.py: RUNNER-SPEC L12, server half.',
             'type Reg = runner.Registry[(), ()]', f'type P: codec {{ {a} }}', f'type Q: codec {{ {rr} }}',
             f'fn ea(take p: P) -> Result[Q, Capacity] {{ Ok(Q({q})) }}',
             'fn eb(take x: u64) -> Result[u64, Capacity] { Ok(x) }', '',
             'fn place(mut r: Reg) -> Result[(), runner.RegisterError] {']
    lines += [f'    runner.compute(mut r, code.describe({e}), 65536u64, runner.DEFAULT_STEPS)?' for e in order]
    lines += ['    Ok(())', '}', '', 'fn described() -> Bytes {', '    let mut r: Reg = runner.registry()',
              '    result.unwrap(place(mut r))', '    let mut b: Bytes = list.new()',
              '    result.unwrap(runner.describe(r, mut b))', '    b', '}', '',
              '// The fingerprint of describe, or 0 when two runs in this process disagree.',
              'pub fn law_l12_describe() -> u64 {', '    let b = described()',
              '    if b != described() {', '        return 0u64', '    }', '    std.codec.fingerprint_of(b)', '}', '',
              "// The fingerprint of ea's 32-byte code revision; 0 when it has none or another length.",
              'pub fn law_l12_revision() -> u64 {', '    match code.revision(code.describe(ea)) {',
              '        Some(r) => {', '            if list.len(r) != 32u64 {', '                return 0u64', '            }',
              '            let mut b: Bytes = list.new()', '            result.unwrap(list.append(mut b, r))',
              '            std.codec.fingerprint_of(b)', '        }', '        None => 0u64,', '    }', '}', '']
    if expect is not None:
        lines += ['@test', 'fn test_l12_describe() -> Result[(), ()] {',
                  f'    if law_l12_describe() == {expect}u64 {{ Ok(()) }} else {{ Error(()) }}', '}', '']
    if revision is not None:
        lines += ['@test', 'fn test_l12_revision() -> Result[(), ()] {',
                  f'    if law_l12_revision() == {revision}u64 {{ Ok(()) }} else {{ Error(()) }}', '}', '']
    path.write_text('\n'.join(lines))


# RUNNER-SPEC v2.11.2 L10, E-only case: one transact entry whose error type E keeps its path
# and changes only its fields. code.identity hashes the path, not the fields, so the entry's
# identity is the same in both variants; its result schema covers Result[R, E], so result_fp
# (and with it the Key a Hello names) differs, and a stale Hello cannot match (Gone, never
# Ready). The Hello half is R4's.
L10 = (('base', 'x: u32'), ('e_field', 'x: u32, y: u32'))


def l10_module(path, e):
    lines = ['// Generated by core/tools/runner_tests.py: RUNNER-SPEC L10, E-only case.',
             'type Reg = runner.Registry[(), ()]', f'type E: codec {{ {e} }}',
             'fn te(mut tx: store.Tx, h: (), take a: u64) -> Result[u64, E] { Ok(a) }', '',
             'fn entry() -> Option[runner.Described] {', '    let mut r: Reg = runner.registry()',
             '    result.unwrap(runner.transact(mut r, code.describe(te), 65536u64, runner.DEFAULT_STEPS))',
             '    let mut b: Bytes = list.new()', '    result.unwrap(runner.describe(r, mut b))',
             '    let c: std.codec.Codec[List[runner.Described]] = std.codec.of()',
             '    match std.codec.decode(c, b) {',
             '        Ok(Ok(take ds)) => if list.len(ds) == 1u64 && ds[0].identity == code.identity(code.describe(te)) {',
             '            Some(result.unwrap(clone(ds[0])))', '        } else {', '            None', '        },',
             '        _ => None,', '    }', '}', '',
             "// The fingerprint of te's identity, or 0 when its one Described is not found.",
             'pub fn law_l10_identity() -> u64 {', '    match entry() {',
             '        Some(d) => std.codec.fingerprint_of(d.identity),', '        None => 0u64,', '    }', '}', '',
             "// te's result_fp, or 0 when its one Described is not found.",
             'pub fn law_l10_result() -> u64 {', '    match entry() {',
             '        Some(d) => std.codec.fingerprint_of(d.result),', '        None => 0u64,', '    }', '}', '']
    path.write_text('\n'.join(lines))


def l10_values(evaluator, root, work, tag, runner=None):
    """(identity fp, result fp) per variant, None where a run did not return."""
    values = {}
    for name, e in L10:
        mod = work / f'runner_l10_{tag}_{name}.hirsel'
        l10_module(mod, e)
        m = manifest(root, work, f'l10_{tag}_{name}', runner, [(mod, 'runner_l10')])
        values[name] = tuple(evaluate(evaluator, m, 'store@1/runner_l10/law_l10_' + law)[0] for law in ('identity', 'result'))
    return values


def l10_holds(values):
    (bi, br), (ei, er) = values['base'], values['e_field']
    return None not in (bi, br, ei, er) and 0 not in (bi, br, ei, er) and bi == ei and br != er


# R1b (RUNNER-SPEC v2.10 L1): runner_crash's scenarios, each run at every W2 cut, and
# its poison scenario, whose one failed journal sync the adapter checks in count mode.
CRASHES = ('runner-cross', 'runner-order')
POISON = 'poison-segment'
# - answer_before_durable: the gate releases a group without waiting for it to be durable,
#   so a cut after the commit loses a version a client saw (run at the cuts).
# - poison_answers: the gate releases a group whose durable failed, so a client sees a
#   version the store never kept (run under the poisoned sync alone).
CRASH_MUTANTS = (
    ('answer_before_durable', '        store.durable(mut hd.db, t)?\n', '        let _ = t\n', CRASHES),
    ('poison_answers', '        store.durable(mut hd.db, t)?\n', '        let _ = store.durable(mut hd.db, t)\n', ()),
)


def crashes(args, root, work, engine, name, runner=None, scenarios=CRASHES):
    """L1 at every cut of each scenario, then POISON once; raises on the first failure."""
    inputs_path = manifest(root, work, 'crash_' + name, runner,
                           [(root / 'core/programs/store/runner_crash.hirsel', 'runner_crash')])
    env = dict(os.environ)
    env['LD_LIBRARY_PATH'] = ':'.join([str(args.bridge.resolve().parent),
                                       str(root / 'tools/buck2/toolchains/llvm-files/lib'),
                                       env.get('LD_LIBRARY_PATH', '')])
    build = work / f'crash_{name}'
    build.mkdir()
    rows = run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc, inputs_path, build, engine,
                       entry='store@1/runner_crash/main', scenarios=scenarios, environment=env)
    checked([str(build / (engine + '-crash') / 'store'), POISON, 'count'], env)
    return rows

def manifest(root, work, name, runner=None, extra=()):
    m = inputs(root)
    sources = m['packages'][-1]['sources']
    if runner is not None:
        for s in sources:
            if s['file'].endswith('/runner.hirsel'):
                s['file'] = str(runner)
    for path, module in extra:
        sources.append(dict(file=str(path), modules=[module], signatures=False))
    out = work / (name + '.json')
    out.write_text(json.dumps(m))
    return out


def evaluate(evaluator, inputs_path, definition):
    wait_load()
    result = subprocess.run([str(evaluator), str(inputs_path), definition], capture_output=True, text=True)
    try:
        report = json.loads(result.stdout)
    except ValueError:
        report = {'stdout': result.stdout, 'stderr': result.stderr}
    out = report.get('outcome') or {}
    value = out.get('values', [None])[0] if result.returncode == 0 and out.get('kind') == 'returned' else None
    return value, report


def frame_module(root, path, omit_complete=False):
    """Construct values beside the shared, hand-derived byte oracle, never from it."""
    vectors = json.loads((root / 'core/programs/store/frame-vectors.json').read_text())['vectors']
    helpers = (root / 'core/programs/store/frame-vector-helpers.hirsel').read_text()
    if omit_complete:
        # A deleted field changes construction arity too. Only the value constructors
        # follow that API change; the expected byte strings remain unchanged.
        helpers = helpers.replace('Batch(0x0102030405060708u64, list.new(), ',
                                  'Batch(0x0102030405060708u64, ')
        helpers = helpers.replace('Batch(0x0102030405060708u64, one(7u32), ',
                                  'Batch(0x0102030405060708u64, ')
    lines = [helpers, '', 'pub fn law_frame_vectors() -> bool {']
    for v in vectors:
        payload = bytes.fromhex(v['hex'])
        if 'wire_bytes' in v:
            header = 2 if len(payload) <= 125 else 4 if len(payload) <= 65535 else 10
            assert len(payload) + header == v['wire_bytes'], v['name']
        lines += [f'    // {v["name"]}: {v["derivation"]}',
                  f'    let c_{v["name"]}: std.codec.Codec[runner.{v["type"]}] = std.codec.of()',
                  f'    if !vector(c_{v["name"]}, {v["value"]}, b("{v["hex"]}")) {{ return false }}']
    lines += ['    true', '}', '', '@test', 'fn test_frame_vectors() -> Result[(), ()] {',
              '    if law_frame_vectors() { Ok(()) } else { Error(()) }', '}', '']
    path.write_text('\n'.join(lines))
    return len(vectors)


def frame_vectors(args, root, work, engines, row):
    module = work / 'runner_wire.hirsel'
    count = frame_module(root, module)
    selected = manifest(root, work, 'wire', extra=[(module, 'runner_wire')])
    definition = 'store@1/runner_wire/law_frame_vectors'
    if 'evaluator' in engines:
        value, report = evaluate(args.evaluator.resolve(), selected, definition)
        row(engine='evaluator', law='frame_vectors', vectors=count, passed=value == 1, result=report)
        if not args.no_mutants:
            text = (root / 'core/programs/store/runner.hirsel').read_text()
            for name, old, new in (
                    ('tag_order', 'Hello(Option[Bytes], List[Want]) | Call(u32, u64, u32, u32, Bytes)',
                     'Call(u32, u64, u32, u32, Bytes) | Hello(Option[Bytes], List[Want])'),
                    ('complete_omitted', 'Batch(u64, List[u32], List[Row], List[Done])',
                     'Batch(u64, List[Row], List[Done])'),
                    ('refusal_tag_inserted', 'NotAllowed | RequestMismatch | HistoryUnavailable',
                     'RequestMismatch | NotAllowed | HistoryUnavailable')):
                if text.count(old) != 1:
                    row(engine='evaluator', mutant=name, passed=False, result='mutation site not found once')
                    continue
                mutated = work / f'wire_{name}' / 'runner.hirsel'
                mutated.parent.mkdir()
                mutated.write_text(text.replace(old, new))
                altered_module = work / f'wire_{name}.hirsel'
                frame_module(root, altered_module, omit_complete=name == 'complete_omitted')
                altered = manifest(root, work, f'wire_{name}', mutated, [(altered_module, 'runner_wire')])
                value, report = evaluate(args.evaluator.resolve(), altered, definition)
                row(engine='evaluator', mutant=name, law='frame_vectors', passed=value == 0, result=report)
    for engine in (e for e in engines if e != 'evaluator'):
        if args.compiler is None or args.runtime is None:
            raise ValueError('native frame vectors require --compiler and --runtime')
        definition = 'store@1/runner_wire/test_frame_vectors'
        code, report = native_run(args, engine, selected, work / f'wire-store-{engine}', definition)
        tests = report.get('tests', [])
        row(engine=engine, law='frame_vectors', vectors=count,
            passed=code == 0 and [t['definition'] for t in tests] == [definition]
            and all(t.get('passed') for t in tests), result=report)


def l12_values(evaluator, root, work, tag, runner=None):
    """The fingerprint per variant (base twice), or None where a run did not return."""
    values = {}
    for v in L12 + (('base_again',) + L12[0][1:],):
        mod = work / f'runner_l12_{tag}_{v[0]}.hirsel'
        l12_module(mod, v)
        m = manifest(root, work, f'l12_{tag}_{v[0]}', runner, [(mod, 'runner_l12')])
        values[v[0]], _ = evaluate(evaluator, m, 'store@1/runner_l12/law_l12_describe')
    return values


def l12_revisions(evaluator, root, work):
    """ea's revision fingerprint per variant, over l12_values(.., 'tree')'s manifests."""
    return {v[0]: evaluate(evaluator, work / f'l12_tree_{v[0]}.json', 'store@1/runner_l12/law_l12_revision')[0]
            for v in L12 + (('base_again',),)}


def revision_holds(revisions):
    """PROG-9 by hand: ea has a revision (Some, 32 bytes), which the registration order
    and the run leave alone and which a change to ea's argument or result type moves.
    The native builds then expect the evaluator's value: one revision on every engine."""
    base = revisions['base']
    return (bool(base) and revisions['permuted'] == base == revisions['base_again']
            and all(revisions[v] not in (None, 0, base) for v in ('a_field', 'r_changed')))


def l12_holds(values):
    base = values['base']
    return (bool(base) and values['base_again'] == base
            and all(values[v[0]] not in (None, 0, base) for v in L12[1:]))


def c_compiler(args):
    return args.cc or (Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
                       if 'LLVM_SYS_231_PREFIX' in os.environ else Path(shutil.which('cc')))


def counting_cc(args, root, work, name):
    """A --cc wrapper that links core/tools/runner_count.c as the program's event sink."""
    kind, want = COUNTS[name]
    assert kind == 7, 'runner_count.c counts TRY_SEND'
    cc = c_compiler(args)
    support = work / f'count_{name}.o'
    subprocess.run([str(cc), '-c', '-fPIC', str(root / 'core/tools/runner_count.c'),
                    '-I' + str(root / 'crates/hirsel-core-rt/include'), f'-DTRY_SENDS={want}',
                    *(['-DHUNG_BODY_SENDS=32664'] if name == 'calls_deadline_hung' else []),
                    '-o', str(support)], check=True)
    wrapper = work / f'count_{name}-cc'
    wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc)) + ' "$@" '
                       + shlex.quote(str(support)) + ' -Wl,--wrap=hirsel_core_os_run_with_sources\n')
    wrapper.chmod(0o755)
    return wrapper


def native_run(args, engine, inputs_path, store, prefix, cc=None):
    command = [str(args.compiler.resolve()), 'test', '--inputs', str(inputs_path), '--source-core',
               '--source-std', '--backend', engine, '--runtime', str(args.runtime.resolve()),
               '--store', str(store), '--json', '--filter', prefix,
               '--limit', str(64 * 1024 * 1024), '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random']
    if cc or args.cc:
        command += ['--cc', str(cc or args.cc.resolve())]
    wait_load()
    result = subprocess.run(command, capture_output=True, text=True)
    try:
        report = json.loads(result.stdout)
    except ValueError:
        report = {'stdout': result.stdout, 'stderr': result.stderr}
    return result.returncode, report


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--evaluator', type=Path, required=True)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--observed', type=Path,
                   default=Path(os.environ['HIRSEL_CORE_RT_OBSERVED_ARCHIVES']) / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
                   if 'HIRSEL_CORE_RT_OBSERVED_ARCHIVES' in os.environ else None)
    p.add_argument('--bridge', type=Path, default=os.environ.get('HIRSEL_STORE_CRASH_BRIDGE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm'])
    p.add_argument('--no-mutants', action='store_true')
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    p.add_argument('--part', choices=['r0', 'r1', 'wire'])
    args = p.parse_args()
    r0, r1 = args.part in (None, 'r0'), args.part in (None, 'r1')
    root = args.root.resolve()
    evaluator = args.evaluator.resolve()
    engines = args.backend or ['evaluator', 'cranelift', 'llvm']
    rows = []

    def row(**r):
        rows.append(r)
        print(' '.join(f'{k}={v}' for k, v in r.items() if k != 'result' or r.get('law') == 'l4_peak_retained'), flush=True)

    with tempfile.TemporaryDirectory(prefix='hirsel-runner-laws-') as tmp:
        work = Path(tmp)
        if r0 or args.part == 'wire':
            frame_vectors(args, root, work, engines, row)
        if args.part == 'wire':
            if args.report:
                args.report.write_text(json.dumps(rows, indent=2) + '\n')
            for r in rows:
                if not r['passed']:
                    print('FAIL', json.dumps(r)[:2000])
            return int(any(not r['passed'] for r in rows))
        base = manifest(root, work, 'inputs')
        if r0:
            values = l12_values(evaluator, root, work, 'tree')
            row(engine='evaluator', law='l12', passed=l12_holds(values), result=values)
            revisions = l12_revisions(evaluator, root, work)
            row(engine='evaluator', law='l12_revision', passed=revision_holds(revisions), result=revisions)
            l10 = l10_values(evaluator, root, work, 'tree')
            row(engine='evaluator', law='l10_e_only', passed=l10_holds(l10), result=l10)
        p19 = work / 'runner_p19.hirsel'
        p19_module(p19)
        if 'evaluator' in engines and r0:
            for name in LAWS:
                value, report = evaluate(evaluator, base, 'store@1/runner_laws/law_' + name)
                row(engine='evaluator', law=name, passed=value == 1, result=report)
            value, report = evaluate(evaluator, manifest(root, work, 'p19', None, [(p19, 'runner_p19')]),
                                     'store@1/runner_p19/law_p19_place_twice')
            row(engine='evaluator', law='p19_place_twice', passed=value == 1, result=report)
        if 'evaluator' in engines and r1:
            for name in CALLS:
                value, report = evaluate(evaluator, base, 'store@1/runner_calls/law_' + name)
                row(engine='evaluator', law=name, passed=value == 1 and counted(name, report), result=report)
            for name in SUBS:
                value, report = evaluate(evaluator, base, 'store@1/runner/law_' + name)
                row(engine='evaluator', law=name, passed=value == 1, result=report)
        # A mutant is red only when it builds and its law fails.
        if r0 and not args.no_mutants:
            text = (root / 'core/programs/store/runner.hirsel').read_text()
            for name, edits, module, law in MUTANTS:
                mutated_text = text
                found = True
                for old, new in edits:
                    found = found and mutated_text.count(old) == 1
                    mutated_text = mutated_text.replace(old, new)
                if not found:
                    row(engine='evaluator', mutant=name, passed=False, result='mutation site not found once')
                    continue
                mutated = work / f'mutant_{name}' / 'runner.hirsel'
                mutated.parent.mkdir()
                mutated.write_text(mutated_text)
                if module == 'l10':
                    seen = l10_values(evaluator, root, work, name, mutated)
                    row(engine='evaluator', mutant=name, law=law,
                        passed=None not in sum(seen.values(), ()) and not l10_holds(seen), result=seen)
                    continue
                if module == 'l12':
                    seen = l12_values(evaluator, root, work, name, mutated)
                    row(engine='evaluator', mutant=name, law=law,
                        passed=None not in seen.values() and not l12_holds(seen))
                    continue
                value, report = evaluate(evaluator, manifest(root, work, 'mutant_' + name, mutated),
                                         f'store@1/{module}/law_{law}')
                # Red: the law returns false, or one of its `want` checks fails it (cause Fail only).
                red = (value == 0 or (report.get('outcome') or {}).get('causes') == ['Fail']
                       or (value == 1 and not counted(law, report)))
                row(engine='evaluator', mutant=name, law=law, passed=red, result=report)
        natives = [e for e in engines if e != 'evaluator']
        if natives:
            if args.compiler is None or args.runtime is None or args.observed is None or (r0 and args.bridge is None):
                p.error('native backends require --compiler, --runtime, --observed and --bridge')
            l12 = work / 'runner_l12.hirsel'
            if r0:
                l12_module(l12, L12[0], values['base'], revisions['base'])
            # The same module with another `place` list (registrations permuted), built after
            # the base list in one store and again in a fresh store: the runner is the
            # heaviest user of code.identity, whose dispatch a store once reused across
            # manifests (reuse-history-fault-1031). The oracle is the evaluator's value.
            permuted = work / 'runner_l12_permuted.hirsel'
            if r0:
                l12_module(permuted, L12[1], values['permuted'], revisions['permuted'])
            l12_want = {'store@1/runner_l12/test_l12_describe', 'store@1/runner_l12/test_l12_revision'}
            # p19's place builds as its own program, in the store the laws primed and in a
            # fresh one: at 10,000 entries it reaches over 10,000 functions, each described.
            # R1's calls build alone under --part r1; 'store@1/runner_l' selects runner_laws and runner_l12.
            calls = ('calls', [], 'store@1/runner_calls/', 'shared',
                     {f'store@1/runner_calls/test_{n}' for n in CALLS})
            subs = ('subs', [], 'store@1/runner/', 'shared', {f'store@1/runner/test_{n}' for n in SUBS})
            groups = (calls, subs) if r1 else ()
            groups += () if not r0 else (
                      ('laws', [(l12, 'runner_l12')], 'store@1/runner_l', 'shared',
                       {f'store@1/runner_laws/test_{n}' for n in LAWS} | l12_want),
                      ('p19', [(p19, 'runner_p19')], 'store@1/runner_p19/', 'shared',
                       {'store@1/runner_p19/test_p19_place_twice'}),
                      ('p19_fresh', [(p19, 'runner_p19')], 'store@1/runner_p19/', 'fresh',
                       {'store@1/runner_p19/test_p19_place_twice'}),
                      ('l12_permuted', [(permuted, 'runner_l12')], 'store@1/runner_l12/', 'shared', l12_want),
                      ('l12_permuted_fresh', [(permuted, 'runner_l12')], 'store@1/runner_l12/', 'fresh', l12_want))
            for engine in natives:
                for group, extra, prefix, store, want in groups:
                    native = manifest(root, work, 'native_' + group, None, extra)
                    code, report = native_run(args, engine, native, work / f'store-{engine}-{group if store == "fresh" else store}', prefix)
                    tests = report.get('tests', [])
                    for t in tests:
                        row(engine=engine, test=t['definition'], passed=bool(t.get('passed')), result=t)
                    selected = {t['definition'] for t in tests}
                    row(engine=engine, group=group, selection=f'{len(selected & want)}/{len(want)}',
                        passed=code == 0 and selected == want,
                        result=sorted(want ^ selected) or report.get('stderr', ''))
                for name in COUNTS if r1 else ():
                    definition = f'store@1/runner_calls/test_{name}'
                    code, report = native_run(args, engine, base, work / f'store-{engine}-shared', definition,
                                              counting_cc(args, root, work, name))
                    tests = report.get('tests', [])
                    row(engine=engine, law=name + '_count',
                        passed=code == 0 and [t['definition'] for t in tests] == [definition]
                        and all(t.get('passed') for t in tests),
                        result=tests or report)
                if r0:
                    held, figures = l4_retained(args, root, work, engine)
                    row(engine=engine, law='l4_peak_retained', passed=held, result=figures)
                    try:
                        row(engine=engine, law='l1_crash', passed=True,
                            result=crashes(args, root, work, engine, engine))
                    except RuntimeError as error:
                        row(engine=engine, law='l1_crash', passed=False, result=str(error)[-2000:])
            # A crash mutant is red only when it builds and the L1 floor check fails it.
            if r0 and not args.no_mutants and 'cranelift' in natives:
                text = (root / 'core/programs/store/runner.hirsel').read_text()
                for name, old, new, scenarios in CRASH_MUTANTS:
                    if text.count(old) != 1:
                        row(engine='cranelift', mutant=name, passed=False, result='mutation site not found once')
                        continue
                    mutated = work / f'crash_runner_{name}' / 'runner.hirsel'
                    mutated.parent.mkdir()
                    mutated.write_text(text.replace(old, new))
                    try:
                        crashes(args, root, work, 'cranelift', 'mutant_' + name, mutated, scenarios)
                        red, why = False, 'green'
                    except RuntimeError as error:
                        why = str(error)
                        red = '(L1)' in why
                    row(engine='cranelift', mutant=name, law='l1_crash', passed=red, result=why[-2000:])
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    failures = [r for r in rows if not r['passed']]
    for r in failures:
        print('FAIL', json.dumps(r)[:2000])
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
