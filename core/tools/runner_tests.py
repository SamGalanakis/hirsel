#!/usr/bin/env python3
"""Run the runner's named laws and cases (RUNNER-SPEC v2.5 §8).

R0 owns core, session, and frame laws, including the generated p19 and L12
fixtures. Its L4/L20 cases compare exact retained bytes across native runs.
R1 owns calls and admission. The wire target runs the socket cases.
Without --part, both R0 and R1 run.

The package's own tests run natively under `hirsel test core/programs/runner`
(store_tests.py). Natively this driver keeps what that cannot express: the
generated p19 and L12 modules, the calls laws that link an event observer
through --cc or need the scheduled runtime, retained bytes, crash cuts, the
socket cases and the host laws.

"""
import argparse
import fcntl
from contextlib import contextmanager
import hashlib
import json
import shlex
import os
import re
import socket
import struct
import sys
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from case_progress import run_case
from std_native import ROOT, simulation_support
from runner_rest import no_jwt_at_rest, no_secret_at_rest
from runner_host import BUCKET_HOST_LAWS, cold_wake_trigger, cutover_survives_host_loss, runner_idle_stop, unpinned, wait_ready
from store_tests import crash_inputs, runner_inputs
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

CALLS = ('atomic_migration', 'serve_bounded_and_settled', 'duplicate_own_reply', 'release_digest', 'ready_after_activation', 'drain_answers_once', 'drain_reopened', 'lag_window', 'calls_two_streams', 'calls_group_version', 'calls_joiners', 'calls_duplicate', 'anonymous_fresh', 'granted_copied', 'bind_granted', 'calls_refusals',
         'calls_runs_once', 'calls_session_charge', 'calls_store_refusal', 'calls_one_outcome_per_send', 'calls_deadline_held', 'calls_deadline_hung', 'calls_held_head', 'calls_retired_head', 'calls_session_mismatch', 'calls_recorded', 'subs_wired', 'query_as_caller', 'publisher_never_blocks', 'repair_delivered',
         'local_one_head', 'local_route_during_unload', 'local_idle_unload', 'local_reopen_keeps')
# The in-process host's laws (RUNNER-SPEC §14.9) bind sockets and use disk stores.
NATIVE_ONLY_CALLS = {'drain_reopened', 'lag_window', 'local_one_head', 'local_route_during_unload', 'local_idle_unload', 'local_reopen_keeps'}
PERSISTENT_PLACE_CALLS = {'atomic_migration', 'ready_after_activation', 'calls_recorded'}

# L3 counts the body, four publisher answers, and the retirement wake.
HUNG_STEPS, HUNG_TOLDS = 65538, 3
COUNTS = {'calls_runs_once': (7, 6), 'calls_deadline_hung': (7, None)}
PLACEMENT_OBSERVERS = {'ready_after_activation': 'READY_BEFORE_ACCEPT', 'lag_window': 'LAG_ADMISSION'}

# Wire floods retain all 10,000 frames. The evaluator's generic 1B test envelope
# predates framing. At 1/100 frames, refusals use 8,148,075/20,157,978 steps,
# session_charge 80,393,142/325,732,340. Extrapolate the 99-frame difference
# to 10,000, double it for margin, then round up. Entry step bounds stay fixed.
WIRE_TEST_INSTRUCTIONS = {
    'runner@1/runner_calls/law_calls_refusals': 2_500_000_000,
    'runner@1/runner_calls/law_calls_session_charge': 50_000_000_000,
}
# Flood laws read through an 8 KiB model window. Byte-split reads stay on every other wire law:
# under the default 8-byte window a 4 KiB frame costs about 830 Decode round trips.
WIDE_WINDOW_LAWS = {'runner@1/runner_calls/law_calls_session_charge'}

# R2's publisher laws, in runner.hirsel itself (RUNNER-SPEC §8.2).
SUBS = ('subs_main', 'range_reruns', 'subs_reset_keeps_diff', 'rows_normalized', 'projection_isolated', 'subscribe_order', 'retired_without_waiting', 'stale_never_released', 'slot_images')


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
    observed = {'runner@1/runner_calls/tally', 'runner@1/runner_calls/hung',
                'runner@1/runner_calls/answer', 'runner@1/runner/offer'}
    retirement = set()
    attributed = []
    for e, site in zip(events, sources):
        if e[0] == 18:  # LATEST_PUBLISH. A closing owner publishes its dead head lease.
            retirement.add(e[1])
        wakes_head = site == 'runner@1/runner/try_head' and e[1] in retirement
        if e[0] != kind or site in observed or wakes_head:
            attributed.append((e, site))
    if name != 'calls_deadline_hung':
        return sum(1 for e, _ in attributed if e[0] == kind) == want
    counts, group = [], []
    for event, site in attributed:
        group.append((event, site))
        if event[0] == kind and site == 'runner@1/runner/try_head':
            count = hung_count(group, HUNG_STEPS + 2 * len(counts))
            if count is None:
                return False
            counts.append(count)
            group.clear()
    return len(counts) == 2 and counts[1] == counts[0] + 1 and not any(
        e[0] in (kind, 21) for e, _ in group)


def hung_count(events, steps):
    sends = {}
    slices = []
    for at, (e, site) in enumerate(events):
        if e[0] == 7:
            sends.setdefault(e[2], []).append((at, e[1], site.rsplit('/', 1)[-1]))
        elif e[0] == 21:
            slices.append((at, e[1]))
    by_site = {seq[0][2]: seq for seq in sends.values()}
    if len(sends) != 4 or set(by_site) != {'hung', 'answer', 'offer', 'try_head'} or len(slices) != 1:
        return None
    body, marker, told, retirement = (by_site[site] for site in ('hung', 'answer', 'offer', 'try_head'))
    cut, head = slices[0]
    if (len(marker) == len(retirement) == 1 and len(told) == HUNG_TOLDS
            and 1 <= len(body) <= (steps + 1) // 2
            and marker[0][0] < told[0][0] and cut < told[0][0] < retirement[0][0]
            and marker[0][1] != head and all(t == head and site == 'hung' for _, t, site in body)
            and all(t != head and site == 'offer' for _, t, site in told)):
        return len(body)
    return None


# L4 compares the exact retained peak for 10 and 10,000 malformed calls on one build.
def charged(args, root, work, engine, module, modes, figure, cc=None, entry=None):
    """Build an entry on the charge-observation runtime and run it in
    each (argv, label) mode: {label: (exit status, figure(charge report))}, or None and the
    build's error."""
    extra = () if entry else [(root / f'core/programs/runner/{module}.hirsel', module)]
    inputs_path = manifest(root, work, module, extra=extra)
    binary = work / f'{module}_{engine}'
    charges = work / f'{module}_{engine}.json'
    command = [str(args.compiler.resolve()), 'build', '--inputs', str(inputs_path), '--source-core',
               '--source-std', '--backend', engine, '--runtime', str(args.observed.resolve()),
               '--store', str(work / f'store-{module}-{engine}'), '--grant', 'Clock',
               '--grant', 'Random', '--entry', f"runner@1/{entry or module + '/main'}",
               '--logical-charge-report', str(charges), '-o', str(binary)]
    if module in ('runner_rss', 'runner_cover'):
        command += ['--grant', 'Tasks', '--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535']
    if engine == 'llvm':
        command.append('--release')
    if cc or args.cc:
        command += ['--cc', str(cc or args.cc.resolve())]
    built = run_case(f'{module}:build', engine, command, capture_output=True, text=True)
    if built.returncode != 0:
        return None, built.stderr[-2000:]
    figures = {}
    rss = {}
    for mode, label in modes:
        charges.unlink(missing_ok=True)
        command = [str(binary), mode]
        if module == 'runner_rss':
            peak = work / f'{module}_{engine}_{label}.rss'
            command = ['/usr/bin/time', '-f', '%M', '-o', str(peak), *command]
        run = run_case(f'{module}:{label}', engine, command, capture_output=True)
        figures[label] = (run.returncode, figure(json.loads(charges.read_text())) if charges.exists() else None)
        if run.returncode:
            figures.setdefault('errors', {})[label] = run.stderr.decode(errors='replace')[-2000:]
        if module == 'runner_rss':
            rss[label] = int(peak.read_text().splitlines()[-1])
    if rss:
        figures['rss_kib'] = rss
    return figures, None


def l4_retained(args, root, work, engine):
    figures, error = charged(args, root, work, engine, 'runner_rss', (('10', 'ten'), ('10000', 'flood')),
                             lambda r: r['peak_retained_bytes'])
    if figures is None:
        return False, {'build': error}
    held = (figures['ten'][0] == 0 and figures['flood'][0] == 0 and figures['ten'][1] is not None
            and figures['flood'][1] == figures['ten'][1])
    return held, figures


# L20 uses runner_mark.c to observe retained bytes after dropping the initial Batch.
# The Table covers span 10 or all 1,954 rows. The Log covers select windows of 10
# over Logs of 100 and 100,000 rows, or a window of 100 over the larger Log.
def subscribed_bytes(report):
    marks = [p for p in report['phases'] if p['name'] == 'subscribed']
    return marks[0]['retained_bytes'] if len(marks) == 1 else None


def l20_retained(args, root, work, engine, modes=(('0', 'ten'), ('1', 'all')), entry=None):
    name = 'slot_image_retention' if entry else 'runner_cover'
    cc = c_compiler(args)
    support = work / f'{name}_mark_{engine}.o'
    run_case(f'{name}:observer', engine, [str(cc), '-c', '-fPIC', str(root / 'core/tools/runner_mark.c'),
                    '-I' + str(root / 'crates/hirsel-core-rt/include'), '-o', str(support)], check=True)
    wrapper = work / f'{name}_mark_{engine}-cc'
    wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc)) + ' "$@" '
                       + shlex.quote(str(support)) + ' -Wl,--wrap=hirsel_core_os_run_with_sources\n')
    wrapper.chmod(0o755)
    figures, error = charged(args, root, work, engine, name, modes, subscribed_bytes, wrapper, entry)
    if figures is None:
        return None, {'build': error}
    samples = [figures[label] for _, label in modes]
    return all(status == 0 and size is not None and size == samples[0][1] for status, size in samples), figures


def p19_module(path):
    lines = ['// Generated by core/tools/runner_tests.py: RUNNER-SPEC p19_place_twice.', 'type Reg = runner.Registry[(), ()]', '']
    lines.extend(f'fn e{i}(take x: u64) -> Result[u64, Capacity] {{ Ok(x + {i}u64) }}' for i in range(max(SIZES)))
    for n in SIZES:
        chunks = range(0, n, CHUNK)
        for c in chunks:
            lines += ['', f'fn place_{n}_{c}(mut r: Reg) -> Result[(), runner.RegisterError] {{']
            lines += [f'    runner.compute(mut r, code.describe(e{i}), 65536u64, runner.DEFAULT_STEPS)?' for i in range(c, min(n, c + CHUNK))]
            lines += ['    Ok(())', '}']
        lines += ['', f'fn place_{n}(mut r: Reg) -> Result[(), runner.RegisterError] {{']
        lines += [f'    place_{n}_{c}(mut r)?' for c in chunks] + ['    Ok(())', '}']
    lines += ['', 'fn described(n: u64, entries: u64) -> Bytes {',
              '    // The caller owns the capture buffer. Place still has its own fixed byte bound.',
              '    let schema_size = list.len(result.unwrap(std.codec.schema(std.codec.of_u64())))',
              f'    let identity_size = list.len(code.identity(code.describe(e{max(SIZES) - 1})))',
              '    let row_bytes = 4u64 + 3u64 * 4u64 + identity_size + 2u64 * schema_size',
              '    let mut b: Bytes = list.with_capacity(4u64 + entries * row_bytes)', '    let made = bound bytes n {',
              '        let mut r: Reg = runner.registry()', '        let placed = match entries {']
    lines += [f'            {n}u64 => place_{n}(mut r),' for n in SIZES]
    lines += ['            _ => Ok(()),', '        }', '        match placed {',
              '            Ok(()) => { result.unwrap(runner.describe(r, mut b)); true },', '            Error(_) => false,',
              '        }', '    }', '    if let Ok(true) = made { take b } else { list.new() }', '}', '',
              'type Ask: send { entries: u64 }', '', f'fn in_task(take a: Ask) -> Bytes {{ described({N_PLACE}u64, a.entries) }}', '',
              '// Two `place` runs, in two tasks, give equal describe bytes holding every entry.',
              'fn twice(tasks: Tasks, entries: u64) -> bool waits {',
              f'    let mine = described({N_PLACE}u64, entries)',
              f'    let t = match task.try_spawn(tasks, bytes {2 * N_PLACE}u64, Ask(entries: entries), in_task) {{',
              '        Ok(take t) => take t,', '        Error(_) => return false,', '    }',
              '    let theirs = match task.join(take t) {', '        Ok(take b) => take b,', '        Error(_) => return false,', '    }',
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
              '            let mut b: Bytes = list.new()', '            list.append(mut b, r)',
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
             'fn te(mut tx: store.Tx, h: (), caller: runner.Caller, take a: u64) -> Result[u64, E] { Ok(a) }', '',
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


def l10_values(evaluator, root, work, tag):
    """(identity fp, result fp) per variant, None where a run did not return."""
    values = {}
    for name, e in L10:
        mod = work / f'runner_l10_{tag}_{name}.hirsel'
        l10_module(mod, e)
        m = manifest(root, work, f'l10_{tag}_{name}', extra=[(mod, 'runner_l10')])
        values[name] = tuple(evaluate(evaluator, m, 'runner@1/runner_l10/law_l10_' + law)[0] for law in ('identity', 'result'))
    return values


def l10_holds(values):
    (bi, br), (ei, er) = values['base'], values['e_field']
    return None not in (bi, br, ei, er) and 0 not in (bi, br, ei, er) and bi == ei and br != er


# R1b (RUNNER-SPEC v2.10 L1): runner_crash's scenarios, each run at every W2 cut, and
# its poison scenario, whose one failed journal sync the adapter checks in count mode.
CRASHES = ('runner-cross', 'runner-order')
POISON = 'poison-segment'
STOP_CASES = ('runner-deadline', 'runner-stop-early', 'runner-stop-twice')
HOST_LAWS = ('two_recipes_one_digest', 'pinned_recording_uses_retained_image', 'foreign_recording_refused',
             'missing_store_mount', 'install_child', 'install_child_crash', 'cutover_survives_host_loss',
             'cold_wake_trigger', *BUCKET_HOST_LAWS)


def host_law(args, root, work, engine, name):
    manifest_path = manifest(root, work, name, crash=True)
    sources = json.loads(manifest_path.read_text())
    for package in sources['packages']:
        for source in package['sources']:
            path = Path(source['file'])
            frozen = work / 'sources' / path.relative_to(root)
            frozen.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, frozen)
            source['file'] = str(frozen)
    manifest_path.write_text(json.dumps(sources))

    compiler = str(args.compiler.resolve())
    if name in BUCKET_HOST_LAWS:
        return BUCKET_HOST_LAWS[name](work, engine, compiler, args.runtime, args.cc, manifest_path, 268435456)
    publish_store = work / 'published'
    launch_store = work / 'place'
    (launch_store / 'live').mkdir(parents=True)
    state = work / 'host-state'
    state.mkdir(mode=0o700)
    operator = state / 'operator.sock'

    recovery_law = name == 'cutover_survives_host_loss'
    entry = {'cutover_survives_host_loss': 'identity_main', 'cold_wake_trigger': 'cold_wake_main'}.get(name, 'host_main')

    def publish(n_place, root_limit=268435456):
        command = [compiler, 'publish', '--inputs', str(manifest_path), '--source-core', '--source-std',
                   '--entry', f'store_crash@1/runner_crash/{entry}',
                   '--backend', engine,
                   '--code-store', str(publish_store), '--store', str(work / 'publish-cache'),
                   '--limit', str(root_limit), '--n-place', str(n_place), '--dir', 'live',
                   '--grant', 'Net=Listen 127.0.0.0/8 0-65535']
        for capability in ('Clock', 'Random', 'Tasks', 'Process', 'Stderr'):
            command += ['--grant', capability]
        return checked(command, engine=engine, case=f'{name}:publish-{n_place}').stdout.strip()

    old_n_place, new_n_place = 1048576, 2097152
    old_recipe = publish(old_n_place)
    if name in ('missing_store_mount', 'install_child', 'cold_wake_trigger'):
        new_recipe = old_recipe
    else:
        new_recipe = publish(old_n_place, 269484032) if recovery_law else publish(new_n_place)
        assert old_recipe != new_recipe, 'OLD and NEW must have distinct recipes'

    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        address = f'127.0.0.1:{probe.getsockname()[1]}'
    register = [compiler, 'register', old_recipe, '--state', str(state), '--group', 'app',
                '--listen', address, '--store', str(work / 'group-store'), '--trusted',
                '--workdir', str(launch_store), '--code-store', str(publish_store)]
    deploy = [compiler, 'deploy', new_recipe, '--state', str(state), '--group', 'app',
              '--code-store', str(publish_store)]

    def ready():
        return wait_ready(address)

    def welcome_n_place():
        with socket.create_connection(('127.0.0.1', int(address.split(':')[1])), timeout=5) as peer:
            with peer.makefile('rb') as stream:
                peer.sendall((f'GET /hirsel HTTP/1.1\r\nHost: {address}\r\n'
                              'Upgrade: websocket\r\nConnection: Upgrade\r\n'
                              'Sec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: hirsel.4\r\n'
                              'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n').encode())
                assert stream.readline().startswith(b'HTTP/1.1 101 '), 'place refused the test session'
                for line in stream:
                    if line == b'\r\n':
                        break
                # Hello(Some(credential), []) binds the fixture's application Caller.
                hello = b'\0' * 4 + b'\x01\0\0\0\x10\0\0\0' + os.urandom(16) + b'\0' * 4
                mask = os.urandom(4)
                masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(hello))
                peer.sendall(bytes([0x82, 0x80 | len(hello)]) + mask + masked)
                opcode, length = stream.read(2)
                length = int.from_bytes(stream.read(2), 'big') if length == 126 else length
                assert opcode == 0x82 and length >= 28, f'unexpected Welcome frame header {opcode:#x} {length}'
                frame = stream.read(length)
                assert frame[:4] == b'\x04\0\0\0' and len(frame) == length, 'first server frame was not Welcome'
                return int.from_bytes(frame[-24:-16], 'little')

    def listening():
        with socket.socket(socket.AF_UNIX) as peer:
            try:
                peer.connect(str(operator))
                return True
            except OSError:
                return False

    @contextmanager
    def running_host(idle_ms=None):
        with tempfile.TemporaryFile(mode='w+') as log:
            command = [compiler, 'host', '--state', str(state)]
            if idle_ms is not None:
                command += ['--idle-ms', str(idle_ms)]
            host = subprocess.Popen(command, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 120
                while time.monotonic() < deadline and host.poll() is None and not listening():
                    time.sleep(0.025)
                if not listening():
                    log.seek(0)
                    raise RuntimeError(f'the host did not listen on {operator}: {log.read()}')
                yield host, log
            except Exception as error:
                log.seek(0)
                detail = log.read().strip()
                if detail:
                    error.add_note(detail)
                raise
            finally:
                if host.poll() is None:
                    comm = subprocess.check_output(['ps', '-o', 'comm=', '-p', str(host.pid)], text=True)
                    assert comm.strip() == 'hirsel', comm
                    host.kill()
                host.wait(timeout=15)

    def outcome(command, case):
        result = checked(command, engine=engine, case=case)
        request = re.search(r'^request ([0-9a-f]{32})$', result.stderr, re.M)
        assert request, f'{case} did not name its request first: {result.stderr}'
        return request.group(1), json.loads(result.stdout.strip().splitlines()[-1])

    if name == 'cold_wake_trigger':
        return cold_wake_trigger(compiler, engine, state, address, old_n_place, register, running_host, outcome, ready)

    if recovery_law:
        return cutover_survives_host_loss(root, work, engine, compiler, state, launch_store, register,
                                          deploy, running_host, outcome, ready, welcome_n_place)

    if name in ('install_child', 'install_child_crash'):
        with running_host():
            _, registered = outcome(register, f'{name}:register')
            if name == 'install_child_crash':
                _, deployed = outcome(deploy, f'{name}:publish-new')
        old = bytes.fromhex(registered['assembly'])
        new = bytes.fromhex(deployed['assembly']) if name == 'install_child_crash' else None
        install_child_law(compiler, state, work, old, new)
        return

    if name == 'missing_store_mount':
        live = launch_store / 'live'
        register[register.index('--store') + 1] = str(live)

        def store_id(directory):
            # STORE-SPEC §2.2 puts the two little-endian words at bytes 32 and 40.
            header = (directory / 'MANIFEST').read_bytes()
            assert header[16:24] == b'HIRSTORE'
            return ''.join(f'{int.from_bytes(header[offset:offset + 8], "little"):016x}' for offset in (32, 40))

        with running_host():
            outcome(register, f'{name}:register')
            original = ready()
            expected = store_id(live)

        saved = launch_store / 'unmounted'
        live.rename(saved)
        live.mkdir()
        with running_host() as (_, log):
            # The new launcher has the same catalog and the same directory path. A GET
            # wakes a cold group too. The refused place must never answer it.
            with socket.create_connection(('127.0.0.1', int(address.split(':')[1])), timeout=5) as peer:
                peer.sendall(f'GET /hirsel/status HTTP/1.1\r\nHost: {address}\r\n\r\n'.encode())
                deadline = time.monotonic() + 15
                while True:
                    detail = os.pread(log.fileno(), 1 << 20, 0).decode()
                    if 'store identity mismatch' in detail:
                        break
                    assert time.monotonic() < deadline, f'no store identity refusal: {detail}'
                    time.sleep(0.05)
                actual = store_id(live)
                assert actual != expected
                assert f'store identity mismatch (expected {expected}, found {actual})' in detail, detail
                peer.settimeout(0.25)
                try:
                    assert not peer.recv(1), 'a place served a request from the replacement store'
                except TimeoutError:
                    pass

        live.rename(launch_store / 'replacement')
        saved.rename(live)
        with running_host():
            restored = ready()
            assert restored['digest'] == original['digest']
            assert store_id(live) == expected
        return

    if name == 'foreign_recording_refused':
        def framed_hash(domain, payload):
            domain = domain.encode()
            return hashlib.sha256(len(domain).to_bytes(8, 'little') + domain
                                  + len(payload).to_bytes(8, 'little') + payload).digest()

        def text(value):
            return bytes([len(value)]) + value.encode()

        entry = 'app@1/main/main'
        callee = 'ffi_laws@1/native/touch'
        target = 'x86_64-unknown-linux-gnu'
        reason = f'FFI-8: a recording build cannot reach foreign code: {entry} reaches {callee}'
        recipe_prefix = (publish_store / 'recipes' / old_recipe).read_bytes()[:13]
        # A complete envelope with an archive object absent from both stores.
        archive = hashlib.sha256(b'abc').digest() + b'\x03'
        frame = (recipe_prefix + text(entry) + b'\0\0' + bytes(32) + b'\0\0\x01'
                 + text('ffi_laws@1/native') + text('hfl_') + b'\x01' + text(target) + archive
                 + b'\x01' + text(callee) + b'\0\0' + text('(core@1/core/Foreign[ffi_laws@1/laws/Laws])->i32')
                 + b'\x01' + archive + b'\0' + bytes([engine == 'llvm']) + text(target) + bytes(18))
        foreign = framed_hash('hirsel.release-recipe.v3', frame)
        (publish_store / 'recipes' / foreign.hex()).write_bytes(frame)
        with running_host():
            outcome([*register, '--record', str(work / 'recordings')], f'{name}:register')
            old = ready()
            foreign_deploy = deploy.copy()
            foreign_deploy[2] = foreign.hex()
            refused = run_case(f'{name}:deploy', engine, foreign_deploy, capture_output=True, text=True)
            assert refused.returncode == 2 and f'deploy: denied: {reason}' in refused.stderr, refused
            assert ready()['digest'] == old['digest'], 'the refused deploy changed the serving image'
        assert not (state / 'code' / 'recipes' / foreign.hex()).exists(), 'the refused recipe was published'
        return

    if name == 'pinned_recording_uses_retained_image':
        recordings = work / 'recordings'
        replay_only = work / 'replay-only'
        with running_host():
            outcome([*register, '--record', str(recordings)], f'{name}:register')
            ready()
            tapes = list(recordings.glob('*.tape'))
            assert len(tapes) == 1, 'the first activation did not start a recording'
            tape = tapes[0]
            outcome(deploy, f'{name}:retire-recording')

            # The replay prefix has the pin reader but no runtime or linker, so it cannot
            # assemble. A successful replay can only use the immutable recording image in
            # the host image store.
            installed = Path(compiler).parent
            replay_only.mkdir()
            shutil.copyfile(compiler, replay_only / 'hirsel')
            (replay_only / 'hirsel').chmod(0o700)
            shutil.copyfile(installed / 'hirsel-core-toolchain.json', replay_only / 'hirsel-core-toolchain.json')
            for directory in ('lib', 'libexec'):
                (replay_only / directory).symlink_to(installed / directory, target_is_directory=True)
            replay = [str(replay_only / 'hirsel'), 'replay', '--state', str(state), '--tape', str(tape)]
            result = checked(replay, engine=engine, case=f'{name}:replay-asked')
            assert result.stdout.strip() == 'Sealed', 'replay did not seal against the pinned image'
            gone = [*replay[:-1], str(unpinned(tape, work / 'unpinned.tape'))]
            refused = run_case(f'{name}:gone-asked', engine, gone, capture_output=True, text=True)
            assert (refused.returncode, refused.stderr.strip()) == (3, 'Unknown(ImageGone)'), refused

            # A host whose operator socket cannot be reached still holds the state, so the
            # offline reader must not run beside it.
            hidden = state / 'operator.sock.hidden'
            operator.rename(hidden)
            try:
                busy = run_case(f'{name}:replay-held', engine, replay, capture_output=True, text=True)
                assert busy.returncode == 75 and 'temporarily unavailable' in busy.stderr, busy
            finally:
                hidden.rename(operator)

        result = checked(replay, engine=engine, case=f'{name}:replay-offline')
        assert result.stdout.strip() == 'Sealed', 'offline replay did not seal against the pinned image'
        refused = run_case(f'{name}:gone-offline', engine, gone, capture_output=True, text=True)
        assert (refused.returncode, refused.stderr.strip()) == (3, 'Unknown(ImageGone)'), refused
        return

    with running_host():
        _, registered = outcome(register, f'{name}:register')
        assert registered['phase'] == 'installed', registered
        old = ready()
        assert old['n_place'] == old_n_place, 'OLD started with the wrong allowance'
        assert welcome_n_place() == old_n_place, 'OLD Welcome used the wrong allowance'
        request, deployed = outcome(deploy, f'{name}:deploy')
        assert deployed['phase'] == 'installed', deployed
        # launch.md "Registering a group": the first release is activation 0.
        assert registered['activation'] == 0, registered
        assert deployed['activation'] > 0, ('the deploy did not publish a later activation', deployed)
        assert ready()['n_place'] == new_n_place, 'NEW started with the wrong allowance'
        assert (launch_store / 'live' / 'MANIFEST').is_file(), 'place opened outside --workdir'
        # The same request again reports its one outcome and starts nothing new.
        again, resent = outcome([*deploy, '--request-id', request], f'{name}:resend')
        assert (again, resent) == (request, deployed), 'a resent request changed its outcome'

    with running_host():
        new = ready()
        assert new['n_place'] == new_n_place, 'restart did not recover the installed assembly'
        assert welcome_n_place() == new_n_place, 'recovered Welcome used the wrong allowance'
        assert new['digest'] == old['digest'], 'varying only n_place changed the image digest'
        assert len(list((state / 'objects' / 'images').iterdir())) == 1, 'the shared image was duplicated or removed'
        status = json.loads(checked([compiler, 'deploy', '--state', str(state), '--status', request],
                                    engine=engine, case=f'{name}:status').stdout)
        assert status == deployed, 'the recovered host forgot the request outcome'


def install_child_law(compiler, state, work, old, new):
    """Drive the actual launcher by the value-codec control session."""
    place = work / 'install-place'
    live = place / 'live'
    live.mkdir(parents=True)
    control = state / 'install-control.sock'
    group = struct.pack('<I1sQ', 1, b'g', 1)
    pack = struct.pack

    def blob(raw):
        return pack('<I', len(raw)) + raw

    def child(tag, generation):
        return group + pack('<IQ', tag, generation)

    def start(assembly, identity, mode_args, args=()):
        payload = (group + blob(assembly) + blob(os.fsencode(place))
                   + pack('<I', len(args)) + b''.join(map(blob, args))
                   + identity[len(group):] + mode_args + pack('<I', 0))
        send(3, payload)
        assert receive() == pack('<I', 1) + identity + pack('<I', 0)

    def send(tag, payload=b''):
        peer.sendall(blob(pack('<I', tag) + payload))

    def receive():
        header = stream.read(4)
        assert len(header) == 4, 'launcher closed its control stream'
        length, = struct.unpack('<I', header)
        assert length <= 2**20
        value = stream.read(length)
        assert len(value) == length, 'launcher frame was truncated'
        return value

    def placed(identity, reply):
        frame = receive()
        assert frame == pack('<I', 9) + identity + reply, frame.hex()

    def end(identity, exit_kind, code):
        assert receive() == pack('<I', 3) + identity + pack('<IiIB', exit_kind, code, 0, 0)
        send(6, identity)

    def probe(assembly, gen, activation, found):
        identity = child(1, gen)
        start(assembly, identity, pack('<QQIII', activation, 60_000, 0, 0, 0))
        frame = receive()
        prefix = pack('<I', 9) + identity + pack('<I', 0 if activation == found else 1)
        assert frame.startswith(prefix) and frame[len(prefix):len(prefix) + 8] == pack('<Q', found), frame
        if found == activation:
            send(5, identity)
            end(identity, 1, 9)
        else:
            end(identity, 0, 1)

    with tempfile.TemporaryFile() as log:
        launcher = subprocess.Popen([compiler, 'launcher', '--control', str(control), '--state', str(state)],
                                    stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            with socket.socket(socket.AF_UNIX) as peer:
                peer.settimeout(15)
                while True:
                    try:
                        peer.connect(str(control))
                        break
                    except (FileNotFoundError, ConnectionRefusedError):
                        assert launcher.poll() is None and time.monotonic() < deadline
                        time.sleep(0.025)
                with peer.makefile('rb') as stream:
                    send(0)
                    assert receive() == bytes(16), 'a fresh launcher has an empty inventory'
                    send(1, group + blob(b'127.0.0.1:0'))
                    assert receive() == pack('<I', 5) + group
                    # Genesis on an empty store commits exactly activation 1.
                    genesis = child(0, 1)
                    start(old, genesis, pack('<I', 0))
                    installed = pack('<IQB', 5, 1, 0)
                    placed(genesis, installed)
                    exited = pack('<IiIB', 0, 0, 0, 0)
                    assert receive() == pack('<I', 3) + genesis + exited
                    # The result is replayable until the host acknowledges the end.
                    send(0)
                    assert receive() == pack('<II', 0, 1) + genesis + pack('<I', 1) + exited + bytes(16)
                    placed(genesis, installed)
                    send(6, genesis)
                    if new is None:
                        # There have been no data commits. A request for 3 must refuse
                        # with the store's next version 2, and leave activation 1 intact.
                        wrong = child(0, 2)
                        start(old, wrong, pack('<IQ', 1, 3))
                        placed(wrong, pack('<IIQ', 11, 8, 2))
                        end(wrong, 0, 1)
                        probe(old, 3, 1, 1)
                        upgrade = child(0, 4)
                        start(old, upgrade, pack('<IQ', 1, 2))
                        placed(upgrade, pack('<IQB', 5, 2, 0))
                        end(upgrade, 0, 0)
                        probe(old, 5, 2, 2)
                        return

                    # The child is in the install's LOCK interval and is unable to
                    # finish migrate. Kill it once. An end without proof is uncertain.
                    crash = child(0, 2)
                    with (live / 'LOCK').open('rb') as lock:
                        start(new, crash, pack('<IQ', 1, 2), [b'hold-install'])
                        deadline = time.monotonic() + 10
                        while True:
                            try:
                                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            except BlockingIOError:
                                break
                            fcntl.flock(lock, fcntl.LOCK_UN)
                            assert time.monotonic() < deadline, 'install did not take the store lock'
                            time.sleep(0.005)
                    send(5, crash)
                    end(crash, 1, 9)
                    # Undecided recovery first probes NEW at 2. Its Current(1)
                    # selects OLD's pair. Only OLD's successful open proves refusal.
                    probe(new, 3, 2, 1)
                    probe(old, 4, 1, 1)
        except Exception as error:
            log.seek(0)
            error.add_note(log.read().decode(errors='replace'))
            raise
        finally:
            if launcher.poll() is None:
                comm = subprocess.check_output(['ps', '-o', 'comm=', '-p', str(launcher.pid)], text=True)
                assert comm.strip() == 'hirsel', comm
                launcher.kill()
            launcher.wait(timeout=15)


def crashes(args, root, work, engine, name):
    """L1 at every cut of each scenario, then POISON once; raises on the first failure."""
    inputs_path = manifest(root, work, 'crash_' + name, crash=True)
    env = dict(os.environ)
    env['LD_LIBRARY_PATH'] = ':'.join([str(args.bridge.resolve().parent),
                                       str(root / 'tools/buck2/toolchains/llvm-files/lib'),
                                       env.get('LD_LIBRARY_PATH', '')])
    build = work / f'crash_{name}'
    build.mkdir()
    rows = run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc, inputs_path, build, engine,
                       entry='store_crash@1/runner_crash/main', scenarios=CRASHES, environment=env,
                       adapter='runner_crash.c')
    checked([str(build / (engine + '-crash') / 'store'), POISON, 'count'], env, engine=engine)
    return rows

def drain_deadline(args, root, work, engine, name, runner=None, activation=False):
    inputs_path = manifest(root, work, name, runner, crash=True)
    build = work / name
    build.mkdir()
    stop_cases = (args.stop_case,) if args.stop_case else STOP_CASES
    return run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc, inputs_path, build, engine,
                       entry='store_crash@1/runner_crash/' + ('main' if activation else 'stop_main'), adapter='runner_crash.c',
                       scenarios=('runner-activation', 'runner-activation-genesis') if activation else (),
                       count_only=('runner-unavailable-genesis', 'runner-unavailable-install') if activation else stop_cases)


def manifest(root, work, name, runner=None, extra=(), crash=False):
    m = crash_inputs(root) if crash else runner_inputs(root)
    sources = next(p for p in m['packages'] if p['package']['name'] == 'runner')['sources']
    if runner is not None:
        for s in sources:
            if s['file'].endswith('/runner.hirsel'):
                s['file'] = str(runner)
    for path, module in extra:
        m['packages'][-1]['sources'].append(dict(file=str(path), modules=[module], signatures=False))
    out = work / (name + '.json')
    out.write_text(json.dumps(m))
    return out


# The evaluator records CONC-10 events only for the laws that read them (counted,
# children_joined): a wire flood makes about 7,500 per frame.
EVENT_LAWS = {*(f'runner@1/runner_calls/law_{name}' for name in COUNTS), 'runner@1/runner/law_children_joined_on_close', 'runner@1/runner/law_shared_view_once'}


def simulation_entry(inputs_path, definition, scratch, window):
    inputs = json.loads(inputs_path.read_text())
    package, *modules, name = definition.split('/')
    package_name, version = package.split('@')
    sources = next(p['sources'] for p in inputs['packages']
                   if p['package'] == {'name': package_name, 'version': version})
    pattern = re.compile(rf'^pub fn {re.escape(name)}\(([^)]*)\)\s*->\s*([a-zA-Z0-9_]+)\s+waits\s*{{', re.M)
    for module in sources:
        if module['modules'] != modules:
            continue
        source = Path(module['file']).read_text()
        if (signature := pattern.search(source)) is not None and re.search(r':\s*Net\b', signature[1]):
            break
    else:
        return inputs_path, definition
    parameters = [tuple(part.strip() for part in pair.split(':')) for pair in signature[1].split(',') if pair.strip()]
    task_name = next((name for name, ty in parameters if ty == 'Tasks'), None)
    others = [f'{name}: {ty}' for name, ty in parameters if ty not in ('Clock', 'Net')]
    if task_name is None:
        task_name = 'simulation_tasks'
        others.append('simulation_tasks: Tasks')
    replacements = {'Clock': 'simulation_time', 'Net': 'simulation_net'}
    arguments = ', '.join(replacements.get(ty, name) for name, ty in parameters)
    source += simulation_support('std.sim')
    source += f"""
pub fn run_simulation({', '.join(others)}) -> {signature[2]} waits {{
    let (simulation_net, simulation_time, simulation_server, simulation_clock) =
        start_contract_simulation({task_name}, {window})
    let simulation_result = {name}({arguments})
    result.unwrap(task.join(take simulation_server))
    result.unwrap(task.join(take simulation_clock))
    simulation_result
}}
"""
    new_source = scratch / Path(module['file']).name
    new_source.write_text(source)
    module['file'] = str(new_source)
    manifest = scratch / 'simulation-inputs.json'
    manifest.write_text(json.dumps(inputs))
    return manifest, '/'.join([package, *modules, 'run_simulation'])


def evaluate(evaluator, inputs_path, definition, launch_image=None):
    with tempfile.TemporaryDirectory(prefix='runner-simulation-', dir=inputs_path.parent) as directory:
        manifest, entry = simulation_entry(inputs_path, definition, Path(directory),
                                           8192 if definition in WIDE_WINDOW_LAWS else 8)
        request = dict(manifest=str(manifest), entry=entry, events=definition in EVENT_LAWS)
        if launch_image is not None:
            request['launch_image'] = str(launch_image)
        if definition in WIRE_TEST_INSTRUCTIONS:
            request['instructions'] = WIRE_TEST_INSTRUCTIONS[definition]
        result = run_case(f'{definition} [{inputs_path.stem}]', 'evaluator', [str(evaluator)],
                          input=json.dumps(request) + '\n', capture_output=True, text=True)
    try:
        report = json.loads(result.stdout)
    except ValueError:
        report = {'stdout': result.stdout, 'stderr': result.stderr}
    out = report.get('outcome') or {}
    value = out.get('values', [None])[0] if result.returncode == 0 and out.get('kind') == 'returned' else None
    return value, report


def mint_waits(args, root, work, row):
    """L36 mint_waits (RUNNER-SPEC v2.69 F3): a Mint whose mint waits is refused on its type."""
    name = 'mint_waits'
    rejected = manifest(root, work, name, extra=[(root / f'core/programs/runner/{name}.hirsel', name)])
    command = [str(args.compiler.resolve()), 'check', '--inputs', str(rejected),
               '--source-core', '--source-std', '--store', str(work / name), '--json']
    checked = run_case(name, 'checker', command, capture_output=True, text=True)
    report = json.loads(checked.stdout)
    actual = [(d.get('rule'), d.get('message')) for d in report.get('diagnostics', [])]
    expected = [('LANG-13', 'the callable does not fit the written callable type')]
    passed = (checked.returncode == 1 and report.get('accepted') is False and actual == expected
              and not report.get('incomplete') and not report.get('resources'))
    row(engine='checker', law=name, passed=passed, result=report)


def activation_body_edit(root, work):
    """Keep the entry name, signature and schemas, and change only its body."""
    cases = root / 'core/programs/runner/runner_calls.hirsel'
    edited = work / cases.name
    source = cases.read_text()
    original = '    match store.put(mut tx, h.t, k, k) {'
    assert source.count(original) == 1
    edited.write_text(source.replace(original, '    match store.put(mut tx, h.t, k, 42u64) {'))
    selection = runner_inputs(root)
    for module in selection['packages'][-1]['sources']:
        if module['modules'] == ['runner_calls']:
            module['file'] = str(edited)
            break
    selected = work / 'activation-body-edit.json'
    selected.write_text(json.dumps(selection))
    return selected


def frame_module(root):
    """Construct values beside the shared, hand-derived byte oracle, never from it."""
    vectors = json.loads((root / 'core/programs/runner/frame-vectors.json').read_text())['vectors']
    helpers = (root / 'core/programs/runner/frame-vector-helpers.hirsel').read_text()
    lines = ['// Generated from frame-vectors.json by core/tools/runner_tests.py, then',
             '// formatted; the r0 frame_vectors part refuses a stale copy.', helpers, '',
             'pub fn law_frame_vectors() -> bool {',
             '    let out = result.unwrap(std.wsock.outbound(Server, runner.wire_defaults(0u64).socket, None))']
    for v in vectors:
        payload = bytes.fromhex(v['hex'])
        if 'wire_bytes' in v:
            header = 2 if len(payload) <= 125 else 4 if len(payload) <= 65535 else 10
            assert len(payload) + header == v['wire_bytes'], v['name']
        lines += [f'    // {v["name"]}: {v["derivation"]}',
                  f'    let c_{v["name"]}: std.codec.Codec[runner.{v["type"]}] = std.codec.of()',
                  f'    if !vector(c_{v["name"]}, {v["value"]}, b("{v["hex"]}")) {{ return false }}']
        # Admission, max_frame and the in-place WebSocket header all take a
        # frame's size from wire_output_size; the encoder fails closed unless
        # it counts exactly the codec's bytes, which the hex fixes by hand.
        if v['type'] == 'Frame':
            # RFC 6455 5.2 unmasked final binary frame header, derived here.
            n = len(payload)
            size = bytes([n]) if n <= 125 else bytes([126]) + n.to_bytes(2, 'big') if n <= 65535 else bytes([127]) + n.to_bytes(8, 'big')
            lines += [f'    if !wire(out, {v["value"]}, b("{(bytes([0x82]) + size + payload).hex()}")) {{ return false }}']
    lines += ['    true', '}', '', '@test', 'fn test_frame_vectors() -> Result[(), ()] {',
              '    if law_frame_vectors() { Ok(()) } else { Error(()) }', '}', '']
    return '\n'.join(lines), len(vectors)


def frame_vectors(args, root, work, engines, row):
    """The package's runner_wire.hirsel is this generator's formatted output;
    `hirsel test` runs it natively."""
    module = root / 'core/programs/runner/runner_wire.hirsel'
    text, count = frame_module(root)
    if args.compiler is not None:
        formatted = run_case('frame_vectors:fmt', 'fmt', [str(args.compiler.resolve()), 'fmt', '--stdin'],
                             input=text, capture_output=True, text=True)
        current = formatted.returncode == 0 and formatted.stdout == module.read_text()
        row(engine='fmt', law='frame_vectors_current', passed=current,
            result=formatted.stderr or 'regenerate core/programs/runner/runner_wire.hirsel')
    selected = manifest(root, work, 'wire', extra=[(module, 'runner_wire')])
    if 'evaluator' in engines:
        value, report = evaluate(args.evaluator.resolve(), selected, 'runner@1/runner_wire/law_frame_vectors')
        row(engine='evaluator', law='frame_vectors', vectors=count, passed=value == 1, result=report)



def children_joined(report):
    events = report.get('events', [])
    children = [e[2] for e in events if e[0] == 0 and e[1] == 0]
    cancellations = [i for i, e in enumerate(events) if e[0] == 2 and e[1] == 0 and e[2] in children]
    joins = [i for i, e in enumerate(events) if e[0] == 4 and e[1] == 0 and e[2] in children]
    return (len(children) == len(cancellations) == len(joins) == 2
            and max(cancellations) < min(joins))


READER_CANCELLATION = 'pool_releases_cancelled_reader'
SESSION_LAWS = ('pool_holds_writer_bytes', READER_CANCELLATION, 'write_spawned_on_block', 'write_capacity_ends',
                'children_joined_on_close', 'session_records', 'serving_budget', 'subscription_room_refunded')


def session_laws(args, root, work, engines, row):
    text = (root / 'core/programs/runner/runner.hirsel').read_text()
    laws = (root / 'core/programs/runner/session-laws.hirsel').read_text()
    adapted = work / 'session' / 'runner.hirsel'
    adapted.parent.mkdir()
    adapted.write_text(text + '\n' + laws)
    selected = manifest(root, work, 'session', adapted)
    names = args.session_law or SESSION_LAWS
    if 'evaluator' in engines:
        for name in names:
            value, report = evaluate(args.evaluator.resolve(), selected, 'runner@1/runner/law_' + name)
            holds = value == 1 and (name != 'children_joined_on_close' or children_joined(report))
            row(engine='evaluator', law=name, passed=holds, result=report)


SETTLEMENT_LAWS = ('after_publication',)
# L28's owner half: a dropped unit's Outcome rows ride in a Reset that never closes 1008,
# and an owner removal reaches the publisher only while the writer holds no content unit.
RELEASE_LAWS = ('dropped_keeps_outcomes', 'removal_waits_for_writer')
# L24's owner half: repairs queued behind a busy writer coalesce, so the newest board arrives
# with no Reset.
REPAIR_LAWS = ('repair_coalesced', 'repair_one_unsent', 'join_waits')


def admission_laws(args, root, work, engines, row):
    text = (root / 'core/programs/runner/runner.hirsel').read_text()
    laws = (root / 'core/programs/runner/admission-laws.hirsel').read_text()
    adapted = work / 'admission-runner.hirsel'
    adapted.write_text(text + '\n' + laws)
    selected = manifest(root, work, 'admission-laws', adapted)
    selection = json.loads(selected.read_text())
    store = next(source for source in selection['packages'][-1]['sources'] if source['modules'] == ['store'])
    store_source = Path(store['file'])
    with_faults = work / 'admission-store.hirsel'
    with_faults.write_text(store_source.read_text() + '\n' + store_source.with_name('assembly_laws.hirsel').read_text())
    store['file'] = str(with_faults)
    selected.write_text(json.dumps(selection))
    durable = ('gone_slot_v5', 'request_mismatch_wire', 'history_below_latest', 'reply_over_done_max')
    state = ('unread_client', 'hello_and_slots', 'join_cleanup', 'full_head_waits')
    names = args.admission_law or (*state, *durable, 'wire_replacement_generation', *SETTLEMENT_LAWS, *RELEASE_LAWS, *REPAIR_LAWS)
    if 'evaluator' in engines:
        for name in names:
            value, report = evaluate(args.evaluator.resolve(), selected, 'runner@1/runner/law_' + name)
            row(engine='evaluator', law=name, passed=value == 1, result=report)

def socket_laws(args, root, work, engines, row):
    names = {'hello_order', 'slot_out_of_range', 'gone_slot_v5', 'second_hello_closes',
             'request_mismatch_wire', 'resend_same_p', 'held_request_mismatch', 'hello_deadline', 'resume_unknown', 'history_below_latest', 'reply_over_done_max', 'faulted_owner_releases_slot', 'welcome_with_small_budget',
             'cli_call_exact_value', 'cli_call_fingerprint_mismatch', 'supplied_id_never_rotates', 'welcome_samples_store_stream_time'}
    for engine in (e for e in engines if e != 'evaluator'):
        output = work / ('socket-' + engine)
        command = [sys.executable, str(root / 'tools/tests/wire_build.py'),
                   '--compiler', str(args.compiler.resolve()), '--runtime', str(args.runtime.resolve()),
                   '--admission-only', '--backend', engine, '--output', str(output)]
        if args.cc:
            command += ['--cc', str(args.cc.resolve())]
        built = run_case('wire_socket_build', engine, command, cwd=root, capture_output=True, text=True)
        if built.returncode:
            row(engine=engine, law='wire_socket_build', passed=False, result=(built.stdout + built.stderr)[-4000:])
            continue
        report = output / 'laws.json'
        laws = [sys.executable, str(root / 'tools/tests/wire_admission.py'), '--server', str(output / ('admission-' + engine)),
                '--report', str(report), '--compiler', str(args.compiler.resolve()), '--runtime', str(args.runtime.resolve())]
        run = run_case('wire_socket_selection', engine, laws + (['--cc', str(args.cc.resolve())] if args.cc else []),
                             cwd=root, capture_output=True, text=True)
        cases = json.loads(report.read_text()) if report.is_file() else []
        for case in cases:
            row(engine=engine, **case)
        row(engine=engine, law='wire_socket_selection', passed=run.returncode == 0
            and len(cases) == len(names) and {r['law'] for r in cases} == names
            and all(r.get('passed') for r in cases), result=(run.stdout + run.stderr)[-4000:])


def l12_values(evaluator, root, work, tag):
    """The fingerprint per variant (base twice), or None where a run did not return."""
    values = {}
    for v in L12 + (('base_again',) + L12[0][1:],):
        mod = work / f'runner_l12_{tag}_{v[0]}.hirsel'
        l12_module(mod, v)
        m = manifest(root, work, f'l12_{tag}_{v[0]}', extra=[(mod, 'runner_l12')])
        values[v[0]], _ = evaluate(evaluator, m, 'runner@1/runner_l12/law_l12_describe')
    return values


def l12_revisions(evaluator, root, work):
    """ea's revision fingerprint per variant, over l12_values(.., 'tree')'s manifests."""
    return {v[0]: evaluate(evaluator, work / f'l12_tree_{v[0]}.json', 'runner@1/runner_l12/law_l12_revision')[0]
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
    run_case(f'{name}:observer', 'cc', [str(cc), '-c', '-fPIC', str(root / 'core/tools/runner_count.c'),
                    '-I' + str(root / 'crates/hirsel-core-rt/include'), f'-DTRY_SENDS={want or 0}',
                    f'-DSOURCE_MANIFEST_DIR={json.dumps(str(work), ensure_ascii=False)}',
                    *([f'-D{PLACEMENT_OBSERVERS[name]}'] if name in PLACEMENT_OBSERVERS else []),
                    *(['-DSHARED_VIEW_ONCE'] if name == 'shared_view_once' else []),
                    *([f'-DHUNG_STEPS={HUNG_STEPS}', f'-DHUNG_TOLDS={HUNG_TOLDS}'] if name == 'calls_deadline_hung' else []),
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


def native_run(args, engine, inputs_path, store, prefix, cc=None, runtime=None, output=None):
    runtime = runtime or args.runtime
    command = [str(args.compiler.resolve()), 'test', '--inputs', str(inputs_path), '--source-core',
               '--source-std', '--backend', engine, '--runtime', str(runtime.resolve()),
               '--store', str(store), '--json', '--filter', prefix,
               '--limit', str(64 * 1024 * 1024), '--grant', 'Clock', '--grant', 'Tasks', '--grant', 'Random',
               '--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535',
               '--dir', str(dir_for(store))]
    if output is not None:
        command += ['-o', str(output)]
    if engine == 'llvm':
        command.append('--release')
    if cc or args.cc:
        command += ['--cc', str(cc or args.cc.resolve())]
    result = run_case(f'{prefix} [{inputs_path.stem}]', engine, command, capture_output=True, text=True)
    try:
        report = json.loads(result.stdout)
    except ValueError:
        report = {'stdout': result.stdout, 'stderr': result.stderr}
    return result.returncode, report


def matched_launch(args, root, work, engine, inputs, name, label):
    """Return the selected compiled test's image and its own native outcome."""
    output = work / f'launch-{engine}-{label}'
    definition = f'runner@1/runner_calls/test_{name}'
    cc = counting_cc(args, root, inputs, name) if name in PLACEMENT_OBSERVERS else None
    native = native_run(args, engine, inputs, work / f'store-{engine}-shared', definition,
                        cc=cc, output=output)
    images = [p for p in output.glob('*') if p.is_file() and os.access(p, os.X_OK)]
    if len(images) != 1:
        raise RuntimeError(f'{definition}: expected one compiled test image, got {len(images)}: {native[1]}')
    return images[0], native


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=ROOT)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--evaluator', type=Path)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--schedule-runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_SCRIPTED_ARCHIVE'))
    p.add_argument('--observed', type=Path,
                   default=Path(os.environ['HIRSEL_CORE_RT_OBSERVED_ARCHIVES']) / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
                   if 'HIRSEL_CORE_RT_OBSERVED_ARCHIVES' in os.environ else None)
    p.add_argument('--bridge', type=Path, default=os.environ.get('HIRSEL_STORE_CRASH_BRIDGE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm'])
    p.add_argument('--stop-case', choices=STOP_CASES)
    p.add_argument('--call-law', action='append', choices=CALLS)
    p.add_argument('--host-law', choices=HOST_LAWS)
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    p.add_argument('--session-law', action='append', choices=SESSION_LAWS)
    p.add_argument('--admission-law', action='append', choices=['wire_replacement_generation', 'unread_client', 'hello_and_slots', 'join_cleanup',
                   'full_head_waits', 'gone_slot_v5', 'request_mismatch_wire', 'history_below_latest', 'reply_over_done_max', *SETTLEMENT_LAWS, *RELEASE_LAWS, *REPAIR_LAWS])
    p.add_argument('--part', choices=['r0', 'r1', 'wire', 'session', 'admission', 'socket', 'calls', 'deadline', 'activation', 'rc1', 'host', 'idle-stop', 'shared-views'])
    args = p.parse_args()
    if args.part == 'host':
        if not args.host_law or not args.compiler or len(args.backend or []) != 1 or args.backend == ['evaluator']:
            p.error('host requires an installed --compiler, one native --backend, and --host-law')
        with tempfile.TemporaryDirectory(prefix='hirsel-host-law-') as tmp:
            host_law(args, args.root.resolve(), Path(tmp), args.backend[0], args.host_law)
        print(f'{args.backend[0]} host {args.host_law}: passed', flush=True)
        return 0
    if args.host_law:
        p.error('--host-law requires --part host')
    if not args.evaluator:
        p.error('--evaluator is required')
    if args.stop_case and args.part != 'deadline':
        p.error('--stop-case requires --part deadline')
    r0, r1 = args.part in (None, 'r0'), args.part in (None, 'r1', 'calls')
    selected_calls = args.call_law or CALLS
    root = args.root.resolve()
    evaluator = args.evaluator.resolve()
    engines = args.backend or ['evaluator', 'cranelift', 'llvm']
    if engines == ['evaluator'] and NATIVE_ONLY_CALLS.intersection(args.call_law or ()):
        p.error('drain_reopened and lag_window require a native backend')
    retired = sorted(set(args.call_law or ()) - set(COUNTS) - set(PLACEMENT_OBSERVERS))
    if retired and engines != ['evaluator']:
        p.error(f'{", ".join(retired)} run natively under `hirsel test core/programs/runner`; pass --backend evaluator')
    if args.part == 'rc1' and (not args.compiler or not args.runtime or engines == ['evaluator']):
        p.error('RC1 requires a native backend, --compiler and --runtime')
    rows = []

    def row(**r):
        rows.append(r)
        print(' '.join(f'{k}={v}' for k, v in r.items() if k != 'result' or r.get('law') == 'l4_peak_retained'), flush=True)

    with tempfile.TemporaryDirectory(prefix='hirsel-runner-laws-') as tmp:
        work = Path(tmp)
        if r0 or args.part == 'shared-views':
            inputs = manifest(root, work, 'shared-views')
            for engine in engines:
                if engine == 'evaluator':
                    value, report = evaluate(evaluator, inputs, 'runner@1/runner/law_shared_view_once')
                    sites = report.get('event_sources', [])
                    events = report.get('events', [])
                    runs = sum(e[0] == 7 and site == 'runner@1/runner/shared_count_query'
                               for e, site in zip(events, sites))
                    passed = value == 1 and len(sites) == len(events) and runs == 2
                    row(engine=engine, law='shared_view_once', passed=passed, result={'value': value, 'runs': runs, 'error': report.get('error')})
                else:
                    cc = counting_cc(args, root, inputs, 'shared_view_once')
                    code, report = native_run(args, engine, inputs, work / 'shared-view-store',
                                              'runner@1/runner/test_shared_view_once', cc)
                    tests = report.get('tests', [])
                    passed = (code == 0 and len(tests) == 1 and tests[0]['passed'])
                    row(engine=engine, law='shared_view_once', passed=passed, result=report)
            if args.part == 'shared-views':
                if args.report:
                    args.report.write_text(json.dumps(rows, indent=2) + '\n')
                return int(any(not result['passed'] for result in rows))
        if args.part in (None, 'r1', 'idle-stop'):
            if args.part == 'idle-stop' and engines == ['evaluator']:
                p.error('idle-stop requires a native backend')
            if any(engine != 'evaluator' for engine in engines):
                if not args.compiler or not args.runtime:
                    p.error('native backends require --compiler and --runtime')
                runner_idle_stop(args, work, engines, row, manifest(root, work, 'idle-stop'), native_run, dir_for)
            if args.part == 'idle-stop':
                if args.report:
                    args.report.write_text(json.dumps(rows, indent=2) + '\n')
                for r in rows:
                    if not r['passed']:
                        print('FAIL', json.dumps(r)[:4000])
                return int(not rows or any(not result['passed'] for result in rows))
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
        if args.part == 'admission' or (r1 and args.part != 'calls'):
            admission_laws(args, root, work, engines, row)
        if r0 or args.part == 'session':
            session_laws(args, root, work, engines, row)
        if r0:
            frame_vectors(args, root, work, engines, row)
        if args.part in ('wire', 'socket'):
            socket_laws(args, root, work, engines, row)
        if args.part in (None, 'r1', 'rc1'):
            for engine in (e for e in engines if e != 'evaluator'):
                no_secret_at_rest(args, work, engine, manifest(root, work, 'no_secret_at_rest', crash=True), row)
                no_jwt_at_rest(args, work, engine, manifest(root, work, 'no_jwt_at_rest', crash=True), row)
        if args.part in ('wire', 'session', 'admission', 'socket', 'rc1'):
            if args.report:
                args.report.write_text(json.dumps(rows, indent=2) + '\n')
            for r in rows:
                if not r['passed']:
                    print('FAIL', json.dumps(r)[:2000])
            return int(any(not r['passed'] for r in rows))
        if r1 and 'granted_copied' in selected_calls and args.compiler is not None:
            mint_waits(args, root, work, row)
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
                value, report = evaluate(evaluator, base, 'runner@1/runner_laws/law_' + name)
                row(engine='evaluator', law=name, passed=value == 1, result=report)
            value, report = evaluate(evaluator, manifest(root, work, 'p19', None, [(p19, 'runner_p19')]),
                                     'runner@1/runner_p19/law_p19_place_twice')
            row(engine='evaluator', law='p19_place_twice', passed=value == 1, result=report)
        launch_pairs = {}
        launch_engine = next((e for e in engines if e != 'evaluator'), 'cranelift')
        if 'evaluator' in engines and r1:
            for name in selected_calls:
                if name in NATIVE_ONLY_CALLS:
                    continue
                definition = 'runner@1/runner_calls/law_' + name
                image = None
                if name in PERSISTENT_PLACE_CALLS:
                    if args.compiler is None or args.runtime is None:
                        p.error('persistent-place evaluator laws require --compiler and --runtime for the launch image')
                    image, launch_pairs[name] = matched_launch(args, root, work, launch_engine, base, name, name)
                value, report = evaluate(evaluator, base, definition, image)
                if name == 'ready_after_activation':
                    revised = activation_body_edit(root, work)
                    edited_image, _ = matched_launch(args, root, work, launch_engine, revised, name, 'activation-edit')
                    changed, edited = evaluate(evaluator, revised, definition, edited_image)
                    row(engine='evaluator', law=name, passed=bool(value and changed and value != changed),
                        result={'base': report, 'body_edit': edited})
                else:
                    row(engine='evaluator', law=name, passed=value == 1 and counted(name, report), result=report)
            for name in (() if args.part == 'calls' else SUBS):
                value, report = evaluate(evaluator, base, 'runner@1/runner/law_' + name)
                row(engine='evaluator', law=name, passed=value == 1, result=report)
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
            l12_want = {'runner@1/runner_l12/test_l12_describe', 'runner@1/runner_l12/test_l12_revision'}
            # p19's place builds as its own program, in the store the laws primed and in a
            # fresh one: at 10,000 entries it reaches over 10,000 functions, each described.
            groups = () if not r0 else (
                      ('l12', [(l12, 'runner_l12')], 'runner@1/runner_l12/', 'shared', l12_want),
                      ('p19', [(p19, 'runner_p19')], 'runner@1/runner_p19/', 'shared',
                       {'runner@1/runner_p19/test_p19_place_twice'}),
                      ('p19_fresh', [(p19, 'runner_p19')], 'runner@1/runner_p19/', 'fresh',
                       {'runner@1/runner_p19/test_p19_place_twice'}),
                      ('l12_permuted', [(permuted, 'runner_l12')], 'runner@1/runner_l12/', 'shared', l12_want),
                      ('l12_permuted_fresh', [(permuted, 'runner_l12')], 'runner@1/runner_l12/', 'fresh', l12_want))
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
                # The other calls laws run in the package under `hirsel test`.
                for name in (n for n in selected_calls if n in COUNTS or n in PLACEMENT_OBSERVERS) if r1 else ():
                    definition = f'runner@1/runner_calls/test_{name}'
                    cc = counting_cc(args, root, base, name) if name in COUNTS or name in PLACEMENT_OBSERVERS else None
                    runtime = args.schedule_runtime if name == 'calls_deadline_hung' else args.runtime
                    if runtime is None:
                        p.error('calls_deadline_hung requires --schedule-runtime')
                    if engine == launch_engine and name in launch_pairs:
                        code, report = launch_pairs[name]
                    else:
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
                    held, figures = l20_retained(args, root, work, engine, (('0', 'short'), ('1', 'long'), ('2', 'wide')), 'runner/slot_image_retention')
                    row(engine=engine, law='l20_tail_retention', passed=bool(held), result=figures)
                    try:
                        row(engine=engine, law='l1_crash', passed=True,
                            result=crashes(args, root, work, engine, engine))
                    except RuntimeError as error:
                        row(engine=engine, law='l1_crash', passed=False, result=str(error)[-2000:])
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
    failures = [r for r in rows if not r['passed']]
    for r in failures:
        print('FAIL', json.dumps(r)[:2000])
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(main())
