#!/usr/bin/env python3
"""Generate well-typed Hirsel packages whose contained scopes the containment oracles judge.

Design: docs/internal/containment-generator.md. A program is addressed by
(VERSION, seed); its package is the whole repro. Every choice comes from a SplitMix64
choice stream that can replay an explicit draw list, so a shrinker can edit the draws and
every candidate is still a generator output.
Example: containment_gen.py --seed 7 --out /tmp/pkg
"""
import argparse
from dataclasses import dataclass, field
import json
from pathlib import Path

# Bumped whenever any production changes, so an old seed never replays through another generator.
VERSION = 2
MASK = (1 << 64) - 1
ENTRIES = 32
LARGE_ENTRIES = 8
# Above every scope's needs and below the 2 GiB measured-process cap (the design's closed world).
ROOT_LIMIT = 1 << 30
# Every run's work sits inside one outer `bound steps ROOT_STEPS`: both engines' metering
# injects its step limit into the first bound a run enters, and check_metering expects
# that bound to cover the whole run (O5). It is far above any scope's n.
ROOT_STEPS = 1 << 62

# Record kinds of the tag stream; each record is three words: scope id, kind, payload.
OK, OUT_OF_TIME, CAPACITY, CAUSED, ISOLATE_CAPACITY, O3A_FAIL = 1, 2, 3, 4, 5, 9
KIND_NAMES = {OK: 'ok', OUT_OF_TIME: 'out_of_time', CAPACITY: 'capacity', CAUSED: 'caused',
              ISOLATE_CAPACITY: 'isolate_capacity', O3A_FAIL: 'o3a_fail'}
CAUSE_BITS = ('Overflow', 'Bounds', 'Division', 'Unwrap', 'Fail', 'Cancelled', 'Capacity',
              'OutOfTime', 'Stack')
STACK_BIT = 1 << CAUSE_BITS.index('Stack')
CAPACITY_BIT = 1 << CAUSE_BITS.index('Capacity')


class Stream:
    """The choice stream. A seed expands through SplitMix64 (as crates/hirsel-core-net/src/
    split_mix.rs does); a replayed draw list is read as given, and draws past its end are 0,
    which always selects the simplest production."""

    def __init__(self, seed=0, draws=None):
        self.state = seed & MASK
        self.replay = None if draws is None else list(draws)
        self.drawn = []
        self.bounds = []   # per draw, the bound `below` reduced it by (None for a raw draw)

    def raw(self):
        if self.replay is not None:
            value = self.replay[len(self.drawn)] if len(self.drawn) < len(self.replay) else 0
        else:
            self.state = (self.state + 0x9E3779B97F4A7C15) & MASK
            z = self.state
            z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
            z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
            value = z ^ (z >> 31)
        self.drawn.append(value)
        self.bounds.append(None)
        return value

    def below(self, bound):
        value = self.raw() % max(bound, 1)
        self.bounds[-1] = max(bound, 1)
        return value

    def log_uniform(self, low_exp, high_exp):
        """A value in [2^low_exp, 2^high_exp], log-uniform; the simplest is 2^low_exp."""
        exp = low_exp + self.below(high_exp - low_exp + 1)
        return min((1 << exp) + self.below(1 << exp), 1 << high_exp)


# Size classes (design, "Sizes"): element counts, string bytes and recursion depth.
SMALL = (0, 12)
LARGE = (16, 20)
LARGE_DEPTH = (15, 19)


def ceil_div(a, b):
    return -(-a // b)


@dataclass
class Floor:
    """A lower bound L on a scope's steps (O3a): `constant` plus, for each (coefficient,
    quantum) term, ceil(coefficient * size / quantum). Each term is a row of MEM-30 copied
    by hand from docs/spec/budgets.md (never from crates/hirsel-core-ids/src/work_steps.rs,
    the code under test); where the text reads two ways the smaller value is taken."""
    constant: int = 0
    terms: list = field(default_factory=list)

    def at(self, size):
        return self.constant + sum(ceil_div(a * size, q) for a, q in self.terms)

    def hirsel(self):
        parts = [f'{self.constant}u64'] + [f'(({a}u64 * size + {q - 1}u64) / {q}u64)' for a, q in self.terms]
        return ' + '.join(parts)


# Body productions, simplest first, so a zero draw picks `loop_static`. Each gives its
# statement, the inputs it reads and its floor term. `recursive` productions recurse in
# user code, where each build fixes its own stack allowance (FAIL-17).
#
# MEM-30 rows used (docs/spec/budgets.md, MEM-30 "Steps", "Work steps", "Depth steps",
# "Determinism"):
# - a loop takes a step each time control returns to its head; every build takes at least
#   one per iteration of a user loop, so `for` over k items floors at k;
# - each call into user code that recursion reaches: a chain walk of depth s floors at s;
# - `list.sum`: elements, quantum 32;
# - a clone, each sequence it copies: element bytes, quantum 4096 (u64: 8 bytes);
# - a clone, each box it copies: 1, quantum 4 (read as summed units: ceil(boxes / 4));
# - derived `==` at a recursive type: one depth step per visit, so a bound of n stops a
#   chain of more than n links;
# - derived `==` on a list: min length times (element bytes plus one), quantum 32; the
#   floor takes 8 per element (ceil(s / 4)), below the row's 9;
# - `==` on String, and string.push: bytes (both operands for a comparison), quantum 4096;
# - a map fill: one loop step per inserted key (the insert's own core-source path is a
#   named gap and adds nothing to L).
PRODUCTIONS = (
    ('loop_static', (), False),
    ('loop_list', ('xs',), False),
    ('sum', ('xs',), False),
    ('clone_list', ('xs',), False),
    ('push_string', ('s1',), False),
    ('eq_string', ('s1', 's2'), False),
    ('eq_list', ('xs', 'ys'), False),
    ('clone_chain', ('c',), False),
    ('eq_chain', ('c', 'c2'), False),
    ('walk_chain', ('c',), True),
    ('map_fill', ('xs',), False),
    ('nested', (), False),
)
NAMES = [p[0] for p in PRODUCTIONS]
# O4a (design, "O4: bytes ≤ bound"): productions whose MEM-24 logical charge the generator
# knows exactly. Only `clone_list` allocates: a clone charges capacity equal to its length,
# so s u64 elements charge 8·s bytes (already a multiple of 8). The others allocate nothing.
# A scope built only from these may take a `bound bytes b` form, charged CLONE_BYTES per
# element per `clone_list`.
BYTES_KNOWN = ('loop_static', 'loop_list', 'sum', 'clone_list', 'eq_string', 'eq_list', 'eq_chain', 'walk_chain')
CLONE_BYTES = 8


def statement(name, stream, floor, k):
    """The Hirsel statement of one production, adding its floor term; `k` keeps its names apart."""
    if name == 'loop_static':
        trips = stream.log_uniform(0, 12)
        floor.constant += trips
        return f'for i in 0u64 .. {trips}u64 {{ h = mix(h, i) }}'
    if name == 'loop_list':
        floor.terms.append((1, 1))
        return 'for x in xs { h = mix(h, x) }'
    if name == 'sum':
        floor.terms.append((1, 32))
        return 'h = mix(h, list.sum(xs))'
    if name == 'clone_list':
        floor.terms.append((8, 4096))
        return f'let d{k}: List[u64] = clone(xs)\nh = mix(h, list.len(d{k}))'
    if name == 'push_string':
        floor.terms.append((1, 4096))
        return f'let mut w{k} = string.new()\nstring.push(mut w{k}, s1)\nh = mix(h, string.byte_len(w{k}))'
    if name == 'eq_string':
        floor.terms.append((2, 4096))
        return 'if s1 == s2 { h = mix(h, 3) }'
    if name == 'eq_list':
        floor.terms.append((1, 4))
        return 'if xs == ys { h = mix(h, 2) }'
    if name == 'clone_chain':
        floor.terms.append((1, 4))
        return f'let dc{k}: Chain = clone(c)\nh = mix(h, head(dc{k}))'
    if name == 'eq_chain':
        floor.terms.append((1, 1))
        return 'if c == c2 { h = mix(h, 1) }'
    if name == 'walk_chain':
        floor.terms.append((1, 1))
        return 'h = mix(h, walk(c))'
    if name == 'map_fill':
        floor.terms.append((1, 1))
        return (f'let mut m{k}: Map[u64, u64] = map.new()\nfor x in xs {{ map.insert(mut m{k}, x, x) }}\n'
                f'h = mix(h, map.len(m{k}))')
    raise ValueError(name)


@dataclass
class Scope:
    id: int
    # 'isolate_steps': isolate { bound steps n { body } }; 'steps_isolate': the reverse;
    # 'isolate_bytes': isolate { bound bytes n { body } }, charged `charge`·size bytes (O4a).
    form: str
    bound: int
    floor: Floor
    recursive: bool
    body: list
    children: list
    productions: list
    charge: int = 0


@dataclass
class Entry:
    index: int
    large: bool
    small_size: int
    large_size: int
    scopes: list
    span: tuple = (0, 0)   # the entry's draws: stream positions [start, end)


@dataclass
class Program:
    seed: int
    policy: list
    entries: list
    scopes: dict
    source: str = ''
    draws: list = field(default_factory=list)
    bounds: list = field(default_factory=list)

    def predictions(self):
        """Per scope: its bound, floor terms, recursion and nesting, for the driver's oracles."""
        return {s.id: dict(bound=s.bound, constant=s.floor.constant, terms=s.floor.terms,
                           recursive=s.recursive, children=[c.id for c in s.children],
                           form=s.form, productions=s.productions, charge=s.charge)
                for s in self.scopes.values()}


class Generator:
    def __init__(self, stream):
        self.stream = stream
        self.scopes = {}

    def policy(self):
        """A batch's swarm policy: a random non-empty subset of the productions."""
        chosen = [name for name in NAMES if self.stream.below(2)]
        return chosen or ['loop_static']

    def bound_for(self, floor, size):
        """n log-uniform in [1, 2^16], nudged so that L lands on either side of n about
        equally often; above, n may pass 2^16 so a deep walk can run past the stack (E2)."""
        n = self.stream.log_uniform(0, 16)
        least = floor.at(size)
        if self.stream.below(2) == 0:
            return max(1, min(n, least - 1)) if least > 1 else n
        return max(n, least << (1 + self.stream.below(6)))

    def scope(self, policy, depth, size):
        scope_id = len(self.scopes)
        self.scopes[scope_id] = None   # claim the id before the children take theirs
        form = ('isolate_steps', 'steps_isolate')[self.stream.below(2)]
        floor = Floor()
        body, children, productions = [], [], []
        recursive = False
        for _ in range(1 + self.stream.below(4)):
            name = policy[self.stream.below(len(policy))]
            if name == 'nested':
                if depth >= 2:
                    continue
                child = self.scope(policy, depth + 1, size)
                children.append(child)
                productions.append(name)
                body.append(f'list.append(mut nested, scope_{child.id}(c, c2, xs, ys, s1, s2, size))')
                continue
            productions.append(name)
            recursive |= name == 'walk_chain'
            body.append(statement(name, self.stream, floor, len(body)))
        result = Scope(scope_id, form, 0, floor, recursive, body, children, productions)
        self.scopes[scope_id] = result
        if depth == 0 and set(productions) <= set(BYTES_KNOWN) and self.stream.below(3) == 0:
            result.form = 'isolate_bytes'
            result.charge = CLONE_BYTES * productions.count('clone_list')
            result.bound = self.bytes_for(result.charge * size)
        else:
            result.bound = self.bound_for(floor, size)
        return result

    def bytes_for(self, charge):
        """b on either side of the predicted charge about equally often (O4a judges b ≥ charge)."""
        if charge and self.stream.below(2) == 0:
            return charge - 1 - self.stream.below(charge // 2 + 1)
        return charge + self.stream.log_uniform(0, 12)

    def program(self, seed):
        policy = self.policy()
        entries = []
        for index in range(ENTRIES):
            start = len(self.stream.drawn)
            large = index >= ENTRIES - LARGE_ENTRIES
            small_size = self.stream.log_uniform(*SMALL)
            large_size = self.stream.log_uniform(*LARGE) if large else small_size
            if large and any(name in policy for name in ('clone_chain', 'eq_chain', 'walk_chain')) and self.stream.below(2):
                large_size = self.stream.log_uniform(*LARGE_DEPTH)
            size = large_size if large else small_size
            scopes = [self.scope(policy, 0, size) for _ in range(1 + self.stream.below(2))]
            entries.append(Entry(index, large, small_size, large_size, scopes, (start, len(self.stream.drawn))))
        program = Program(seed, policy, entries, self.scopes)
        program.source = render(program)
        program.draws, program.bounds = list(self.stream.drawn), list(self.stream.bounds)
        return program


def generate(seed, draws=None):
    stream = Stream(seed, draws)
    return Generator(stream).program(seed)


PRELUDE = '''// Generated by core/tools/containment_gen.py, version {version}, seed {seed}.
// Policy: {policy}.
type Chain: key = Nil | Link(u64, Box[Chain])

fn mix(h: u64, v: u64) -> u64 {{ ((h ^ v) % 4294967291) * 31 + 7 }}

fn build_chain(n: u64) -> Chain {{
  let mut c: Chain = Nil
  let mut i: u64 = 0
  while i < n {{ let prev = take c; c = Link(i % 1000, box.new(take prev)); i += 1 }}
  c
}}

fn build_list(n: u64) -> List[u64] {{
  let mut xs: List[u64] = list.with_capacity(n)
  for i in 0u64 .. n {{ list.push(mut xs, i % 1000) }}
  xs
}}

fn build_string(n: u64) -> String {{
  let mut s = string.new()
  for i in 0u64 .. n {{ string.push(mut s, "a") }}
  s
}}

fn head(c: Chain) -> u64 {{ match c {{ Nil => 0, Link(v, _) => v }} }}

fn walk(c: Chain) -> u64 {{ match c {{ Nil => 0, Link(v, b) => (v % 7) + walk(b) }} }}

fn cause_mask(causes: Causes) -> u64 {{
  let mut m: u64 = 0
  for c in causes {{
    m = m | match c.kind {{
      Overflow => 1, Bounds => 2, Division => 4, Unwrap => 8, Fail(_) => 16, Cancelled => 32,
      Capacity(_) => 64, OutOfTime => 128, Stack => 256,
    }}
  }}
  m
}}

fn rec(mut out: List[u64], id: u64, kind: u64, payload: u64) {{
  list.push(mut out, id)
  list.push(mut out, kind)
  list.push(mut out, payload)
}}

// Prints each record as `scope <id> <kind> <payload>`; false when an O3a self-check failed.
fn emit(out: Stdout, recs: List[u64]) -> bool waits {{
  let mut good = true
  let mut i: u64 = 0
  while i + 2 < list.len(recs) {{
    let mut line = string.new()
    string.push(mut line, "scope ")
    string.push_u64(mut line, recs[i])
    string.push(mut line, " ")
    string.push_u64(mut line, recs[i + 1])
    string.push(mut line, " ")
    string.push_u64(mut line, recs[i + 2])
    string.push(mut line, "\\n")
    let _ = stdout.write(out, line)
    if recs[i + 1] == {o3a} {{ good = false }}
    i += 3
  }}
  good
}}
'''

PARAMS = 'c: Chain, c2: Chain, xs: List[u64], ys: List[u64], s1: String, s2: String, size: u64'


def indent(lines, depth):
    pad = '  ' * depth
    return '\n'.join(pad + line for chunk in lines for line in chunk.split('\n'))


def render_scope(scope):
    n = f'{scope.bound}u64'
    body = indent(['let mut h: u64 = 17', 'let mut nested: List[u64] = list.new()', *scope.body,
                   '(h, nested)'], 3)
    i = scope.id
    if scope.form == 'isolate_steps':
        head = (f'  let r = isolate(fn() -> Result[(u64, List[u64]), OutOfTime] {{\n'
                f'    bound steps {n} {{\n{body}\n    }}\n  }})\n')
        arms = (f'    Ok(Ok(Ok((v, nested)))) => {{ rec(mut out, {i}, {OK}, v); list.append(mut out, nested) }},\n'
                f'    Ok(Ok(Error(_))) => rec(mut out, {i}, {OUT_OF_TIME}, {n}),\n'
                f'    Ok(Error(causes)) => rec(mut out, {i}, {CAUSED}, cause_mask(causes)),\n'
                f'    Error(_) => rec(mut out, {i}, {ISOLATE_CAPACITY}, 0),\n')
        timed = 'Ok(Ok(Error(_))) => true'
        stack = 'Ok(Error(causes)) => (cause_mask(causes) & 256) != 0'
    elif scope.form == 'isolate_bytes':
        head = (f'  let r = isolate(fn() -> Result[(u64, List[u64]), Capacity] {{\n'
                f'    bound bytes {n} {{\n{body}\n    }}\n  }})\n')
        arms = (f'    Ok(Ok(Ok((v, nested)))) => {{ rec(mut out, {i}, {OK}, v); list.append(mut out, nested) }},\n'
                f'    Ok(Ok(Error(_))) => rec(mut out, {i}, {CAPACITY}, {n}),\n'
                f'    Ok(Error(causes)) => rec(mut out, {i}, {CAUSED}, cause_mask(causes)),\n'
                f'    Error(_) => rec(mut out, {i}, {ISOLATE_CAPACITY}, 0),\n')
        return (f'fn scope_{i}({PARAMS}) -> List[u64] {{\n{head}'
                f'  let mut out: List[u64] = list.new()\n  match r {{\n{arms}  }}\n  out\n}}\n')
    else:
        head = (f'  let r = bound steps {n} {{\n    isolate(fn() -> (u64, List[u64]) {{\n{body}\n    }})\n  }}\n')
        arms = (f'    Ok(Ok(Ok((v, nested)))) => {{ rec(mut out, {i}, {OK}, v); list.append(mut out, nested) }},\n'
                f'    Ok(Ok(Error(causes))) => rec(mut out, {i}, {CAUSED}, cause_mask(causes)),\n'
                f'    Ok(Error(_)) => rec(mut out, {i}, {ISOLATE_CAPACITY}, 0),\n'
                f'    Error(_) => rec(mut out, {i}, {OUT_OF_TIME}, {n}),\n')
        timed = 'Error(_) => true'
        stack = 'Ok(Ok(Error(causes))) => (cause_mask(causes) & 256) != 0'
    # O3a self-check: L > n forces out_of_time on every build; a scope that recurses in
    # user code may also stop with Stack (FAIL-17).
    accepted = f'match r {{ {timed}, {stack}, _ => false }}' if scope.recursive else f'match r {{ {timed}, _ => false }}'
    return (f'fn scope_{i}({PARAMS}) -> List[u64] {{\n{head}'
            f'  let mut out: List[u64] = list.new()\n  match r {{\n{arms}  }}\n'
            f'  let least: u64 = {scope.floor.hirsel()}\n'
            f'  let accepted = ({accepted})\n'
            f'  if least > {n} && !accepted {{ rec(mut out, {i}, {O3A_FAIL}, least) }}\n'
            f'  out\n}}\n')


# O5 exclusions (design, "O5: the R4 floor"): a map's path depends on the process's hash key,
# and a spawning entry's on its schedule, so such entries stay out of `check_metering`.
# The generator has no spawning production, so only maps are excluded today.
UNMETERED = ('map_fill',)


def metered(entry):
    def walk(scope):
        return not set(scope.productions) & set(UNMETERED) and all(walk(c) for c in scope.children)
    return all(walk(scope) for scope in entry.scopes)


def needed(entry):
    names = set()
    def walk(scope):
        for p in scope.productions:
            names.update(dict((q[0], q[1]) for q in PRODUCTIONS)[p])
        for child in scope.children:
            walk(child)
    for scope in entry.scopes:
        walk(scope)
    return names


def render_entry(entry):
    uses = needed(entry)
    def built(name, builder):
        return f'  let {name} = {builder}({"size" if name in uses else "0"})\n'
    lines = [f'fn case_{entry.index}(out: Stdout, size: u64) -> bool waits {{\n',
             built('c', 'build_chain'), built('c2', 'build_chain'), built('xs', 'build_list'),
             built('ys', 'build_list'), built('s1', 'build_string'), built('s2', 'build_string'),
             '  let mut good = true\n']
    for scope in entry.scopes:
        lines.append(f'  if !emit(out, scope_{scope.id}(c, c2, xs, ys, s1, s2, size)) {{ good = false }}\n')
    lines.append('  good\n}\n')
    lines.append(f'\npub fn ev_{entry.index}(out: Stdout) -> Result[(), ()] waits {{\n'
                 f'  let run = bound steps {ROOT_STEPS}u64 {{ case_{entry.index}(out, {entry.small_size}u64) }}\n'
                 f'  match run {{ Ok(true) => Ok(()), _ => Error(()) }}\n}}\n')
    return ''.join(lines)


def render(program):
    parts = [PRELUDE.format(version=VERSION, seed=program.seed, policy=', '.join(program.policy), o3a=O3A_FAIL)]
    parts += [render_scope(program.scopes[i]) for i in sorted(program.scopes)]
    parts += [render_entry(entry) for entry in program.entries]
    dispatch = '\n'.join(f'  if which == {e.index}u64 {{ good = case_{e.index}(out, size) }}' for e in program.entries)
    dispatch = dispatch.replace('\n  ', '\n    ').replace('  if', '    if', 1)
    parts.append('pub fn main(argv: Args, out: Stdout) -> Result[(), Exit] waits {\n'
                 f'  let run = bound steps {ROOT_STEPS}u64 {{\n'
                 '    let a0 = option.unwrap(args.get(argv, 0))\n'
                 '    let a1 = option.unwrap(args.get(argv, 1))\n'
                 '    let p0: Result[u64, ParseError] = int.parse(a0)\n'
                 '    let p1: Result[u64, ParseError] = int.parse(a1)\n'
                 '    let which = result.unwrap(p0)\n'
                 '    let size = result.unwrap(p1)\n'
                 '    let mut good = false\n'
                 f'{dispatch}\n'
                 '    good\n  }\n'
                 '  match run { Ok(true) => Ok(()), Ok(false) => Error(Exit(status: 3)), Error(_) => Error(Exit(status: 4)) }\n}\n')
    return '\n'.join(parts)


MANIFEST = '''[package]
name = "gen"
major = 1
charge = "charge-v1"

[sources]
"main.hirsel" = "main"
'''


def write_package(program, directory):
    """The repro: source, manifest and the predictions and argv the driver judged it by."""
    directory.mkdir(parents=True, exist_ok=True)
    (directory / 'hirsel.toml').write_text(MANIFEST)
    (directory / 'main.hirsel').write_text(program.source)
    meta = dict(version=VERSION, seed=program.seed, policy=program.policy, root_limit=ROOT_LIMIT,
                entries=[dict(index=e.index, large=e.large, small_size=e.small_size, large_size=e.large_size,
                              metered=metered(e), scopes=[s.id for s in e.scopes]) for e in program.entries],
                scopes=program.predictions())
    (directory / 'program.json').write_text(json.dumps(meta, indent=1) + '\n')
    return directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--seed', type=int, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    write_package(generate(args.seed), args.out)


if __name__ == '__main__':
    main()
