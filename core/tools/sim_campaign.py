#!/usr/bin/env python3
"""Prepare the native laws for one sim.world seed campaign."""
import argparse
from collections import defaultdict
import hashlib
import json
import os
import sys
from pathlib import Path
import re
import shutil
import signal
import subprocess

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/toolchain-dist'))
import hcfg

import runner_tests
import std_native


ROOT = std_native.ROOT
SEED = 'HIRSEL_TEST_SIM_SEED'
DEFAULT_SEEDS = 32
# Seed zero has the same execution bound as the sweep in tools/sim-campaign.
SEED_TIMEOUT_SECONDS = 300
# A preparation that writes no executable for this long is stopped, so one
# pathological compile marks its law prepare_red instead of holding the night.
# It covers one seed-zero run after the executable is written.
STALL_SECONDS = 900
STALL_POLL_SECONDS = 10
CANARY = 'std@1/tests/campaign_canary/seed_residue'
GENERATED_KINDS = ('postgres', 'contracts', 'runner-adapters', 'canary')
SEEDED_TEST = re.compile(r'^@test\s+fn (\w+)\(([^)]*)\)', re.M)
# Keep the 10,000-frame floods intact.
FLOOD_SEEDS = {
    'runner@1/runner_calls/law_calls_refusals': 2,
    'runner@1/runner_calls/law_calls_session_charge': 2,
}
TRACE_FLOORS = {'store@1/store/bucket_test_m2_register_fence': 2}
PROBE_MARKER = re.compile(r'^\s*// vopr probe(?: (rare))?\s*$')
PROBE_CALL = re.compile(r'^\s*let _ = std\.prop\.sometimes\(("(?:[^"\\]|\\.)+"),')
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


def source_probes(source, path, laws):
    """Read the markers next to the branch observations in a seeded law file."""
    probes = {}
    lines = source.splitlines()
    for index, line in enumerate(lines):
        if not line.lstrip().startswith('// vopr probe'):
            continue
        marker = PROBE_MARKER.fullmatch(line)
        call = PROBE_CALL.match(lines[index + 1]) if index + 1 < len(lines) else None
        if marker is None or call is None or len(laws) != 1:
            raise ValueError(f'{path}:{index + 1}: expected a probe marker before a literal sometimes call in a one-law source')
        label = json.loads(call[1])
        if label in probes:
            raise ValueError(f'{path}:{index + 1}: duplicate probe {label!r}')
        probes[label] = marker[1] == 'rare'
    return probes


def seeded_package_laws(package, probes=None):
    """A package test opts in with its simulation-specific Env parameter."""
    directory = ROOT / 'core/programs' / package
    manifest = hcfg.package(directory)
    version = manifest['version'].split('.')[0]
    laws = {}
    for file, module in manifest.get('tests', {}).get('sources', {}).items():
        path = directory / file
        source = path.read_text()
        definitions = []
        for name, parameters in SEEDED_TEST.findall(source):
            if re.search(r'(^|,)\s*seed_grant\s*:\s*Env\s*(,|$)', parameters):
                definition = f'{manifest["name"]}@{version}/{module.replace(".", "/")}/{name}'
                laws[definition] = definition
                definitions.append(definition)
        if probes is not None and (declared := source_probes(source, path, definitions)):
            probes[definitions[0]] = declared
    return laws


def seeded_package_inventory(probes=None):
    """Every workspace package with at least one seeded law."""
    workspace = hcfg.read(ROOT / 'core/programs/hirsel.hcfg', 'pkg_v1.Workspace')
    inventory = {}
    for package in workspace['members']:
        if laws := seeded_package_laws(package, probes):
            inventory[package] = laws
    return inventory


def selected_laws(skips=None, probes=None):
    laws = {'postgres': {f'hirsel.ai/postgres@1/sim_tests/{name}': name for name in POSTGRES}}
    direct = set()
    for case in std_native.discover(ROOT, []):
        if case.name in DIRECT or std_native.simulated_network_case(case):
            module = case.test_of or f'tests/{case.file.stem}'
            name = case.call.split('(')[0]
            key = f'{case.package}@1/{module}/{name}'
            direct.add(case.name)
            if fixture := unsupported_fixture(case):
                if skips is not None:
                    skips[key] = f'unsupported campaign fixture: {fixture}'
                continue
            laws.setdefault('contracts', {})[key] = case
    if DIRECT - direct:
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
    for package, seeded in seeded_package_inventory(probes).items():
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


def contract_inputs(group, work, source_laws=None):
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
        if source_laws is not None:
            source_laws[file.name] = {law for law, _ in cases}
        package['sources'].append({'file': str(file), 'modules': modules, 'signatures': in_core and bool(first.test_of)})
        if bridge:
            file = work / f'bridge-{index}.hirsel'
            file.write_text(bridge)
            if source_laws is not None:
                source_laws[file.name] = {law for law, _ in cases}
            packages[1]['sources'].append({'file': str(file), 'modules': ['tests', f'campaign_core_{index}'],
                                           'signatures': False})
    return {'core': core, 'packages': packages}, entries


def add_canary(inputs, entries, scratch):
    file = scratch / 'campaign-canary.hirsel'
    file.write_text('''@test
fn seed_residue(seed_grant: Env) -> Result[(), ()] {
    let seed = option.unwrap(sim.world.test_seed(seed_grant))
    if seed % 2 == 1 { Error(()) } else { Ok(()) }
}
''')
    inputs['packages'][1]['sources'].append({
        'file': str(file), 'modules': ['tests', 'campaign_canary'], 'signatures': False,
    })
    entries[CANARY] = CANARY


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


def outputs(directory):
    # Names and change times; the compiler renames finished files into place.
    seen = set()
    for path in directory.iterdir() if directory.is_dir() else ():
        try:
            seen.add((path.name, path.stat().st_mtime_ns))
        except FileNotFoundError:
            pass
    return frozenset(seen)


def run_preparation(command, scratch, report, expected, output):
    command = command + [argument for definition in expected for argument in ('--exact', definition)]
    with report.open('w') as log, report.with_suffix('.stderr').open('w') as stderr:
        child = subprocess.Popen(command, stdout=log, stderr=stderr, cwd=scratch, start_new_session=True)
        seen, quiet = outputs(output), 0
        while True:
            try:
                returncode = child.wait(STALL_POLL_SECONDS)
                break
            except subprocess.TimeoutExpired:
                pass
            current = outputs(output)
            quiet = 0 if current != seen else quiet + STALL_POLL_SECONDS
            seen = current
            if quiet >= STALL_SECONDS:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
                raise ValueError(f'wrote no executable for {STALL_SECONDS} s and was stopped; '
                                 f'see {report} and {report.with_suffix(".stderr")}')
    if returncode not in (0, 1):
        raise ValueError(f'preparation exited {returncode}; see {report} and {report.with_suffix(".stderr")}')
    try:
        document = json.loads(report.read_text())
        if document.get('accepted') is False:
            return {}, {Path(row['primary']['module']).name for row in document['diagnostics']}
        tests = document['tests']
        rows = {row['definition']: row for row in tests}
        if len(tests) != len(rows) or rows.keys() - expected:
            raise ValueError('unexpected or duplicate law definitions')
    except (AttributeError, KeyError, TypeError, ValueError) as error:
        raise ValueError(f'invalid preparation report; see {report} and {report.with_suffix(".stderr")}') from error
    return rows, set()


def prepare_leg(command, scratch, report, leg, expected, errors):
    covered = leg == 'bin'
    try:
        rows, refused = run_preparation(command + ['--coverage'] * covered, scratch, report, expected,
                                        scratch / leg)
    except ValueError as error:
        if len(expected) == 1:
            errors.setdefault(expected[0], []).append(f'{leg}: {error}')
            return {}, set()
        # One program's failed or stalled build must not cost the other laws:
        # prepare each law alone, so only the failing one turns prepare_red.
        print(f'{leg}: {error}; preparing each law alone', flush=True)
        rows, refused = {}, set()
        for index, definition in enumerate(expected):
            alone = report.with_name(f'{report.stem}-{index}.json')
            prepared, rejected = prepare_leg(command, scratch, alone, leg, [definition], errors)
            rows.update(prepared)
            refused |= rejected
        return rows, refused
    for definition in expected:
        binary = scratch / leg / output_name(definition)
        row = rows.get(definition, {})
        if refused:
            reason = f'{leg} input was refused'
        elif row.get('source') != 'executed' or not binary.is_file():
            reason = f'produced no {leg} executable'
            if stopped := row.get('stopped'):
                reason += f': {stopped}'
        elif covered and not Path(f'{binary}.covmap').is_file():
            reason = 'produced no coverage map'
        else:
            continue
        errors.setdefault(definition, []).append(f'{reason}; see {report}')
    return rows, refused


def prepare(compiler, work, kind, group, jobs, seeds, build, runtime, recording_runtime,
            results=None, attempt=0, probes=None, *, defer_probe_gate=False):
    """Build a covered normal leg and a recording leg for SEM-8 replay."""
    scratch = work / (kind if not attempt else f'{kind}-{attempt}')
    scratch.mkdir()
    reports = scratch if results is None else results / 'prepared' / kind
    reports.mkdir(parents=True, exist_ok=True)
    prefix = '' if not attempt else f'retry-{attempt}-'
    base = [str(compiler), 'test', '--release', '--backend', 'llvm', '-j', str(jobs), '--seed', '0',
            '--test-timeout', str(SEED_TIMEOUT_SECONDS),
            '--store', str(scratch / 'objects'), '--json']
    normal = ['--runtime', str(runtime)] if runtime else []
    libraries = ['--libraries', str(ROOT / 'core')] if runtime else []
    recording = ['--laws', *(['--runtime', str(recording_runtime)] if recording_runtime else [])]
    errors = {}
    rows = {}
    probes = probes or {}
    if kind not in GENERATED_KINDS:
        # Package mode writes a workspace lock. Stage its sources away from the checkout.
        programs = work / 'programs'
        if not programs.exists():
            shutil.copytree(ROOT / 'core/programs', programs,
                            ignore=shutil.ignore_patterns('hirsel.lock', 'node_modules'))
        entries = group
        print(f'Preparing {len(entries)} {kind} laws', flush=True)
        for index, definition in enumerate(entries.values()):
            covered = recording if definition in probes else normal
            for leg, launch in (('bin', covered), ('recording', recording)):
                command = base + ['-o', str(scratch / leg)] + launch + libraries + [str(programs / kind)]
                report = reports / f'prepare-{leg}-{index}.json'
                prepared, _ = prepare_leg(command, scratch, report, leg, [definition], errors)
                if leg == 'bin':
                    rows.update(prepared)
    else:
        source_laws = {}
        if kind == 'postgres':
            package = scratch / 'postgres'
            shutil.copytree(ROOT / 'core/programs/postgres', package, ignore=shutil.ignore_patterns('hirsel.lock'))
            entries = {law: law for law in group}
            base.extend(libraries + [str(package)])
        else:
            if kind == 'runner-adapters':
                inputs, entries = runner_inputs(group, scratch)
            else:
                contracts = group if kind == 'contracts' else {}
                inputs, entries = contract_inputs(contracts, scratch, source_laws)
            if kind == 'canary':
                add_canary(inputs, entries, scratch)
            manifest = scratch / 'inputs.json'
            manifest.write_text(json.dumps(inputs))
            limits = {case.root_limit for case in group.values()} if kind == 'contracts' else {64 << 20}
            limit, = limits
            base.extend(['--inputs', str(manifest), '--source-core', '--source-std',
                         '--limit', str(limit), '--grant', 'Tasks', '--grant', f'Env={SEED}'])
        print(f'Preparing {len(entries)} {kind} laws', flush=True)
        for leg, launch in (('bin', normal), ('recording', recording)):
            report = reports / f'{prefix}prepare-{leg}.json'
            # A law whose bin leg failed is already prepare_red; do not build it again.
            expected = [definition for definition in entries.values() if definition not in errors]
            if not expected:
                break
            prepared, refused = prepare_leg(base + ['-o', str(scratch / leg)] + launch, scratch, report,
                                            leg, expected, errors)
            rejected = {law for module in refused for law in source_laws.get(module, ())}
            if results is not None and rejected and rejected != group.keys():
                # Contract fixtures share a compiler invocation. Drop the refused
                # input files and prepare the other fixtures in a fresh directory.
                for law in sorted(rejected):
                    prepare_outcome(results, build, law, 'prepare_red', '; '.join(errors[entries[law]]))
                if (scratch / 'objects').exists():
                    shutil.rmtree(scratch / 'objects')
                remaining = {law: case for law, case in group.items() if law not in rejected}
                return prepare(compiler, work, kind, remaining, jobs, seeds, build, runtime,
                               recording_runtime, results, attempt + 1, probes,
                               defer_probe_gate=defer_probe_gate)
            if leg == 'bin':
                rows.update(prepared)
    laws = []
    for law, definition in entries.items():
        if definition in errors:
            reason = '; '.join(errors[definition])
            if results is None:
                raise ValueError(f'{law} {reason}')
            prepare_outcome(results, build, law, 'prepare_red', reason)
            continue
        binary = scratch / 'bin' / output_name(definition)
        row = rows[definition]
        seed_zero = {'status': 0 if row['passed'] else (row.get('status') or 1),
                     'stdout': row.get('stdout', ''), 'stderr': row.get('stderr', row.get('stopped', ''))}
        if row.get('timed_out'):
            seed_zero.update(status=124, timed_out=True)
        # Two consecutive allocated seeds always exercise both canary residues.
        count = 2 if law == CANARY else seeds if seeds is not None else FLOOD_SEEDS.get(law, DEFAULT_SEEDS)
        laws.append({'build': build, 'law': law, 'executable': str(binary),
                     'recording': str(scratch / 'recording' / binary.name), 'seeds': count,
                     'seed_zero': seed_zero, 'min_traces': min(count, TRACE_FLOORS.get(law, 0)),
                     'required_witnesses': list(probes.get(law, {})), 'defer_probe_gate': defer_probe_gate,
                     'coverage': {'group': kind, 'map': f'{binary}.covmap', 'tool': str(compiler)}})
    if (scratch / 'objects').exists():
        shutil.rmtree(scratch / 'objects')
    return laws


def prepare_outcome(results, build, law, kind, reason):
    print(f'{law}: {reason}', flush=True)
    with (results / 'prepare.jsonl').open('a') as stream:
        print(json.dumps({'build': build, 'law': law, 'kind': kind, 'reason': reason,
                          'status': int(kind == 'prepare_red')}), file=stream)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path, help='normal runtime archive for an uninstalled compiler')
    parser.add_argument('--recording-runtime', type=Path, help='recording archive for an uninstalled compiler')
    parser.add_argument('--work', type=Path)
    results_options = parser.add_mutually_exclusive_group()
    results_options.add_argument('--nightly-results', type=Path,
                                 help='retain reports and defer probe gates to the full nightly sweep')
    results_options.add_argument('--prepare-results', type=Path,
                                 help='retain reports and write a partial manifest on preparation failures')
    parser.add_argument('--jobs', type=int, default=4, choices=range(1, 5))
    parser.add_argument('--seeds', type=int, help='override the default seed count for every law')
    parser.add_argument('--law', action='append', help='prepare or list an exact law (repeatable)')
    parser.add_argument('--package', action='append', choices=sorted(seeded_package_inventory()),
                        help='prepare or list only this package (repeatable)')
    parser.add_argument('--list', action='store_true')
    args = parser.parse_args()
    skips = {}
    probes = {}
    inventory = selected_laws(skips, probes)
    if args.package:
        packages = set(args.package)
        if 'runner' in packages:
            packages.add('runner-adapters')
        inventory = {kind: laws for kind, laws in inventory.items() if kind in packages}
        skips = {name: reason for name, reason in skips.items() if name.split('@')[0] in packages}
    requested = set(args.law or ())
    available = {law for group in inventory.values() for law in group} | skips.keys()
    if unknown := requested - available:
        parser.error(f'unknown simulation law: {", ".join(sorted(unknown))}')
    if args.list:
        names = [name for laws in inventory.values() for name in laws if not requested or name in requested]
        if args.nightly_results is not None:
            names.extend(name for name in skips if not requested or name in requested)
            names.append(CANARY)
            (args.nightly_results / 'probe-declarations.json').write_text(
                json.dumps({name: probes[name] for name in names if name in probes}) + '\n')
        print('\n'.join(names))
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
    results = args.nightly_results or args.prepare_results
    if results is not None:
        results = results.resolve(strict=True)
        (results / 'prepare.jsonl').touch(exist_ok=False)
    for law, reason in skips.items():
        if not requested or law in requested:
            if results is None:
                print(f'{law}: {reason}')
            else:
                prepare_outcome(results, build, law, 'prepare_skip', reason)
    laws = []
    planned = 0
    for kind, group in inventory.items():
        group = {name: case for name, case in group.items() if not requested or name in requested}
        if group:
            planned += len(group)
            laws.extend(prepare(args.compiler.resolve(), work, kind, group, args.jobs, args.seeds,
                                build, runtime, recording_runtime, results, probes=probes,
                                defer_probe_gate=args.nightly_results is not None))
    if args.nightly_results is not None:
        laws.extend(prepare(args.compiler.resolve(), work, 'canary', {CANARY: None}, args.jobs,
                            args.seeds, build, runtime, recording_runtime, results, defer_probe_gate=True))
    (work / 'manifest.json').write_text(json.dumps(laws, indent=2) + '\n')
    return int(args.prepare_results is not None and len(laws) != planned)


if __name__ == '__main__':
    raise SystemExit(main())
