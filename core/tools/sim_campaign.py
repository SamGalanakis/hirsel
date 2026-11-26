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
GENERATED_KINDS = ('postgres', 'contracts', 'runner-adapters')
SEEDED_TEST = re.compile(r'^@test\s+fn (\w+)\(([^)]*)\)', re.M)
# Keep the 10,000-frame floods intact.
FLOOD_SEEDS = {
    'runner@1/runner_calls/law_calls_refusals': 2,
    'runner@1/runner_calls/law_calls_session_charge': 2,
}
TRACE_FLOORS = {'store@1/store/bucket_test_m2_register_fence': 2}
REACH_WITNESSES = {
    'store@1/store/test_model': ['64 or more commands'],
    # One label per L69 op, counted where the op takes effect (a guarded draw can do nothing).
    'store@1/store/test_l69_random_arm': [
        'edit', 'delete a key', 'fork', 'pin a head', 'capture history', 'pin history', 'unpin',
        'delete a branch', 'cut', 'crash before the rename', 'crash after the rename',
    ],
    # Each L63 schedule move, counted where it takes effect; seeds 0-23 reach all of them.
    'store@1/store/test_l63_seeded_call_schedule': [
        'park', 'commit without parking', 'head move', 'held write', 'resume parks again',
        'resume commits', 'settle', 'cancel',
    ],
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


def unsupported_fixture(case):
    """Name the native fixture a campaign build cannot reproduce, or None.

    A file-level `test-env` is not one: it adds a second native run beside the
    base run, and the campaign's seeded run is that base run."""
    for fixture in ('stdout', 'stderr', 'schedule', 'host_op'):
        if getattr(case, fixture):
            return fixture
    return None


def seeded_package_laws(package):
    """A package test opts in with its simulation-specific Env parameter."""
    directory = ROOT / 'core/programs' / package
    manifest = tomllib.loads((directory / 'hirsel.toml').read_text())
    version = manifest['package']['major']
    laws = {}
    for file, module in manifest.get('tests', {}).get('sources', {}).items():
        for name, parameters in SEEDED_TEST.findall((directory / file).read_text()):
            if re.search(r'(^|,)\s*seed_grant\s*:\s*Env\s*(,|$)', parameters):
                definition = f'{package}@{version}/{module.replace(".", "/")}/{name}'
                laws[definition] = definition
    return laws


def seeded_package_inventory():
    """Every workspace package with at least one seeded law."""
    workspace = tomllib.loads((ROOT / 'core/programs/hirsel.toml').read_text())
    inventory = {}
    for package in workspace['workspace']['members']:
        if laws := seeded_package_laws(package):
            inventory[package] = laws
    return inventory


def selected_laws():
    laws = {'postgres': {f'postgres@1/sim_tests/{name}': name for name in POSTGRES}}
    for case in std_native.discover(ROOT, []):
        if (case.name in DIRECT or std_native.simulated_network_case(case)) and not unsupported_fixture(case):
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
    laws['runner-adapters'] = runner
    for package, seeded in seeded_package_inventory().items():
        if package in laws:
            raise ValueError(f'{package} needs one campaign preparation mode')
        laws[package] = seeded
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
        source = std_native.fixture_source(path)
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
            if fixture := unsupported_fixture(case):
                raise ValueError(f'{law} needs an unsupported campaign fixture: {fixture}')
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
    for definition in expected:
        command.extend(['--exact', definition])
    with report.open('w') as log:
        result = subprocess.run(command, stdout=log, cwd=scratch)
    if result.returncode not in (0, 1):
        raise ValueError(f'preparation exited {result.returncode}; see {report}')
    tests = json.loads(report.read_text())['tests']
    rows = {row['definition']: row for row in tests}
    if len(tests) != len(rows) or rows.keys() != set(expected):
        raise ValueError(f'preparation did not discover exactly the campaign entries; see {report}')
    return rows


def covered_preparation(command, scratch, report, expected):
    """Prepare the normal (`bin`) leg as a coverage build and return its rows.
    Every law must produce its executable and `.covmap`."""
    rows = run_preparation(command + ['--coverage'], scratch, report, expected)
    for definition in expected:
        binary = scratch / 'bin' / output_name(definition)
        if rows[definition].get('source') != 'executed' or not Path(f'{binary}.covmap').is_file():
            raise ValueError(f'{definition} produced no coverage executable and map; see {report}')
    return rows


def prepare(compiler, work, kind, group, jobs, seeds, build, runtime, recording_runtime):
    """Build each law twice: the normal build gives a seed's verdict, and the
    recording build records it for the SEM-8 replay differential.

    The normal build is also a coverage build: its `<HEX>.covmap` sits beside
    the executable, every seed run writes a raw file read against it, and the
    campaign merges one profile for each law group (`kind`)."""
    scratch = work / kind
    scratch.mkdir()
    base = [str(compiler), 'test', '--release', '--backend', 'llvm', '-j', str(jobs), '--seed', '0',
            '--store', str(scratch / 'objects'), '--json']
    normal = ['--runtime', str(runtime)] if runtime else []
    libraries = ['--libraries', str(ROOT / 'core')] if runtime else []
    recording = ['--laws', *(['--runtime', str(recording_runtime)] if recording_runtime else [])]
    legs = {'bin': lambda definition: recording if definition in REACH_WITNESSES else normal,
            'recording': lambda definition: recording}
    reports = {}
    rows = {}
    if kind not in GENERATED_KINDS:
        # Package mode writes a workspace lock. Stage the manifests and their
        # relative dependencies away from the checkout.
        programs = work / 'programs'
        if not programs.exists():
            shutil.copytree(ROOT / 'core/programs', programs,
                            ignore=shutil.ignore_patterns('hirsel.lock', 'node_modules'))
        entries = group
        print(f'Preparing {len(entries)} {kind} laws', flush=True)
        for leg, launch in legs.items():
            for index, definition in enumerate(entries.values()):
                report = scratch / f'prepare-{leg}-{index}.json'
                reports[leg, definition] = report
                command = base + ['-o', str(scratch / leg)] + launch(definition) + libraries + [str(programs / kind)]
                if leg == 'bin':
                    prepared = covered_preparation(command, scratch, report, [definition])
                else:
                    prepared = run_preparation(command, scratch, report, [definition])
                rows[leg, definition] = prepared[definition]
    else:
        command = list(base)
        if kind == 'postgres':
            package = scratch / 'postgres'
            shutil.copytree(ROOT / 'core/programs/postgres', package, ignore=shutil.ignore_patterns('hirsel.lock'))
            entries = {law: law for law in group}
            command.extend(libraries + [str(package)])
        else:
            inputs, entries = (contract_inputs if kind == 'contracts' else runner_inputs)(group, scratch)
            manifest = scratch / 'inputs.json'
            manifest.write_text(json.dumps(inputs))
            limits = {case.root_limit for case in group.values()} if kind == 'contracts' else {64 << 20}
            limit, = limits
            command.extend(['--inputs', str(manifest), '--source-core', '--source-std',
                            '--limit', str(limit),
                            '--grant', 'Tasks', '--grant', f'Env={SEED}'])
        print(f'Preparing {len(entries)} {kind} laws', flush=True)
        for leg in legs:
            report = scratch / f'prepare-{leg}.json'
            if leg == 'bin':
                prepared = covered_preparation(command + ['-o', str(scratch / leg)] + normal,
                                               scratch, report, list(entries.values()))
            else:
                prepared = run_preparation(command + ['-o', str(scratch / leg)] + recording,
                                           scratch, report, entries.values())
            for definition in entries.values():
                reports[leg, definition] = report
                rows[leg, definition] = prepared[definition]
    laws = []
    for law, definition in entries.items():
        binaries = {}
        for leg in legs:
            binary = scratch / leg / output_name(definition)
            if rows[leg, definition].get('source') != 'executed' or not binary.is_file():
                raise ValueError(f'{law} produced no {leg} executable; see {reports[leg, definition]}')
            binaries[leg] = str(binary)
        row = rows['bin', definition]
        seed_zero = {'status': 0 if row['passed'] else (row.get('status') or 1),
                     'stdout': row.get('stdout', ''), 'stderr': row.get('stderr', row.get('stopped', ''))}
        count = seeds if seeds is not None else FLOOD_SEEDS.get(law, DEFAULT_SEEDS)
        laws.append({'build': build, 'law': law, 'executable': binaries['bin'],
                     'recording': binaries['recording'], 'seeds': count,
                     'seed_zero': seed_zero, 'min_traces': min(count, TRACE_FLOORS.get(law, 0)),
                     'required_witnesses': REACH_WITNESSES.get(law, []),
                     'coverage': {'group': kind, 'map': binaries['bin'] + '.covmap', 'tool': str(compiler)}})
    shutil.rmtree(scratch / 'objects')
    return laws


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path, help='normal runtime archive for an uninstalled compiler')
    parser.add_argument('--recording-runtime', type=Path, help='recording archive for an uninstalled compiler')
    parser.add_argument('--work', type=Path)
    parser.add_argument('--jobs', type=int, default=4, choices=range(1, 5))
    parser.add_argument('--seeds', type=int, help='override the default seed count for every law')
    parser.add_argument('--law', help='prepare or list just one exact law')
    parser.add_argument('--package', action='append', choices=sorted(seeded_package_inventory()),
                        help='prepare or list only this package (repeatable)')
    parser.add_argument('--list', action='store_true')
    args = parser.parse_args()
    inventory = selected_laws()
    if args.package:
        packages = set(args.package)
        if 'runner' in packages:
            packages.add('runner-adapters')
        inventory = {kind: laws for kind, laws in inventory.items() if kind in packages}
    if args.law and not any(args.law in laws for laws in inventory.values()):
        parser.error('unknown simulation law')
    if args.list:
        print('\n'.join(name for laws in inventory.values() for name in laws if not args.law or name == args.law))
        return
    if not args.compiler or not args.work or (args.seeds is not None and args.seeds <= 0):
        parser.error('preparation needs --compiler and --work, with a positive --seeds override')
    if bool(args.runtime) != bool(args.recording_runtime):
        parser.error('--runtime and --recording-runtime must be supplied together')
    build = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    runtime = args.runtime.resolve(strict=True) if args.runtime else None
    recording_runtime = args.recording_runtime.resolve(strict=True) if args.recording_runtime else None
    work = args.work.resolve()
    work.mkdir()
    laws = []
    for kind, group in inventory.items():
        group = {name: case for name, case in group.items() if not args.law or name == args.law}
        if group:
            laws.extend(prepare(args.compiler.resolve(), work, kind, group, args.jobs, args.seeds,
                                build, runtime, recording_runtime))
    (work / 'manifest.json').write_text(json.dumps(laws, indent=2) + '\n')


if __name__ == '__main__':
    main()
