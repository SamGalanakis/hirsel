"""O3b and O3c for the containment generator: the evaluator's work tally, judged per bound.

Design: docs/internal/containment-generator.md, "O3: work ≤ K·steps". The evaluator writes the
raw counts (crates/hirsel-core-eval/src/work_tally.rs); the rule lives here:

- O3b: a bound's bulk bytes (Copy, Move and compare_bytes, both operands of a comparison) at
  MEM-30's copy-row quantum of 4096 never exceed the steps it debited. 4096 is the table's
  largest per-byte quantum, so a red is real excess, not a quantum choice. The bytes map
  hashing reads are a named MEM-30 gap: counted, reported, never red.
- O3c: every step-free cycle the lowered module holds. Core-source loops are a named gap;
  any other cycle (generated code, user code, glue) is red, unless the body takes work steps
  before the loop (prepaid).
- The instruction rule (instructions ≤ S × (steps + 1)) needs S, the longest step-free path,
  which is unbounded while any step-free cycle exists. Core-source loops are in every program,
  so the rule is reported as skipped, with the count of runs it could not judge.

The gap controls are fixed programs, one per named gap the tally can see. Each must show its
excess every run; one that stops is flagged, since the gap has closed or the tally went blind.
"""
import json
import os
from pathlib import Path

QUANTUM = 4096
CONTROL_BOUND = 1_000_000_000

# (entry, MEM-30 gap, what it must show). 'excess': copy-row units above the steps debited.
# 'core-loop': O3c names a step-free core-source loop. 'charged': a clone the table charges, so
# the tally sees its 8 bytes per element and the rule holds (the tally's calibration).
CONTROLS = (
    ('gap_0', 'map key hashing and equality (String key)', 'excess'),
    ('gap_1', 'a sequence nested in a derived ==', 'excess'),
    ('gap_2', '== at a type parameter', 'excess'),
    ('gap_3', 'the copy of a large Inline by assignment', 'excess'),
    ('gap_4', 'map key hashing and equality (derived key)', 'excess'),
    ('gap_5', 'core-source loops outside the table', 'core-loop'),
    ('gap_6', 'calibration: clone of 65536 u64', 'charged'),
)
# Not written as controls: `==` on Deque and Map does not check (LANG-21: not key), and key
# derivation has no operation of its own apart from the derived key's hashing (gap_4).
CALIBRATION_ELEMENTS = 65536

CONTROL_SOURCE = '''// Gap controls for the containment generator's O3b and O3c (core/tools/containment_gaps.py).
type Key: key = Key(u64, String)

fn doubled(k: u64) -> String {
  let mut s = string.new()
  string.push(mut s, "a")
  for i in 0u64 .. k { let t: String = clone(s); string.push(mut s, t) }
  s
}

fn same[T: key](a: T, b: T) -> bool { a == b }

fn done(r: Result[u64, OutOfTime]) -> Result[(), ()] { match r { Ok(_) => Ok(()), Error(_) => Error(()) } }

pub fn gap_0() -> Result[(), ()] {
  let s = doubled(16)
  let mut m: Map[String, u64] = map.new()
  map.insert(mut m, clone(s), 1)
  done(bound steps 1000000000u64 {
    let mut h: u64 = 0
    for i in 0u64 .. 64u64 { match map.get(m, s) { Some(_) => { h += 1 }, None => {} } }
    h
  })
}

pub fn gap_1() -> Result[(), ()] {
  let s = doubled(16)
  let mut a: List[String] = list.new()
  let mut b: List[String] = list.new()
  for i in 0u64 .. 4u64 { list.push(mut a, clone(s)); list.push(mut b, clone(s)) }
  done(bound steps 1000000001u64 { if a == b { 1u64 } else { 0u64 } })
}

pub fn gap_2() -> Result[(), ()] {
  let s = doubled(20)
  let t: String = clone(s)
  done(bound steps 1000000002u64 { if same(s, t) { 1u64 } else { 0u64 } })
}

pub fn gap_3() -> Result[(), ()] {
  let pad: Inline[u64, 32768] = [0u64; 32768]
  done(bound steps 1000000003u64 { let mut q = pad; q[5] = 1; q[5] + pad[6] })
}

pub fn gap_4() -> Result[(), ()] {
  let s = doubled(16)
  let mut m: Map[Key, u64] = map.new()
  map.insert(mut m, Key(1, clone(s)), 1)
  let k = Key(1, clone(s))
  done(bound steps 1000000004u64 {
    let mut h: u64 = 0
    for i in 0u64 .. 64u64 { match map.get(m, k) { Some(_) => { h += 1 }, None => {} } }
    h
  })
}

pub fn gap_5() -> Result[(), ()] {
  done(bound steps 1000000005u64 {
    let mut line = string.new()
    for i in 0u64 .. 64u64 { string.push_u64(mut line, 18446744073709551615u64) }
    string.byte_len(line)
  })
}

pub fn gap_6() -> Result[(), ()] {
  let mut xs: List[u64] = list.new()
  for i in 0u64 .. 65536u64 { list.push(mut xs, i) }
  done(bound steps 1000000006u64 { let d: List[u64] = clone(xs); list.len(d) })
}
'''


def units(line, hashed=False):
    return -(-(line['bytes'] + (line['hashed'] if hashed else 0)) // QUANTUM)


def core_source(function):
    """A loop written in core source (a named gap), as opposed to generated glue (`$...`)."""
    return function.startswith('core@1/') and '$' not in function


def lines_since(path, offset):
    """The tally lines written after `offset`, and the new offset."""
    if not path.exists():
        return [], offset
    with path.open('rb') as tally:
        tally.seek(offset)
        data = tally.read()
    return [json.loads(line) for line in data.splitlines() if line.strip()], offset + len(data)


def scope_of_bound(program, entry):
    """{bound n: scope id} over an entry's scopes and their children; the first scope wins a tie."""
    found, todo = {}, list(next(e for e in program['entries'] if e['index'] == entry)['scopes'])
    while todo:
        scope_id = todo.pop(0)
        scope = program['scopes'][str(scope_id)]
        found.setdefault(scope['bound'], scope_id)
        todo.extend(scope['children'])
    return found


def judge(program, entry, size, lines, reds, tally, sanctioned, red, form):
    """O3b and O3c on one evaluator run's tally lines."""
    owners = scope_of_bound(program, entry)
    for line in lines:
        if 'cycles' in line:
            free = [c for c in line['cycles'] if not c['prepaid']]
            tally['o3_instructions_skipped'] += bool(free)
            for cycle in free:
                if core_source(cycle['function']):
                    sanctioned.add(cycle['function'])
                else:
                    reds.append(red(f"containment:o3c:{cycle['function']}", 'evaluator', entry, size,
                                    f"step-free {cycle['kind']} over {cycle['blocks']} blocks"))
            continue
        tally['o3b_bounds'] += 1
        owner = owners.get(line['bound'])
        if units(line) > line['spent']:
            where = form(program, owner) if owner is not None else 'root'
            reds.append(red(f'containment:o3b:{where}', 'evaluator', entry, size,
                            f"bound {line['bound']}: {line['bytes']} bytes = {units(line)} copy-row units "
                            f"> {line['spent']} steps debited"))
        elif units(line, hashed=True) > line['spent']:
            tally['o3b_hashing'] += 1


def controls(evaluator, root, work, manifest, root_limit, quiet):
    """Run every gap control once with the tally on: one report per control."""
    from contract_exec import Evaluator
    work.mkdir(parents=True, exist_ok=True)
    source = work / 'main.hirsel'
    source.write_text(CONTROL_SOURCE)
    inputs = work / 'inputs.json'
    inputs.write_text(json.dumps(manifest(root, source.resolve())))
    path = work / 'tally.jsonl'
    path.unlink(missing_ok=True)
    reports, offset = [], 0
    with Evaluator(evaluator, env=tally_env(path)) as resident:
        for k, (entry, gap, shows) in enumerate(CONTROLS):
            result = resident.evaluate(inputs, f'gen@1/main/{entry}', root_limit=root_limit, progress=quiet)
            lines, offset = lines_since(path, offset)
            bound = next((l for l in lines if l.get('bound') == CONTROL_BOUND + k), None)
            loops = sorted({c['function'] for l in lines for c in l.get('cycles', ()) if core_source(c['function'])})
            if bound is None:
                detail, shown = f'no tally for the control: {result.stdout[-300:]!r}', False
            elif shows == 'core-loop':
                detail, shown = f"step-free core loops {', '.join(loops)}", bool(loops)
            elif shows == 'charged':
                least = 8 * CALIBRATION_ELEMENTS
                detail = f"{bound['bytes']} bytes (at least {least}), {units(bound)} units ≤ {bound['spent']} steps"
                shown = bound['bytes'] >= least and units(bound) <= bound['spent']
            else:
                detail = (f"{units(bound, hashed=True)} copy-row units ({bound['bytes']} bytes, {bound['hashed']} hashed)"
                          f" > {bound['spent']} steps")
                shown = units(bound, hashed=True) > bound['spent']
            reports.append(dict(gap=gap, shown=shown, detail=detail))
            # A step-free cycle outside core in a control is not a generator red, but is reported:
            # gap_3's `[0u64; 32768]` lowers to a fill loop with no step, which MEM-30 does not name.
            reports += [dict(gap=f'O3c in {entry}', shown=None, detail=f"step-free {c['kind']} in {c['function']}")
                        for l in lines for c in l.get('cycles', ())
                        if not c['prepaid'] and not core_source(c['function'])]
    return reports


def tally_env(path):
    return {**os.environ, 'HIRSEL_TEST_WORK_TALLY': str(Path(path))}
