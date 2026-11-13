#!/usr/bin/env python3
"""Fail a release check/lower cost regression on the fixed M1 and core/std corpora."""
import argparse
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
from measure import source_lines


def process_instructions(path):
    rows = [line.split(';') for line in path.read_text().splitlines() if not line.startswith('#')]
    values = [row[0].strip() for row in rows if len(row) > 2 and row[2].strip() == 'instructions:u']
    if len(values) != 1 or not values[0].isdigit() or int(values[0]) <= 0:
        raise ValueError('process instruction counter produced no complete sample')
    return int(values[0])


def check_ceiling(metrics, lines, ceiling, process):
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
    failures = []
    for name, instructions, maximum in [
        ('check/lower', lower, ceiling['check_lower_per_line']),
        ('driver request', total, ceiling['whole_request_per_line']),
        ('whole process', process, ceiling['whole_process_per_line']),
    ]:
        if instructions > maximum * lines:
            failures.append(f'{name}: {instructions / lines:.2f} instructions/line exceeds {maximum}')
    return lower / lines, total / lines, failures



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


def run(compiler, output, ceilings, request_compiler=None):
    manifest = json.loads((ROOT / 'core/programs/m1/manifest.json').read_text())
    manifest.pop('entries', None)
    for package in manifest['packages']:
        for source in package['sources']:
            source['file'] = str((ROOT / source['file']).resolve())
    failures = []
    for name in ['m1', 'std']:
        inputs = copy.deepcopy(manifest)
        if name == 'std':
            inputs['packages'] = [p for p in inputs['packages'] if p['package']['name'] in ['core', 'std']]
        directory = output / name
        directory.mkdir()
        (directory / 'inputs.json').write_text(json.dumps(inputs))
        files = {Path(s['file']) for p in inputs['packages'] for s in p['sources']}
        lines = sum(source_lines(p) for p in sorted(files))
        env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1'}
        admit()
        command = ['taskset', '-c', '16', str(ROOT / 'tools/capped'),
                   'perf', 'stat', '--no-big-num', '-x', ';', '-e', 'instructions:u',
                   '-o', str(directory / 'process-perf.txt'), '--', str(compiler), 'check',
                   '--inputs', str(directory / 'inputs.json'), '--store', str(directory / 'store'),
                   '--metrics', str(directory / 'metrics.json'), '--json']
        with (directory / 'result.json').open('w') as stdout, (directory / 'stderr.log').open('w') as stderr:
            result = subprocess.run(command, cwd=ROOT, env=env, stdout=stdout, stderr=stderr)
        # Until the checker is complete, benchmark corpora legitimately contain GAPs.
        # A tool crash or missing counter never passes as a cheap check.
        if result.returncode not in (0, 1, 2):
            raise ValueError(f'{name}: driver exited {result.returncode}')
        product = json.loads((directory / 'result.json').read_text())
        if 'incomplete' not in product or 'diagnostics' not in product:
            raise ValueError(f'{name}: driver did not return a check result')
        metrics = json.loads((directory / 'metrics.json').read_text())
        process = process_instructions(directory / 'process-perf.txt')
        ceiling = ceilings['corpora'][name]
        if lines != ceiling['code_lines']:
            print(f"{name}: baseline {ceiling['code_lines']} code lines; current {lines}; "
                  "costs use the deterministic current count")
        lower, total, errors = check_ceiling(metrics, lines, ceiling, process)
        print(f'{name}: {lines} code lines; check/lower {lower:.2f}; driver request {total:.2f}; whole process {process / lines:.2f} instructions/line')
        projection = ceilings['completion_forecast'][name]
        forecast, budget_errors = finished_forecast(
            process, lines, projection, ceilings['finished_budget_per_line'])
        print(f"{name}: finished whole process forecast {forecast:.2f}; "
              f"budget {ceilings['finished_budget_per_line']} instructions/line; "
              f"recorded completion {projection['complete_bodies']}/{projection['bodies']} bodies "
              f"at {ceilings['forecast_revision']}")
        failures.extend(f'{name}: {e}' for e in errors + budget_errors)
    if request_compiler is not None:
        failures.extend(run_product(request_compiler, output / 'product', ceilings))
    if failures:
        raise ValueError('\n'.join(failures) + '\n' + ceilings['regression_policy'])



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
    observed = sample(['taskset', '-c', '16', str(compiler), str(manifest), entry,
                      str(directory / 'store'), str(cache), mode,
                      str(directory / 'metrics.json')], directory)
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
    manifest = json.loads((ROOT / 'core/programs/m1/manifest.json').read_text())
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
    limits = ceilings['corpora']['product']
    failures = []
    fixed = {}
    for mode in ['std', 'core']:
        request = copy.deepcopy(empty)
        if mode == 'core':
            request['packages'] = [p for p in request['packages'] if p['package']['name'] != 'std']
            request['packages'][-1]['dependencies'] = [request['core']]
        fixed[mode] = product_sample(compiler, request, 'empty@1/main/main', mode,
                                     directory / ('fixed-' + mode), cache)
        maximum = limits['fixed_' + mode + '_instructions']
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
            limits['entries'][name]['marginal_per_line'], ceilings['finished_budget_per_line'])
        rows.append(dict(entry=name, own_lines=lines, process=process,
                         fixed=fixed['std'], marginal=marginal))
        print(f'product {name}: {lines} own lines; process {process}; fixed {fixed["std"]}; marginal {marginal:.2f}')
        failures.extend(f'product {name}: {e}' for e in errors)
    (directory / 'results.json').write_text(json.dumps(dict(fixed=fixed, entries=rows), indent=2) + '\n')
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--request-compiler', type=Path, required=True)
    parser.add_argument('--out', type=Path, help='new evidence directory, otherwise a temporary directory')
    parser.add_argument('--ceilings', type=Path, default=Path(__file__).with_name('cost-ceilings.json'))
    args = parser.parse_args()
    ceilings = json.loads(args.ceilings.read_text())
    if args.out:
        args.out.mkdir(parents=True, exist_ok=False)
        run(args.compiler.resolve(), args.out.resolve(), ceilings, args.request_compiler.resolve())
    else:
        with tempfile.TemporaryDirectory(prefix='hirsel-check-cost-') as out:
            run(args.compiler.resolve(), Path(out), ceilings, args.request_compiler.resolve())


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
