#!/usr/bin/env python3
"""Independent front-stage land gate. Nested image costs never sum into phase totals."""
import argparse
import json
import math
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
          'body keys', 'image read', 'image digest')
COUNTS = ('records_opened', 'bytes_read', 'digests', 'digest_bytes')


def front(metrics):
    if metrics.get('counter_refusal') or metrics['front_image_access'].get('counter_refusal'):
        raise ValueError('front instruction counter refused: ' + str(metrics.get('counter_refusal') or metrics['front_image_access']['counter_refusal']))
    if metrics['source_bytes'] <= 0 or metrics['tokens'] <= 0:
        raise ValueError('front has no source bytes or semantic tokens')
    stages = {s['name']: s['instructions'] for s in metrics['stages']}
    sub = {s['name']: s['instructions'] for s in metrics['substages']}
    if any(stages.get(k) is None for k in ('parse', 'front/input counts', 'index', 'resolve', 'declarations/layout')):
        raise ValueError('missing front stage counter')
    if any(sub.get(k) is None for k in ('body keys/new', 'body keys/for_body')):
        raise ValueError('missing body key substage counter')
    image = metrics['front_image_access']
    if any(not isinstance(image[k], int) or image[k] < 0 for k in (*COUNTS, 'read_instructions', 'digest_instructions')):
        raise ValueError('invalid image read counter')
    if image['digest_instructions'] > image['read_instructions']:
        raise ValueError('image digest lies outside image read interval')
    costs = {k: stages[k] for k in ('parse', 'index', 'resolve')}
    costs['input counts'] = stages['front/input counts']
    costs['declarations'] = stages['declarations/layout'] - sub['body keys/new']
    costs.update({k: sub[k] for k in ('body keys/new', 'body keys/for_body')})
    costs['body keys'] = costs['body keys/new'] + costs['body keys/for_body']
    costs['image read'] = image['read_instructions']
    costs['image digest'] = image['digest_instructions']
    if min(costs.values()) < 0:
        raise ValueError('negative front stage')
    return dict(source_bytes=metrics['source_bytes'], tokens=metrics['tokens'], costs=costs,
                image_access=image)


def compare(row, ceiling):
    failures = []
    for name in STAGES:
        value = row['costs'][name] / row['tokens']
        maximum = ceiling['instructions_per_token'][name]
        if value > maximum:
            failures.append(f'{name}: {value:.6f} instructions/token exceeds {maximum}')
    for name in COUNTS:
        value = row['image_access'][name] / row['tokens']
        maximum = ceiling['image_per_token'][name]
        if value > maximum:
            failures.append(f'image {name}: {value:.6f}/token exceeds {maximum}')
    return failures


def baseline(row):
    # 2%, rounded up to 100 instructions (counts to whole units), before
    # conversion to a token-normalised ceiling. Never absorb another stage's red.
    return dict(source_bytes=row['source_bytes'], tokens=row['tokens'], observation=row,
                instructions_per_token={k: math.ceil(v * 1.02 / 100) * 100 / row['tokens']
                                        for k, v in row['costs'].items()},
                image_per_token={k: math.ceil(row['image_access'][k] * 1.02) / row['tokens'] for k in COUNTS})


def print_row(name, row):
    print(f'{name}: {row["source_bytes"]} source bytes, {row["tokens"]} semantic tokens (trivia/EOF excluded)')
    for stage in STAGES:
        cost = row['costs'][stage]
        print(f'  {stage}: {cost} instructions; {cost / row["source_bytes"]:.3f}/byte; {cost / row["tokens"]:.3f}/token')
    print('  image work: ' + json.dumps({k: row['image_access'][k] for k in COUNTS}))


def suite_edit(compiler, runtime, out):
    # 948's exact image-mode manifest and native argv, shared with its harness.
    request_cost.manifests(ROOT, out)
    snapshot = out / 'base.snapshot'
    store = snapshot
    env = os.environ | {'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1',
                        'XDG_CACHE_HOME': str(out / 'xdg-cache'), 'XDG_DATA_HOME': str(out / 'xdg-data')}
    for tag, manifest in [('base', 'suite-inputs.json'), ('edit', 'suite-edited-inputs.json')]:
        if tag == 'edit':
            store = out / 'store'
            shutil.copytree(snapshot, store, symlinks=True)
        GUARD['admit']()
        command = request_cost.command(compiler, runtime, out, out / manifest,
                                       'langarena@1/suite/main', store, tag)
        if tag == 'edit':
            command = ['perf', 'stat', '--no-big-num', '-x', ';', '-e', 'instructions:u',
                       '-o', str(out / 'process-perf.txt'), '--', *command]
        with (out / (tag + '.log')).open('w') as log:
            result = subprocess.run(['taskset', '-c', '16', str(ROOT / 'tools/capped'), *command],
                                    cwd=ROOT, env=env, stdout=log, stderr=log)
        if result.returncode:
            raise ValueError(f'suite {tag}: exit {result.returncode}; {out / (tag + ".log")}')
    result = json.loads((out / 'edit.log').read_text().splitlines()[0])
    # Capped summaries use stderr and the compiler emits one JSON line.
    if not result.get('accepted') or result['rechecked'] != 1:
        raise ValueError(f'suite edit must check exactly one body: {result}')
    return front(json.loads((out / 'edit.metrics.json').read_text()))


def run(compiler, runtime, out, ceilings=None):
    rows = {}
    for name, lines, product, metrics, process in GUARD['sample_sources'](compiler, out):
        if product['diagnostics'] or product['incomplete'] or not product['accepted']:
            raise ValueError(name + ': source corpus did not check to the end')
        rows[name] = front(metrics)
    rows['suite-edit'] = suite_edit(compiler, runtime, out / 'suite-edit')
    failures = []
    for name, row in rows.items():
        print_row(name, row)
        if ceilings:
            failures.extend(name + ': ' + e for e in compare(row, ceilings['corpora'][name]))
    (out / 'front-results.json').write_text(json.dumps(rows, indent=2) + '\n')
    if ceilings:
        print('Suite F targets (instructions, separate from current ceilings): '
              + json.dumps(ceilings['suite_targets_instructions']))
    if failures:
        raise ValueError('\n'.join(failures))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--ceilings', type=Path, default=Path(__file__).with_name('front-cost-ceilings.json'))
    parser.add_argument('--record', action='store_true', help='initial same-binary baseline; later updates must only lower saved stage limits')
    args = parser.parse_args()
    runtime = args.runtime
    if runtime is None:
        runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVES']) / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
    compiler, runtime = args.compiler.resolve(), runtime.resolve()
    limits = None if args.record else json.loads(args.ceilings.read_text())
    def execute(out):
        out.mkdir(parents=True, exist_ok=True)
        rows = run(compiler, runtime, out, limits)
        if args.record:
            args.ceilings.write_text(json.dumps(dict(
                revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                policy='Each F land lowers only its measured stage. Initial margin 2%, rounded up to 100 instructions. Budget targets remain separate. M1 legacy check/lower unchanged.',
                nested='Body keys remain in legacy declarations/check stages; image read includes digest and overlaps the consuming phase. Do not sum nested rows.',
                suite_targets_instructions=dict(parse=8000000, resolve=7000000, declarations=5000000, index=600000, **{'body keys':8000000}),
                corpora={name: baseline(row) for name, row in rows.items()},
                history=[dict(reason='Initial exact front counters, 2% stage margin; no legacy ceilings or fixed 60000 budget changed.')]), indent=2) + '\n')
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
