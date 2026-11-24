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

SCENARIOS = ('checkpoint', 'gap', 'oversize', 'schema', 'refusal', 'genesis', 'branches', 'migrate', 'install', 'read-only', 'window')
WINDOW_FAILURES = ('window-chunk-failure', 'window-sync-failure')
JOURNAL_SCENARIOS = ('journal', 'journal-group', 'journal-rotate', 'journal-place-split',
                     'journal-place-idle', 'journal-fork')
SMOKE_KINDS = ('append', 'overwrite', 'create', 'rename', 'unlink')
NIGHTLY_SEEDS = 4


def checked(command, env=None, *, engine, case=None):
    result = run_case(case or ' '.join(command[1:]), engine, command, capture_output=True, text=True, env=env)
    if result.returncode:
        raise RuntimeError(f'{shlex.join(map(str, command))} exited {result.returncode}\n{result.stdout}\n{result.stderr}')
    return result


def run_cuts(executable, engine, env=None, scenarios=SCENARIOS):
    rows = []
    for scenario in scenarios:
        count, _ = count_cuts(executable, scenario, engine, env)
        for cut in range(count + 1):
            checked([str(executable), scenario, str(cut)], env, engine=engine)
        rows.append(dict(scenario=scenario, mutations=count, cuts=count + 1, passed=True))
        print(f'{engine} crash {scenario}: {count + 1}/{count + 1} cuts passed', flush=True)
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
                count_only=()):
    executable = build_crash(root, compiler, runtime, bridge, cc, manifest, work, engine, entry, environment, adapter)
    env = bridge_env(bridge, environment)
    rows = run_cuts(executable, engine, env, scenarios)
    for scenario in count_only:
        checked([str(executable), scenario, 'count'], env, engine=engine)
        rows.append(dict(scenario=scenario, passed=True))
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


def smoke(executable, env):
    scenario = 'journal-rotate'
    count, trace = count_cuts(executable, scenario, 'cranelift', dict(env, HIRSEL_W2_TRACE_CUTS='1'))
    cuts = {}
    for cut, kind in re.findall(r'^w2-cut ([0-9]+) ([a-z-]+)$', trace, re.M):
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
