#!/usr/bin/env python3
"""Build each M1 entry once per engine and compare every applicable corpus case."""
import argparse
import copy
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def inputs_for(manifest, entry, root):
    """Supply the complete package dependency closure; the driver selects definitions."""
    packages = {(p['package']['name'], p['package']['version']): p for p in manifest['packages']}
    selected = (entry['package']['name'], entry['package']['version'])
    pending, reached = [selected], set()
    while pending:
        name = pending.pop()
        if name in reached:
            continue
        reached.add(name)
        pending.extend((p['name'], p['version']) for p in packages[name].get('dependencies', []))
    inputs = {'core': manifest['core'], 'packages': []}
    for name in sorted(reached):
        package = copy.deepcopy(packages[name])
        if name == selected and not any(s['modules'] == entry['module'].split('/') for s in package['sources']):
            raise ValueError(f'entry module missing: {entry}')
        for source in package['sources']:
            source['file'] = str((root / source['file']).resolve())
        inputs['packages'].append(package)
    return inputs


def first_stop(product, entry):
    prefix = f"{entry['package']['name']}@{entry['package']['version']}/"
    rows = [(d, d['rule']) for d in product.get('diagnostics', [])]
    rows += [(d, 'checker gap') for d in product.get('incomplete', [])]
    rows.sort(key=lambda row: not row[0]['primary']['module'].startswith(prefix))
    if rows:
        d, rule = rows[0]
        site = d['primary']
        span = site['span']
        return f"check {rule} {site['module']}:{span['start']}..{span['end']}: {d['message']}"
    return product.get('stopped') or 'build failed without a diagnostic'


CHECK_SLICES = ('A1', 'B', 'C', 'E', 'writers/sequences/regions/tasks', 'A2', 'D', 'unassigned')


def checker_slice(message, expression=""):
    """Route reported implementation gaps; unknown constructs stay visible."""
    text = message.lower()
    if text == 'this generic core operation is not built':
        head = re.match(r'\s*([A-Za-z_]\w*)', expression)
        if not head:
            return 'unassigned'
        module = head[1]
        if module in ('channel', 'task', 'writer', 'span', 'seq', 'indexed', 'region', 'build_in', 'emit'):
            return 'writers/sequences/regions/tasks'
        if module in ('list', 'map', 'string', 'box', 'rc', 'arc', 'option', 'result', 'replace', 'clone'):
            return 'A1'
        if module in ('int', 'float', 'char'):
            return 'E'
        return 'unassigned'
    if 'function, constant and module' in text or 'kind of name' in text:
        return 'B'
    if any(word in text for word in ('callable', 'closure', 'calls through values')):
        return 'C'
    if any(word in text for word in ('pattern', 'match', '`?`')):
        return 'A2'
    if 'iteration' in text:
        return 'E'
    if any(word in text for word in ('region', 'stable', 'writer', 'sequence', 'span', 'task', 'channel')):
        return 'writers/sequences/regions/tasks'
    if any(word in text for word in ('generic', 'type parameter', 'type argument', 'argument is not built', 'call')):
        return 'B'
    if any(word in text for word in ('index', 'iteration', 'range', 'integer', 'float', 'array', 'for loop',
                                      'expression form', 'operator', 'character literal', 'byte literal')):
        return 'E'
    if any(word in text for word in ('resource', 'cleanup', 'literal', 'construction', 'values of', 'views of',
                                      'field', 'tuple', 'assignment', '`view`', 'comparison of this type')):
        return 'A1'
    if any(word in text for word in ('declaration', 'outlives', 'sized parameter')):
        return 'D'
    return 'unassigned'


def gap_sites(incomplete, sources=None, first_stops=()):
    """Count source spans, not duplicate messages or the first message per body."""
    sites = {}
    instance_gaps = [dict(message=stop['construct'], primary=stop['primary'], rule=stop['rule'])
                     for stop in first_stops if stop['rule'] == 'instances']
    for gap in [*incomplete, *instance_gaps]:
        primary = gap['primary']
        span = primary['span']
        source = (sources or {}).get(primary['module'])
        expression = source[span['start']:span['end']].decode(errors='replace') if source is not None else ''
        owner = checker_slice(gap['message'], expression)
        if gap.get('rule') == 'instances':
            module = primary['module']
            if gap['message'] == 'native body is unavailable':
                if module.startswith(('core@1/string', 'core@1/list', 'core@1/map', 'core@1/box', 'core@1/rc', 'core@1/arc', 'core@1/option', 'core@1/result')):
                    owner = 'A1'
                elif module.startswith(('core@1/int', 'core@1/float', 'core@1/char')):
                    owner = 'E'
                elif module.startswith(('core@1/host', 'core@1/args', 'core@1/task', 'core@1/channel', 'core@1/region', 'core@1/writer', 'core@1/span', 'core@1/seq', 'core@1/indexed')):
                    owner = 'writers/sequences/regions/tasks'
            elif 'generic' in gap['message']:
                owner = 'B'
            elif 'indirect' in gap['message']:
                owner = 'C'
        key = (owner, primary['module'], span['start'], span['end'])
        sites.setdefault(key, dict(slice=owner, primary=primary, message=gap['message'], expression=expression))
    return list(sites.values())


def files_below(directory):
    return {str(p.relative_to(directory)): p.read_bytes() for p in directory.rglob('*') if p.is_file()}


class IncompleteWorkload(AssertionError):
    """The artifact did not demonstrate execution of the selected workload."""


def compare_case(binary, directory, case, scratch):
    """Each run owns an isolated directory. Only explicit outputs may remain."""
    work = scratch / case['name']
    work.mkdir()
    input_path = directory / case['input']
    # Inputs are copied so one case cannot mutate a later case's fixture.
    local_input = work / case['input']
    local_input.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(input_path, local_input)
    for name, source in case.get('inputs', {}).items():
        destination = work / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(directory / source, destination)
    before = files_below(work)
    result = subprocess.run([str(binary), case['input'], case['variant']], cwd=work,
                            env={**os.environ, **case.get('env', {})}, capture_output=True)
    expected = directory / 'EXPECTED' / case['name']
    want = (expected.with_suffix('.stdout').read_bytes(), expected.with_suffix('.stderr').read_bytes(),
            int(expected.with_suffix('.exit').read_text()))
    if want[0] and not result.stdout:
        raise IncompleteWorkload(
            f'empty stdout against nonempty EXPECTED; exit status {result.returncode}, stderr {result.stderr!r}')
    labels = ('stdout', 'stderr', 'exit status')
    for label, actual, desired in zip(labels, (result.stdout, result.stderr, result.returncode), want):
        if actual != desired:
            return f'{label} differs: got {actual!r}, expected {desired!r}'
    # EXPECTED/<case>.files is an optional directory of exact relative output
    # paths. Its absence means no files may change. Deletion is also a change.
    output_dir = expected.with_suffix('.files')
    desired = files_below(output_dir) if output_dir.exists() else {}
    after = files_below(work)
    changes = {name: data for name, data in after.items() if before.get(name) != data}
    removed = sorted(before.keys() - after.keys())
    if changes != desired or removed:
        return f'files differ: changed={sorted(changes)}, removed={removed}, expected={sorted(desired)}'
    return None


def run(compiler, runtime, engines, manifest_path, root, store, cc=None, only=None):
    manifest = json.loads(manifest_path.read_text())
    entries = [e for e in manifest['entries'] if not only or e['workload'] in only]
    if not entries:
        raise ValueError('manifest selection contains no entries')
    rows = []
    with tempfile.TemporaryDirectory(prefix='hirsel-m1-') as temp:
        temp = Path(temp)
        for entry in entries:
            corpus = root / 'core/corpus/m1' / entry['workload']
            cases = [c for c in json.loads((corpus / 'cases.json').read_text())
                     if entry['variant'] in c.get('layouts', ['idiomatic', 'matched'])]
            if not cases:
                raise ValueError(f'entry has no applicable cases: {entry}')
            selected_inputs = inputs_for(manifest, entry, root)
            sources = {}
            for package in selected_inputs['packages']:
                for source in package['sources']:
                    file = Path(source['file'])
                    if file.is_file():
                        name = package['package']
                        module = f"{name['name']}@{name['version']}/" + '/'.join(source['modules'])
                        sources[module] = file.read_bytes()
            for engine in engines:
                name = f"{entry['workload']}-{entry['variant']}-{engine}"
                scratch = temp / name
                scratch.mkdir()
                inputs = scratch / 'inputs.json'
                inputs.write_text(json.dumps(selected_inputs))
                binary = scratch / 'program'
                path = entry['package']
                entry_path = f"{path['name']}@{path['version']}/{entry['module']}/{entry['function']}"
                command = [str(compiler), 'build', '--inputs', str(inputs), '--entry', entry_path,
                           '--backend', engine, '--runtime', str(runtime),
                           '--dir', '.', '--grant', 'Stdout', '--grant', 'Stderr', '--grant', 'Tasks', '--store',
                           str(store / entry['workload'] / entry['variant']), '--json', '-o', str(binary)]
                if engine == 'llvm':
                    command.append('--release')
                if cc:
                    command.extend(['--cc', str(cc)])
                build = subprocess.run(command, cwd=root, capture_output=True)
                row = dict(program=entry['workload'], variant=entry['variant'], engine=engine,
                           checked=False, built=False, passed=0, cases=len(cases), first_failure=None,
                           stopped=None)
                rows.append(row)
                try:
                    product = json.loads(build.stdout)
                except (ValueError, UnicodeDecodeError):
                    row['stopped'] = f'build tool: exit {build.returncode}: {build.stderr.decode(errors="replace").strip()}'
                    continue
                row['diagnostics'] = product.get('diagnostics', [])
                row['incomplete'] = product.get('incomplete', [])
                row['gap_sites'] = gap_sites(row['incomplete'], sources, product.get('reachable', {}).get('first_stops', []))
                row['checked'] = bool(product['accepted'])
                row['census'] = product.get('reachable', {}).get('census', {})
                row['first_stops'] = product.get('reachable', {}).get('first_stops', [])
                row['reachable_bodies'] = product.get('reachable', {}).get('bodies')
                row['reachable_declarations'] = product.get('reachable', {}).get('declarations')
                if build.returncode or not product.get('native'):
                    row['stopped'] = first_stop(product, entry)
                    continue
                expects_output = any((corpus / 'EXPECTED' / c['name']).with_suffix('.stdout').stat().st_size
                                     for c in cases)
                if expects_output and (row['reachable_bodies'] is None or row['reachable_bodies'] <= 1):
                    row['stopped'] = (f"runner assertion: reachable closure has {row['reachable_bodies']} bodies "
                                      'against nonempty EXPECTED; workload requires more than its entry wrapper')
                    continue
                row['built'] = True
                for case in cases:
                    try:
                        failure = compare_case(binary, corpus, case, scratch)
                    except IncompleteWorkload as error:
                        row['built'] = False
                        row['passed'] = 0
                        if row['first_failure'] is None:
                            row['first_failure'] = case['name']
                            row['stopped'] = f'runner assertion: {error}'
                        else:
                            row['stopped'] += f"; runner assertion at {case['name']}: {error}"
                        break
                    except (OSError, ValueError) as error:
                        failure = f'run tool: {error}'
                    if failure:
                        if row['first_failure'] is None:
                            row['first_failure'] = case['name']
                            row['stopped'] = f'run {failure}'
                    else:
                        row['passed'] += 1
        for engine in engines:
            command = [str(compiler), 'image', '--libraries', str(root / 'core'), '--store',
                       str(store / 'canonical-image'), '--json']
            result = subprocess.run(command, cwd=root, capture_output=True)
            row = dict(program='core + std fully checked; canonical image accepted', variant='libraries', engine=engine,
                       checked=False, built=False, passed=0, cases=0, first_failure=None, stopped=None)
            rows.append(row)
            try:
                product = json.loads(result.stdout)
                row['checked'] = bool(product['accepted'])
                row['built'] = result.returncode == 0 and bool(product.get('image'))
                if not row['built']:
                    row['stopped'] = first_stop(product, {'package': manifest['core']})
            except (ValueError, UnicodeDecodeError):
                row['stopped'] = f'image tool: exit {result.returncode}: {result.stderr.decode(errors="replace").strip()}'
    return rows


def census_table(rows):
    lines = ['| program | variant | engine | reachable bodies / declarations | first-stop construct | first-stop definitions | all reported gap sites |',
             '|---|---|---|---|---|---|---|']
    for row in rows:
        if row['variant'] == 'libraries' or row['built'] and row['passed'] == row['cases']:
            continue
        if 'incomplete' not in row:
            lines.append(f"| {row['program']} | {row['variant']} | {row['engine']} | ? / ? | census unavailable: build tool stopped before JSON | ? | ? |")
            continue
        counts = dict(row.get('census', {}))
        if not counts and row['checked']:
            counts = {'no checker stops': 0}
        all_sites = row['gap_sites'] if 'gap_sites' in row else gap_sites(row.get('incomplete', []), first_stops=row.get('first_stops', []))
        for gap in [*row.get('incomplete', []), *all_sites]:
            counts.setdefault(gap['message'], 0)
        for construct, count in sorted(counts.items()):
            sites = gap_sites([g for g in row.get('incomplete', []) if g['message'] == construct],
                              first_stops=[g for g in row.get('first_stops', []) if g['construct'] == construct])
            construct = construct.replace('|', r'\|').replace('\n', ' ')
            lines.append(f"| {row['program']} | {row['variant']} | {row['engine']} | "
                         f"{row.get('reachable_bodies', '?')} / {row.get('reachable_declarations', '?')} | {construct} | {count} | {len(sites)} |")
    return '\n'.join(lines)


def gap_table(rows):
    lines = ['| program | variant | engine | ' + ' | '.join(CHECK_SLICES) + ' | distinct gap sites | refusal sites |',
             '|---|---|---|' + '---|' * (len(CHECK_SLICES) + 2)]
    for row in rows:
        if row['variant'] == 'libraries':
            continue
        if 'incomplete' not in row:
            lines.append(f"| {row['program']} | {row['variant']} | {row['engine']} | " + ' | '.join(['?'] * (len(CHECK_SLICES) + 2)) + ' |')
            continue
        sites = row['gap_sites'] if 'gap_sites' in row else gap_sites(row.get('incomplete', []), first_stops=row.get('first_stops', []))
        counts = [sum(s['slice'] == owner for s in sites) for owner in CHECK_SLICES]
        def source_key(site):
            return (site['module'], site['span']['start'], site['span']['end'])
        total = len({source_key(s['primary']) for s in sites})
        refusals = len({source_key(d['primary']) for d in row.get('diagnostics', [])})
        lines.append(f"| {row['program']} | {row['variant']} | {row['engine']} | " +
                     ' | '.join(map(str, [*counts, total, refusals])) + ' |')
    return '\n'.join(lines)


def report(rows):
    return (table(rows) + '\n\nReachable closure census. Each definition contributes its first stop; '
            'the second count includes every reported gap span for that construct. Completed bodies follow emitted calls, '
            'while incomplete bodies follow resolved source names.\n\n' + census_table(rows) +
            '\n\nDistinct reported gap sites by checker slice. A1 includes values, cleanup and resource literals. '
            'Counts include checker gaps and reported missing-instance bodies; they deduplicate module and byte span within each slice; totals deduplicate across slices. '
            'Refusals are separate from implementation gaps. The checker suppresses repeated constructs within a body '
            'and some early stops prevent later checks, so these are observed lower bounds, not a count of unseen constructs.\n\n' +
            gap_table(rows))


def table(rows):
    lines = ['| program | variant | engine | checked | built | passed | first failure / stop |',
             '|---|---|---|---|---|---|---|']
    for r in rows:
        reason = ((r['first_failure'] + ': ') if r['first_failure'] else '') + (r['stopped'] or '')
        reason = reason.replace('|', r'\|').replace('\n', ' ')
        lines.append(f"| {r['program']} | {r['variant']} | {r['engine']} | {'yes' if r['checked'] else 'no'} | "
                     f"{'yes' if r['built'] else 'no'} | {r['passed']}/{r['cases']} | {reason} |")
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'), required='HIRSEL_CORE_RT_ARCHIVE' not in os.environ)
    parser.add_argument('--backend', choices=['cranelift', 'llvm', 'both'], default='both')
    parser.add_argument('--manifest', type=Path, default=ROOT / 'core/programs/m1/manifest.json')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--store', type=Path)
    parser.add_argument('--cc', type=Path, default=(Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang') if 'LLVM_SYS_231_PREFIX' in os.environ else None)
    parser.add_argument('--only', nargs='+')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    engines = ['cranelift', 'llvm'] if args.backend == 'both' else [args.backend]
    with tempfile.TemporaryDirectory(prefix='hirsel-m1-store-') as store:
        rows = run(args.compiler.resolve(), args.runtime.resolve(), engines, args.manifest.resolve(),
                   args.root.resolve(), args.store.resolve() if args.store else Path(store),
                   args.cc.resolve() if args.cc else None, args.only)
    print(report(rows))
    if args.report:
        args.report.write_text(json.dumps(rows, indent=2) + '\n')
        args.report.with_suffix('.md').write_text(report(rows) + '\n')
    return int(any(not r['built'] or r['passed'] != r['cases'] for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
