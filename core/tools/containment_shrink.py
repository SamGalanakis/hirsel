"""Minimise one containment red (docs/internal/containment-generator.md, "Minimising").

The predicate: the red's key appears again for the red's entry, alone in its package, on the
cheapest build that reproduces it (Cranelift dev before the LLVM builds). Three stages:

1. choice-stream shrinking of the entry's own draws: delete blocks, lower each draw's choice
   toward 0. Every candidate comes out of the generator, so its floors stay exact;
2. AST deletion: a top-level scope, then a statement of any scope (a nested scope goes with
   its statement). An edited scope's O3a floor is dropped to 0, never guessed, so O3a cannot
   force on it; work-floor reds skip this stage;
3. size bisection: the smallest size that still reproduces.

Each red gets at most RUNS predicate runs or CPU_SECONDS of child CPU, whichever comes first.
"""
from collections import Counter
import copy
import json
import re
import resource
import shutil

import containment_gaps as gaps
import containment_gen as gen

RUNS = 300
CPU_SECONDS = 600
# Keys one entry cannot re-judge: metering needs the evaluator reference and the testing
# runtime, a build red has no entry, and a timeout costs 300 s a try.
UNMINIMISED = ('containment:o5', 'containment:o4b', 'containment:o4c', 'containment:metering',
               'containment:build:', 'containment:generator:', 'containment:timeout:')


def child_cpu():
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime + usage.ru_stime


def isolate(program, index):
    """A copy of `program` holding only entry `index` and the scopes it reaches."""
    program = copy.deepcopy(program)
    entry = next(e for e in program.entries if e.index == index)
    keep, todo = {}, list(entry.scopes)
    while todo:
        scope = todo.pop()
        keep[scope.id] = scope
        todo.extend(scope.children)
    program.entries, program.scopes = [entry], {i: keep[i] for i in sorted(keep)}
    return program


def without_scope(program, scope_id):
    program = copy.deepcopy(program)
    entry = program.entries[0]
    entry.scopes = [s for s in entry.scopes if s.id != scope_id]
    return isolate(program, entry.index)


def without_statement(program, scope_id, at):
    """Drop statement `at` of a scope; its floor goes to 0 and its byte charge is recounted."""
    program = copy.deepcopy(program)
    scope = program.scopes[scope_id]
    if scope.productions[at] == 'nested':
        called = int(re.search(r'scope_(\d+)\(', scope.body[at]).group(1))
        scope.children = [c for c in scope.children if c.id != called]
    del scope.body[at], scope.productions[at]
    scope.floor = gen.Floor()
    scope.recursive = 'walk_chain' in scope.productions
    if scope.form == 'isolate_bytes':
        scope.charge = gen.CLONE_BYTES * scope.productions.count('clone_list')
    return isolate(program, program.entries[0].index)


class Predicate:
    """Does the key reproduce for the program's one entry at `size` on `build`?"""

    def __init__(self, fz, args, key, build, work):
        self.fz, self.args, self.key, self.build, self.work = fz, args, key, build, work
        self.runs, self.start = 0, child_cpu()
        self.with_evaluator = build == 'evaluator' or key.startswith('containment:disagree')
        self.resident, self.offset = None, 0
        self.binary_source = None

    def spent(self):
        return self.runs >= RUNS or child_cpu() - self.start >= CPU_SECONDS

    def close(self):
        if self.resident is not None:
            self.resident.__exit__(None, None, None)

    def __call__(self, program, size):
        from contract_exec import Evaluator, native_build
        fz, args = self.fz, self.args
        self.runs += 1
        entry = program.entries[0]
        if self.with_evaluator:   # the evaluator's entry runs its size as written
            entry.small_size = size
        program.source = gen.render(program)
        package = gen.write_package(program, self.work / 'package')
        meta = json.loads((package / 'program.json').read_text())
        inputs = self.work / 'inputs.json'
        inputs.write_text(json.dumps(fz.manifest(args.root, (package / 'main.hirsel').resolve())))
        reds, tally, reference = [], Counter(), None
        if self.with_evaluator:
            if self.resident is None:
                self.resident = Evaluator(args.evaluator, env=gaps.tally_env(self.work / 'tally.jsonl'))
            outcome = fz.run_evaluator(self.resident, inputs, entry.index)
            lines, self.offset = gaps.lines_since(self.work / 'tally.jsonl', self.offset)
            if outcome[0] == 'harness':
                return False
            reference = fz.judge_run(meta, 'evaluator', entry.index, size, outcome, reds, tally)
            gaps.judge(meta, entry.index, size, lines, reds, tally, set(), fz.red, fz.form)
        if self.build in fz.NATIVE:
            backend, release = fz.NATIVE[self.build]
            binary = self.work / 'program'
            if self.binary_source != program.source:   # a size alone reuses the build
                self.binary_source = None
                built = native_build(args.compiler, args.runtime, inputs, 'gen@1/main/main', backend,
                                     self.work / 'store', binary, limit=gen.ROOT_LIMIT, release=release,
                                     grants=['Stdout'], cc=args.cc)
                shutil.rmtree(self.work / 'store', ignore_errors=True)
                if built.returncode or not binary.exists():
                    return False
                self.binary_source = program.source
            found = fz.judge_run(meta, self.build, entry.index, size, fz.run_native(binary, entry.index, size),
                                 reds, tally)
            if reference is not None:
                fz.agree(meta, entry.index, size, reference, found, self.build, reds, tally)
        return any(r['key'] == self.key for r in reds)


def shrink_draws(program, index, large, test, log):
    """Stage 1 on the entry's span of the choice stream; returns the smallest reproducing program."""
    start, end = next(e for e in program.entries if e.index == index).span
    prefix, span = program.draws[:start], list(program.draws[start:end])

    def attempt(candidate):
        regenerated = gen.generate(program.seed, prefix + candidate)
        found = isolate(regenerated, index)
        found.draws, found.bounds = regenerated.draws, regenerated.bounds
        entry = found.entries[0]
        return found if test(found, entry.large_size if large else entry.small_size) else None

    best = None
    improved = True
    while improved and not test.spent():
        improved = False
        for width in (8, 4, 2, 1):
            at = 0
            while at < len(span) and not test.spent():
                candidate = span[:at] + span[at + width:]
                if found := attempt(candidate):
                    span, best, improved = candidate, found, True
                else:
                    at += width
        bounds = (best or program).bounds[start:start + len(span)]
        for at in range(len(span)):
            bound = bounds[at] if at < len(bounds) else None
            choice = span[at] % bound if bound else span[at]
            for lower in sorted({0, choice // 2, choice - 1} - {choice}):
                if lower < 0 or test.spent():
                    continue
                candidate = span[:at] + [lower] + span[at + 1:]
                if found := attempt(candidate):
                    span, best, improved = candidate, found, True
                    break
    log['draws'] = [end - start, len(span)]
    return best


def shrink_ast(program, size, test, log):
    statements = lambda p: sum(len(s.body) for s in p.scopes.values())
    before = statements(program)
    for scope in list(program.entries[0].scopes):
        if len(program.entries[0].scopes) > 1 and not test.spent():
            candidate = without_scope(program, scope.id)
            if test(candidate, size):
                program = candidate
    for scope_id in sorted(program.scopes):
        at = 0
        while scope_id in program.scopes and at < len(program.scopes[scope_id].body) and not test.spent():
            candidate = without_statement(program, scope_id, at)
            if test(candidate, size):
                program = candidate
            else:
                at += 1
    log['statements'] = [before, statements(program)]
    return program


def shrink_size(program, size, test, log):
    low, high = -1, size   # high reproduces; find the least size that does
    if size and test(program, 0):
        high = 0
    else:
        low = 0
    while high - low > 1 and not test.spent():
        mid = (low + high) // 2
        if test(program, mid):
            high = mid
        else:
            low = mid
    log['size'] = [size, high]
    return high


def cheapest(builds, build, key):
    """Cranelift dev reproduces a native red more cheaply than the LLVM builds, when it ran."""
    if build in ('llvm-dev', 'llvm-release') and 'cranelift-dev' in builds and not key.startswith('containment:disagree'):
        return ['cranelift-dev', build]
    return [build]


def minimise(fz, args, seed, found, out):
    """Minimise one red; returns its alert fields: the package path, or why there is none."""
    key, entry_index, size = found['key'], found['entry'], found['size']
    if key.startswith(UNMINIMISED) or entry_index is None:
        return dict(minimised=False, reason='not re-judged on one entry')
    program = gen.generate(seed)
    large = size != program.entries[entry_index].small_size
    slug = re.sub(r'[^A-Za-z0-9_.-]+', '-', key.removeprefix('containment:'))
    work = out / f'{slug}.work'
    for build in cheapest(args.builds, found['build'], key):
        shutil.rmtree(work, ignore_errors=True)
        work.mkdir(parents=True)
        test = Predicate(fz, args, key, build, work)
        log = {}
        try:
            base = isolate(program, entry_index)
            if not test(base, size):
                continue
            if smaller := shrink_draws(program, entry_index, large, test, log):
                base = smaller
                entry = base.entries[0]
                size = entry.large_size if large else entry.small_size
            if not key.startswith('containment:work-floor') and not test.spent():
                base = shrink_ast(base, size, test, log)
            size = shrink_size(base, size, test, log)
            entry = base.entries[0]
            entry.small_size = entry.large_size = size
            base.source = gen.render(base)
            package = gen.write_package(base, out / slug)
            repro = dict(key=key, seed=seed, entry=entry_index, size=size, build=build, runs=test.runs,
                         cpu_seconds=round(child_cpu() - test.start, 1), capped=test.spent(), stages=log)
            (package / 'repro.json').write_text(json.dumps(repro, indent=1) + '\n')
            return dict(minimised=True, path=str(package), build=build, size=size, runs=test.runs)
        finally:
            test.close()
            shutil.rmtree(work, ignore_errors=True)
    return dict(minimised=False, reason='did not reproduce alone')
