#!/usr/bin/env python3
"""Guard native compiler cost on M1, std and published-program requests.

One native measurement path: setarch -R pins address layout, perf counts the
compiler from exec through its answer, then an acknowledged disable excludes
optional time-metrics formatting and writing. Startup and publication remain
counted. No /usr/bin/time process is counted. Wall and child peak RSS come from
the capped harness. Native residual is 12 instructions; proposed ceilings keep
both the existing 2% allowance and at least that absolute residual. The
finished budget remains 60000.
"""
import argparse
import copy
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
from measure import source_lines, sample

# Frozen, absolute allowances owned by core-check-index-912. They never scale
# with source growth and never apply to the fixed finished-process budget.
ANCHOR_RESIDUAL = {'m1': 3371157, 'std': 973518}
PROCESS_RESIDUAL = 12
COMPLETE = dict(complete_bodies=1, bodies=1,
                missing_check_lower_per_line=0, additional_publication_per_line=0)


def margin(value):
    return math.ceil(value * 1.02 / 100) * 100



def process_instructions(path):
    rows = [line.split(';') for line in path.read_text().splitlines() if not line.startswith('#')]
    values = [row[0].strip() for row in rows if len(row) > 2 and row[2].strip() == 'instructions:u']
    if len(values) != 1 or not values[0].isdigit() or int(values[0]) <= 0:
        raise ValueError('process instruction counter produced no complete sample')
    return int(values[0])


def check_ceiling(metrics, lines, ceiling, process, anchor_residual=0):
    if metrics.get('counter_refusal'):
        raise ValueError(metrics['counter_refusal'])
    stages = {s['name']: s['instructions'] for s in metrics['stages']}
    lower = stages.get('check/lower')
    total = metrics.get('instructions')
    if lower is None or total is None:
        raise ValueError('instruction counter produced no complete sample')
    if lines <= 0:
        raise ValueError('corpus contains no code lines')
    if process < total:
        raise ValueError('process counter does not cover the driver interval')
    if anchor_residual < 0:
        raise ValueError('negative recorded anchor residual')
    failures = []
    for name, instructions, maximum in [
        ('check/lower', lower, ceiling.get('check_lower_per_line', float('inf'))),
        ('driver request', total, ceiling.get('whole_request_per_line', float('inf'))),
        ('whole process', process, ceiling.get('whole_process_per_line', float('inf'))),
    ]:
        adjusted = instructions - anchor_residual
        if adjusted > maximum * lines:
            context = ' beyond the named anchor residual' if anchor_residual else ''
            failures.append(f'{name}: {adjusted / lines:.2f} instructions/line{context} exceeds {maximum}')
    return lower / lines, total / lines, failures



def completion_errors(product, projection):
    if (projection['complete_bodies'] == projection['bodies']
            and (product['diagnostics'] or product['incomplete'])):
        return ['recorded finished corpus no longer checks every body to the end']
    return []


def finished_forecast(process, lines, projection, budget):
    # Keep the fixed language budget independent of the current-work ceilings.
    # The missing-work terms come from the recorded construct/payload profile;
    # startup and all current work always use this invocation's process counter.
    body = projection['missing_check_lower_per_line']
    publication = projection['additional_publication_per_line']
    if body < 0 or publication < 0:
        raise ValueError('completion forecast has negative missing work')
    forecast = process / lines + body + publication
    failures = []
    if forecast > budget:
        failures.append(f'finished whole process forecast: {forecast:.2f} instructions/line exceeds budget {budget}')
    return forecast, failures


def sample_sources(compiler, output, profile_front=False):
    import m1
    manifest = m1.load_manifest(ROOT / 'core/programs/m1/manifest.json')
    manifest.pop('entries', None)
    for package in manifest['packages']:
        for source in package['sources']:
            source['file'] = str((ROOT / source['file']).resolve())
    for name in ['m1', 'std']:
        inputs = copy.deepcopy(manifest)
        if name == 'std':
            inputs['packages'] = [p for p in inputs['packages'] if p['package']['name'] in ['core', 'std']]
        directory = output / name
        directory.mkdir()
        (directory / 'inputs.json').write_text(json.dumps(inputs))
        files = {Path(s['file']) for p in inputs['packages'] for s in p['sources']}
        lines = sum(source_lines(p) for p in sorted(files))
        (directory / 'code-lines.txt').write_text(str(lines) + '\n')
        env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1'}
        admit()
        command = [str(compiler), 'check',
                   '--inputs', str(directory / 'inputs.json'), '--store', str(directory / 'store'),
                   '--metrics', str(directory / 'metrics.json'), '--json',
                   *(['--profile-front'] if profile_front else [])]
        observed = sample(command, directory / 'process', compiler_only=True, env=env)
        for source, target in [('stdout', 'result.json'), ('stderr', 'stderr.log'), ('perf.csv', 'process-perf.txt')]:
            (directory / target).write_bytes((directory / 'process' / source).read_bytes())
        accepted_statuses = ('ok', 'exit:1', 'exit:2')
        # Until the checker is complete, benchmark corpora legitimately contain GAPs.
        # A tool crash or missing counter never passes as a cheap check.
        if observed['status'] not in accepted_statuses:
            raise ValueError(f'{name}: driver exited {observed["status"]}')
        product = json.loads((directory / 'result.json').read_text())
        if 'incomplete' not in product or 'diagnostics' not in product:
            raise ValueError(f'{name}: driver did not return a check result')
        metrics = json.loads((directory / 'metrics.json').read_text())
        process = observed['instructions']
        yield name, lines, product, metrics, process


def run(compiler, output, ceilings, request_compiler=None, report_only=False):
    failures, observations, proposed, floors = [], {}, {}, {}
    for name, lines, product, metrics, process in sample_sources(compiler, output):
        ceiling = ceilings['corpora'].get(name, {})
        residual = ANCHOR_RESIDUAL.get(name, 0)
        lower, total, errors = check_ceiling(metrics, lines, ceiling, process, residual)
        if residual:
            print(f'{name}: KNOWN EXCESS core-check-index-912: {residual} absolute anchor instructions; budget uses the full process')
        print(f'{name}: {lines} code lines; check/lower {lower:.2f}; driver request {total:.2f}; whole process {process / lines:.2f} instructions/line')
        projection = COMPLETE
        completion = completion_errors(product, projection)
        failures.extend(f'{name}: {error}' for error in completion)
        forecast, budget_errors = finished_forecast(
            process, lines, projection, ceilings['finished_budget_per_line'])
        print(f"{name}: finished whole process forecast {forecast:.2f}; "
              f"budget {ceilings['finished_budget_per_line']} instructions/line; "
              f"current corpus complete: {not (product['diagnostics'] or product['incomplete'])}")
        failures.extend(f'{name}: {e}' for e in errors + budget_errors)
        raw = {stage['name']: stage['instructions'] for stage in metrics['stages']}
        values = dict(check_lower_per_line=(raw['check/lower'] - residual) / lines,
                      whole_request_per_line=(metrics['instructions'] - residual) / lines,
                      whole_process_per_line=(process - residual) / lines)
        observations[name] = dict(code_lines=lines, process=process, metrics=metrics,
                                  diagnostics=product['diagnostics'], incomplete=product['incomplete'])
        if not completion:
            floors[name] = {key: max(0, value) + PROCESS_RESIDUAL / lines for key, value in values.items()}
            proposed[name] = {key: max(margin(max(0, value)), floors[name][key]) for key, value in values.items()}
    if request_compiler is not None:
        errors, observed, limits = run_product(request_compiler, output / 'product', ceilings)
        failures.extend(errors)
        observations['product'], proposed['product'] = observed, limits
        floors['product'] = {'fixed_' + mode + '_instructions': value + PROCESS_RESIDUAL for mode, value in observed['fixed'].items()}
        floors['product']['entries'] = {row['entry']: dict(marginal_per_line=row['marginal'] + 2 * PROCESS_RESIDUAL / row['own_lines']) for row in observed['entries']}
    print('COST_REPORT ' + json.dumps(dict(target='core-check-cost', findings=failures, observations=observations, ceilings=proposed, floors=floors)), flush=True)
    if failures:
        message = '\n'.join(failures) + '\n' + ceilings['policy']
        if report_only:
            print(message)
        else:
            raise ValueError(message)



def product_ceiling(process, fixed, lines, ceiling, budget):
    if fixed <= 0 or lines <= 0 or process < fixed:
        raise ValueError('invalid fixed/marginal product sample')
    marginal = (process - fixed) / lines
    failures = []
    if marginal > ceiling:
        failures.append(f'marginal {marginal:.2f} exceeds observation ceiling {ceiling}')
    if marginal > budget:
        failures.append(f'marginal {marginal:.2f} exceeds budget {budget}')
    return marginal, failures


def admit():
    while os.getloadavg()[0] > 32:
        time.sleep(5)


def product_sample(compiler, inputs, entry, mode, directory, cache):
    from measure import sample
    (directory.parent).mkdir(parents=True, exist_ok=True)
    manifest = directory.with_suffix('.inputs.json')
    manifest.write_text(json.dumps(inputs))
    admit()
    observed = sample([str(compiler), str(manifest), entry,
                      str(directory / 'store'), str(cache), mode,
                      str(directory / 'metrics.json')], directory, compiler_only=True)
    if observed['status'] != 'ok':
        raise ValueError(f'{directory.name}: {observed["status"]}')
    result = json.loads((directory / 'stdout').read_text())
    expected = sum(len(p['sources']) for p in inputs['packages']
                   if p['package']['name'] not in ['core', 'std'])
    if not result['accepted'] or result['files'] != expected:
        raise ValueError(f'{directory.name}: product did not check exactly the program sources: {result}')
    if result['library_bodies_after_check'] != 0:
        raise ValueError('check-only request unexpectedly loaded library bodies')
    return observed['instructions']


def run_product(compiler, directory, ceilings):
    import m1
    os.environ['HIRSEL_MEMORY_LIMIT'] = '2G'
    os.environ['HIRSEL_JOBS'] = '1'
    directory.mkdir()
    manifest = m1.load_manifest(ROOT / 'core/programs/m1/manifest.json')
    inputs = m1.inputs_for(manifest, manifest['entries'][0], ROOT)
    cache = directory / 'packages'
    prepare = directory / 'prepare.json'
    prepare.write_text(json.dumps(inputs))
    admit()
    env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1'}
    with (directory / 'prepare.log').open('w') as log:
        subprocess.run([str(compiler), 'produce', str(cache), str(prepare)],
                       cwd=ROOT, env=env, stdout=log, stderr=log, check=True)
    source = directory / 'main.hirsel'
    source.write_text('pub fn main() -> Result[(), Exit] { Ok(()) }\n')
    empty = copy.deepcopy(inputs)
    empty['packages'] = [p for p in empty['packages']
                         if p['package']['name'] in ['core', 'std']]
    empty['packages'].append({'package': {'name': 'empty', 'version': '1'},
        'dependencies': [p['package'] for p in empty['packages']],
        'sources': [{'file': str(source), 'modules': ['main'], 'signatures': False}]})
    limits = ceilings['corpora'].get('product', {})
    failures = []
    fixed = {}
    for mode in ['std', 'core']:
        request = copy.deepcopy(empty)
        if mode == 'core':
            request['packages'] = [p for p in request['packages'] if p['package']['name'] != 'std']
            request['packages'][-1]['dependencies'] = [request['core']]
        fixed[mode] = product_sample(compiler, request, 'empty@1/main/main', mode,
                                     directory / ('fixed-' + mode), cache)
        maximum = limits.get('fixed_' + mode + '_instructions', float('inf'))
        print(f'product: fixed {mode} {fixed[mode]} instructions; ceiling {maximum}')
        if fixed[mode] > maximum:
            failures.append(f'product fixed {mode}: {fixed[mode]} exceeds {maximum}')
    rows = []
    for entry in manifest['entries']:
        name = entry['workload'] + '-' + entry['variant']
        request = m1.inputs_for(manifest, entry, ROOT)
        own = {Path(s['file']) for p in request['packages']
               if p['package']['name'] not in ['core', 'std'] for s in p['sources']}
        lines = sum(source_lines(p) for p in sorted(own))
        path = entry['package']['name'] + '@1/' + entry['module'] + '/' + entry['function']
        process = product_sample(compiler, request, path, 'std', directory / name, cache)
        marginal, errors = product_ceiling(process, fixed['std'], lines,
            limits.get('entries', {}).get(name, {}).get('marginal_per_line', float('inf')), ceilings['finished_budget_per_line'])
        rows.append(dict(entry=name, own_lines=lines, process=process,
                         fixed=fixed['std'], marginal=marginal))
        print(f'product {name}: {lines} own lines; process {process}; fixed {fixed["std"]}; marginal {marginal:.2f}')
        failures.extend(f'product {name}: {e}' for e in errors)
    (directory / 'results.json').write_text(json.dumps(dict(fixed=fixed, entries=rows), indent=2) + '\n')
    proposals = {'fixed_' + mode + '_instructions': max(math.ceil(value * 1.02), value + PROCESS_RESIDUAL) for mode, value in fixed.items()}
    proposals['entries'] = {row['entry']: dict(marginal_per_line=margin(row['marginal'])) for row in rows}
    return failures, dict(fixed=fixed, entries=rows), proposals


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--request-compiler', type=Path, required=True)
    parser.add_argument('--out', type=Path, help='new evidence directory, otherwise a temporary directory')
    parser.add_argument('--ceilings', type=Path, default=Path(__file__).with_name('cost-ceilings.json'))
    parser.add_argument('--report-only', action='store_true', help='report ceiling findings; tool errors still fail')
    args = parser.parse_args()
    ceilings = json.loads(args.ceilings.read_text())
    ceilings['policy'] = ceilings.get('policy', 'Compare observations against the committed ceilings.')
    if args.out:
        args.out.mkdir(parents=True, exist_ok=False)
        run(args.compiler.resolve(), args.out.resolve(), ceilings, args.request_compiler.resolve(), args.report_only)
    else:
        with tempfile.TemporaryDirectory(prefix='hirsel-check-cost-') as out:
            run(args.compiler.resolve(), Path(out), ceilings, args.request_compiler.resolve(), args.report_only)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
