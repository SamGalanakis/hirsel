#!/usr/bin/env python3
"""Run the runner's named laws and cases (RUNNER-SPEC v2.5 §8).

R0 owns core, session, and frame laws, including the generated p19 and L12
fixtures. Its L4/L20 cases compare exact retained bytes across native runs.
R1 owns calls, admission, and ingress. The wire target runs the socket cases.
Without --part, both R0 and R1 run.

The crash adapter runs the W2 filesystem cuts and one failed journal sync.
RC1's L31 checks that the disk store never writes the plaintext Hello token.
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

from case_progress import run_case
from std_native import MODEL_NET_RULES, ROOT
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

CALLS = ('atomic_migration', 'serve_bounded_and_settled', 'duplicate_own_reply', 'release_digest', 'ready_after_activation', 'drain_answers_once', 'drain_reopened', 'lag_window', 'calls_two_streams', 'calls_group_version', 'calls_joiners', 'calls_duplicate', 'calls_refusals',
         'calls_runs_once', 'calls_session_charge', 'calls_store_refusal', 'calls_one_outcome_per_send', 'calls_deadline_held', 'calls_deadline_hung', 'calls_held_head', 'calls_retired_head', 'calls_session_mismatch', 'calls_recorded', 'subs_wired', 'publisher_never_blocks', 'repair_delivered',
         'dir_one_head', 'dir_open_during_close', 'dir_idle_unload', 'dir_reopen_keeps')
# The place directory's laws (RUNNER-SPEC §13.6) bind sockets and use disk stores.
NATIVE_ONLY_CALLS = {'drain_reopened', 'lag_window', 'dir_one_head', 'dir_open_during_close', 'dir_idle_unload', 'dir_reopen_keeps'}
PERSISTENT_PLACE_CALLS = {'atomic_migration', 'ready_after_activation', 'calls_recorded'}

# L3 (RUNNER-SPEC v2.6): execution counts read from CONC-10 events, (event kind, count).
# TRY_SEND is kind 7 in crates/hirsel-core-layout/src/event.rs. calls_runs_once
# reaches tally once and receives four answers. The session sends one retirement
# wake to the head, so the total is 1 + 4 + 1. The evaluator's count comes from
# its event log; natively, core/tools/runner_count.c is linked into the case's test
# executable as its event sink and fails the run on any other count.
# L7 adds the source-derived sends from hung and one marker by the other session's
# compute body. RUNNER-SPEC.md §8, L6+L7 orders the compute before the hung
# call's stop, observed at its first publisher offer. MEM-30 makes the slice
# a scheduling point. The three answers to s0 go to one inbox.
# The body's prelude inside its 65,536-step bound is 158 steps: the 165 counted at
# 3a3ccdc8, less the seven calls STORE v2.30 AN1 deleted from that path
# (current_authority, share_authority, arc.share and full_scope's call and
# list.new in draft_body's Tx; clone of the granted scope in stage_reply; and
# check_key in put).
HUNG_BODY_SENDS = (65536 - 158 + 1) // 2
HUNG_TOLDS = 3
COUNTS = {'calls_runs_once': (7, 6), 'calls_deadline_hung': (7, HUNG_BODY_SENDS + HUNG_TOLDS + 2)}
PLACEMENT_OBSERVERS = {'ready_after_activation': 'READY_BEFORE_ACCEPT', 'lag_window': 'LAG_ADMISSION'}

# Wire floods retain all 10,000 frames. The evaluator's generic 1B test envelope
# predates framing. At 1/100 frames, refusals use 8,148,075/20,157,978 steps,
# session_charge 80,393,142/325,732,340. Extrapolate the 99-frame difference
# to 10,000, double it for margin, then round up. Entry step bounds stay fixed.
WIRE_TEST_INSTRUCTIONS = {
    'store@1/runner_calls/law_calls_refusals': 2_500_000_000,
    'store@1/runner_calls/law_calls_session_charge': 50_000_000_000,
}
# Flood laws read through an 8 KiB model window. Byte-split reads stay on every other wire law:
# under the default 8-byte window a 4 KiB frame costs about 830 Decode round trips.
WIDE_WINDOW_LAWS = {'store@1/runner_calls/law_calls_session_charge'}

# R2's publisher laws, in runner.hirsel itself (RUNNER-SPEC §8.2).
SUBS = ('subs_main', 'subs_reset_keeps_diff', 'rows_normalized', 'projection_isolated', 'subscribe_order', 'retired_without_waiting', 'stale_never_released')


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
            attributed.append((e, site))
    if sum(1 for e, _ in attributed if e[0] == kind) != want:
        return False
    if name == 'calls_deadline_hung':
        sends = {}
        for at, (e, site) in enumerate(attributed):
            if e[0] == kind:
                sends.setdefault(e[2], []).append((at, e[1], site))
        if sorted(map(len, sends.values())) != [1, 1, HUNG_TOLDS, HUNG_BODY_SENDS]:
            return False
        told = next(v for v in sends.values() if len(v) == HUNG_TOLDS)
        marker = next((v[0] for v in sends.values() if len(v) == 1
                       and v[0][2] == 'store@1/runner_calls/answer'), None)
        retirement = next((v[0] for v in sends.values() if len(v) == 1
                           and v[0][2] == 'store@1/runner/try_head'), None)
        body = next(v for v in sends.values() if len(v) == HUNG_BODY_SENDS)
        slices = [(at, e[1]) for at, (e, _) in enumerate(attributed) if e[0] == 21]
        return (marker is not None and retirement is not None and len(slices) == 1
                and marker[0] < told[0][0]
                and slices[0][0] < told[0][0] < retirement[0]
                and marker[1] != slices[0][1] == body[0][1]
                and all(t != slices[0][1] and site == 'store@1/runner/offer' for _, t, site in told))
    return True


# L4 (RUNNER-SPEC v2.5 §8): runner_rss.hirsel's program, built per native backend against
# the charge-observation runtime and run in two modes, one valid call (0) and 10,000
# malformed frames before it (1). Both exit 0 and their --logical-charge-report
# peak_retained_bytes (the runtime's exact peak of live backing requests) are equal: a
# refused frame that leaves any bytes behind raises mode 1's peak. ru_maxrss, the
# earlier figure, moves by 256 KB steps from run to run and cannot see a small leak.
def charged(args, root, work, engine, module, modes, figure, cc=None):
    """Build programs/store/<module>.hirsel on the charge-observation runtime and run it in
    each (argv, label) mode: {label: (exit status, figure(charge report))}, or None and the
    build's error."""
    name = module
    inputs_path = manifest(root, work, name, extra=[(root / f'core/programs/store/{module}.hirsel', module)])
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
    built = run_case(f'{name}:build', engine, command, capture_output=True, text=True)
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
        run = run_case(f'{name}:{label}', engine, command, capture_output=True)
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


def l20_retained(args, root, work, engine):
    name = 'runner_cover'
    cc = c_compiler(args)
    support = work / f'{name}_mark_{engine}.o'
    run_case(f'{name}:observer', engine, [str(cc), '-c', '-fPIC', str(root / 'core/tools/runner_mark.c'),
                    '-I' + str(root / 'crates/hirsel-core-rt/include'), '-o', str(support)], check=True)
    wrapper = work / f'{name}_mark_{engine}-cc'
    wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc)) + ' "$@" '
                       + shlex.quote(str(support)) + ' -Wl,--wrap=hirsel_core_os_run_with_sources\n')
    wrapper.chmod(0o755)
    figures, error = charged(args, root, work, engine, 'runner_cover', (('0', 'ten'), ('1', 'all')),
                             subscribed_bytes, wrapper)
    if figures is None:
        return None, {'build': error}
    ran = figures['ten'][0] == 0 and figures['all'][0] == 0 and figures['ten'][1] is not None and figures['all'][1] is not None
    return ran and figures['ten'][1] == figures['all'][1], figures if ran else {'run': figures}


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
        values[name] = tuple(evaluate(evaluator, m, 'store@1/runner_l10/law_l10_' + law)[0] for law in ('identity', 'result'))
    return values


def l10_holds(values):
    (bi, br), (ei, er) = values['base'], values['e_field']
    return None not in (bi, br, ei, er) and 0 not in (bi, br, ei, er) and bi == ei and br != er


# R1b (RUNNER-SPEC v2.10 L1): runner_crash's scenarios, each run at every W2 cut, and
# its poison scenario, whose one failed journal sync the adapter checks in count mode.
CRASHES = ('runner-cross', 'runner-order')
POISON = 'poison-segment'
STOP_CASES = ('runner-deadline', 'runner-stop-early', 'runner-stop-twice')


def crashes(args, root, work, engine, name):
    """L1 at every cut of each scenario, then POISON once; raises on the first failure."""
    inputs_path = manifest(root, work, 'crash_' + name, extra=[(root / 'core/programs/store/runner_crash.hirsel', 'runner_crash')])
    env = dict(os.environ)
    env['LD_LIBRARY_PATH'] = ':'.join([str(args.bridge.resolve().parent),
                                       str(root / 'tools/buck2/toolchains/llvm-files/lib'),
                                       env.get('LD_LIBRARY_PATH', '')])
    build = work / f'crash_{name}'
    build.mkdir()
    rows = run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc, inputs_path, build, engine,
                       entry='store@1/runner_crash/main', scenarios=CRASHES, environment=env,
                       adapter='runner_crash.c')
    checked([str(build / (engine + '-crash') / 'store'), POISON, 'count'], env, engine=engine)
    return rows

def drain_deadline(args, root, work, engine, name, runner=None, activation=False):
    inputs_path = manifest(root, work, name, runner,
                           [(root / 'core/programs/store/runner_crash.hirsel', 'runner_crash')])
    if activation and args.activation_legacy_rev:
        add_legacy_activation(root, work, inputs_path, args.activation_legacy_rev)
    build = work / name
    build.mkdir()
    stop_cases = (args.stop_case,) if args.stop_case else STOP_CASES
    return run_crashes(root, args.compiler, args.runtime, args.bridge, args.cc, inputs_path, build, engine,
                       entry='store@1/runner_crash/' + ('main' if activation else 'stop_main'), adapter='runner_crash.c',
                       scenarios=('runner-activation', 'runner-activation-genesis') if activation else (),
                       count_only=('runner-unavailable-genesis', 'runner-unavailable-install') if activation else stop_cases,
                       extra_grants=() if activation else ('Process',))


def add_legacy_activation(root, work, manifest_path, revision):
    """Compile the pre-fence store and runner for the recovered W2 generations."""
    model = json.loads(manifest_path.read_text())
    store = model['packages'][-1]
    store['dependencies'].append({'name': 'legacy', 'version': '1'})
    legacy = {'package': {'name': 'legacy', 'version': '1'},
              'dependencies': [{'name': n, 'version': '1'} for n in ('core', 'std')],
              'sources': []}
    for module in ('store', 'runner', 'ingress'):
        path = work / f'legacy-{module}.hirsel'
        path.write_bytes(subprocess.check_output([
            'git', '-C', str(root), 'show', f'{revision}:core/programs/store/{module}.hirsel'
        ]))
        legacy['sources'].append(dict(file=str(path), modules=[module], signatures=False))
    model['packages'].insert(-1, legacy)
    fixture = next(s for s in store['sources'] if s['modules'] == ['runner_crash'])
    crash = Path(fixture['file']).read_text()
    marker = '        if !activated && !genesis {'
    assert crash.count(marker) == 1
    crash = crash.replace(marker, '        probe_pre_fence(d, c, r, tasks, activated)\n' + marker)
    crash += """
fn old_place(mut _reg: legacy.runner.Registry[(), ()]) -> Result[(), legacy.runner.RegisterError] { Ok(()) }

fn old_migrate(mut _phase: legacy.store.Migration) -> Result[(), legacy.store.Error] {
    Error(Missing("legacy reached app code"))
}

fn old_handles(_schema: legacy.store.Schema) -> Result[(), legacy.store.Error] {
    fail("legacy continued past the migration witness")
}

fn probe_pre_fence(d: Dir, c: Clock, r: Random, tasks: Tasks, fenced: bool) -> () waits {
    let mut opt = legacy.store.defaults()
    opt.checkpoint_task_bytes = 1048576u64
    opt.resident_limit = 1048576u64
    opt.pack_bytes = 512u64
    let attempted = legacy.store.open(tasks, d, c, r, opt)
    match take attempted {
        Error(Unsupported(0x0070)) => want(fenced, "legacy rejected the unfenced generation"),
        Ok(take db) => {
            want(!fenced, "legacy recovered a fenced generation")
            let (_sender, from): (Sender[legacy.runner.Req], Receiver[legacy.runner.Req]) = result.unwrap(channel.new(1u64))
            let mail = result.unwrap(legacy.runner.mailbox(1u64))
            let opened = legacy.runner.place_head(take db, code.describe(old_place), N_PLACE, code.describe(old_migrate),
                code.describe(old_handles), take from, take mail, tasks, 1048576u64)
            match take opened {
                Error(Migrate(Missing(_))) => (),
                _ => fail("legacy did not reach app code on the unfenced generation"),
            }
        },
        _ => fail("legacy open returned the wrong error"),
    }
}
"""
    path = work / 'runner-crash-with-legacy.hirsel'
    path.write_text(crash)
    fixture['file'] = str(path)
    manifest_path.write_text(json.dumps(model))


def no_secret_at_rest(args, root, work, engine, row):
    name = 'no_secret_at_rest'
    source = root / 'core/programs/store'
    fixture = [(source / 'runner_crash.hirsel', 'runner_crash')]
    selection = manifest(root, work, name, extra=fixture)
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
    checked(command, os.environ, engine=engine, case=f'{name}:build')
    nonce = os.urandom(16)
    encoded = base64.b64encode(b'anon:' + nonce).decode('ascii')
    ran = run_case(name, engine, [str(binary), encoded], capture_output=True, text=True)
    files = [p for p in directory.rglob('*') if p.is_file()]
    found = sorted(str(p.relative_to(directory)) for p in files if nonce in p.read_bytes())
    passed = ran.returncode == 0 and bool(files) and not found
    row(engine=engine, law='no_secret_at_rest', passed=passed,
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


# The evaluator records CONC-10 events only for the laws that read them (counted,
# children_joined): a wire flood makes about 7,500 per frame.
EVENT_LAWS = {*(f'store@1/runner_calls/law_{name}' for name in COUNTS), 'store@1/runner/law_children_joined_on_close'}


def evaluate(evaluator, inputs_path, definition, launch_image=None):
    request = dict(manifest=str(inputs_path), entry=definition, model_net=MODEL_NET_RULES,
                   events=definition in EVENT_LAWS)
    if launch_image is not None:
        request['launch_image'] = str(launch_image)
    if definition in WIRE_TEST_INSTRUCTIONS:
        request['instructions'] = WIRE_TEST_INSTRUCTIONS[definition]
    if definition in WIDE_WINDOW_LAWS:
        request['model_window'] = 8192
    result = run_case(f'{definition} [{inputs_path.stem}]', 'evaluator', [str(evaluator)],
                      input=json.dumps(request) + '\n', capture_output=True, text=True)
    try:
        report = json.loads(result.stdout)
    except ValueError:
        report = {'stdout': result.stdout, 'stderr': result.stderr}
    out = report.get('outcome') or {}
    value = out.get('values', [None])[0] if result.returncode == 0 and out.get('kind') == 'returned' else None
    return value, report


def activation_body_edit(root, work):
    """Keep the entry name, signature and schemas, and change only its body."""
    cases = root / 'core/programs/store/runner_calls.hirsel'
    edited = work / cases.name
    source = cases.read_text()
    original = '    match store.put(mut tx, h.t, k, k) {'
    assert source.count(original) == 1
    edited.write_text(source.replace(original, '    match store.put(mut tx, h.t, k, 42u64) {'))
    selection = inputs(root)
    for module in selection['packages'][-1]['sources']:
        if module['modules'] == ['runner_calls']:
            module['file'] = str(edited)
            break
    selected = work / 'activation-body-edit.json'
    selected.write_text(json.dumps(selection))
    return selected


def frame_module(root, path):
    """Construct values beside the shared, hand-derived byte oracle, never from it."""
    vectors = json.loads((root / 'core/programs/store/frame-vectors.json').read_text())['vectors']
    helpers = (root / 'core/programs/store/frame-vector-helpers.hirsel').read_text()
    lines = [helpers, '', 'pub fn law_frame_vectors() -> bool {',
             '    let out = result.unwrap(std.wsock.outbound(Server, runner.wire_defaults(0u64, 0u64).socket, None))']
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


READER_CANCELLATION = 'pool_releases_cancelled_reader'
SESSION_LAWS = ('pool_holds_writer_bytes', READER_CANCELLATION, 'writer_one_frame',
                'children_joined_on_close', 'session_records', 'serving_budget', 'subscription_room_refunded')


def session_laws(args, root, work, engines, row):
    text = (root / 'core/programs/store/runner.hirsel').read_text()
    laws = (root / 'core/programs/store/session-laws.hirsel').read_text()
    adapted = work / 'session' / 'runner.hirsel'
    adapted.parent.mkdir()
    adapted.write_text(text + '\n' + laws)
    selected = manifest(root, work, 'session', adapted)
    names = args.session_law or SESSION_LAWS
    if 'evaluator' in engines:
        for name in names:
            value, report = evaluate(args.evaluator.resolve(), selected, 'store@1/runner/law_' + name)
            holds = value == 1 and (name != 'children_joined_on_close' or children_joined(report))
            row(engine='evaluator', law=name, passed=holds, result=report)
    native_laws = []
    if any(name != READER_CANCELLATION for name in names):
        native_laws.append('wire_session')
    if READER_CANCELLATION in names:
        native_laws.append(READER_CANCELLATION)
    for engine in (e for e in engines if e != 'evaluator'):
        for law in native_laws:
            definition = 'store@1/runner/test_' + law
            code, report = native_run(args, engine, selected, work / f'session-store-{engine}', definition)
            tests = report.get('tests', [])
            row(engine=engine, law=law, passed=code == 0
                and [t['definition'] for t in tests] == [definition]
                and all(t.get('passed') for t in tests), result=report)


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


SETTLEMENT_LAWS = ('after_publication', 'settled_callinfo')
# L28's owner half: a dropped unit's Outcome rows ride in a Reset that never closes 1008,
# and an owner removal reaches the publisher only while the writer holds no content unit.
RELEASE_LAWS = ('dropped_keeps_outcomes', 'removal_waits_for_writer')
# L24's owner half: repairs queued behind a busy writer coalesce, so the newest board arrives
# with no Reset.
REPAIR_LAWS = ('repair_coalesced',)


def admission_laws(args, root, work, engines, row):
    text = (root / 'core/programs/store/runner.hirsel').read_text()
    laws = (root / 'core/programs/store/admission-laws.hirsel').read_text()
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
    state = ('unread_client', 'hello_and_slots', 'join_cleanup', 'refused_send_releases_timer')
    names = args.admission_law or (*state, *durable, 'wire_replacement_generation', *SETTLEMENT_LAWS, *RELEASE_LAWS, *REPAIR_LAWS)
    if 'evaluator' in engines:
        for name in names:
            value, report = evaluate(args.evaluator.resolve(), selected, 'store@1/runner/law_' + name)
            row(engine='evaluator', law=name, passed=value == 1, result=report)
    for engine in (e for e in engines if e != 'evaluator'):
        groups = [('wire_admission', state), ('durable_wire_admission', durable),
                  ('wire_replacement_generation', ('wire_replacement_generation',))]
        groups.extend((name, (name,)) for name in (*SETTLEMENT_LAWS, *RELEASE_LAWS, *REPAIR_LAWS))
        for group, members in groups:
            if not any(name in names for name in members):
                continue
            definition = 'store@1/runner/test_' + group
            code, report = native_run(args, engine, selected, work / f'{group}-store-{engine}', definition)
            tests = report.get('tests', [])
            row(engine=engine, law=group, passed=code == 0
                and [t['definition'] for t in tests] == [definition]
                and all(t.get('passed') for t in tests), result=report)


def socket_laws(args, root, work, engines, row):
    names = {'hello_order', 'slot_out_of_range', 'gone_slot_v5', 'second_hello_closes',
             'session_limit_503', 'principal_limit_1013', 'call_rate_overloaded',
             'request_mismatch_wire', 'resend_same_p', 'held_request_mismatch', 'hello_deadline', 'resume_unknown', 'history_below_latest', 'reply_over_done_max', 'faulted_owner_releases_slot', 'welcome_with_small_budget',
             'cli_call_exact_value', 'cli_call_fingerprint_mismatch'}
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
    run_case(f'{name}:observer', 'cc', [str(cc), '-c', '-fPIC', str(root / 'core/tools/runner_count.c'),
                    '-I' + str(root / 'crates/hirsel-core-rt/include'), f'-DTRY_SENDS={want}',
                    f'-DSOURCE_MANIFEST_DIR={json.dumps(str(work), ensure_ascii=False)}',
                    *([f'-D{PLACEMENT_OBSERVERS[name]}'] if name in PLACEMENT_OBSERVERS else []),
                    *([f'-DHUNG_BODY_SENDS={HUNG_BODY_SENDS}', f'-DHUNG_TOLDS={HUNG_TOLDS}'] if name == 'calls_deadline_hung' else []),
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
    definition = f'store@1/runner_calls/test_{name}'
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
    p.add_argument('--evaluator', type=Path, required=True)
    p.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    p.add_argument('--schedule-runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_SCRIPTED_ARCHIVE'))
    p.add_argument('--observed', type=Path,
                   default=Path(os.environ['HIRSEL_CORE_RT_OBSERVED_ARCHIVES']) / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
                   if 'HIRSEL_CORE_RT_OBSERVED_ARCHIVES' in os.environ else None)
    p.add_argument('--bridge', type=Path, default=os.environ.get('HIRSEL_STORE_CRASH_BRIDGE'))
    p.add_argument('--cc', type=Path)
    p.add_argument('--backend', action='append', choices=['evaluator', 'cranelift', 'llvm'])
    p.add_argument('--activation-legacy-rev')
    p.add_argument('--stop-case', choices=STOP_CASES)
    p.add_argument('--call-law', action='append', choices=CALLS)
    p.add_argument('--report', type=Path)
    p.add_argument('--test-threads', type=int, choices=[1], default=1)
    p.add_argument('--session-law', action='append', choices=SESSION_LAWS)
    p.add_argument('--admission-law', action='append', choices=['wire_replacement_generation', 'unread_client', 'hello_and_slots', 'join_cleanup',
                   'refused_send_releases_timer', 'gone_slot_v5', 'request_mismatch_wire', 'history_below_latest', 'reply_over_done_max', *SETTLEMENT_LAWS, *RELEASE_LAWS, *REPAIR_LAWS])
    p.add_argument('--part', choices=['r0', 'r1', 'wire', 'session', 'ingress', 'admission', 'socket', 'calls', 'deadline', 'activation', 'rc1'])
    args = p.parse_args()
    if args.stop_case and args.part != 'deadline':
        p.error('--stop-case requires --part deadline')
    r0, r1 = args.part in (None, 'r0'), args.part in (None, 'r1', 'calls')
    selected_calls = args.call_law or CALLS
    root = args.root.resolve()
    evaluator = args.evaluator.resolve()
    engines = args.backend or ['evaluator', 'cranelift', 'llvm']
    if engines == ['evaluator'] and NATIVE_ONLY_CALLS.intersection(args.call_law or ()):
        p.error('drain_reopened and lag_window require a native backend')
    if args.part == 'rc1' and (not args.compiler or not args.runtime or engines == ['evaluator']):
        p.error('RC1 requires a native backend, --compiler and --runtime')
    rows = []

    def row(**r):
        rows.append(r)
        print(' '.join(f'{k}={v}' for k, v in r.items() if k != 'result' or r.get('law') == 'l4_peak_retained'), flush=True)

    with tempfile.TemporaryDirectory(prefix='hirsel-runner-laws-') as tmp:
        work = Path(tmp)
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
        if args.part == 'ingress' or (r1 and args.part != 'calls'):
            ingress_laws(args, root, work, engines, row)
        if r0 or args.part == 'session':
            session_laws(args, root, work, engines, row)
        if r0:
            frame_vectors(args, root, work, engines, row)
        if args.part in ('wire', 'socket'):
            socket_laws(args, root, work, engines, row)
        if args.part in (None, 'r1', 'rc1'):
            for engine in (e for e in engines if e != 'evaluator'):
                no_secret_at_rest(args, root, work, engine, row)
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
        launch_pairs = {}
        launch_engine = next((e for e in engines if e != 'evaluator'), 'cranelift')
        if 'evaluator' in engines and r1:
            for name in selected_calls:
                if name in NATIVE_ONLY_CALLS:
                    continue
                definition = 'store@1/runner_calls/law_' + name
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
                value, report = evaluate(evaluator, base, 'store@1/runner/law_' + name)
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
