#!/usr/bin/env python3
"""Run the M2b library slices' cases on the evaluator, Cranelift and LLVM.

Each case in core/corpus/m2b/<workload>/cases.json names a u64 the case function
`case_<name>` returns, hand-derived in that directory's SPEC.md, and the bytes still
admitted when it returns (`used`). The evaluator runs `case_<name>` directly through
hirsel-core-contract-eval; a native engine builds the workload's `main` once and runs
it with the case name, which prints the same value. A case may list engines under
`red` with a cause: on those engines it must still disagree, so the row cannot outlive
the gap it records. A red entry {"cause": ..., "gives": N} also names the value the gap
produces, which the engine must return exactly; it may add
"used" when the gap keeps a different number of bytes admitted.

A workload whose entry has `"dir": true` is built with `--dir` on one spool directory
per engine, emptied before each case; `--host` runs exactly those workloads (they need
the invoking host's openat2), and a run without it skips them. An entry with `"role":
true` has no cases of its own: it is built for a `"pair"` workload, whose cases start
a server role and a client role as two processes on that spool directory (arguments
`server_args`, default `serve`, and `client_args`, default the case name); the client
prints the case's value, the server its `server_value`. A manifest package may carry an
`id`, and an entry may `use` it in place of the package of the same name.

A native engine checks `used` too. The shipping runtime has no test hook that reads
the root's admitted bytes, so the runner builds a probe entry instead (`probe_source`). It has
the workload main's signature and calls `case_<name>` between two refused requests
whose `Capacity.available` it prints; `used` is the difference. Any other argument
goes to the workload's own `main`, so the probe also serves as a pair role. Pair
cases have no case function and no `used`; they compare stdout only.

Example: m2b.py --compiler hirsel --evaluator hirsel-core-contract-eval --runtime libhirsel_core_rt.a
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
import re
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile

from case_progress import Case, run_case

ROOT = Path(__file__).resolve().parents[2]
ENGINES = ('evaluator', 'cranelift', 'llvm')


def inputs(root, manifest, workload):
    """core and std from their installed sources, then the workload's package closure."""
    core = {'name': 'core', 'version': '1'}
    packages = []
    for name in ('core', 'std'):
        packages.append({'package': core if name == 'core' else {'name': 'std', 'version': '1'},
                         'dependencies': [] if name == 'core' else [core],
                         'sources': [{'file': str(p.resolve()), 'modules': [p.stem], 'signatures': True}
                                     for p in sorted((root / 'core' / name).glob('*.hirsel'))]})
    entry = next(e for e in manifest['entries'] if e['workload'] == workload)
    by_name = {p.get('id', p['package']['name']): p for p in manifest['packages']}
    use = entry.get('use', {})
    pending, reached = [entry['package']['name']], []
    while pending:
        name = pending.pop()
        name = use.get(name, name)
        if name in reached or name not in by_name:
            continue
        reached.append(name)
        pending.extend(d['name'] for d in by_name[name]['dependencies'])
    for name in sorted(reached):
        package = json.loads(json.dumps(by_name[name]))
        package.pop('id', None)
        for source in package['sources']:
            source['file'] = str((root / source['file']).resolve())
        existing = next((p for p in packages if p['package'] == package['package']), None)
        if existing is None:
            packages.append(package)
        else:
            existing['sources'].extend(package['sources'])
    return entry, {'core': core, 'packages': packages}


PROBE = 'm2bprobe'


def probe_source(main_source, package, names):
    """A main that runs each case_<name> between two reads of the root's available bytes."""
    signature = re.search(r'pub fn main\(([^)]*)\)', main_source).group(1)
    params = [tuple(x.strip() for x in p.split(':')) for p in signature.split(',')]
    by_type = {t: n for n, t in params}
    lines = ['fn available() -> u64 {',
             '    let mut probe: Bytes = list.new()',
             '    let huge: u64 = 18446744073709551615',
             '    match list.try_reserve(mut probe, huge) {',
             '        Ok(_) => 0,', '        Error(c) => c.available,', '    }', '}', '',
             'fn report_text(v: u64, before: u64, after: u64) -> String {',
             '    let mut s = string.new()']
    for part, sep in (('v', ' '), ('before', ' '), ('after', '\\n')):
        lines += [f'    string.push_u64(mut s, {part})', f'    string.push(mut s, "{sep}")']
    lines += ['    s', '}', '',
              'fn report(out: Stdout, v: u64, before: u64, after: u64) -> Result[(), Exit] waits {',
              '    let Ok(Ok(s)) = isolate(scoped fn() -> String { report_text(v, before, after) }) else {',
              '        return Error(Exit(status: 3))', '    }',
              '    match stdout.write(out, s) {', '        Ok(()) => Ok(()),', '        Error(_) => Error(Exit(status: 2)),',
              '    }', '}', '', f'pub fn main({signature}) -> Result[(), Exit] waits {{',
              f'    let Some(name) = args.get({by_type["Args"]}, 0) else {{', '        return Error(Exit(status: 2))', '    }']
    for name in names:
        found = re.search(rf'pub fn case_{name}\(([^)]*)\)', main_source)
        if not found:
            continue
        passed = ', '.join(by_type[p.split(':')[1].strip()] for p in found.group(1).split(',') if p.strip())
        lines += [f'    if name == "{name}" {{', '        let before = available()',
                  f'        let v = {package}.main.case_{name}({passed})', '        let after = available()',
                  f'        return report({by_type["Stdout"]}, v, before, after)', '    }']
    lines += [f'    {package}.main.main({", ".join(n for n, _ in params)})', '}', '']
    return '\n'.join(lines)


def evaluate(evaluator, manifest_file, entry, case):
    path = f"{entry['package']['name']}@{entry['package']['version']}/{entry['module']}/case_{case['name']}"
    run = run_case(f'{entry["workload"]}/{case["name"]}', 'evaluator',
                   [str(evaluator), str(manifest_file), path], capture_output=True)
    try:
        report = json.loads(run.stdout)
    except ValueError:
        return f'evaluator exit {run.returncode}: {run.stderr.decode(errors="replace")[-400:]}'
    if 'error' in report:
        return f"evaluator refused: {report['error'][:400]}"
    outcome = report['outcome']
    if outcome['kind'] != 'returned':
        return f'evaluator {outcome["kind"]}: {json.dumps(outcome)[:400]}'
    if outcome['values'] != [case['value']]:
        return f"returned {outcome['values']}, expected [{case['value']}]"
    if report['used'] != case['used']:
        return f"used {report['used']} bytes after return, expected {case['used']}"
    return None


def build(compiler, runtime, cc, manifest_file, entry, engine, scratch, spool=None):
    binary = scratch / f'program-{engine}'
    path = f"{PROBE}@1/main/main"
    command = [str(compiler), 'build', '--inputs', str(manifest_file), '--entry', path, '--backend', engine,
               '--runtime', str(runtime), '--grant', 'Stdout', '--store', str(scratch / f'store-{engine}'),
               '--json', '-o', str(binary)]
    for grant in entry.get('grants', []):
        command.extend(['--grant', grant])
    if entry.get('dir'):
        command.extend(['--dir', str(spool)])
    if engine == 'llvm':
        command.append('--release')
    if cc:
        command.extend(['--cc', str(cc)])
    result = run_case(f'{entry["workload"]}:build', engine, command, cwd=ROOT, capture_output=True)
    (scratch / f'build-{engine}.json').write_bytes(result.stdout)
    if result.returncode:
        try:
            product = json.loads(result.stdout)
            stops = [*product.get('diagnostics', []), *product.get('incomplete', [])]
            reason = '; '.join(f"{d.get('rule', 'gap')} {d['primary']['module']} {d['primary']['span']}: {d['message']}"
                               for d in stops[:3]) or product.get('stopped') or 'no diagnostic'
        except (ValueError, KeyError):
            reason = result.stderr.decode(errors='replace')[-400:]
        return None, f'build failed: {reason}'
    return binary, None


def mixed_store(args, entry, selected, scratch, cases, with_probe):
    """One package edited between two builds in one Cranelift store: the units
    of the other packages hit and the edited unit is emitted, so one executable
    links hit objects with fresh ones. The entry's `mixed` names the copied
    packages, the edit, and the bodies rechecked and units compiled; the
    executable must be byte-equal to a fresh store's, and every case agree."""
    spec = entry['mixed']
    work = scratch / 'mixed'
    shutil.rmtree(work, ignore_errors=True)
    shared, fresh = work / 'shared', work / 'fresh'
    for d in (work, shared, fresh):
        d.mkdir()
    edited = None
    for package in selected['packages']:
        if package['package']['name'] in spec['copy']:
            for source in package['sources']:
                copy = work / Path(source['file']).name
                shutil.copyfile(source['file'], copy)
                if Path(source['file']) == (args.root / spec['file']).resolve():
                    edited = copy
                source['file'] = str(copy)
    if edited is None:
        return [f"mixed: {spec['file']} is not a source of {spec['copy']}"]
    manifest_file = with_probe(selected, work)
    failures = []
    binary, stop = build(args.compiler, args.runtime, args.cc, manifest_file, entry, 'cranelift', shared)
    if stop:
        return [f'mixed first build: {stop}']
    text = edited.read_text()
    if spec['from'] not in text:
        return [f"mixed: the edit's text is not in {spec['file']}"]
    edited.write_text(text.replace(spec['from'], spec['to'], 1))
    binary, stop = build(args.compiler, args.runtime, args.cc, manifest_file, entry, 'cranelift', shared)
    if stop:
        return [f'mixed second build: {stop}']
    second = json.loads((shared / 'build-cranelift.json').read_text())
    counts = (second['rechecked'], second['native']['compiled'])
    if counts != (spec['rechecked'], spec['compiled']):
        failures.append(f"mixed second build rechecked {counts[0]} and compiled {counts[1]}, "
                        f"expected {spec['rechecked']} and {spec['compiled']}")
    oracle, stop = build(args.compiler, args.runtime, args.cc, manifest_file, entry, 'cranelift', fresh)
    if stop:
        return failures + [f'mixed fresh build: {stop}']
    if binary.read_bytes() != oracle.read_bytes():
        failures.append("mixed: the same-store executable is not the fresh store's")
    for case in cases:
        if 'cranelift' not in case.get('red', {}):
            failure = native(binary, case)
            if failure:
                failures.append(f"mixed {case['name']}: {failure}")
    return failures


def empty(spool):
    if spool is not None:
        shutil.rmtree(spool, ignore_errors=True)
        spool.mkdir()


def native(binary, case, spool=None, engine='cranelift'):
    empty(spool)
    run = run_case(case['name'], engine, [str(binary), case['name']], capture_output=True)
    words = run.stdout.decode(errors='replace').split()
    if run.returncode or run.stderr or len(words) != 3 or not all(w.isdigit() for w in words):
        return f"exit {run.returncode}, stdout {run.stdout!r}, stderr {run.stderr[-300:]!r}; expected {case['value']} and used"
    value, used = int(words[0]), int(words[1]) - int(words[2])
    if value != case['value']:
        return f"returned {value}, expected {case['value']}"
    if used != case['used']:
        return f"used {used} bytes after return, expected {case['used']}"
    return None


def pair(server, client, case, spool, engine):
    """One request between two processes: the server starts first, the client's reply ends it."""
    empty(spool)
    with Case(case['name'], engine) as progress, ThreadPoolExecutor(max_workers=2) as readers:
        serving = subprocess.Popen([str(server), *case.get('server_args', ['serve'])],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        stdout, stderr = bytearray(), bytearray()
        out_done = readers.submit(progress.collect, serving.stdout, stdout.extend)
        err_done = readers.submit(progress.collect, serving.stderr, stderr.extend)
        run = progress.run([str(client), *case.get('client_args', [case['name']])], capture_output=True)
        serving.wait()
        out_done.result()
        err_done.result()
        out, err = bytes(stdout), bytes(stderr)
    want, served = f"{case['value']}\n".encode(), f"{case['server_value']}\n".encode()
    if run.returncode or run.stdout != want or run.stderr:
        return f'client exit {run.returncode}, stdout {run.stdout!r}, stderr {run.stderr[-300:]!r}; expected {want!r}'
    if serving.returncode or out != served or err:
        return f'server exit {serving.returncode}, stdout {out!r}, stderr {err[-300:]!r}; expected {served!r}'
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--evaluator', type=Path)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--cc', type=Path, default=(Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang')
                        if 'LLVM_SYS_231_PREFIX' in os.environ else None)
    parser.add_argument('--engine', choices=ENGINES, action='append')
    parser.add_argument('--only', nargs='+')
    parser.add_argument('--host', action='store_true', help='run the workloads that need a real directory')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    engines = args.engine or list(ENGINES)
    if 'evaluator' in engines and not args.evaluator:
        parser.error('--evaluator is required for the evaluator engine')
    if set(engines) - {'evaluator'} and not (args.compiler and args.runtime):
        parser.error('--compiler and --runtime are required for native engines')
    manifest = json.loads((args.root / 'core/programs/m2b/manifest.json').read_text())
    workloads = [e['workload'] for e in manifest['entries']
                 if (not args.only or e['workload'] in args.only) and not e.get('role')
                 and bool(e.get('dir') or e.get('pair')) == args.host]
    unexpected, passed, reds = [], 0, 0
    with tempfile.TemporaryDirectory(prefix='hirsel-m2b-') as temp:
        spools = {engine: Path(temp) / f'spool-{engine}' for engine in engines}
        for spool in spools.values():
            spool.mkdir()
        prepared, built = {}, {}

        def prepare(workload):
            if workload not in prepared:
                scratch = Path(temp) / workload
                scratch.mkdir()
                entry, selected = inputs(args.root, manifest, workload)
                main_file = next(Path(s['file']) for p in selected['packages']
                                 if p['package'] == entry['package'] for s in p['sources']
                                 if entry['module'] in s['modules'])
                case_file = args.root / 'core/corpus/m2b' / workload / 'cases.json'
                names = [c['name'] for c in json.loads(case_file.read_text())] if case_file.exists() else []
                probe_text = probe_source(main_file.read_text(), entry['package']['name'], names)

                def with_probe(selected, scratch):
                    probe = scratch / 'probe.hirsel'
                    probe.write_text(probe_text)
                    deps = [{'name': 'core', 'version': '1'}, {'name': 'std', 'version': '1'}, entry['package']]
                    selected['packages'].append({'package': {'name': PROBE, 'version': '1'}, 'dependencies': deps,
                                                 'sources': [{'file': str(probe), 'modules': ['main'], 'signatures': False}]})
                    manifest_file = scratch / 'inputs.json'
                    manifest_file.write_text(json.dumps(selected))
                    return manifest_file
                manifest_file = with_probe(json.loads(json.dumps(selected)), scratch)
                prepared[workload] = scratch, entry, manifest_file, selected, with_probe
            return prepared[workload][:3]

        def binary_for(workload, engine):
            if (workload, engine) not in built:
                scratch, entry, manifest_file = prepare(workload)
                built[workload, engine] = build(args.compiler, args.runtime, args.cc, manifest_file, entry,
                                                engine, scratch, spools[engine])
            return built[workload, engine]

        for workload in workloads:
            scratch, entry, manifest_file = prepare(workload)
            cases = json.loads((args.root / 'core/corpus/m2b' / workload / 'cases.json').read_text())
            if entry.get('mixed') and 'cranelift' in engines:
                _, _, _, selected, with_probe = prepared[workload]
                failures = mixed_store(args, entry, json.loads(json.dumps(selected)), scratch, cases, with_probe)
                unexpected.extend(f'{workload} cranelift {f}' for f in failures)
                if not failures:
                    passed += 1
                    print(f'pass {workload}/mixed-store cranelift')
            for engine in engines:
                if entry.get('pair') and engine == 'evaluator':
                    continue
                spool = spools[engine] if entry.get('dir') or entry.get('pair') else None
                binary, stop = (None, None) if engine == 'evaluator' or entry.get('pair') else binary_for(workload, engine)
                for case in cases:
                    if entry.get('pair'):
                        (server, stop), (client, stop_client) = (binary_for(case['server'], engine),
                                                                 binary_for(case['client'], engine))
                        stop = stop or stop_client
                    if stop:
                        failure = stop
                    elif engine == 'evaluator':
                        failure = evaluate(args.evaluator, manifest_file, entry, case)
                    elif entry.get('pair'):
                        failure = pair(server, client, case, spool, engine)
                    else:
                        failure = native(binary, case, spool, engine)
                    red = case.get('red', {}).get(engine)
                    label = f'{workload}/{case["name"]} {engine}'
                    if isinstance(red, dict) and stop:
                        unexpected.append(f"{label}: red row expects {red['gives']}: {stop}")
                        continue
                    if isinstance(red, dict):
                        # The gap's own value: the engine must give exactly it, so the
                        # row turns when the gap closes or when something else breaks.
                        gives = dict(case, value=red['gives'], used=red.get('used', case['used']))
                        wrong = (evaluate(args.evaluator, manifest_file, entry, gives) if engine == 'evaluator'
                                 else pair(server, client, gives, spool, engine) if entry.get('pair')
                                 else native(binary, gives, spool, engine))
                        failure = None if wrong else f"gives {red['gives']} as the gap says"
                        if wrong:
                            unexpected.append(f"{label}: red row expects {red['gives']}: {wrong}")
                            continue
                        red = red['cause']
                    if red and failure:
                        reds += 1
                        print(f'red (expected: {red}) {label}: {failure}')
                    elif red:
                        unexpected.append(f'{label} now agrees: remove {engine} from its red row')
                    elif failure:
                        unexpected.append(f'{label}: {failure}')
                    else:
                        passed += 1
                        print(f'pass {label}')
    for line in unexpected:
        print(f'FAIL {line}')
    print(f'm2b: {passed} passed, {reds} expected red, {len(unexpected)} failed')
    return 1 if unexpected or not passed else 0


if __name__ == '__main__':
    sys.exit(main())
