#!/usr/bin/env python3
"""Prepare the native laws for one sim.world seed campaign."""
import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

import runner_tests
import std_native


ROOT = std_native.ROOT
SEED = 'HIRSEL_TEST_SIM_SEED'
DEFAULT_SEEDS = 32
PACKAGE_LAWS = ('store',)
SEEDED_TEST = re.compile(r'^@test\s+fn (\w+)\(([^)]*)\)', re.M)
# These laws drive 10,000 frames per seed. Keep the complete floods in each
# nightly batch while giving their disjoint ranges fewer seeds.
FLOOD_SEEDS = {
    'runner@1/runner_calls/law_calls_refusals': 2,
    'runner@1/runner_calls/law_calls_session_charge': 2,
}
POSTGRES = (
    'fragmented_startup_error', 'eof_in_error_response',
    'startup_ready_deadline', 'fatal_error_during_sync', 'comma_host_refused',
)
DIRECT = {
    'exec:output_drains_past_limits',
    'exec:run_until_escalates',
    'exec:run_until_exits_early',
    'prop:prop_tasks_script_shrink_replay',
    'provide_parity:sim_rename_durability',
    'provide_parity:sim_script_monotone_pick',
    'provide_parity:sim_script_provider_isolation',
    'provide_parity:sim_withdrawn_read_keeps_byte',
    'provide_parity:sim_cancelled_read_keeps_byte',
    'provide_parity:sim_captured_stop',
}


def seeded_package_laws(package):
    """A package test opts in with its simulation-specific Env parameter."""
    directory = ROOT / 'core/programs' / package
    manifest = tomllib.loads((directory / 'hirsel.toml').read_text())
    version = manifest['package']['major']
    laws = {}
    for file, module in manifest['tests']['sources'].items():
        for name, parameters in SEEDED_TEST.findall((directory / file).read_text()):
            if re.search(r'(^|,)\s*seed_grant\s*:\s*Env\s*(,|$)', parameters):
                definition = f'{package}@{version}/{module.replace(".", "/")}/{name}'
                laws[definition] = definition
    if not laws:
        raise ValueError(f'{package} has no seeded package tests')
    return laws


def selected_laws():
    laws = {'postgres': {f'postgres@1/sim_tests/{name}': name for name in POSTGRES}}
    for case in std_native.discover(ROOT, []):
        if case.name in DIRECT or std_native.simulated_network_case(case):
            module = case.test_of or f'tests/{case.file.stem}'
            name = case.call.split('(')[0]
            key = f'{case.package}@1/{module}/{name}'
            laws.setdefault('contracts', {})[key] = case
    if DIRECT - {case.name for case in laws['contracts'].values()}:
        raise ValueError('a direct simulation law is missing from the contract inventory')

    source = (ROOT / 'core/programs/runner/runner_calls.hirsel').read_text()
    runner = {}
    for name in runner_tests.CALLS:
        # Persistent and observed laws require the existing launch/trace adapters.
        if name in runner_tests.NATIVE_ONLY_CALLS | runner_tests.PERSISTENT_PLACE_CALLS or name in runner_tests.COUNTS:
            continue
        signature = re.search(rf'^pub fn law_{name}\(([^)]*)\) -> bool waits \{{', source, re.M)
        if signature is None:
            continue
        types = [pair.split(':')[1].strip() for pair in signature[1].split(',') if pair.strip()]
        if 'Net' in types:
            runner[f'runner@1/runner_calls/law_{name}'] = types
    laws['runner'] = runner
    laws.update((package, seeded_package_laws(package)) for package in PACKAGE_LAWS)
    return laws


def adapter(test, module, call, expected, waits, setup=''):
    effect = ' waits' if waits else ''
    finish = '\n    result.unwrap(task.join(take server))\n    result.unwrap(task.join(take driver))' if setup else ''
    return f"""
@test
fn {test}(tasks: Tasks, seed_grant: Env) -> Result[(), ()]{effect} {{
    let seed = option.unwrap({module}.world.test_seed(seed_grant))
    {setup}
    let verdict = {call}{finish}
    if verdict == {expected} {{ Ok(()) }} else {{ Error(()) }}
}}
"""


def network_adapter(test, module, call, expected, window):
    setup = f'let (network, time, server, driver) = start_seeded_simulation(tasks, {window}, seed)'
    return adapter(test, module, call, expected, True, setup)


def contract_inputs(group, work):
    core, std = {'name': 'core', 'version': '1'}, {'name': 'std', 'version': '1'}
    packages = [
        {'package': core, 'sources': std_native.library_sources(ROOT / 'core/core', True), 'dependencies': []},
        {'package': std, 'sources': std_native.library_sources(ROOT / 'core/std', False), 'dependencies': [core]},
    ]
    groups = defaultdict(list)
    for law, case in group.items():
        groups[case.file].append((law, case))
    entries = {}
    for index, (path, cases) in enumerate(groups.items()):
        source = path.read_text()
        first = cases[0][1]
        modules = [first.test_of] if first.test_of else ['tests', path.stem]
        in_core = first.package == 'core'
        package = packages[0 if in_core else 1]
        if first.test_of:
            original = next(row for row in package['sources'] if row['modules'] == modules)
            source = Path(original['file']).read_text() + '\n' + source
            package['sources'].remove(original)
        bridge = ''
        if any(std_native.simulated_network_case(case) for _, case in cases):
            if in_core:
                bridge = std_native.simulation_support('sim')
            else:
                source += std_native.simulation_support('sim')
        for law, case in cases:
            if case.stdout or case.stderr or case.schedule or case.host_op or case.run_env:
                raise ValueError(f'{law} needs an unsupported campaign fixture')
            name = case.call.split('(')[0]
            test = f'campaign_{name}'
            if in_core:
                module = '.'.join(modules)
                source += f"""
pub fn campaign_call_{name}(network: Net, time: Clock, tasks: Tasks) -> bool waits {{
    {name}(network, time, tasks)
}}
"""
                bridge += network_adapter(test, 'sim', f'core.{module}.campaign_call_{name}(network, time, tasks)',
                                          case.expected, 8)
                entries[law] = f'std@1/tests/campaign_core_{index}/{test}'
            else:
                if std_native.simulated_network_case(case):
                    source += network_adapter(test, 'sim', f'{name}(network, time, tasks)', case.expected, 8)
                else:
                    parameters = 'tasks, seed' if case.call.endswith('(tasks)') else 'seed'
                    source += adapter(test, 'sim', f'{name}_seeded({parameters})', case.expected, case.waits)
                entries[law] = f'std@1/{"/".join(modules)}/{test}'
        file = work / f'contract-{index}.hirsel'
        file.write_text(source)
        package['sources'].append({'file': str(file), 'modules': modules, 'signatures': in_core and bool(first.test_of)})
        if bridge:
            file = work / f'bridge-{index}.hirsel'
            file.write_text(bridge)
            packages[1]['sources'].append({'file': str(file), 'modules': ['tests', f'campaign_core_{index}'],
                                           'signatures': False})
    return {'core': core, 'packages': packages}, entries


def runner_inputs(group, work):
    inputs = runner_tests.runner_inputs(ROOT)
    sources = inputs['packages'][-1]['sources']
    row = next(row for row in sources if row['modules'] == ['runner_calls'])
    source = Path(row['file']).read_text() + std_native.simulation_support('std.sim')
    entries = {}
    arguments = {'Net': 'network', 'Clock': 'time', 'Tasks': 'tasks', 'Random': 'provide.random(seed)'}
    for law, types in group.items():
        name = law.rsplit('/', 1)[-1]
        call = f'{name}({", ".join(arguments[ty] for ty in types)})'
        test = f'campaign_{name}'
        window = 8192 if law in runner_tests.WIDE_WINDOW_LAWS else 8
        source += network_adapter(test, 'std.sim', call, 'true', window)
        entries[law] = f'runner@1/runner_calls/{test}'
    path = work / 'runner_calls.hirsel'
    path.write_text(source)
    row['file'] = str(path)
    return inputs, entries


def output_name(definition):
    # hirsel test names outputs with ContentHash::of_bytes of the definition.
    parts = (b'hirsel.text.v1', definition.encode())
    return hashlib.sha256(b''.join(len(part).to_bytes(8, 'little') + part for part in parts)).hexdigest()


def run_preparation(command, scratch, report, expected):
    with report.open('w') as log:
        result = subprocess.run(command, stdout=log, cwd=scratch)
    if result.returncode not in (0, 1):
        raise ValueError(f'preparation exited {result.returncode}; see {report}')
    rows = {row['definition']: row for row in json.loads(report.read_text())['tests']}
    if rows.keys() != set(expected):
        raise ValueError(f'preparation did not discover exactly the campaign entries; see {report}')
    return rows


def prepare(compiler, work, kind, group, jobs, seeds):
    scratch = work / kind
    scratch.mkdir()
    output = scratch / 'bin'
    command = [str(compiler), 'test', '--release', '--backend', 'llvm', '-j', str(jobs), '--seed', '0',
               '--store', str(scratch / 'objects'), '-o', str(output), '--json']
    reports = {}
    rows = {}
    if kind in PACKAGE_LAWS:
        # Package mode writes a workspace lock. Stage the manifests and their
        # relative dependencies away from the checkout.
        programs = work / 'programs'
        if not programs.exists():
            shutil.copytree(ROOT / 'core/programs', programs,
                            ignore=shutil.ignore_patterns('hirsel.lock', 'node_modules'))
        entries = group
        print(f'Preparing {len(entries)} {kind} laws', flush=True)
        for index, definition in enumerate(entries.values()):
            report = scratch / f'prepare-{index}.json'
            reports[definition] = report
            rows.update(run_preparation(command + [str(programs / kind), '--filter', definition],
                                        scratch, report, [definition]))
    elif kind == 'postgres':
        package = scratch / 'postgres'
        shutil.copytree(ROOT / 'core/programs/postgres', package, ignore=shutil.ignore_patterns('hirsel.lock'))
        entries = {law: law for law in group}
        selector = '/sim_tests/' if len(group) == len(POSTGRES) else next(iter(group))
        command.extend([str(package), '--filter', selector])
    else:
        inputs, entries = (contract_inputs if kind == 'contracts' else runner_inputs)(group, scratch)
        manifest = scratch / 'inputs.json'
        manifest.write_text(json.dumps(inputs))
        limits = {case.root_limit for case in group.values()} if kind == 'contracts' else {64 << 20}
        limit, = limits
        command.extend(['--inputs', str(manifest), '--source-core', '--source-std',
                        '--filter', '/campaign_', '--limit', str(limit),
                        '--grant', 'Tasks', '--grant', f'Env={SEED}'])
    if kind not in PACKAGE_LAWS:
        print(f'Preparing {len(entries)} {kind} laws', flush=True)
        report = scratch / 'prepare.json'
        reports = dict.fromkeys(entries.values(), report)
        rows = run_preparation(command, scratch, report, entries.values())
    laws = []
    for law, definition in entries.items():
        report = reports[definition]
        binary = output / output_name(definition)
        if rows[definition].get('source') != 'executed' or not binary.is_file():
            raise ValueError(f'{law} produced no executable; see {report}')
        if not rows[definition]['passed']:
            print(f'{law}: preparation seed 0 failed; see {report}', flush=True)
        count = seeds if seeds is not None else FLOOD_SEEDS.get(law, DEFAULT_SEEDS)
        laws.append({'law': law, 'executable': str(binary), 'seeds': count})
    shutil.rmtree(scratch / 'objects')
    return laws


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--work', type=Path)
    parser.add_argument('--jobs', type=int, default=4, choices=range(1, 5))
    parser.add_argument('--seeds', type=int, help='override the default seed count for every law')
    parser.add_argument('--law', help='prepare or list just one exact law')
    parser.add_argument('--package', action='append', choices=PACKAGE_LAWS,
                        help='prepare or list only this package (repeatable)')
    parser.add_argument('--list', action='store_true')
    args = parser.parse_args()
    inventory = selected_laws()
    if args.package:
        inventory = {kind: laws for kind, laws in inventory.items() if kind in args.package}
    if args.law and not any(args.law in laws for laws in inventory.values()):
        parser.error('unknown simulation law')
    if args.list:
        print('\n'.join(name for laws in inventory.values() for name in laws if not args.law or name == args.law))
        return
    if not args.compiler or not args.work or (args.seeds is not None and args.seeds <= 0):
        parser.error('preparation needs --compiler and --work, with a positive --seeds override')
    work = args.work.resolve()
    work.mkdir()
    laws = []
    for kind, group in inventory.items():
        group = {name: case for name, case in group.items() if not args.law or name == args.law}
        if group:
            laws.extend(prepare(args.compiler.resolve(), work, kind, group, args.jobs, args.seeds))
    (work / 'manifest.json').write_text(json.dumps(laws, indent=2) + '\n')


if __name__ == '__main__':
    main()
