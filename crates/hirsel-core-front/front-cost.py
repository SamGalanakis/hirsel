#!/usr/bin/env python3
"""Measure front-stage costs against their ceilings. Nested image costs never sum into phase totals."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
import request_cost
GUARD = runpy.run_path(str(Path(__file__).with_name('check-cost.py')))
STAGES = ('parse', 'input counts', 'index', 'resolve', 'declarations', 'body keys/new', 'body keys/for_body',
          'body keys', 'image index', 'image read', 'image digest')
COUNTS = ('records_opened', 'bytes_read', 'digests', 'digest_bytes', 'index_calls', 'key_probes', 'row_demands', 'nodes_decoded')


def front(metrics):
    if metrics.get('counter_refusal') or metrics['front_image_access'].get('counter_refusal'):
        raise ValueError('front instruction counter refused: ' + str(metrics.get('counter_refusal') or metrics['front_image_access']['counter_refusal']))
    if metrics['source_bytes'] <= 0:
        raise ValueError('front has no source bytes')
    tokens = GUARD['semantic_tokens'](metrics)
    stages = {s['name']: s['instructions'] for s in metrics['stages']}
    sub = {s['name']: s['instructions'] for s in metrics['substages']}
    if any(stages.get(k) is None for k in ('parse', 'front/input counts', 'index', 'resolve', 'declarations/layout')):
        raise ValueError('missing front stage counter')
    if any(sub.get(k) is None for k in ('body keys/new', 'body keys/for_body')):
        raise ValueError('missing body key substage counter')
    image = metrics['front_image_access']
    if any(not isinstance(image[k], int) or image[k] < 0 for k in (*COUNTS, 'index_instructions', 'read_instructions', 'digest_instructions')):
        raise ValueError('invalid image read counter')
    if image['digest_instructions'] > image['read_instructions']:
        raise ValueError('image digest lies outside image read interval')
    costs = {k: stages[k] for k in ('parse', 'index', 'resolve')}
    costs['input counts'] = stages['front/input counts']
    costs['declarations'] = stages['declarations/layout'] - sub['body keys/new']
    costs.update({k: sub[k] for k in ('body keys/new', 'body keys/for_body')})
    costs['body keys'] = costs['body keys/new'] + costs['body keys/for_body']
    costs['image index'] = image['index_instructions']
    costs['image read'] = image['read_instructions']
    costs['image digest'] = image['digest_instructions']
    if min(costs.values()) < 0:
        raise ValueError('negative front stage')
    return dict(source_bytes=metrics['source_bytes'], tokens=tokens, costs=costs,
                image_access=image)


def compare(row, ceiling):
    failures = []
    for name in STAGES:
        value = row['costs'][name] / row['tokens']
        maximum = ceiling.get('instructions_per_token', {}).get(name, float('inf'))
        if value > maximum:
            failures.append(f'{name}: {value:.6f} instructions/token exceeds {maximum}')
    for name in COUNTS:
        value = row['image_access'][name] / row['tokens']
        maximum = ceiling.get('image_per_token', {}).get(name, float('inf'))
        if value > maximum:
            failures.append(f'image {name}: {value:.6f}/token exceeds {maximum}')
    return failures


def baseline(row):
    # Instructions use a 2% margin rounded up to 100 instructions. Counts are
    # whole events, so the normalized ceiling must leave one whole event above
    # the observation when that is larger than the percentage margin. Keep the
    # resulting count fractional until comparison; rounding it up would admit
    # two events for small observations and hide a two-event regression.
    return dict(instructions_per_token={k: GUARD['instruction_rate_ceiling'](v, row['tokens'])
                                        for k, v in row['costs'].items()},
                image_per_token={k: max(row['image_access'][k] * 1.02,
                                       row['image_access'][k] + 1) / row['tokens']
                                 for k in COUNTS})


def print_row(name, row):
    print(f'{name}: {row["source_bytes"]} source bytes, {row["tokens"]} semantic tokens (trivia/EOF excluded)')
    for stage in STAGES:
        cost = row['costs'][stage]
        print(f'  {stage}: {cost} instructions; {cost / row["source_bytes"]:.3f}/byte; {cost / row["tokens"]:.3f}/token')
    print('  image work: ' + json.dumps({k: row['image_access'][k] for k in COUNTS}))


def suite_edit(compiler, runtime, out, tools, profile_front=False, prepare=True):
    # 948's exact image-mode manifest and native argv, shared with its harness.
    request_cost.manifests(ROOT, out)
    snapshot = out / 'base.snapshot'
    store = snapshot
    env = os.environ | {'LLVM_SYS_231_PREFIX': tools['llvm'], 'HIRSEL_LINK_KIT': tools['kit'],
                        'LD_LIBRARY_PATH': str(Path(tools['llvm']) / 'lib'), 'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1',
                        'XDG_CACHE_HOME': str(out / 'xdg-cache'), 'XDG_DATA_HOME': str(out / 'xdg-data')}
    requests = [('base', 'suite-inputs.json'), ('edit', 'suite-edited-inputs.json')] if prepare else [('edit', 'suite-edited-inputs.json')]
    for tag, manifest in requests:
        if tag == 'edit':
            store = out / 'store'
            if store.exists():
                shutil.rmtree(store)
            shutil.copytree(snapshot, store, symlinks=True)
        GUARD['admit']()
        command = request_cost.command(compiler, runtime, out, out / manifest,
                                       'langarena@1/suite/main', store, tag,
                                       Path(tools['clang']), profile_front=profile_front)
        if tag == 'edit':
            command = ['perf', 'stat', '--no-big-num', '-x', ';', '-e', 'instructions:u',
                       '-o', str(out / 'process-perf.txt'), '--', *command]
        with (out / (tag + '.log')).open('w') as log:
            result = subprocess.run(['taskset', '-c', '16', str(ROOT / 'tools/capped'), *command],
                                    cwd=ROOT, env=env, stdout=log, stderr=log)
        if result.returncode:
            path = out / (tag + '.log')
            tail = '\n'.join(path.read_text(errors='replace').splitlines()[-40:])
            raise ValueError(f'suite {tag}: exit {result.returncode}; {path}\n{tail}')
        request_cost.stamp_metrics(out / (tag + '.metrics.json'), tools)
    result = json.loads((out / 'edit.log').read_text().splitlines()[0])
    # Capped summaries use stderr and the compiler emits one JSON line.
    if not result.get('accepted') or result['rechecked'] != 1:
        raise ValueError(f'suite edit must check exactly one body: {result}')
    metrics = json.loads((out / 'edit.metrics.json').read_text())
    # B2: an unchanged body keeps its recorded key; only the edited body's is derived.
    if metrics['counters']['body_keys'] != 1:
        raise ValueError(f"suite edit must derive exactly one body key: {metrics['counters']['body_keys']}")
    return front(metrics)


def run(compiler, runtime, out, ceilings=None, tools=None, image_profiler=None, report_only=False):
    tools = tools or request_cost.toolchain(ROOT)
    rows = {}
    for name, lines, product, metrics, process in GUARD['sample_sources'](compiler, out):
        if product['diagnostics'] or product['incomplete'] or not product['accepted']:
            raise ValueError(name + ': source corpus did not check to the end')
        rows[name] = front(metrics)
    suite = out / 'suite-edit'
    rows['suite-edit'] = suite_edit(compiler, runtime, suite, tools)
    for name in ['edit.log', 'edit.metrics.json', 'process-perf.txt']:
        (suite / name).rename(suite / ('plain-' + name))
    # Executable identity keys the store. Prime the diagnostic compiler's own
    # snapshot, then compare its two modes without mixing executable identities.
    suite_edit(image_profiler or compiler, runtime, suite, tools, prepare=True)
    control_result = json.loads((suite / 'edit.log').read_text().splitlines()[0])
    for name in ['edit.log', 'edit.metrics.json', 'process-perf.txt']:
        (suite / name).rename(suite / ('image-control-' + name))
    profiled = suite_edit(image_profiler or compiler, runtime, suite, tools, profile_front=True, prepare=False)
    profiled_result = json.loads((suite / 'edit.log').read_text().splitlines()[0])
    if control_result['root'] != profiled_result['root']:
        raise ValueError('image profiling changed the published product root')
    row = rows['suite-edit']
    if (row['source_bytes'], row['tokens']) != (profiled['source_bytes'], profiled['tokens']):
        raise ValueError('image profiling changed the input counts')
    row['costs']['image index'] = profiled['costs']['image index']
    for name in ('index_calls', 'key_probes', 'row_demands', 'nodes_decoded'):
        row['image_access'][name] = profiled['image_access'][name]
    row['profiled_image_access'] = profiled['image_access']
    row['configuration'] = 'Stages/read/digest: production release --metrics; image index/calls: separate diagnostic release --profile-front. Identical manifest/store paths; per-binary base snapshots; diagnostic on/off published roots identical. Never sum modes.'
    failures = []
    for name, row in rows.items():
        row['linker_sha256'] = tools['linker_sha256']
        row['compiler_sha256'] = hashlib.sha256(compiler.read_bytes()).hexdigest()
        if name == 'suite-edit':
            row['image_profiler_sha256'] = hashlib.sha256((image_profiler or compiler).read_bytes()).hexdigest()
        print_row(name, row)
        print('  identities: ' + json.dumps({k: row[k] for k in ('compiler_sha256', 'linker_sha256', 'image_profiler_sha256') if k in row}))
        if ceilings:
            failures.extend(name + ': ' + e for e in compare(row, ceilings['corpora'].get(name, {})))
    (out / 'front-results.json').write_text(json.dumps(rows, indent=2) + '\n')
    if ceilings:
        print('Suite F targets (instructions, separate from current ceilings): '
              + json.dumps(ceilings['suite_targets_instructions']))
    print('COST_REPORT ' + json.dumps(dict(target='core-front-cost', findings=failures, observations=rows,
                                          ceilings={name: baseline(row) for name, row in rows.items()})), flush=True)
    if failures:
        if report_only:
            print('\n'.join(failures))
        else:
            raise ValueError('\n'.join(failures))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path)
    parser.add_argument('--image-profiler', type=Path, required=True)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--ceilings', type=Path, default=Path(__file__).with_name('front-cost-ceilings.json'))
    parser.add_argument('--test-threads', type=int, help=argparse.SUPPRESS)
    parser.add_argument('--buck-tool-inputs', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--report-only', action='store_true', help='report ceiling findings; tool errors still fail')
    args = parser.parse_args()
    runtime = args.runtime
    if runtime is None:
        runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVES']) / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
    compiler, runtime = args.compiler.resolve(), runtime.resolve()
    limits = json.loads(args.ceilings.read_text())
    inputs = dict(llvm=os.environ['LLVM_SYS_231_PREFIX'], kit=os.environ['HIRSEL_LINK_KIT']) if args.buck_tool_inputs else None
    tools = request_cost.toolchain(ROOT, inputs)
    def execute(out):
        out.mkdir(parents=True, exist_ok=True)
        run(compiler, runtime, out, limits, tools, args.image_profiler.resolve(), args.report_only)
    if args.out:
        execute(args.out.resolve())
    else:
        with tempfile.TemporaryDirectory(prefix='hirsel-front-cost-') as temp:
            execute(Path(temp))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError) as e:
        print(e, file=sys.stderr)
        sys.exit(1)
