#!/usr/bin/env python3
"""Run generated containment programs on the evaluator and native builds and judge them.

Design: docs/internal/containment-generator.md. Oracles: O1 (no run stop inside a contained
scope), O2 (outcomes agree across engines, up to the spec's tolerances) and O3a (the
predicted step floor forces OutOfTime).
Example: containment_fuzz.py once --seed 7 --builds evaluator,cranelift-dev \\
    --compiler hirsel --runtime libhirsel_core_rt.a --evaluator hirsel-core-contract-eval --out /tmp/c
         containment_fuzz.py range --seeds 0..200 --jobs 8 ... --out /tmp/c
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import resource
import shutil
import subprocess
import sys
import time

import containment_gen as gen
from contract_exec import Evaluator, library_sources, native_build

ROOT = Path(__file__).resolve().parents[2]
# Each program run is capped, as SEED_TIMEOUT_SECONDS is in tools/sim-campaign; a run that
# reaches the cap is itself a red.
RUN_SECONDS = 300
NATIVE = {'cranelift-dev': ('cranelift', False), 'llvm-dev': ('llvm', False), 'llvm-release': ('llvm', True)}
BUILDS = ('evaluator', *NATIVE)
VACUOUS = 0.8


def manifest(root, source):
    core = {'name': 'core', 'version': '1'}
    package = {'name': 'gen', 'version': '1'}
    return {'core': core, 'packages': [
        {'package': core, 'sources': library_sources(root / 'core/core', True), 'dependencies': []},
        {'package': package, 'sources': [{'file': str(source), 'modules': ['main'], 'signatures': False}],
         'dependencies': [core]}]}


def records(stdout):
    """The tag stream: {scope id: (kind, payload)}."""
    found = {}
    for line in stdout.decode(errors='replace').splitlines():
        words = line.split()
        if len(words) == 4 and words[0] == 'scope':
            found[int(words[1])] = (int(words[2]), int(words[3]))
    return found


def form(program, scope_id):
    scope = program['scopes'][str(scope_id)]
    return '+'.join(sorted(set(scope['productions']))) or 'empty'


# Productions that recurse over a chain: a stop in a scope holding one is keyed by these alone,
# so one engine fault does not split across every mix of the cheap productions beside it.
DEEP = ('clone_chain', 'eq_chain', 'walk_chain')


def stop_form(program, scope_id):
    productions = set(program['scopes'][str(scope_id)]['productions'])
    return '+'.join(sorted(productions & set(DEEP))) or form(program, scope_id)


def forced(scope, size):
    """O3a: L > n, so every build must report out_of_time."""
    least = scope['constant'] + sum(-(-a * size // q) for a, q in scope['terms'])
    return least > scope['bound']


def accepted_forced(scope, record):
    kind, payload = record
    return kind == gen.OUT_OF_TIME or (scope['recursive'] and kind == gen.CAUSED and payload & gen.STACK_BIT)


def red(key, build, entry, size, detail):
    return dict(key=key, build=build, entry=entry, size=size, detail=detail)


def judge_run(program, build, entry, size, outcome, reds):
    """O1 and O3a on one run. `outcome` is (status, stdout, stderr), status None for a timeout."""
    status, stdout, stderr = outcome
    found = records(stdout)
    # A scope prints after it ends, so a stop lies in the first top-level scope without a record.
    scopes = entry_scopes(program, entry)
    last = stop_form(program, next((s for s in scopes if s not in found), scopes[-1]))
    if status is None:
        reds.append(red(f'containment:timeout:{last}', build, entry, size, f'over {RUN_SECONDS} s'))
    elif status == 102 or b'run stop' in stderr:
        name = stderr.decode(errors='replace').strip().splitlines()[-1:] or ['']
        reds.append(red(f'containment:run-stop:{last}', build, entry, size, name[0]))
    elif status < 0 or status > 128:
        reds.append(red(f'containment:signal:{last}', build, entry, size, f'status {status}: {stderr[-400:]!r}'))
    elif status not in (0, 3):
        reds.append(red(f'containment:escaped:{last}', build, entry, size, f'status {status}: {stderr[-400:]!r}'))
    for scope_id, record in found.items():
        scope = program['scopes'][str(scope_id)]
        if record[0] == gen.O3A_FAIL or (forced(scope, size) and not accepted_forced(scope, record)):
            reds.append(red(f'containment:work-floor:{form(program, scope_id)}', build, entry, size,
                            f'scope {scope_id}: n {scope["bound"]}, record {gen.KIND_NAMES.get(record[0])} {record[1]}'))
    if status == 3 and not any(r[0] == gen.O3A_FAIL for r in found.values()):
        reds.append(red(f'containment:escaped:{last}', build, entry, size, 'entry false without an O3a record'))
    return found


def entry_scopes(program, entry):
    return program['entries'][entry]['scopes']


def ancestors(program):
    parent = {}
    for scope_id, scope in program['scopes'].items():
        for child in scope['children']:
            parent[child] = int(scope_id)
    return parent


def tolerated(scope, size, a, b):
    """O2's relation for two records of one scope (design, "O2": each row from spec text)."""
    if a == b:
        return True
    kinds = {a[0], b[0]}
    if any(k == gen.CAUSED and p & gen.STACK_BIT for k, p in (a, b)):
        return True   # FAIL-17: each build fixes its own stack allowance
    if gen.OUT_OF_TIME in kinds and kinds <= {gen.OK, gen.CAUSED, gen.OUT_OF_TIME}:
        return not forced(scope, size)   # MEM-30 Determinism, unless O3a forces out_of_time
    return False


def agree(program, entry, size, reference, other, build, reds, tally):
    """O2 between the evaluator's records and a native build's at one size."""
    parent = ancestors(program)
    differs = set()
    for scope_id in sorted(set(reference) | set(other)):
        scope = program['scopes'][str(scope_id)]
        a, b = reference.get(scope_id), other.get(scope_id)
        above = parent.get(scope_id)
        if a is None or b is None:
            # A record missing on one side follows only from a tolerated difference above it.
            while above is not None and above not in differs:
                above = parent.get(above)
            if above is None:
                reds.append(red(f'containment:disagree:{form(program, scope_id)}', build, entry, size,
                                f'scope {scope_id}: evaluator {a}, {build} {b}'))
            differs.add(scope_id)
            continue
        tally['compared'] += 1
        if a == b:
            tally['exact'] += 1
            continue
        differs.add(scope_id)
        if not tolerated(scope, size, a, b):
            reds.append(red(f'containment:disagree:{form(program, scope_id)}', build, entry, size,
                            f'scope {scope_id}: evaluator {gen.KIND_NAMES.get(a[0])} {a[1]}, '
                            f'{build} {gen.KIND_NAMES.get(b[0])} {b[1]}'))


def run_native(program_path, entry, size):
    try:
        done = subprocess.run([str(program_path), str(entry), str(size)], capture_output=True, timeout=RUN_SECONDS)
        return done.returncode, done.stdout, done.stderr
    except subprocess.TimeoutExpired as expired:
        return None, expired.stdout or b'', expired.stderr or b''


class Quiet:
    """Progress sink for the resident evaluator: a batch prints a summary, not a line per entry."""
    def output(self, stream, chunk):
        pass

    def end_stream(self, stream):
        pass


def run_evaluator(resident, inputs, entry):
    result = resident.evaluate(inputs, f'gen@1/main/ev_{entry}', root_limit=gen.ROOT_LIMIT, progress=Quiet())
    try:
        report = json.loads(result.stdout)
    except ValueError:
        return 'harness', b'', result.stderr
    if report.get('error'):
        return 'harness', b'', report['error'].encode()
    outcome = report.get('outcome', {})
    stdout = bytes(report.get('stdout', []))
    if outcome.get('kind') == 'stopped':
        return 102, stdout, f"run stop: {outcome.get('name')}".encode()
    if outcome.get('kind') == 'returned':
        return (0 if outcome.get('values') == [1] else 3), stdout, b''
    if outcome.get('kind') == 'failed':
        return 101, stdout, json.dumps(outcome).encode()
    return 'harness', stdout, json.dumps(outcome).encode()


def once(args, seed):
    """One batch: one package of gen.ENTRIES entries, compiled once per build."""
    started = time.monotonic()
    work = args.out / 'seeds' / str(seed)
    shutil.rmtree(work, ignore_errors=True)
    package = gen.write_package(gen.generate(seed), work / 'package')
    program = json.loads((package / 'program.json').read_text())
    inputs = work / 'inputs.json'
    inputs.write_text(json.dumps(manifest(args.root, (package / 'main.hirsel').resolve())))
    reds, tally, costs = [], dict(compared=0, exact=0), {}
    small = {}
    if 'evaluator' in args.builds:
        clock = time.monotonic()
        with Evaluator(args.evaluator) as resident:
            for entry in program['entries']:
                outcome = run_evaluator(resident, inputs, entry['index'])
                if outcome[0] == 'harness':
                    reds.append(red('containment:generator:evaluator', 'evaluator', entry['index'],
                                    entry['small_size'], outcome[2].decode(errors='replace')[:600]))
                    break
                small[entry['index']] = judge_run(program, 'evaluator', entry['index'], entry['small_size'], outcome, reds)
        costs['evaluator'] = round(time.monotonic() - clock, 2)
    for build in [b for b in args.builds if b in NATIVE]:
        backend, release = NATIVE[build]
        clock = time.monotonic()
        binary = work / f'{build}.program'
        built = native_build(args.compiler, args.runtime, inputs, 'gen@1/main/main', backend, work / 'store', binary,
                             limit=gen.ROOT_LIMIT, release=release, grants=['Stdout'], cc=args.cc)
        costs[f'{build}-compile'] = round(time.monotonic() - clock, 2)
        if built.returncode or not binary.exists():
            text = (built.stdout + built.stderr).decode(errors='replace')
            key = 'containment:generator:check' if 'not accepted' in text or 'LANG-' in text else f'containment:build:{build}'
            reds.append(red(key, build, None, None, text[-1500:]))
            continue
        clock = time.monotonic()
        jobs = [(entry['index'], entry['small_size']) for entry in program['entries']]
        jobs += [(entry['index'], entry['large_size']) for entry in program['entries'] if entry['large']]
        with ThreadPoolExecutor(max_workers=args.run_jobs) as pool:
            outcomes = list(pool.map(lambda job: run_native(binary, *job), jobs))
        for (entry, size), outcome in zip(jobs, outcomes):
            found = judge_run(program, build, entry, size, outcome, reds)
            if entry in small and size == program['entries'][entry]['small_size']:
                agree(program, entry, size, small[entry], found, build, reds, tally)
        costs[f'{build}-runs'] = round(time.monotonic() - clock, 2)
        binary.unlink(missing_ok=True)
    shutil.rmtree(work / 'store', ignore_errors=True)
    result = dict(seed=seed, version=gen.VERSION, builds=args.builds, reds=reds, records=tally, costs=costs,
                  seconds=round(time.monotonic() - started, 2),
                  package=str(package))
    (work / 'result.json').write_text(json.dumps(result, indent=1) + '\n')
    if not reds:
        shutil.rmtree(package)
    return result


def summarize(results, out, cpu_seconds):
    compared = sum(r['records']['compared'] for r in results)
    exact = sum(r['records']['exact'] for r in results)
    keys = {}
    for r in results:
        for found in r['reds']:
            keys.setdefault(found['key'], []).append((r['seed'], found))
    print(f"seeds {len(results)}; records compared {compared}, exact {exact}"
          f" ({exact / compared:.1%})" if compared else f'seeds {len(results)}; no records compared')
    print(f'CPU {cpu_seconds} s over child processes')
    for key, hits in sorted(keys.items()):
        seed, first = hits[0]
        print(f"RED {key}: {len(hits)} runs, first seed {seed} entry {first['entry']} size {first['size']}"
              f" on {first['build']}: {first['detail'][:200]}")
    summary = dict(seeds=[r['seed'] for r in results], compared=compared, exact=exact,
                   reds={k: [dict(seed=s, **f) for s, f in v] for k, v in keys.items()},
                   cpu_seconds=cpu_seconds)
    (out / 'summary.json').write_text(json.dumps(summary, indent=1) + '\n')
    vacuous = compared and exact / compared < VACUOUS
    if vacuous:
        print(f'VACUOUS: under {VACUOUS:.0%} of records compared exactly')
    return int(bool(keys) or bool(vacuous))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('mode', choices=['once', 'range'])
    parser.add_argument('--seed', type=int)
    parser.add_argument('--seeds', help='A..B, end exclusive')
    parser.add_argument('--builds', default='evaluator,cranelift-dev')
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path)
    parser.add_argument('--evaluator', type=Path)
    parser.add_argument('--cc', type=Path, default=(Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
                                                    if 'LLVM_SYS_231_PREFIX' in os.environ else None))
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--jobs', type=int, default=4, help='seeds run at once (range)')
    parser.add_argument('--run-jobs', type=int, default=2, help='native runs at once within a seed')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.builds = [b.strip() for b in args.builds.split(',') if b.strip()]
    unknown = [b for b in args.builds if b not in BUILDS]
    if unknown:
        parser.error(f'unknown builds {unknown}; choose from {", ".join(BUILDS)}')
    if 'evaluator' in args.builds and args.evaluator is None:
        parser.error('the evaluator build needs --evaluator')
    if any(b in NATIVE for b in args.builds) and (args.compiler is None or args.runtime is None):
        parser.error('native builds need --compiler and --runtime')
    args.out.mkdir(parents=True, exist_ok=True)
    args.root = args.root.resolve()
    if args.mode == 'once':
        if args.seed is None:
            parser.error('once needs --seed')
        seeds = [args.seed]
    else:
        if not args.seeds or '..' not in args.seeds:
            parser.error('range needs --seeds A..B')
        low, high = (int(x) for x in args.seeds.split('..'))
        seeds = list(range(low, high))
    # Seeds share this process, so CPU is counted once over the whole batch of child processes.
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        results = list(pool.map(lambda s: once(args, s), seeds))
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
    return summarize(results, args.out, round(cpu, 1))


if __name__ == '__main__':
    sys.path.insert(0, str(Path(__file__).parent))
    raise SystemExit(main())
