#!/usr/bin/env python3
"""Run the runner's named laws and cases (RUNNER-SPEC v2.5 §8) from checkout sources.

Every law runs on the evaluator (a law_ function) and natively on each selected backend
(its test_ twin): R0's in core/programs/store/runner_laws.hirsel, R1's in
core/programs/store/runner_calls.hirsel, and two generated modules, runner_p19 (one
`place` of 1, 100, 1,000 and 10,000 entries, run in two tasks) and runner_l12 (describe across permuted
registrations, an added A field and a changed R; and ea's code revision, which every
engine must give alike, PROG-9). Each mutant applies its text changes
to a copy of runner.hirsel and must turn its law red. Every control runs on the evaluator
with its explicit model network grant. L4's high-water
case builds core/programs/store/runner_rss.hirsel per native backend and compares
the runtime's exact peak of live backing bytes across its two modes. L20's builds
core/programs/store/runner_cover.hirsel the same way and compares the live backing bytes
at its "subscribed" checkpoint across a 10-row and a 1,954-row Cover; its mutant runs on
Cranelift only.

--part r1 runs R1's calls (transactions at durable versions, runs-once, store refusal)
and their native twins. --part r0 runs R0's laws, p19, l12, L4, R1b's L1, the
wire frame vectors, and every mutant. --part wire selects only
frame_vectors and its evaluator mutants. Without --part both R0 and R1 run.

R1b's L1 builds core/programs/store/runner_crash.hirsel per native backend under the
store's W2 crash adapter (core/tools/store_crash_tests.py): every cut of each scenario,
and one failed journal sync. Its two mutants need that adapter, so they run natively, on
Cranelift only, and are red when the L1 floor check fails them.

RC1's L31 uses the disk store and WebSocket client in runner_crash.hirsel.
The native law runs in --part r1. --part rc1 selects it and, on Cranelift,
restores the old stream key for the mutation control.
"""
import argparse
import base64
import json
import shlex
import os
import sys
from pathlib import Path
import shutil
import subprocess
import tempfile

from std_native import MODEL_NET_RULES, ROOT, wait_load
from store_tests import inputs
from store_crash_tests import checked, run_crashes


LAWS = ('granted_tasks_copy_steps', 'p01_call', 'p02_start_finish', 'p09_describe', 'p13_duplicate_refused', 'p15_types_roundtrip',
        'p19_place_bound', 'l4_malformed_flood', 'step_bounds')
SIZES = (1, 100, 1000, 10000)
# place_n registers in chunks of CHUNK calls, one function each, so no one function's
# frame grows with n (N13(b): a frame holds about 3,000 calls).
CHUNK = 500
# The bound one `place` and its describe run in: 10,000 entries take about 12 MiB on the
# evaluator (the registry's map grows to 2 MiB while its old table is live).
N_PLACE = 16 * 1024 * 1024

CALLS = ('release_digest', 'ready_after_activation', 'drain_answers_once', 'drain_reopened', 'lag_window', 'calls_two_streams', 'calls_group_version', 'calls_joiners', 'calls_duplicate', 'calls_refusals',
         'calls_runs_once', 'calls_session_charge', 'calls_store_refusal', 'calls_one_outcome_per_send', 'calls_deadline_held', 'calls_deadline_hung', 'calls_held_head', 'calls_retired_head', 'l14_handles', 'calls_session_mismatch', 'calls_recorded', 'subs_wired', 'publisher_never_blocks', 'owed_ended_delivered')

# L3 (RUNNER-SPEC v2.6): execution counts read from CONC-10 events, (event kind, count).
# TRY_SEND is kind 7 in crates/hirsel-core-layout/src/event.rs. calls_runs_once
# reaches tally once and receives four answers. The session sends one retirement
# wake to the head, so the total is 1 + 4 + 1. The evaluator's count comes from
# its event log; natively, core/tools/runner_count.c is linked into the case's test
# executable as its event sink and fails the run on any other count.
# L7 adds32664 sends by the hung body and one marker by the other session's
# compute body; the marker precedes the head's exhaustion slice (kind21). The head's three
# answers to s0 go to one inbox. Its head retirement wake follows the slice.
COUNTS = {'calls_runs_once': (7, 6), 'calls_deadline_hung': (7, 32669)}
PLACEMENT_OBSERVERS = {'ready_after_activation': 'READY_BEFORE_ACCEPT', 'lag_window': 'LAG_ADMISSION'}
HUNG_TOLDS = 3

# Wire floods retain all 10,000 frames. The evaluator's generic 1B test envelope
# predates framing. At 1/100 frames, refusals use 8,148,075/20,157,978 steps,
# session_charge 80,393,142/325,732,340. Extrapolate the 99-frame difference
# to 10,000, double it for margin, then round up. Entry step bounds stay fixed.
WIRE_TEST_INSTRUCTIONS = {
    'store@1/runner_calls/law_calls_refusals': 2_500_000_000,
    'store@1/runner_calls/law_calls_session_charge': 50_000_000_000,
}

# R2's publisher laws, in runner.hirsel itself (RUNNER-SPEC §8.2).
SUBS = ('subs_main', 'subs_reset_keeps_diff', 'rows_normalized', 'projection_isolated', 'subscribe_order', 'retired_without_waiting')


def counted(name, report):
    if name not in COUNTS:
        return True
    kind, want = COUNTS[name]
    events = report.get('events', [])
    sources = report.get('event_sources', [])
    if len(sources) != len(events) or any(e[0] == kind and site is None for e, site in zip(events, sources)):
        return False
    # Match the body/publisher/compute-marker observation of runner_count.c.
    # Keep other event kinds in place for the head's slice-order assertion.
    observed = {'store@1/runner_calls/tally', 'store@1/runner_calls/hung',
                'store@1/runner_calls/answer', 'store@1/runner/offer'}
    retirement = set()
    attributed = []
    for e, site in zip(events, sources):
        if e[0] == 18:  # LATEST_PUBLISH. A closing owner publishes its dead head lease.
            retirement.add(e[1])
        wakes_head = site == 'store@1/runner/try_head' and e[1] in retirement
        if e[0] != kind or site in observed or wakes_head:
            attributed.append(e)
    events = attributed
    if sum(1 for e in events if e[0] == kind) != want:
        return False
    if name == 'calls_deadline_hung':
        sends = {}
        for at, e in enumerate(events):
            if e[0] == 7:
                sends.setdefault(e[2], []).append((at, e[1]))
        if sorted(map(len, sends.values())) != [1, 1, HUNG_TOLDS, 32664]:
            return False
        told = next(v for v in sends.values() if len(v) == HUNG_TOLDS)
        marker, retirement = sorted(v[0] for v in sends.values() if len(v) == 1)
        body = next(v for v in sends.values() if len(v) == 32664)
        slices = [(at, e[1]) for at, e in enumerate(events) if e[0] == 21]
        return (len(slices) == 1 and marker[0] < slices[0][0] < retirement[0]
                and marker[1] != slices[0][1] == body[0][1]
                and all(t != slices[0][1] for _, t in told))
    return True


# L4 (RUNNER-SPEC v2.5 §8): runner_rss.hirsel's program, built per native backend against
# the charge-observation runtime and run in two modes, one valid call (0) and 10,000
# malformed frames before it (1). Both exit 0 and their --logical-charge-report
# peak_retained_bytes (the runtime's exact peak of live backing requests) are equal: a
# refused frame that leaves any bytes behind raises mode 1's peak. ru_maxrss, the
# earlier figure, moves by 256 KB steps from run to run and cannot see a small leak.
def charged(args, root, work, engine, module, modes, figure, cc=None, runner=None, name=None):
    """Build programs/store/<module>.hirsel on the charge-observation runtime and run it in
    each (argv, label) mode: {label: (exit status, figure(charge report))}, or None and the
    build's error."""
    name = name or module
    inputs_path = manifest(root, work, name, runner, [(root / f'core/programs/store/{module}.hirsel', module)])
    binary = work / f'{name}_{engine}'
    charges = work / f'{name}_{engine}.json'
    command = [str(args.compiler.resolve()), 'build', '--inputs', str(inputs_path), '--source-core',
               '--source-std', '--backend', engine, '--runtime', str(args.observed.resolve()),
               '--store', str(work / f'store-{name}-{engine}'), '--grant', 'Clock', '--grant', 'Tasks',
               '--grant', 'Random', '--entry', f'store@1/{module}/main',
               '--logical-charge-report', str(charges), '-o', str(binary)]
    if module in ('runner_rss', 'runner_cover'):
        command += ['--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535']
        if engine == 'llvm':
            command.append('--release')
    if cc or args.cc:
        command += ['--cc', str(cc or args.cc.resolve())]
    wait_load()
    built = subprocess.run(command, capture_output=True, text=True)
    if built.returncode != 0:
        return None, built.stderr[-2000:]
    figures = {}
    rss = {}
    for mode, label in modes:
        charges.unlink(missing_ok=True)
        command = [str(binary), mode]
        if module == 'runner_rss':
            peak = work / f'{name}_{engine}_{label}.rss'
            command = ['/usr/bin/time', '-f', '%M', '-o', str(peak), *command]
        run = subprocess.run(command, capture_output=True)
        figures[label] = (run.returncode, figure(json.loads(charges.read_text())) if charges.exists() else None)
        if run.returncode:
            figures.setdefault('errors', {})[label] = run.stderr.decode(errors='replace')[-2000:]
        if module == 'runner_rss':
            rss[label] = int(peak.read_text().splitlines()[-1])
    if rss:
        figures['rss_kib'] = rss
    return figures, None


def l4_retained(args, root, work, engine):
    figures, error = charged(args, root, work, engine, 'runner_rss', (('0', 'valid'), ('1', 'flood')),
                             lambda r: r['peak_retained_bytes'])
    if figures is None:
        return False, {'build': error}
    held = (figures['valid'][0] == 0 and figures['flood'][0] == 0 and figures['valid'][1] is not None
            and figures['flood'][1] == figures['valid'][1])
    return held, figures


# L20 (RUNNER-SPEC v2.11 §8, cover_only_retention): runner_cover.hirsel's program, linked
# with core/tools/runner_mark.c, which records the checkpoint "subscribed" when the client
# has dropped its initial Batch. The program stores 1,954 rows and holds one subscription,
# whose Cover spans 10 rows (mode 0) or all 1,954 (mode 1). Both exit 0 and the
# checkpoint's retained_bytes (live backing requests at that moment) are equal: a
# subscription that keeps a copy of its covered rows raises mode 1's.
def subscribed_bytes(report):
    marks = [p for p in report['phases'] if p['name'] == 'subscribed']
    return marks[0]['retained_bytes'] if len(marks) == 1 else None


def l20_retained(args, root, work, engine, runner=None, name='runner_cover'):
    cc = c_compiler(args)
    support = work / f'{name}_mark_{engine}.o'
    subprocess.run([str(cc), '-c', '-fPIC', str(root / 'core/tools/runner_mark.c'),
                    '-I' + str(root / 'crates/hirsel-core-rt/include'), '-o', str(support)], check=True)
    wrapper = work / f'{name}_mark_{engine}-cc'
    wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc)) + ' "$@" '
                       + shlex.quote(str(support)) + ' -Wl,--wrap=hirsel_core_os_run_with_sources\n')
    wrapper.chmod(0o755)
    figures, error = charged(args, root, work, engine, 'runner_cover', (('0', 'ten'), ('1', 'all')),
                             subscribed_bytes, wrapper, runner, name)
    if figures is None:
        return None, {'build': error}
    ran = figures['ten'][0] == 0 and figures['all'][0] == 0 and figures['ten'][1] is not None and figures['all'][1] is not None
    return ran and figures['ten'][1] == figures['all'][1], figures if ran else {'run': figures}


# copy_rows_per_sub (L20): a subscription that keeps a copy of its initial rows beside its Cover.
COVER_MUTANTS = (
    ('copy_rows_per_sub',
     [('type Sub { session: u64, sub: u32, generation: u64, key: Bytes, arg: Bytes, cover: Cover, reads: List[store.Read] }',
       'type Sub { session: u64, sub: u32, generation: u64, key: Bytes, arg: Bytes, cover: Cover, reads: List[store.Read], kept: List[Row] }'),
      ('cover: take c, reads: take reads)))',
       'cover: take c, reads: take reads, kept: result.unwrap(clone(o.rows)))))')]),
)


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
     [('            projection_row(out, sub, take removed, None)?\n', '')],
     'runner', 'subs_main'),
    # R5 (RUNNER-SPEC §7): an aborted run's reply kept without its call record (STORE G1),
    # and records that name no revision.
    ('abort_unrecorded',
     [('    match answered(mut tx, q, o, revision) {\n        Ok(()) => Error(Alone(take o)),',
       '    match reply(mut tx, q.stream, q.p, o) {\n        Ok(()) => Error(Alone(take o)),')],
     'runner_calls', 'calls_recorded'),
    ('record_without_revision',
     [('    let rev = match revision {\n        Some(b) => Some(copy_bytes(b)?),\n',
       '    let rev: Option[Bytes] = match revision {\n        Some(_) => None,\n')],
     'runner_calls', 'calls_recorded'),
    # L22 (subscribe_order, v2.12), each a Subscribe not at its place in PubIn's order:
    # second_receiver, a publisher that reads a Subscribe queued behind a Release before
    # applying that Release, as a second receiver for Subscribes would (it peeks only while
    # it has no subs, so it never drops the law's Halt); subscribe_past_queue,
    # a Subscribe that runs past the Releases queued behind it, at the newest version (the
    # spec's name for it, subscribe_at_latest, is subs_main's Board-level mutant above).
    ('second_receiver',
     [('                let Release(version: v, snapshot: take snap, answers: take answers) = take rel\n',
       '                let Release(version: v, snapshot: take snap, answers: take answers) = take rel\n'
       '                if list.len(b.subs) == 0u64 {\n'
       '                    match channel.try_receive(from) {\n'
       '                        Ok(take ahead) => {\n'
       '                            let PubIn(e: take next) = take ahead\n'
       '                            match take next {\n'
       '                                Subscribed(who, sub, take key, take arg) => {\n'
       '                                    publish_subscription(mut b, mut joined, mut owes, who, sub, 0u64, take key, take arg)\n'
       '                                },\n'
       '                                _ => (),\n'
       '                            }\n'
       '                        },\n'
       '                        _ => (),\n'
       '                    }\n'
       '                }\n')],
     'runner', 'subscribe_order'),
    ('subscribe_past_queue',
     [('                publish_subscription(mut b, mut joined, mut owes, who, sub, 0u64, take key, take arg)\n',
       '                match channel.try_receive(from) {\n'
       '                    Ok(take ahead) => {\n'
       '                        let PubIn(e: take next) = take ahead\n'
       '                        match take next {\n'
       '                            Released(take rel) => {\n'
       '                                let Release(version: v, snapshot: take snap, answers: _) = take rel\n'
       '                                match take snap {\n'
       '                                    Some(take s) => {\n'
       '                                        at = v\n'
       '                                        b.newest = Some(take s)\n'
       '                                        owes = round(mut b)\n'
       '                                    },\n'
       '                                    None => (),\n'
       '                                }\n'
       '                            },\n'
       '                            _ => (),\n'
       '                        }\n'
       '                    },\n'
       '                    Error(_) => (),\n'
       '                }\n'
       '                publish_subscription(mut b, mut joined, mut owes, who, sub, 0u64, take key, take arg)\n')],
     'runner', 'subscribe_order'),
    # L24 (owed_ended_delivered, v2.13): the owner never wakes the publisher, so a waiting
    # Ended needs other traffic; and an owner that wakes it once per session, then never.
    ('owed_ended_waits_for_traffic',
     [
         ('            if let Some(_) = work.head { let _ = channel.try_send(wire_publisher(s), PubIn(e: Drained)) }',
          ''),
         ('    let _ = channel.try_send(owner.post, PubIn(e: Drained))',
          '')
     ],
     'runner_calls', 'owed_ended_delivered'),
    ('wake_once',
     [
         ('    if run == capacity { run = 0u64; true } else { false }',
          '    if run == capacity { run = capacity + 1u64; true } else { false }')
     ],
     'runner_calls', 'owed_ended_delivered'),
    # L23 (publisher_never_blocks): the publisher sends to an owner and waits.
    ('publisher_blocks_on_full_session',
     [
         ('fn offer(to: Sender[WireEvent], take e: Notice) -> Offered {\n    match channel.try_send(to, wire_event(FromRunner(take e))) {\n        Ok(()) => Took,\n        Error(Full(WireEvent(e: FromRunner(take back)))) => Bounced(take back),\n        _ => Shut,\n    }\n}\n',
          'fn offer(to: Sender[WireEvent], take e: Notice) -> Offered waits {\n    match channel.send(to, wire_event(FromRunner(take e))) {\n        Ok(()) => Took,\n        Error(_) => Shut,\n    }\n}\n'),
         ('take owes: List[Owe], at: u64) {', 'take owes: List[Owe], at: u64) -> () waits {')
     ],
     'runner_calls', 'publisher_never_blocks'),
    # L25 (retired_without_waiting, v2.14): a marked session's end sends its Drained and waits
    # for a PubIn slot; the publisher sweeps only after a Released, so a quiet place keeps a
    # dead session; the sweep reads Empty as an end, retiring a live session.
    ('retire_waits_for_slot',
     [
         ('fn wire_finish_owner[C: send](mut service: Service[C], pub_to: Sender[PubIn]) {',
          'fn wire_finish_owner[C: send](mut service: Service[C], pub_to: Sender[PubIn]) -> () waits {'),
         ('    if let Some(take left) = (service.finish)(mut service.context) { let _ = channel.try_send(pub_to, take left) }',
          '    if let Some(take left) = (service.finish)(mut service.context) { let _ = channel.send(pub_to, take left) }')
     ],
     'runner', 'retired_without_waiting'),
    ('sweep_skipped_when_quiet',
     [('        sweep(mut b.subs, mut joined)\n        settle(', '        settle('),
      ('                keep_answers(mut waiting, take answers)\n',
       '                keep_answers(mut waiting, take answers)\n                sweep(mut b.subs, mut joined)\n')],
     'runner', 'retired_without_waiting'),
    ('sweep_retires_live',
     [('                Error(Closed) => true,\n                _ => false,\n            },\n            None => false,',
       '                Error(_) => true,\n                _ => false,\n            },\n            None => false,')],
     'runner', 'retired_without_waiting'),
    # L25: only the session's own end code offers a Drained, so a failed or cancelled
    # session in a quiet place is kept.
    ('no_joiner_wake',
     [('    let _ = channel.try_send(to_publisher, PubIn(e: Drained))\n', '')],
     'runner', 'retired_without_waiting'),
    # L17 on the wire (v2.12's one publisher inbox): an initial result split from the Batch
    # that names its sub, and an Unsubscribe the publisher ignores.
    ('initial_split',
     [('        let _ = channel.send(to, Batched(KeyedBatch(version: v, complete: take complete, rows: take heard, outcomes: take outcomes)))\n',
       '        let _ = channel.send(to, Batched(KeyedBatch(version: v, complete: take complete, rows: list.new(), outcomes: take outcomes)))\n'
       '        let _ = channel.send(to, Batched(KeyedBatch(version: v, complete: list.new(), rows: take heard, outcomes: list.new())))\n')],
     'runner_calls', 'subs_wired'),
    ('unsubscribe_ignored',
     [
         ('                wire_stop_sub(mut s, mut work, sub)',
          '                let _ = sub')
     ],
     'runner_calls', 'subs_wired'),
    # L17 (RUNNER-SPEC §8.2, complete_marks_empty): an empty initial result names its sub in
    # no Batch's complete, so the client never learns that sub is current.
    ('no_complete_on_empty',
     [('            result.unwrap(list.push(mut o.complete, sub))\n',
       '            if list.len(o.rows) > 0u64 {\n                result.unwrap(list.push(mut o.complete, sub))\n            }\n')],
     'runner', 'subs_main'),
    # L16 (RUNNER-SPEC §8.2): a row's value is the published value (F10), never the raw
    # stored bytes, nor stored bytes with a fixed two-byte header stripped.
    ('raw_value',
     [('{ store.published_value(mine, c) }',
       '{\n        if list.len(c.tree_key) == 0u64 {\n            return store.published_value(mine, c)\n        }\n'
       '        match c.value {\n            Some(v) => Ok(Some(result.unwrap(clone(v)))),\n            None => Ok(None),\n        }\n    }')],
     'runner', 'rows_normalized'),
    ('strip_two_bytes',
     [('{ store.published_value(mine, c) }',
       '{\n        if list.len(c.tree_key) == 0u64 {\n            return store.published_value(mine, c)\n        }\n'
       '        match c.value {\n            Some(v) => Ok(Some(result.unwrap(clone(list.slice(v, 2u64, list.len(v)))))),\n            None => Ok(None),\n        }\n    }')],
     'runner', 'rows_normalized'),
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
       '    result.unwrap(list.push(mut answers, Answer(recipient: Recipient(owner: owner, to: take to), row: KeyedDone(stream: take stream, p: p, outcome: take o))))\n'
       '    let _ = channel.send(hd.out, PubIn(e: Released(Release(version: 0u64, snapshot: None, answers: take answers))))',
       '    let mut rows: List[KeyedDone] = list.new()\n'
       '    result.unwrap(list.push(mut rows, KeyedDone(stream: take stream, p: p, outcome: take o)))\n'
       '    let e = update(0u64, None, take rows)\n'
       '    let _ = channel.send(to, wire_event(FromRunner(take e)))')],
     'runner_calls', 'calls_duplicate'),
    ('release_at_commit_version',
     [('    let d = store.version(store.snapshot(hd.db))', '    let mut d = store.version(store.snapshot(hd.db))'),
      ('let Worker(p: p, digest: _, version: _, outcome: take o, waiters: take waiters) = take w',
       'let Worker(p: p, digest: _, version: wv, outcome: take o, waiters: take waiters) = take w\n'
       '        if stream == group[0u64] { d = wv }')],
     'runner_calls', 'calls_group_version'),
    ('queue_outside_account',
     [
         ('        units += if h.forwarded && live == 0u64 { 1u64 } else { live }',
          '        units += live')
     ],
     'runner_calls', 'calls_held_head'),
    ('join_without_digest',
     [('Some(w) => Some((w.p, w.digest == q.digest)),', 'Some(w) => Some((w.p, true)),')],
     'runner_calls', 'calls_joiners'),
    ('session_join_without_digest',
     [
         ('(h.forwarded, h.digest == sent.digest, h.queued)',
          '(h.forwarded, true, h.queued)'),
         ('            if digest != store.request_digest(key, arg) { return Some(1008u16) }',
          '            if false { return Some(1008u16) }')
     ],
     'runner_calls', 'calls_session_mismatch'),
    ('mismatch_row_in_session',
     [
         ('    if forwarded && !same { return Error(1008u16) }',
          '    if forwarded && !same { return wire_refused(mut s, mut work, stream, p, RequestMismatch) }'),
         ('            if digest != store.request_digest(key, arg) { return Some(1008u16) }',
          '            if false { return Some(1008u16) }')
     ],
     'runner_calls', 'calls_session_mismatch'),
    ('late_completion_resent',
     [
         ('        let owed = replace(mut h.owed, list.new())\n        let count = list.len(owed)',
          '        let mut owed = replace(mut h.owed, list.new())\n        if list.len(owed) == 0u64 { result.unwrap(list.push(mut owed, WireOwed(generation: 0u64))) }\n        let count = list.len(owed)')
     ],
     'runner_calls', 'calls_one_outcome_per_send'),
    ('one_for_joined',
     [
         ('        for take owing in owed {\n',
          '        let mut sent_one = false\n        for take owing in owed {\n            if sent_one { continue }\n            sent_one = true\n')
     ],
     'runner_calls', 'calls_one_outcome_per_send'),
    ('latest_deadline',
     [
         ('            if h.owed[index].generation == tick.generation {',
          '            if h.owed[list.len(h.owed) - 1u64].generation == tick.generation {')
     ],
     'runner_calls', 'calls_held_head'),
    ('refused_frames_kept',
     [
         ('    held: Map[Bytes, WireHeld], refusals:',
          '    kept: List[Bytes], held: Map[Bytes, WireHeld], refusals:'),
         ('asking: false, pending: None, held: map.new()',
          'asking: false, pending: None, kept: list.new(), held: map.new()'),
         ('    if !allowed { return wire_refused(mut s, mut work, stream, p, Overloaded) }',
          '    result.unwrap(list.push(mut s.kept, result.unwrap(clone(arg))))\n    if !allowed { return wire_refused(mut s, mut work, stream, p, Overloaded) }')
     ],
     'runner_calls', 'calls_session_charge'),
    ('join_outside_account',
     [
         ('Some(h) => !h.forwarded || h.queued != 0u64 || list.len(h.owed) != 0u64, _ => true',
          'Some(h) => !h.forwarded, _ => true')
     ],
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
                       entry='store@1/runner_crash/main', scenarios=scenarios, environment=env,
                       adapter='runner_crash.c')
    checked([str(build / (engine + '-crash') / 'store'), POISON, 'count'], env)
    return rows

def drain_deadline(args, root, work, engine, name, runner=None, activation=False):
    inputs_path = manifest(root, work, name, runner,
                           [(root / 'core/programs/store/runner_crash.hirsel', 'runner_crash')])
    build = work / name
    build.mkdir()
    return run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc, inputs_path, build, engine,
                       entry='store@1/runner_crash/main', adapter='runner_crash.c',
                       scenarios=('runner-activation',) if activation else (),
                       count_only=() if activation else ('runner-deadline',))


def no_secret_at_rest(args, root, work, engine, row, mutant=False):
    name = 'token_in_stream_key' if mutant else 'no_secret_at_rest'
    source = root / 'core/programs/store'
    fixture = [(source / 'runner_crash.hirsel', 'runner_crash')]
    runner = None
    if mutant:
        # Restore the v2.14 stream key: the full Hello token, then the stream.
        ingress = source / 'ingress.hirsel'
        text = ingress.read_text()
        lo = text.index('    let tag =', text.index('pub fn caller('))
        hi = text.index('    Ok(Some(runner.Caller(', lo)
        changed_ingress = work / 'token_ingress.hirsel'
        changed_ingress.write_text(text[:lo] + '    let subject: Bytes = clone(token)?\n' + text[hi:])
        text = (source / 'runner.hirsel').read_text()
        old = '    caller_key(c, mut stream)?'
        assert text.count(old) == 1, name
        runner = work / 'token_runner.hirsel'
        runner.write_text(text.replace(old, '    list.append(mut stream, c.subject)?'))
    selection = manifest(root, work, name, runner, fixture)
    if mutant:
        inputs_data = json.loads(selection.read_text())
        for entry in inputs_data['packages'][-1]['sources']:
            if entry['modules'] == ['ingress']:
                entry['file'] = str(changed_ingress)
        selection.write_text(json.dumps(inputs_data))
    directory = work / f'{name}-{engine}-disk'
    directory.mkdir()
    binary = work / f'{name}-{engine}'
    command = [str(args.compiler.resolve()), 'build', '--inputs', str(selection), '--source-core',
               '--source-std', '--entry', 'store@1/runner_crash/no_secret_at_rest', '--backend', engine,
               '--runtime', str(args.runtime.resolve()), '--store', str(work / f'l31-objects-{engine}'),
               '--limit', str(64 * 1024 * 1024), '--dir', str(directory), '-o', str(binary),
               '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random',
               '--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535']
    if args.cc:
        command += ['--cc', str(args.cc.resolve())]
    wait_load()
    checked(command, os.environ)
    nonce = os.urandom(16)
    encoded = base64.b64encode(b'anon:' + nonce).decode('ascii')
    ran = subprocess.run([str(binary), encoded], capture_output=True, text=True)
    files = [p for p in directory.rglob('*') if p.is_file()]
    found = sorted(str(p.relative_to(directory)) for p in files if nonce in p.read_bytes())
    passed = ran.returncode == 0 and bool(files) and bool(found) == mutant
    row(engine=engine, law='no_secret_at_rest', mutant=name if mutant else None, passed=passed,
        result={'exit': ran.returncode, 'files': len(files), 'matches': found, 'stderr': ran.stderr})


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
    request = dict(manifest=str(inputs_path), entry=definition, model_net=MODEL_NET_RULES)
    if definition in WIRE_TEST_INSTRUCTIONS:
        request['instructions'] = WIRE_TEST_INSTRUCTIONS[definition]
    result = subprocess.run([str(evaluator)], input=json.dumps(request) + '\n', capture_output=True, text=True)
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



def children_joined(report):
    events = report.get('events', [])
    children = [e[2] for e in events if e[0] == 0 and e[1] == 0]
    cancellations = [i for i, e in enumerate(events) if e[0] == 2 and e[1] == 0 and e[2] in children]
    joins = [i for i, e in enumerate(events) if e[0] == 4 and e[1] == 0 and e[2] in children]
    return (len(children) == len(cancellations) == len(joins) == 2
            and max(cancellations) < min(joins))


def session_laws(args, root, work, engines, row):
    text = (root / 'core/programs/store/runner.hirsel').read_text()
    laws = (root / 'core/programs/store/session-laws.hirsel').read_text()
    adapted = work / 'session' / 'runner.hirsel'
    adapted.parent.mkdir()
    adapted.write_text(text + '\n' + laws)
    selected = manifest(root, work, 'session', adapted)
    names = args.session_law or ('pool_holds_writer_bytes', 'writer_one_frame', 'children_joined_on_close', 'session_records', 'serving_budget', 'subscription_room_refunded')
    if 'evaluator' in engines:
        for name in names:
            value, report = evaluate(args.evaluator.resolve(), selected, 'store@1/runner/law_' + name)
            holds = value == 1 and (name != 'children_joined_on_close' or children_joined(report))
            row(engine='evaluator', law=name, passed=holds, result=report)
    for engine in (e for e in engines if e != 'evaluator'):
        definition = 'store@1/runner/test_wire_session'
        code, report = native_run(args, engine, selected, work / f'session-store-{engine}', definition)
        tests = report.get('tests', [])
        row(engine=engine, law='wire_session', passed=code == 0
            and [t['definition'] for t in tests] == [definition]
            and all(t.get('passed') for t in tests), result=report)
    if args.no_mutants:
        return
    mutants = (
        ('encode_outside_pool', [('    if let Error(_) = channel.try_send(pool, Encode(take frame, channel.sender(to))) { return Error(Protocol(1013u16)) }',
          '    let mut out = result.unwrap(std.wsock.outbound(Server, std.wsock.defaults(), None))\n'
          '    let payload = wire_payload(take frame, 262144u64)?\n'
          '    return std.wsock.encode(mut out, true, 2u8, payload)')], 'pool_holds_writer_bytes'),
        ('subscription_refund_on_handoff', [
          ('    Ok(SubscriptionInput(room: Some(take lease), key: take key, arg: take arg))',
           '    let _ = take lease\n    Ok(SubscriptionInput(room: None, key: take key, arg: take arg))')],
          'subscription_room_refunded'),
        ('writer_queue', [('fn wire_ready(busy: bool) -> bool { !busy }', 'fn wire_ready(busy: bool) -> bool { true }')], 'writer_one_frame'),
        ('orphan_reader', [('    task.cancel(mut reader)\n    task.cancel(mut writer)\n'
          '    let _ = task.join(take reader)\n    let _ = task.join(take writer)',
          '    task.cancel(mut writer)\n    let _ = task.join(take writer)')], 'children_joined_on_close'),
    )
    for name, edits, law in mutants:
        if args.session_mutant and name not in args.session_mutant:
            continue
        altered = text
        for old, new in edits:
            if altered.count(old) != 1:
                row(engine='evaluator', mutant=name, passed=False, result='mutation site not found once')
                break
            altered = altered.replace(old, new)
        else:
            changed = work / name / 'runner.hirsel'
            changed.parent.mkdir()
            changed.write_text(altered + '\n' + laws)
            value, report = evaluate(args.evaluator.resolve(), manifest(root, work, name, changed),
                                     'store@1/runner/law_' + law)
            red = (value == 0 or (report.get('outcome') or {}).get('causes') == ['Fail']
                   or value == 1 and law == 'children_joined_on_close' and not children_joined(report))
            row(engine='evaluator', mutant=name, law=law, passed=red, result=report)


def ingress_laws(args, root, work, engines, row):
    names = ('limits', 'rates_and_expiry', 'retirement_steps', 'reply_isolated')
    selected = manifest(root, work, 'ingress')
    if 'evaluator' in engines:
        for name in names:
            value, report = evaluate(args.evaluator.resolve(), selected, 'store@1/ingress/law_' + name)
            row(engine='evaluator', law='ingress_' + name, passed=value == 1, result=report)
    for engine in (e for e in engines if e != 'evaluator'):
        definition = 'store@1/ingress/test_ingress'
        code, report = native_run(args, engine, selected, work / f'ingress-store-{engine}', definition)
        tests = report.get('tests', [])
        row(engine=engine, law='ingress', passed=code == 0
            and [t['definition'] for t in tests] == [definition]
            and all(t.get('passed') for t in tests), result=report)
    if args.no_mutants or args.part != 'ingress':
        return
    text = (root / 'core/programs/store/ingress.hirsel').read_text()
    mutants = (
        ('no_ip_session_limit', 'if s.free == 0u64 { RejectedPolicy }', 'if false { RejectedPolicy }', 'limits'),
        ('no_call_rate', 'if b.credit < period { return false }', 'if b.credit < period { return true }', 'rates_and_expiry'),
        ('expiry_resets_active', 'if full {', 'if true {', 'rates_and_expiry'),
    )
    for name, old, new, law in mutants:
        assert text.count(old) == 1, name
        changed = work / (name + '.hirsel')
        changed.write_text(text.replace(old, new))
        description = inputs(root)
        for source in description['packages'][-1]['sources']:
            if source['modules'] == ['ingress']:
                source['file'] = str(changed)
        altered = work / (name + '.json')
        altered.write_text(json.dumps(description) + '\n')
        value, report = evaluate(args.evaluator.resolve(), altered, 'store@1/ingress/law_' + law)
        red = value == 0 or (report.get('outcome') or {}).get('causes') in (['Fail'], ['Overflow'])
        row(engine='evaluator', mutant=name, law='ingress_' + law, passed=red, result=report)


def admission_laws(args, root, work, engines, row):
    text = (root / 'core/programs/store/runner.hirsel').read_text()
    laws = (root / 'core/programs/store/admission-laws.hirsel').read_text()
    adapted = work / 'admission-runner.hirsel'
    adapted.write_text(text + '\n' + laws)
    selected = manifest(root, work, 'admission-laws', adapted)
    durable = ('gone_slot_v5', 'request_mismatch_wire', 'history_below_latest', 'reply_over_done_max')
    state = ('unread_client', 'hello_and_slots', 'join_cleanup', 'refused_send_releases_timer')
    names = args.admission_law or (*state, *durable, 'wire_replacement_generation')
    qualified = set()
    if 'evaluator' in engines:
        for name in names:
            value, report = evaluate(args.evaluator.resolve(), selected, 'store@1/runner/law_' + name)
            row(engine='evaluator', law=name, passed=value == 1, result=report)
            if value == 1:
                qualified.add(name)
    for engine in (e for e in engines if e != 'evaluator'):
        groups = [('wire_admission', state), ('durable_wire_admission', durable),
                  ('wire_replacement_generation', ('wire_replacement_generation',))]
        for group, members in groups:
            if not any(name in names for name in members):
                continue
            definition = 'store@1/runner/test_' + group
            code, report = native_run(args, engine, selected, work / f'{group}-store-{engine}', definition)
            tests = report.get('tests', [])
            row(engine=engine, law=group, passed=code == 0
                and [t['definition'] for t in tests] == [definition]
                and all(t.get('passed') for t in tests), result=report)
    if args.no_mutants or args.part != 'admission':
        return
    mutants = (
        ('slot_freed_at_handoff', '            return Ok(wire_take_output(s, mut work))',
         '            let handed = wire_take_output(s, mut work)\n'
         '            wire_sent(mut s, mut work)\n'
         '            return Ok(take handed)', 'unread_client'),
        ('slot_never_freed', 'WSent => { wire_sent(mut s, mut work); return Ok(None) },',
         'WSent => return Ok(None),', 'unread_client'),
        ('slot_freed_at_answer', 'h.queued += count', 'h.queued = 0u64', 'unread_client'),
        ('refusal_rows_uncapped', 'if s.refusals >= 1u64 + s.config.q { return Error(1008u16) }', 'if false { return Error(1008u16) }', 'unread_client'),
        ('reset_drops_held', 'wire_queue(mut output, Reset(version, take done), take marks, list.new())', 'wire_queue(mut output, Reset(version, list.new()), take marks, list.new())', 'unread_client'),
    )
    for name, old, new, law in mutants:
        if args.admission_mutant and name not in args.admission_mutant:
            continue
        if law not in qualified:
            row(engine='evaluator', mutant=name, law=law, passed=False, result='baseline did not qualify; control not executed')
            continue
        assert text.count(old) == 1, name
        changed = work / (name + '.hirsel')
        changed.write_text(text.replace(old, new) + '\n' + laws)
        altered = manifest(root, work, name, changed)
        value, report = evaluate(args.evaluator.resolve(), altered, 'store@1/runner/law_' + law)
        red = value == 0 or (report.get('outcome') or {}).get('causes') == ['Fail']
        row(engine='evaluator', mutant=name, law=law, passed=red, result=report)


def socket_laws(args, root, work, engines, row):
    names = {'hello_order', 'slot_out_of_range', 'gone_slot_v5', 'second_hello_closes',
             'session_limit_503', 'principal_limit_1013', 'call_rate_overloaded',
             'request_mismatch_wire', 'resend_same_p', 'held_request_mismatch', 'hello_deadline', 'resume_unknown', 'history_below_latest', 'reply_over_done_max', 'faulted_owner_releases_slot', 'welcome_with_small_budget'}
    for engine in (e for e in engines if e != 'evaluator'):
        output = work / ('socket-' + engine)
        command = [sys.executable, str(root / 'tools/tests/wire_build.py'),
                   '--compiler', str(args.compiler.resolve()), '--runtime', str(args.runtime.resolve()),
                   '--admission-only', '--backend', engine, '--output', str(output)]
        if args.cc:
            command += ['--cc', str(args.cc.resolve())]
        built = subprocess.run(command, cwd=root, capture_output=True, text=True)
        if built.returncode:
            row(engine=engine, law='wire_socket_build', passed=False, result=(built.stdout + built.stderr)[-4000:])
            continue
        report = output / 'laws.json'
        run = subprocess.run([sys.executable, str(root / 'tools/tests/wire_admission.py'),
                              '--server', str(output / ('admission-' + engine)), '--report', str(report)],
                             cwd=root, capture_output=True, text=True)
        cases = json.loads(report.read_text()) if report.is_file() else []
        for case in cases:
            row(engine=engine, **case)
        row(engine=engine, law='wire_socket_selection', passed=run.returncode == 0
            and len(cases) == len(names) and {r['law'] for r in cases} == names
            and all(r.get('passed') for r in cases), result=(run.stdout + run.stderr)[-4000:])


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


def counting_cc(args, root, inputs_path, name):
    """A --cc wrapper that links core/tools/runner_count.c as the program's event sink."""
    kind, want = COUNTS.get(name, (7, 0))
    assert kind == 7, 'runner_count.c observes TRY_SEND'
    cc = c_compiler(args)
    work = inputs_path.parent.absolute()
    support = work / f'count_{name}.o'
    subprocess.run([str(cc), '-c', '-fPIC', str(root / 'core/tools/runner_count.c'),
                    '-I' + str(root / 'crates/hirsel-core-rt/include'), f'-DTRY_SENDS={want}',
                    f'-DSOURCE_MANIFEST_DIR={json.dumps(str(work), ensure_ascii=False)}',
                    *([f'-D{PLACEMENT_OBSERVERS[name]}'] if name in PLACEMENT_OBSERVERS else []),
                    *(['-DHUNG_BODY_SENDS=32664', f'-DHUNG_TOLDS={HUNG_TOLDS}'] if name == 'calls_deadline_hung' else []),
                    '-o', str(support)], check=True)
    wrapper = work / f'count_{name}-cc'
    wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc)) + ' "$@" '
                       + shlex.quote(str(support)) + ' -Wl,--wrap=hirsel_core_os_run_with_sources -Wl,--wrap=hirsel_core_spawn -Wl,--wrap=hirsel_core_channel_new -Wl,--wrap=hirsel_core_try_send\n')
    wrapper.chmod(0o755)
    return wrapper


# calls_recorded's disk store goes in a directory of its own beside the test store.
def dir_for(store):
    host = Path(str(store) + '-dir')
    host.mkdir(exist_ok=True)
    return host


def native_run(args, engine, inputs_path, store, prefix, cc=None, runtime=None):
    runtime = runtime or args.runtime
    command = [str(args.compiler.resolve()), 'test', '--inputs', str(inputs_path), '--source-core',
               '--source-std', '--backend', engine, '--runtime', str(runtime.resolve()),
               '--store', str(store), '--json', '--filter', prefix,
               '--limit', str(64 * 1024 * 1024), '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random',
               '--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535',
               '--dir', str(dir_for(store))]
    if engine == 'llvm':
        command.append('--release')
    if cc or args.cc:
        command += ['--cc', str(cc or args.cc.resolve())]
    wait_load()
    result = subprocess.run(command, capture_output=True, text=True)
    try:
        report = json.loads(result.stdout)
    except ValueError:
        report = {'stdout': result.stdout, 'stderr': result.stderr}
    return result.returncode, report


def run_mutants(args, root, work, evaluator, row):
    text = (root / 'core/programs/store/runner.hirsel').read_text()
    for name, edits, module, law in MUTANTS:
        if args.mutant and name not in args.mutant:
            continue
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
        selected = manifest(root, work, 'mutant_' + name, mutated)
        value, report = evaluate(evaluator, selected,
                                 f'store@1/{module}/law_{law}')
        # Red: the law returns false, or one of its `want` checks fails it (cause Fail only).
        red = (value == 0 or (report.get('outcome') or {}).get('causes') == ['Fail']
               or (value == 1 and not counted(law, report)))
        row(engine='evaluator', mutant=name, law=law, passed=red, result=report)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--evaluator', type=Path, required=True)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--schedule-runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_SCRIPTED_ARCHIVE'))
    p.add_argument('--observed', type=Path,
                   default=Path(os.environ['HIRSEL_CORE_RT_OBSERVED_ARCHIVES']) / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
                   if 'HIRSEL_CORE_RT_OBSERVED_ARCHIVES' in os.environ else None)
    p.add_argument('--bridge', type=Path, default=os.environ.get('HIRSEL_STORE_CRASH_BRIDGE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm'])
    p.add_argument('--call-law', action='append', choices=CALLS)
    p.add_argument('--no-mutants', action='store_true')
    p.add_argument('--mutant', action='append', choices=[m[0] for m in MUTANTS])
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    p.add_argument('--session-law', action='append', choices=['pool_holds_writer_bytes', 'writer_one_frame', 'children_joined_on_close', 'session_records', 'serving_budget', 'subscription_room_refunded'])
    p.add_argument('--session-mutant', action='append', choices=['encode_outside_pool', 'writer_queue', 'orphan_reader', 'subscription_refund_on_handoff'])
    p.add_argument('--admission-law', action='append', choices=['wire_replacement_generation', 'unread_client', 'hello_and_slots', 'join_cleanup',
                   'refused_send_releases_timer', 'gone_slot_v5', 'request_mismatch_wire', 'history_below_latest', 'reply_over_done_max'])
    p.add_argument('--admission-mutant', action='append', choices=['slot_freed_at_handoff', 'slot_never_freed', 'slot_freed_at_answer', 'refusal_rows_uncapped', 'reset_drops_held'])
    p.add_argument('--part', choices=['r0', 'r1', 'wire', 'session', 'ingress', 'admission', 'socket', 'calls', 'deadline', 'activation', 'rc1'])
    args = p.parse_args()
    r0, r1 = args.part in (None, 'r0'), args.part in (None, 'r1', 'calls')
    selected_calls = args.call_law or CALLS
    root = args.root.resolve()
    evaluator = args.evaluator.resolve()
    engines = args.backend or ['evaluator', 'cranelift', 'llvm']
    if args.part == 'rc1' and (not args.compiler or not args.runtime or engines == ['evaluator']):
        p.error('RC1 requires a native backend, --compiler and --runtime')
    rows = []

    def row(**r):
        rows.append(r)
        print(' '.join(f'{k}={v}' for k, v in r.items() if k != 'result' or r.get('law') == 'l4_peak_retained'), flush=True)

    with tempfile.TemporaryDirectory(prefix='hirsel-runner-laws-') as tmp:
        work = Path(tmp)
        if args.mutant:
            run_mutants(args, root, work, evaluator, row)
            if args.report:
                args.report.write_text(json.dumps(rows, indent=2) + '\n')
            return int(any(not r['passed'] for r in rows))
        crash_laws = {'deadline': 'one_drain_deadline', 'activation': 'atomic_activation'}
        for part, law in crash_laws.items():
            if args.part not in (None, 'r1', part):
                continue
            for engine in (name for name in engines if name != 'evaluator'):
                try:
                    row(engine=engine, law=law, passed=True,
                        result=drain_deadline(args, root, work, engine, f'{law}-{engine}', activation=part == 'activation'))
                except RuntimeError as error:
                    row(engine=engine, law=law, passed=False, result=str(error))
        if args.part in crash_laws:
            if args.report:
                args.report.write_text(json.dumps(rows, indent=2) + '\n')
            return int(not rows or any(not result['passed'] for result in rows))
        if args.part in ('admission', 'wire') or (r1 and args.part != 'calls'):
            admission_laws(args, root, work, engines, row)
        if args.part == 'ingress' or (r1 and args.part != 'calls'):
            ingress_laws(args, root, work, engines, row)
        if r0 or args.part in ('wire', 'session'):
            session_laws(args, root, work, engines, row)
        if r0 or args.part == 'wire':
            frame_vectors(args, root, work, engines, row)
        if args.part in ('wire', 'socket'):
            socket_laws(args, root, work, engines, row)
        if args.part in (None, 'r1', 'rc1'):
            for engine in (e for e in engines if e != 'evaluator'):
                no_secret_at_rest(args, root, work, engine, row)
                if args.part == 'rc1' and not args.no_mutants and engine == 'cranelift':
                    no_secret_at_rest(args, root, work, engine, row, mutant=True)
        if args.part in ('wire', 'session', 'ingress', 'admission', 'socket', 'rc1'):
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
            for name in selected_calls:
                value, report = evaluate(evaluator, base, 'store@1/runner_calls/law_' + name)
                row(engine='evaluator', law=name, passed=value == 1 and counted(name, report), result=report)
            for name in (() if args.part == 'calls' else SUBS):
                value, report = evaluate(evaluator, base, 'store@1/runner/law_' + name)
                row(engine='evaluator', law=name, passed=value == 1, result=report)
        # A mutant is red only when it builds and its law fails.
        if r0 and not args.no_mutants:
            run_mutants(args, root, work, evaluator, row)
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
            subs = ('subs', [], 'store@1/runner/', 'shared', {f'store@1/runner/test_{n}' for n in SUBS})
            groups = ((subs,) if args.part != 'calls' else ()) if r1 else ()
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
                for name in selected_calls if r1 else ():
                    definition = f'store@1/runner_calls/test_{name}'
                    cc = counting_cc(args, root, base, name) if name in COUNTS or name in PLACEMENT_OBSERVERS else None
                    runtime = args.schedule_runtime if name == 'calls_deadline_hung' else args.runtime
                    if runtime is None:
                        p.error('calls_deadline_hung requires --schedule-runtime')
                    code, report = native_run(args, engine, base, work / f'store-{engine}-shared', definition, cc, runtime)
                    tests = report.get('tests', [])
                    row(engine=engine, law=name,
                        passed=code == 0 and [t['definition'] for t in tests] == [definition]
                        and all(t.get('passed') for t in tests),
                        result=tests or report)
                if r0:
                    held, figures = l4_retained(args, root, work, engine)
                    row(engine=engine, law='l4_peak_retained', passed=held, result=figures)
                    held, figures = l20_retained(args, root, work, engine)
                    row(engine=engine, law='l20_cover_only_retention', passed=bool(held), result=figures)
                    try:
                        row(engine=engine, law='l1_crash', passed=True,
                            result=crashes(args, root, work, engine, engine))
                    except RuntimeError as error:
                        row(engine=engine, law='l1_crash', passed=False, result=str(error)[-2000:])
            # A crash mutant is red only when it builds and the L1 floor check fails it.
            if r0 and not args.no_mutants and 'cranelift' in natives:
                text = (root / 'core/programs/store/runner.hirsel').read_text()
                # A cover mutant is red only when it builds, runs and the two figures differ.
                for name, edits in COVER_MUTANTS:
                    mutated_text = text
                    for old, new in edits:
                        mutated_text = mutated_text.replace(old, new) if mutated_text.count(old) == 1 else None
                        if mutated_text is None:
                            break
                    if mutated_text is None:
                        row(engine='cranelift', mutant=name, passed=False, result='mutation site not found once')
                        continue
                    mutated = work / f'cover_runner_{name}' / 'runner.hirsel'
                    mutated.parent.mkdir()
                    mutated.write_text(mutated_text)
                    held, figures = l20_retained(args, root, work, 'cranelift', mutated, 'cover_' + name)
                    row(engine='cranelift', mutant=name, law='l20_cover_only_retention',
                        passed=held is False and 'run' not in figures, result=figures)
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
