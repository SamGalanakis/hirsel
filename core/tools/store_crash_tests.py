#!/usr/bin/env python3
"""Run compiled store recovery once at every W2 filesystem mutation boundary.

W2 restart discards volatile directory names as a group. The gap scenario
constructs the v2.5 partial-cleanup state explicitly before enumerating cuts;
this does not enumerate arbitrary persistence subsets of directory operations.
"""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess

SCENARIOS = ('checkpoint', 'gap', 'oversize', 'schema', 'refusal', 'genesis', 'branches', 'migrate')


def checked(command, env=None):
    result = subprocess.run(command, capture_output=True, text=True, env=env)
    if result.returncode:
        raise RuntimeError(f'{shlex.join(map(str, command))}\n{result.stdout}\n{result.stderr}')
    return result


def run_cuts(executable, engine, env=None, scenarios=SCENARIOS):
    rows = []
    for scenario in scenarios:
        result = checked([str(executable), scenario, 'count'], env)
        match = re.fullmatch(r'cuts ([0-9]+)\n', result.stdout)
        if match is None or int(match[1]) == 0:
            raise RuntimeError(f'{scenario}: no mutation count: {result.stdout!r}')
        count = int(match[1])
        for cut in range(count + 1):
            checked([str(executable), scenario, str(cut)], env)
        rows.append(dict(scenario=scenario, mutations=count, cuts=count + 1, passed=True))
        print(f'{engine} crash {scenario}: {count + 1}/{count + 1} cuts passed', flush=True)
    return rows


def run_crashes(root, compiler, runtime, bridge, cc, manifest, work, engine,
                entry="store@1/store/disk_crash_main", scenarios=SCENARIOS, environment=None, adapter="store_crash.c",
                count_only=()):
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
                          ' -Wl,--wrap=hirsel_core_host_rename -Wl,--wrap=hirsel_core_send'
                          ' -Wl,--wrap=hirsel_core_receive -Wl,--wrap=hirsel_core_join\n'))
    wrapper.chmod(0o755)
    executable = build / 'store'
    command = [str(compiler.resolve()), 'build', '--inputs', str(manifest), '--source-core', '--source-std',
               '--entry', entry, '--backend',
               'llvm' if engine == 'llvm-release' else engine, '--runtime', str(runtime.resolve()),
               '--cc', str(wrapper), '--store', str(build / 'objects'), '--limit', str(64 * 1024 * 1024),
               '--dir', str(build), '-o', str(executable), '--json']
    for grant in ('Clock', 'Random', 'Tasks', 'Stderr', 'Stdout'):
        command += ['--grant', grant]
    if adapter == 'runner_crash.c':
        command += ['--grant', 'Net=Listen 127.0.0.0/8 0-65535;Connect 127.0.0.0/8 0-65535']
    if engine == 'llvm-release' or (engine == 'llvm' and adapter == 'runner_crash.c'):
        command.append('--release')
    result = checked(command, environment)
    report = json.loads(result.stdout)
    if not report.get('accepted') or not report.get('native', {}).get('linked'):
        raise RuntimeError(result.stdout)
    env = dict(os.environ if environment is None else environment)
    env['LD_LIBRARY_PATH'] = str(bridge.resolve().parent) + ':' + env.get('LD_LIBRARY_PATH', '')
    rows = run_cuts(executable, engine, env, scenarios)
    for scenario in count_only:
        checked([str(executable), scenario, 'count'], env)
        rows.append(dict(scenario=scenario, passed=True))
    return rows
