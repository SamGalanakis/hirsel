#!/usr/bin/env python3
"""Build and run the compiled store's clean and damaged W2 crash cuts."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
from case_progress import run_case

SCENARIOS = ('checkpoint', 'gap', 'oversize', 'schema', 'refusal', 'genesis', 'branches', 'migrate', 'install', 'read-only', 'window', 'index-build', 'catch-up')
WINDOW_FAILURES = ('window-chunk-failure', 'window-sync-failure')
JOURNAL_SCENARIOS = ('journal', 'journal-group', 'journal-rotate', 'journal-place-split',
                     'journal-place-idle', 'journal-fork', 'journal-tail')
# STORE-SPEC §3.3b: a Direct extension (creation block or window step) and a
# Direct overwrite of the zeroed window keep any subset of their sectors; a
# Durable END rewrite's one sector straddles EOF and keeps old or new bytes.
SMOKE_KINDS = ('extend', 'window', 'straddle', 'create', 'rename', 'unlink')
SECTOR_KINDS = {'extend': 'Partial', 'window': 'Partial', 'straddle': 'Old'}
# Seeds 0-4 select each Direct sector mask: none, all, first, last, alternate.
SECTOR_SEEDS = range(5)
NIGHTLY_SEEDS = 4


def checked(command, env=None, *, engine, case=None):
    result = run_case(case or ' '.join(command[1:]), engine, command, capture_output=True, text=True, env=env)
    if result.returncode:
        raise RuntimeError(f'{shlex.join(map(str, command))} exited {result.returncode}\n{result.stdout}\n{result.stderr}')
    return result


def run_cuts(executable, engine, env=None, scenarios=SCENARIOS, receipts=False):
    """Every cut 0..N of each scenario recovers. With receipts (CLOUD #3), the
    cut at k must name the same step k + 1, logical role and completed-prefix
    hash as the uncut run's trace, k = N must name none, and k = N + 1 fails."""
    rows = []
    for scenario in scenarios:
        count, trace = count_cuts(executable, scenario, engine,
                                  dict(env or os.environ, HIRSEL_W2_TRACE_CUTS='1') if receipts else env)
        steps = dict(re.findall(r'^w2-cut ([0-9]+) (.+)$', trace, re.M))
        if receipts and sorted(map(int, steps)) != list(range(1, count + 1)):
            raise RuntimeError(f'{scenario}: trace names {len(steps)} of {count} steps')
        for cut in range(count + 1):
            result = checked([str(executable), scenario, str(cut)], env, engine=engine)
            if not receipts:
                continue
            got = re.findall(r'^w2-receipt ([0-9]+) (.+)$', result.stderr, re.M)
            want = [(str(cut + 1), steps[str(cut + 1)])] if cut < count else []
            if got != want:
                raise RuntimeError(f'{scenario} cut {cut}: receipt {got}, uncut trace {want}')
        if receipts:
            beyond = [str(executable), scenario, str(count + 1)]
            ran = run_case(' '.join(beyond[1:]), engine, beyond, capture_output=True, text=True, env=env)
            if ran.returncode != 120 or f'W2 cut {count + 1} was not reached' not in ran.stderr:
                raise RuntimeError(f'{scenario} cut {count + 1} past the end passed: {ran.returncode}\n{ran.stderr}')
            rows += eio_cases(executable, scenario, trace, engine, env)
        rows.append(dict(scenario=scenario, mutations=count, cuts=count + 1, passed=True))
        print(f'{engine} crash {scenario}: {count + 1}/{count + 1} cuts passed', flush=True)
    return rows


EIO_SCENARIOS = ('checkpoint',)


def eio_cases(executable, scenario, trace, engine, env):
    """Fail each Directory call in the write phase, from open through close.
    Init supplies a durable fixture and read checks it after a power loss.
    Also verify V6 at the recovery open of the already-published MANIFEST."""
    if scenario not in EIO_SCENARIOS:
        return []
    calls = re.findall(r'^w2-call (\S+) ([0-9]+) (\S+) (\S+)$', trace, re.M)
    manifest = next((call for call in calls if call[0] == 'read' and call[2:] == ('open', 'MANIFEST')), None)
    if manifest is None:
        raise RuntimeError(f'{scenario}: the read phase did not open MANIFEST')
    write = [call for call in calls if call[0] == 'write']
    if [int(call[1]) for call in write] != list(range(1, len(write) + 1)):
        raise RuntimeError(f'{scenario}: gapped or duplicate write ordinals: {write}')
    renamed = next((i for i, call in enumerate(write) if call[2:] == ('rename', 'MANIFEST.tmp>MANIFEST')), None)
    removal = next((call for call in write[renamed:] if call[2] == 'remove'), None) if renamed is not None else None
    if removal is None:
        raise RuntimeError(f'{scenario}: no MANIFEST rename and removal after it in {write}')
    cases = [':'.join(call) for call in write] + [':'.join(('reopen', *manifest[1:]))]
    failures = []
    for case in cases:
        try:
            checked([str(executable), scenario, 'eio:' + case], env, engine=engine)
        except RuntimeError as error:
            failures.append(str(error))
    if failures:
        raise RuntimeError(f'{scenario}: {len(failures)}/{len(cases)} EIO cases failed\n' + '\n'.join(failures))
    beyond = f'write:{len(write) + 1}:open:MANIFEST'
    never = [str(executable), scenario, 'eio:' + beyond]
    ran = run_case(' '.join(never[1:]), engine, never, capture_output=True, text=True, env=env)
    if ran.returncode != 120 or f'injected EIO {beyond} was not reached' not in ran.stderr:
        raise RuntimeError(f'{scenario}: an unreached EIO passed: {ran.returncode}\n{ran.stderr}')
    print(f'{engine} crash {scenario}: {len(cases)}/{len(cases)} EIO cases passed', flush=True)
    return [dict(scenario=scenario, eio=case, passed=True) for case in cases]


LATE_SCENARIOS = ('late-group', 'late-group-failed', 'late-follow', 'late-follow-failed', 'late-cut', 'late-genesis')
NO_CUT = 2**64 - 1


def late_writes(executable, engine, env):
    """BL12/BL13, BL14 and G1: the dead writer's held group write (a creation
    block for late-cut and late-genesis) becomes readable at
    r and complete (or failed) at c, over the P schedule points of the new
    owner's open and first commit, or of a follower's capture. Sweep r with c just after it or after the run, and
    a power cut at r (before the bytes are readable) and every later point."""
    rows = []
    for scenario in LATE_SCENARIOS:
        result = checked([str(executable), scenario, 'count'], env, engine=engine)
        match = re.fullmatch(r'points ([0-9]+)\n', result.stdout)
        if match is None or int(match[1]) < 2:
            raise RuntimeError(f'{scenario}: no schedule points: {result.stdout!r}')
        points = int(match[1])
        schedules = 0
        for readable in range(1, points + 1):
            for complete in sorted({readable + 1, points + 1}):
                for power_cut in (*range(readable, points + 2), NO_CUT):
                    checked([str(executable), scenario, f'{readable}:{complete}:{power_cut}'], env, engine=engine)
                    schedules += 1
        rows.append(dict(scenario=scenario, points=points, schedules=schedules, passed=True))
        print(f'{engine} {scenario}: {schedules} schedules over {points} points passed', flush=True)
    return rows


def count_cuts(executable, scenario, engine, env):
    result = checked([str(executable), scenario, 'count'], env, engine=engine)
    match = re.fullmatch(r'cuts ([0-9]+)\n', result.stdout)
    if match is None or int(match[1]) == 0:
        raise RuntimeError(f'{scenario}: no mutation count: {result.stdout!r}')
    return int(match[1]), result.stderr


def entry_grants(manifest, entry, directory):
    """Read the selected crash entry's capability parameters from its input source."""
    package_path, *modules, function = entry.split('/')
    name, version = package_path.rsplit('@', 1)
    packages = json.loads(manifest.read_text())['packages']
    declaration = re.compile(rf'(?m)^\s*pub fn {re.escape(function)}\s*\(([^)]*)\)')
    texts = ((manifest.parent / source['file']).read_text() for package in packages
             if package['package'] == {'name': name, 'version': version}
             for source in package['sources'] if source['modules'] == modules)
    signature = next(filter(None, map(declaration.search, texts)), None)
    if signature is None:
        raise ValueError(f'crash entry declaration missing: {entry}')
    grants = []
    for parameter in signature[1].split(','):
        if not parameter.strip():
            continue
        _, capability = parameter.split(':', 1)
        capability = capability.strip()
        if capability == 'Dir':
            grants.extend(['--dir', str(directory)])
        elif capability == 'Net':
            grants.extend(['--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535'])
        elif capability != 'Args':
            grants.extend(['--grant', capability])
    return grants


def build_crash(root, compiler, runtime, bridge, cc, manifest, work, engine, entry, environment, adapter):
    build = work / (engine + '-crash')
    build.mkdir()
    if cc is None:
        prefix = Path(os.environ.get('LLVM_SYS_231_PREFIX', root / 'tools/buck2/toolchains/llvm-files'))
        cc = prefix / 'bin/hirsel-clang'
    wrapper = build / 'cc'
    # Name files directly: a -L/rpath directory becomes an unsupported directory
    # input to the native link recipe.
    wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(str(cc.resolve())) + ' "$@" '
                       + shlex.quote(str(root / 'core/tools' / adapter)) + ' '
                       + shlex.quote(str(bridge.resolve())) + ' -I'
                       + shlex.quote(str(root / 'crates/hirsel-core-rt/include'))
                       + ' -Wl,--wrap=hirsel_core_os_run_with_sources'
                       + ('\n' if adapter == 'runner_crash.c' else
                          ' -Wl,--wrap=hirsel_core_spawn -Wl,--wrap=hirsel_core_host_op'
                          ' -Wl,--wrap=hirsel_core_host_at -Wl,--wrap=hirsel_core_host_stat'
                          ' -Wl,--wrap=hirsel_core_host_rename -Wl,--wrap=hirsel_core_send'
                          ' -Wl,--wrap=hirsel_core_receive -Wl,--wrap=hirsel_core_join\n'))
    wrapper.chmod(0o755)
    executable = build / 'store'
    command = [str(compiler.resolve()), 'build', '--inputs', str(manifest), '--source-core', '--source-std',
               '--entry', entry, '--backend',
               'llvm' if engine == 'llvm-release' else engine, '--runtime', str(runtime.resolve()),
               '--cc', str(wrapper), '--store', str(build / 'objects'), '--limit', str(64 * 1024 * 1024),
               *entry_grants(manifest, entry, build), '-o', str(executable), '--json']
    if engine == 'llvm-release' or (engine == 'llvm' and adapter == 'runner_crash.c'):
        command.append('--release')
    result = checked(command, environment, engine=engine, case=f'{entry}:build')
    report = json.loads(result.stdout)
    if not report.get('accepted') or not report.get('native', {}).get('linked'):
        raise RuntimeError(result.stdout)
    return executable


def bridge_env(bridge, environment=None):
    env = dict(os.environ if environment is None else environment)
    env['LD_LIBRARY_PATH'] = str(bridge.resolve().parent) + ':' + env.get('LD_LIBRARY_PATH', '')
    return env


def run_crashes(root, compiler, runtime, bridge, cc, manifest, work, engine,
                entry="store@1/store/disk_crash_main", scenarios=SCENARIOS, environment=None, adapter="store_crash.c",
                count_only=(), sectors=(), receipts=False, eio_only=False):
    executable = build_crash(root, compiler, runtime, bridge, cc, manifest, work, engine, entry, environment, adapter)
    env = bridge_env(bridge, environment)
    if eio_only:
        rows = []
        for scenario in EIO_SCENARIOS:
            _, trace = count_cuts(executable, scenario, engine, dict(env, HIRSEL_W2_TRACE_CUTS='1'))
            rows += eio_cases(executable, scenario, trace, engine, env)
        return rows
    rows = run_cuts(executable, engine, env, scenarios, receipts)
    for scenario in count_only:
        checked([str(executable), scenario, 'count'], env, engine=engine)
        rows.append(dict(scenario=scenario, passed=True))
    if sectors:
        if sector_images(executable, sectors, engine, env):
            raise RuntimeError(f'{engine}: sector images broke the commit law or were vacuous')
        rows.append(dict(scenario='sector-images', passed=True))
    return rows


def run_damage(executable, scenario, cuts, seeds, env):
    failed = 0
    for seed in seeds:
        for cut in cuts:
            command = [str(executable), scenario, str(cut), '0', str(seed)]
            try:
                ran = subprocess.run(command, env=env, capture_output=True, text=True, timeout=300)
            except subprocess.TimeoutExpired:
                print(f'{shlex.join(command)} timed out', flush=True)
                failed += 1
                continue
            if ran.returncode:
                print(f'{shlex.join(command)} exited {ran.returncode}\n{ran.stdout}{ran.stderr}', flush=True)
                failed += 1
    total = len(cuts) * len(seeds)
    print(f'{scenario}: {total - failed}/{total} damaged cuts passed, seeds {seeds[0]}–{seeds[-1]}', flush=True)
    return failed


def sector_images(executable, scenarios, engine, env):
    """Damage every Direct and Durable sector cut of the scenarios under the
    commit law. Across them, each kind must leave an image that is neither its
    clean cut nor its completed write (for the one-sector END, the old bytes)."""
    failed = 0
    seen = {kind: set() for kind in SECTOR_KINDS}
    for scenario in scenarios:
        _, trace = count_cuts(executable, scenario, engine, dict(env, HIRSEL_W2_TRACE_CUTS='1'))
        cuts = [(int(cut), kind) for cut, kind in re.findall(r'^w2-cut ([0-9]+) ([a-z-]+) ', trace, re.M)
                if kind in SECTOR_KINDS]
        for seed in SECTOR_SEEDS:
            for cut, kind in cuts:
                command = [str(executable), scenario, str(cut), '0', str(seed)]
                ran = subprocess.run(command, env=env, capture_output=True, text=True, timeout=300)
                if ran.returncode:
                    print(f'{shlex.join(command)} exited {ran.returncode}\n{ran.stdout}{ran.stderr}', flush=True)
                    failed += 1
                seen[kind].update(re.findall(r'^w2-image ([A-Za-z]+)$', ran.stderr, re.M))
        print(f'{engine} {scenario}: {len(cuts) * len(SECTOR_SEEDS)} sector cuts', flush=True)
    for kind, wanted in SECTOR_KINDS.items():
        if wanted not in seen[kind]:
            print(f'{engine} {kind}: no {wanted} image in {sorted(seen[kind])}', flush=True)
            failed += 1
    return failed


# test_provider.rs SECTOR_SUBSET: the seed's low bits name the candidate
# sectors that land.
SECTOR_SUBSET = 1 << 62
TAIL_MAX_SECTORS = 12


def tail_subsets(executable, scenario, engine, env):
    """BL2 case (a): every subset of a group write's sectors over a tail block
    recovers under the commit law. A tail block holds the HEADER (a segment's
    first group, at 72) or acked frames, so a lost sector must not reach them."""
    _, trace = count_cuts(executable, scenario, engine, dict(env, HIRSEL_W2_TRACE_CUTS='1'))
    writes = [(int(cut), int(first)) for cut, _, first in
              re.findall(r'^w2-direct ([0-9]+) (\S+) ([0-9]+)$', trace, re.M) if int(first) % 4096]
    failed = 0
    wide = {'fresh': 0, 'acked': 0}
    runs = 0
    for cut, first in writes:
        probe = subprocess.run([str(executable), scenario, str(cut), '0', '0'], env=env, capture_output=True,
                               text=True, timeout=300)
        counts = re.findall(r'^w2-candidates ([0-9]+)$', probe.stderr, re.M)
        if probe.returncode or len(counts) != 1 or not 1 <= int(counts[0]) <= TAIL_MAX_SECTORS:
            print(f'{scenario} cut {cut}: candidate sectors {counts} (exit {probe.returncode})\n{probe.stderr}',
                  flush=True)
            failed += 1
            continue
        sectors = int(counts[0])
        if sectors >= 3:
            wide['fresh' if first % 4096 == 72 else 'acked'] += 1
        for mask in range(1 << sectors):
            command = [str(executable), scenario, str(cut), '0', str(SECTOR_SUBSET | mask)]
            ran = subprocess.run(command, env=env, capture_output=True, text=True, timeout=300)
            runs += 1
            if ran.returncode:
                print(f'{shlex.join(command)} exited {ran.returncode}\n{ran.stdout}{ran.stderr}', flush=True)
                failed += 1
    print(f'{engine} {scenario}: {runs} tail-block subsets over {len(writes)} group writes, wide {wide}',
          flush=True)
    if not all(wide.values()):
        print(f'{engine} {scenario}: no wide group over a fresh and an acked tail block {wide}', flush=True)
        failed += 1
    return failed


def smoke(executable, env):
    scenario = 'journal-rotate'
    count, trace = count_cuts(executable, scenario, 'cranelift', dict(env, HIRSEL_W2_TRACE_CUTS='1'))
    cuts = {}
    for cut, kind in re.findall(r'^w2-cut ([0-9]+) ([a-z-]+) ', trace, re.M):
        cuts.setdefault(kind, int(cut))
    selected = [cuts.get(kind) for kind in SMOKE_KINDS]
    if any(cut is None or not 1 <= cut <= count for cut in selected):
        raise RuntimeError(f'{scenario}: missing targeted damage cuts {dict(zip(SMOKE_KINDS, selected))}')
    print(f'{scenario} damage smoke: {dict(zip(SMOKE_KINDS, selected))}', flush=True)
    return run_damage(executable, scenario, selected, (0,), env)


def compile_damage(args, work, kind, engine):
    from store_tests import crash_inputs

    root = Path(__file__).resolve().parents[2]
    work.mkdir()
    manifest = work / 'inputs.json'
    manifest.write_text(json.dumps(crash_inputs(root)))
    return build_crash(root, args.compiler, args.runtime, args.bridge, None, manifest, work, engine,
                       f'store@1/store/{kind}_crash_main', None, 'store_crash.c')


def prepare_sweep(args):
    bundle = args.bundle.resolve()
    bundle.mkdir()
    with tempfile.TemporaryDirectory(prefix='w2-build-') as scratch:
        for kind in ('disk', 'journal'):
            executable = compile_damage(args, Path(scratch) / kind, kind, 'llvm-release')
            shutil.copy2(executable, bundle / kind)
    shutil.copy2(args.bridge.resolve(), bundle / args.bridge.resolve().name)
    for name in ('store_crash_tests.py', 'case_progress.py'):
        shutil.copy2(Path(__file__).with_name(name), bundle / name)


def sweep(bundle, first_seed):
    env = bridge_env(bundle / 'journal')
    seeds = range(first_seed, first_seed + NIGHTLY_SEEDS)
    failed = 0
    for kind, scenarios in (('disk', SCENARIOS), ('journal', JOURNAL_SCENARIOS)):
        executable = bundle / kind
        for scenario in scenarios:
            try:
                count, _ = count_cuts(executable, scenario, 'llvm-release', env)
            except RuntimeError as error:
                print(error, flush=True)
                failed += 1
                continue
            failed += run_damage(executable, scenario, range(count + 1), seeds, env)
    return failed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for command in ('smoke', 'prepare'):
        build = commands.add_parser(command)
        build.add_argument('--compiler', type=Path, required=True)
        build.add_argument('--runtime', type=Path, required=True)
        build.add_argument('--bridge', type=Path, required=True)
        if command == 'prepare':
            build.add_argument('--bundle', type=Path, required=True)
    campaign = commands.add_parser('sweep')
    campaign.add_argument('first_seed', type=int)
    args = parser.parse_args()
    if args.command == 'prepare':
        prepare_sweep(args)
        return 0
    if args.command == 'smoke':
        with tempfile.TemporaryDirectory(prefix='w2-smoke-') as scratch:
            executable = compile_damage(args, Path(scratch) / 'journal', 'journal', 'cranelift')
            return int(smoke(executable, bridge_env(args.bridge)) != 0)
    if not 0 <= args.first_seed <= 2**64 - NIGHTLY_SEEDS:
        parser.error('the seed range must fit in u64')
    return int(sweep(Path(__file__).resolve().parent, args.first_seed) != 0)


if __name__ == '__main__':
    raise SystemExit(main())
