#!/usr/bin/env python3
"""Curated negative controls and seeded multi-feature generator for Hirsel soundness fuzzing.

Targets ownership, aliasing, move/drop-flag, scoped borrow/region escape, capability
confinement, multi-feature interactions (closures x regions x isolate x units x
spawn/channel x Held), and failure-path unwinding through moved-from, maybe-live,
and lent places.
"""
from dataclasses import dataclass, field

VERSION = 2
MASK = (1 << 64) - 1
SURFACES = ('aliasing', 'move', 'escape', 'capability', 'interaction', 'unwinding')
CHECKER_SURFACES = ('aliasing', 'move', 'escape', 'capability')
# Reserved extension point for std.data.decode once landed.
FUTURE_SURFACES = ('data_decode',)


class Stream:
    """Deterministic SplitMix64 choice stream supporting draw-list replay for shrinking."""

    def __init__(self, seed: int = 0, draws=None):
        self.state = seed & MASK
        self.replay = None if draws is None else list(draws)
        self.drawn = []
        self.bounds = []

    def raw(self) -> int:
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

    def below(self, bound: int) -> int:
        bound = max(bound, 1)
        value = self.raw() % bound
        self.bounds[-1] = bound
        return value

    def pick(self, items):
        return items[self.below(len(items))]


@dataclass(frozen=True)
class NegativeControl:
    name: str
    surface: str
    expected_rules: tuple
    source: str
    entry: str | None = None


NEGATIVE_CONTROLS: tuple[NegativeControl, ...] = (
    # (a) Aliasing mut views & disjoint-field splits
    NegativeControl(
        'alias_mut_mut_same_list', 'aliasing', ('OWN-9',),
        'fn two(mut a: List[u64], mut b: List[u64]) { list.push(mut a, 1); list.push(mut b, 2) }\n'
        'fn bad(mut xs: List[u64]) { two(mut xs, mut xs) }\n',
    ),
    NegativeControl(
        'alias_mut_view_same_list', 'aliasing', ('OWN-9',),
        'fn mv(mut a: List[u64], b: List[u64]) { list.push(mut a, list.len(b)) }\n'
        'fn bad(mut xs: List[u64]) { mv(mut xs, xs) }\n',
    ),
    NegativeControl(
        'alias_index_overlap', 'aliasing', ('OWN-9',),
        'fn bad(mut xs: List[List[u64]], i: u64, j: u64) { list.append(mut xs[i], xs[j]) }\n',
    ),
    NegativeControl(
        'alias_parent_child_field', 'aliasing', ('OWN-9',),
        'type Pair { left: List[u64], right: List[u64] }\n'
        'fn both(mut p: Pair, mut l: List[u64]) { list.push(mut p.right, list.len(l)) }\n'
        'fn bad(mut p: Pair) { both(mut p, mut p.left) }\n',
    ),
    NegativeControl(
        'alias_live_view_then_mut', 'aliasing', ('OWN-10',),
        'type Pair { left: List[u64], right: List[u64] }\n'
        'fn bad(mut p: Pair) -> u64 { let v = view p.left; list.push(mut p.left, 1); list.len(v) }\n',
    ),
    NegativeControl(
        'alias_reborrow_result_then_mut', 'aliasing', ('OWN-10',),
        'fn grow(mut s: String) -> View[String] in s { string.push(mut s, "x"); string.slice(s, 0, 1) }\n'
        'fn bad(mut s: String) -> u64 { let a = grow(mut s); let b = grow(mut s); string.byte_len(a) + string.byte_len(b) }\n',
    ),
    NegativeControl(
        'alias_match_mut_retag_enum', 'aliasing', ('OWN-9',),
        'type Choice = Text(String) | Num(u64)\n'
        'fn bad(mut c: Choice) { match c { Text(mut s) => { string.push(mut s, "x"); c = Num(1) }, Num(_) => {} } }\n',
    ),
    NegativeControl(
        'alias_match_mut_list_push', 'aliasing', ('OWN-9',),
        'type Choice = Text(String) | Num(u64)\n'
        'fn bad(mut xs: List[Choice]) { match xs[0] { Text(mut s) => { list.push(mut xs, Num(0)) }, Num(_) => {} } }\n',
    ),
    NegativeControl(
        'alias_map_update_capture_receiver', 'aliasing', ('OWN-9',),
        'fn bad(mut m: Map[u64, u64]) -> Option[()] { map.update(mut m, 1, fn(mut v: u64) { v = map.len(m) }) }\n',
    ),
    NegativeControl(
        'alias_assign_reserved_index', 'aliasing', ('OWN-12',),
        'fn bad(mut xs: List[u64]) { xs[0] = { xs = list.new(); 7 } }\n',
    ),
    NegativeControl(
        'alias_box_rc_write', 'aliasing', ('OWN-12',),
        'type Boxed: shareable { n: u64 }\n'
        'fn bad(mut b: Box[Rc[Boxed]]) { b.n = 1 }\n',
    ),
    # (b) Use-after-move (take)
    NegativeControl(
        'move_simple_use_after_let', 'move', ('OWN-5',),
        'fn bad(take s: String) -> String { let t = s; s }\n',
    ),
    NegativeControl(
        'move_if_branch_maybe', 'move', ('OWN-16',),
        'fn bad(take s: String, c: bool) -> String { if c { let _ = string.into_bytes(take s) }; s }\n',
    ),
    NegativeControl(
        'move_match_arm_maybe', 'move', ('OWN-16',),
        'fn bad(take o: Option[String]) -> u64 {\n'
        '  let _ = match o { Some(take s) => string.byte_len(s), None => 0u64 }\n'
        '  match o { Some(s) => string.byte_len(s), None => 0u64 }\n}\n',
    ),
    NegativeControl(
        'move_loop_backedge', 'move', ('OWN-17',),
        'fn bad(take s: String) { loop { let _ = string.into_bytes(take s) } }\n',
    ),
    NegativeControl(
        'move_loop_continue_no_reinit', 'move', ('OWN-17',),
        'fn bad(take s: String, c: bool) {\n'
        '  let mut cur = s\n'
        '  while c { let _ = string.into_bytes(take cur); if c { continue }; cur = string.new() }\n}\n',
    ),
    NegativeControl(
        'move_owning_closure_then_use', 'move', ('OWN-5',),
        'fn bad(take s: String) -> u64 {\n'
        '  let f: own fn() -> u64 = fn() -> u64 { string.byte_len(s) }\n'
        '  f() + string.byte_len(s)\n}\n',
    ),
    NegativeControl(
        'move_before_try_early_return', 'move', ('OWN-5',),
        'fn bad(take s: String, r: Result[u64, u64]) -> Result[String, u64] {\n'
        '  let _ = string.into_bytes(take s)\n  let _ = r?\n  Ok(take s)\n}\n',
    ),
    NegativeControl(
        'move_partial_field_take', 'move', ('OWN-14',),
        'type Person { name: String, age: u64 }\n'
        'fn bad(take p: Person) -> String { take p.name }\n',
    ),
    NegativeControl(
        'move_take_mut_param', 'move', ('OWN-11',),
        'fn bad(mut xs: List[u64]) -> List[u64] { take xs }\n',
    ),
    NegativeControl(
        'move_view_param_return', 'move', ('OWN-7',),
        'fn bad(s: String) -> String { s }\n',
    ),
    NegativeControl(
        'move_generic_duplicate', 'move', ('OWN-5',),
        'fn bad[T](take x: T) -> (T, T) { (x, x) }\n',
    ),
    NegativeControl(
        'move_held_use_after_take_as', 'move', ('OWN-5',),
        'fn bad(take s: String) -> u64 {\n'
        '  let h: Held = code.hold(take s)\n'
        '  let _: Result[String, (code.TakeError, Held)] = code.take_as(take h)\n'
        '  let _: Result[u64, (code.TakeError, Held)] = code.take_as(take h)\n  0u64\n}\n',
    ),
    # (c) Escaping scoped borrows / handles / non-send / non-cross
    NegativeControl(
        'escape_view_block_local', 'escape', ('OWN-10',),
        'fn bad() -> u64 { string.byte_len({ let s = string.new(); view s }) }\n',
    ),
    NegativeControl(
        'escape_view_match_arm', 'escape', ('OWN-10',),
        'fn bad(take o: Option[String]) -> u64 {\n'
        '  let v = match o { Some(take s) => view s, None => return 0 }\n'
        '  string.byte_len(v)\n}\n',
    ),
    NegativeControl(
        'escape_view_return_missing_anchor', 'escape', ('OWN-20',),
        'fn bad(xs: List[String]) -> Option[View[String]] { list.get(xs, 0) }\n',
    ),
    NegativeControl(
        'escape_view_while_let', 'escape', ('OWN-17',),
        'fn bad(xs: List[String]) -> u64 {\n'
        '  let mut last: Option[View[String]] = None\n  let mut i: u64 = 0\n'
        '  while let Some(s) = list.get(xs, i) { last = Some(s); i += 1 }\n  0\n}\n',
    ),
    NegativeControl(
        'escape_view_in_own_struct', 'escape', ('OWN-19',),
        'type BadHolder { v: View[String] }\n',
    ),
    NegativeControl(
        'escape_view_in_list', 'escape', ('OWN-19',),
        'fn bad() -> List[View[String]] { list.new() }\n',
    ),
    NegativeControl(
        'escape_view_in_own_closure', 'escape', ('LANG-14',),
        'fn bad(s: String) -> own fn() -> u64 { let v = view s; fn() -> u64 { string.byte_len(v) } }\n',
    ),
    NegativeControl(
        'escape_scoped_cb_local_return', 'escape', ('OWN-21',),
        'fn bad(a: u64) -> (scoped fn() -> u64) in a { scoped fn() -> u64 { a } }\n',
    ),
    NegativeControl(
        'escape_isolate_mut_capture', 'escape', ('FAIL-16',),
        'fn bad(mut n: u64) -> u64 { let _ = isolate(fn() -> u64 { n = n + 1; n }); n }\n',
    ),
    NegativeControl(
        'escape_isolate_transitive_cb', 'escape', ('FAIL-16',),
        'fn bad(xs: List[u64]) -> u64 {\n'
        '  let mut seen: u64 = 0\n  let tick = scoped fn() { seen = seen + 1 }\n'
        '  let _ = isolate(fn() -> u64 { tick(); list.len(xs) })\n  seen\n}\n',
    ),
    NegativeControl(
        'escape_view_in_channel', 'escape', ('OWN-19',),
        'fn bad() { let _: (Sender[View[String]], Receiver[View[String]]) = channel.new(4) }\n',
    ),
    NegativeControl(
        'escape_rc_in_spawn', 'escape', ('CONC-2',),
        'type Node: shareable { n: u64 }\n'
        'fn bad(tasks: Tasks, take r: Rc[Node]) -> Task[u64] {\n'
        '  task.spawn(tasks, bytes 4096, take r, fn(take x: Rc[Node]) -> u64 { x.n })\n}\n',
    ),
    NegativeControl(
        'escape_arc_in_spawn_unit', 'escape', ('CONC-15',),
        'type Node: shareable + send { n: u64 }\n'
        'fn bad(tasks: Tasks, take a: Arc[Node]) {\n'
        '  let _ = task.spawn_unit(tasks, bytes 4096, take a, fn(take x: Arc[Node]) -> u64 { x.n })\n}\n',
    ),
    NegativeControl(
        'escape_held_non_send_in_spawn', 'escape', ('CONC-2',),
        'fn bad(tasks: Tasks, take h: Held) -> Task[u64] {\n'
        '  task.spawn(tasks, bytes 4096, take h, fn(take x: Held) -> u64 { let _ = take x; 1u64 })\n}\n',
    ),
    NegativeControl(
        'escape_task_handle_in_channel', 'escape', ('CONC-2',),
        'fn bad(tasks: Tasks) {\n'
        '  let h = task.spawn(tasks, bytes 4096, 1u64, fn(take x: u64) -> u64 { x })\n'
        '  let (tx, _rx): (Sender[Task[u64]], Receiver[Task[u64]]) = channel.new(2)\n'
        '  let _ = channel.try_send(tx, take h)\n}\n',
    ),
    NegativeControl(
        'escape_scoped_send_decl', 'escape', ('OWN-4', 'CONC-2'),
        'scoped type BadScoped: send { v: View[String] }\n',
    ),
    NegativeControl(
        'escape_region_text_out', 'escape', ('MEM-6',),
        'fn bad() -> Text { region r { text.copy_in(r, "hi") } }\n',
    ),
    # (d) Capability capture & ambient leakage
    NegativeControl(
        'cap_missing_clock_arg', 'capability', ('CAP-1',),
        'fn bad() -> u64 { host.mono_ns() }\n',
    ),
    NegativeControl(
        'cap_forge_opaque_dir', 'capability', ('PROG-2',),
        'fn bad() -> Dir { Dir(fd: 3) }\n',
    ),
    NegativeControl(
        'cap_const_dir_option', 'capability', ('CAP-1',),
        'const LEAK: Option[Dir] = None\n',
    ),
    NegativeControl(
        'cap_const_nested_clock_tuple', 'capability', ('CAP-1',),
        'const LEAK: (u64, Option[Clock]) = (1, None)\n',
    ),
    NegativeControl(
        'cap_const_compiler', 'capability', ('CAP-1',),
        'const LEAK: Option[Compiler] = None\n',
    ),
    NegativeControl(
        'cap_own_closure_dir', 'capability', ('LANG-14',),
        'fn bad(d: Dir) -> own fn(String) -> Result[HostFile, IoError] waits {\n'
        '  fn(p: String) -> Result[HostFile, IoError] waits { host.open(d, p, Read) }\n}\n',
    ),
    NegativeControl(
        'cap_own_closure_net', 'capability', ('LANG-14',),
        'fn bad(n: Net) -> own fn() -> NetDenials { fn() -> NetDenials { net.denials(n) } }\n',
    ),
    NegativeControl(
        'cap_own_closure_clock', 'capability', ('LANG-14',),
        'fn bad(c: Clock) -> own fn() -> u64 { fn() -> u64 { host.mono_ns(c) } }\n',
    ),
    NegativeControl(
        'cap_own_closure_random', 'capability', ('LANG-14',),
        'fn bad(r: Random) -> own fn() -> u64 {\n'
        '  fn() -> u64 { let mut b: Bytes = list.with_capacity(8); host.random(r, mut b); list.len(b) }\n}\n',
    ),
    NegativeControl(
        'cap_own_closure_exec', 'capability', ('LANG-14',),
        'fn bad(e: Exec) -> own fn(ExecRule) -> Result[Exec, RuleError] {\n'
        '  fn(r: ExecRule) -> Result[Exec, RuleError] { exec.narrow(e, r) }\n}\n',
    ),
    NegativeControl(
        'cap_own_closure_compiler', 'capability', ('LANG-14',),
        'fn bad(c: Compiler) -> own fn(compiler.CheckLibrary) -> Result[compiler.CheckedLibrary, compiler.Refusal] waits {\n'
        '  fn(req: compiler.CheckLibrary) -> Result[compiler.CheckedLibrary, compiler.Refusal] waits { compiler.check_library(c, req) }\n}\n',
    ),
    NegativeControl(
        'cap_own_closure_wrapped_generic', 'capability', ('LANG-14',),
        'type Wrap[T: plain] { item: T }\n'
        'fn bad[T: plain](w: Wrap[T]) -> own fn() -> u64 { fn() -> u64 { let _ = w; 1 } }\n',
    ),
    NegativeControl(
        'cap_codec_holds_dir', 'capability', ('OWN-4',),
        'type Leak: codec { d: Dir }\n',
    ),
    NegativeControl(
        'cap_spawn_closure_captures_clock', 'capability', ('LANG-14',),
        'fn bad(tasks: Tasks, c: Clock) -> Task[u64] {\n'
        '  task.spawn(tasks, bytes 4096, 1u64, fn(take x: u64) -> u64 { x + host.mono_ns(c) })\n}\n',
    ),
    NegativeControl(
        'cap_isolate_mut_clock', 'capability', ('FAIL-16',),
        'fn bad(mut c: Clock, c2: Clock) -> u64 {\n'
        '  let _ = isolate(fn() -> u64 { c = c2; host.mono_ns(c) }); host.mono_ns(c)\n}\n',
    ),
    NegativeControl(
        'cap_const_provide_random', 'capability', ('LANG-20',),
        'const R: u64 = { let r = provide.random(1u64); let mut b: Bytes = list.new(); host.random(r, mut b); list.len(b) }\n',
    ),
    NegativeControl(
        'cap_main_non_cap_param', 'capability', ('PROG-3',),
        'pub fn main(x: u64) -> Result[(), Exit] { Ok(()) }\n',
        entry='gen@1/main/main',
    ),
)


PRELUDE = '''// Generated by core/tools/ownership_soundness_gen.py, version {version}.
type Pair {{ left: List[u64], right: List[u64], label: String }}
type Outer {{ inner: Pair, extra: List[u64], tag: u64 }}
type Choice = Text(String) | PairVal(List[u64], List[u64]) | Num(u64)
type ShareBox: shareable + send {{ n: u64 }}
type UnitJob: cross {{ reply: Client[u64], seed: u64, should_fail: bool }}
type ChildWork: send {{ tx: Sender[u64], arc: Arc[ShareBox], held: Held[send], fail_kind: u64 }}
scoped type Cursor {{ input: View[String], pos: u64 }}
resource type Track {{ id: u64, note: String }} drop(t) {{ let _ = t.id }}

fn mix(h: u64, v: u64) -> u64 {{ ((h ^ v) % 4294967291u64) * 31u64 + 7u64 }}

fn mk_list(seed: u64, n: u64) -> List[u64] {{
  let mut xs: List[u64] = list.with_capacity(n)
  for i in 0u64 .. n {{ list.push(mut xs, (seed + i * 3u64) % 97u64) }}
  xs
}}

fn mk_pair(seed: u64) -> Pair {{
  Pair(left: mk_list(seed, 3u64), right: mk_list(seed + 5u64, 2u64), label: string.from("seed"))
}}

fn mk_outer(seed: u64) -> Outer {{
  Outer(inner: mk_pair(seed), extra: mk_list(seed + 11u64, 2u64), tag: seed % 19u64)
}}

fn two_mut(mut a: List[u64], mut b: List[u64], x: u64) -> u64 {{
  list.push(mut a, x)
  list.push(mut b, x + list.len(a))
  list.len(a) + list.len(b)
}}

fn mut_and_view(mut dst: List[u64], src: List[u64]) -> u64 {{
  let n = list.len(src)
  list.push(mut dst, n + 1u64)
  list.sum(dst)
}}

fn grow_view(mut s: String) -> View[String] in s {{
  string.push(mut s, "x")
  string.slice(s, 0u64, string.byte_len(s))
}}

fn make_cursor(s: String, pos: u64) -> Cursor in s {{ Cursor(input: view s, pos: pos) }}
fn apply_scoped(v: u64, f: scoped fn(u64) -> u64) -> u64 {{ f(v) }}
fn apply_view_cb(c: Clock, f: scoped[view] fn(Clock) -> u64) -> u64 {{ f(c) }}

fn try_keep_or_move(take s: String, fail_early: bool) -> Result[u64, u64] {{
  if fail_early {{
    let b = string.into_bytes(take s)
    return Error(list.len(b) + 1u64)
  }}
  Ok(string.byte_len(s) + 10u64)
}}

fn trigger_failure(kind: u64, xs: List[u64]) -> u64 {{
  if kind == 1u64 {{
    let hi: u64 = 18446744073709551615u64
    hi + 1u64
  }} else if kind == 2u64 {{
    xs[999u64]
  }} else if kind == 3u64 {{
    let z: u64 = 0u64
    17u64 / z
  }} else if kind == 4u64 {{
    let none_v: Option[u64] = None
    option.unwrap(none_v)
  }} else if kind == 5u64 {{
    fail("unwind")
  }} else {{
    list.len(xs) + 3u64
  }}
}}

fn fail_while_mut(mut p: Pair, kind: u64, cond: bool) -> u64 {{
  list.push(mut p.left, list.len(p.right) + 5u64)
  string.push(mut p.label, "!")
  let tr = result.unwrap(Track(id: 7u64, note: string.from("callee")))
  let mut tmp = string.from("callee_tmp")
  if cond {{ let _ = string.into_bytes(take tmp) }}
  let r = trigger_failure(kind, p.left)
  r + tr.id
}}

fn unit_worker(take job: UnitJob) -> u64 waits {{
  let UnitJob(reply: reply, seed: seed, should_fail: should_fail) = take job
  let mut local_s = string.from("unit")
  let mut acc = seed + 11u64
  let rval = region r {{
    let t = text.copy_in(r, local_s)
    let cb = scoped fn(d: u64) -> u64 {{ text.len(t) + d }}
    let iso = isolate(fn() -> u64 {{
      let tr = result.unwrap(Track(id: seed, note: string.from("u")))
      let base = cb(tr.id)
      if should_fail {{ fail("unit_iso") }}
      base + string.byte_len(tr.note)
    }})
    match iso {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs) + 100u64, Error(_) => 200u64 }}
  }}
  acc = mix(acc, rval)
  let _ = conn.send(reply, acc)
  if should_fail && (seed % 3u64 == 0u64) {{ let _ = trigger_failure(1u64, mk_list(seed, 1u64)) }}
  acc + 1u64
}}

fn child_worker(take work: ChildWork) -> u64 waits {{
  let ChildWork(tx: tx, arc: arc, held: held, fail_kind: fail_kind) = take work
  let tr = result.unwrap(Track(id: arc.n, note: string.from("child")))
  let recovered = match (code.take_as(take held) : Result[u64, (code.TakeError, Held[send])]) {{
    Ok(v) => v + 1000u64,
    Error((_, take back)) => match (code.take_as(take back) : Result[String, (code.TakeError, Held[send])]) {{
      Ok(take s) => string.byte_len(s) + 50u64,
      Error((_, take _drop)) => 9u64,
    }},
  }}
  let _ = channel.send(tx, recovered + arc.n)
  let xs = mk_list(arc.n, 2u64)
  let tail = trigger_failure(fail_kind, xs)
  recovered + tail + tr.id
}}

fn clock_read(c: Clock) -> u64 {{ host.mono_ns(c) + host.wall_ms(c) }}

fn rand_byte(r: Random, n: u64) -> u64 {{
  let mut b: Bytes = list.with_capacity(n)
  list.fill(mut b, n, 0u8)
  host.random(r, mut b)
  if list.len(b) > 0u64 {{ int.widen(b[0u64]) }} else {{ 0u64 }}
}}

fn cause_mask(causes: Causes) -> u64 {{
  let mut m: u64 = 0u64
  for c in causes {{
    m = m | match c.kind {{
      Overflow => 1u64, Bounds => 2u64, Division => 4u64, Unwrap => 8u64,
      Fail(_) => 16u64, Cancelled => 32u64, Capacity(_) => 64u64,
      OutOfTime => 128u64, Stack => 256u64,
    }}
  }}
  m
}}

fn emit_result(out: Stdout, idx: u64, val: u64) waits {{
  let mut line = string.new()
  string.push(mut line, "case ")
  string.push_u64(mut line, idx)
  string.push(mut line, " ")
  string.push_u64(mut line, val)
  string.push(mut line, "\\n")
  let _ = stdout.write(out, line)
}}
'''


@dataclass
class GeneratedProgram:
    seed: int
    surface: str
    kind: str  # 'valid' or 'mutated_unsound'
    mutation: str | None
    expected_rules: tuple | None
    stmts: list
    draws: list = field(default_factory=list)
    bounds: list = field(default_factory=list)

    def render_body(self, fn_name: str, emit_idx: int) -> str:
        body_lines = '\n'.join(f'  {line}' for stmt in self.stmts for line in stmt.splitlines())
        return (
            f'pub fn {fn_name}(out: Stdout, tasks: Tasks) -> u64 waits {{\n'
            f'  let mut acc: u64 = {(self.seed * 7 + 13) & 0xFFFF}u64\n'
            f'  let mut outer = mk_outer({self.seed & 0xFF}u64)\n'
            f'  let mut choice: Choice = Text(string.from("alpha"))\n'
            f'  let mut local_s = string.from("owned")\n'
            f'  let mut m: Map[u64, List[u64]] = map.new()\n'
            f'  map.insert(mut m, 1u64, mk_list(3u64, 2u64))\n'
            f'  let mut grid: List[List[u64]] = list.new()\n'
            f'  list.push(mut grid, mk_list(1u64, 2u64))\n'
            f'  list.push(mut grid, mk_list(2u64, 2u64))\n'
            f'  let flag = {"true" if (self.seed % 2) == 0 else "false"}\n'
            f'{body_lines}\n'
            f'  acc = mix(acc, list.len(outer.inner.left) + list.len(outer.inner.right) '
            f'+ string.byte_len(outer.inner.label) + map.len(m) + list.len(grid))\n'
            f'  emit_result(out, {emit_idx}u64, acc)\n'
            f'  acc\n'
            f'}}\n'
        )

    def render_standalone(self) -> str:
        return (
            PRELUDE.format(version=VERSION)
            + '\n'
            + self.render_body('case_0', 0)
            + '\npub fn main(out: Stdout, tasks: Tasks) -> Result[(), Exit] waits {\n'
            '  let _ = case_0(out, tasks)\n  Ok(())\n}\n'
        )


def render_batch(programs: list[GeneratedProgram]) -> str:
    parts = [PRELUDE.format(version=VERSION)]
    for idx, prog in enumerate(programs):
        parts.append(prog.render_body(f'case_{idx}', idx))
    dispatch = '\n'.join(f'  if which == {idx}u64 {{ let _ = case_{idx}(out, tasks) }}' for idx in range(len(programs)))
    parts.append(
        'pub fn main(argv: Args, out: Stdout, tasks: Tasks) -> Result[(), Exit] waits {\n'
        '  let a0 = option.unwrap(args.get(argv, 0))\n'
        '  let p0: Result[u64, ParseError] = int.parse(a0)\n'
        '  let which = result.unwrap(p0)\n'
        f'{dispatch}\n  Ok(())\n}}\n'
    )
    return '\n'.join(parts)


# --- Valid multi-feature & unwinding grammar builders ---

def valid_aliasing_stmt(stream: Stream, k: int) -> str:
    choice = stream.below(6)
    v = 1 + stream.below(29)
    if choice == 0:
        return f'acc = mix(acc, two_mut(mut outer.inner.left, mut outer.inner.right, {v}u64))'
    if choice == 1:
        return (
            f'{{ let gv{k} = grow_view(mut outer.inner.label); acc = mix(acc, string.byte_len(gv{k})) }}\n'
            f'string.push(mut outer.inner.label, "z")'
        )
    if choice == 2:
        return (
            f'match choice {{\n'
            f'  Text(mut s{k}) => {{ string.push(mut s{k}, "!"); acc = mix(acc, string.byte_len(s{k})) }},\n'
            f'  PairVal(mut a{k}, mut b{k}) => {{ acc = mix(acc, two_mut(mut a{k}, mut b{k}, {v}u64)) }},\n'
            f'  Num(mut n{k}) => {{ n{k} = n{k} + {v}u64; acc = mix(acc, n{k}) }},\n}}'
        )
    if choice == 3:
        return (
            f'let mut bx{k} = box.new(mk_pair({v}u64))\n'
            f'acc = mix(acc, mut_and_view(mut bx{k}.left, outer.extra))\n'
            f'let old{k} = replace(mut bx{k}.label, string.from("bx"))\n'
            f'acc = mix(acc, string.byte_len(old{k}))'
        )
    if choice == 4:
        key = 1 + stream.below(4)
        return (
            f'let u{k} = map.update_or_insert(mut m, {key}u64, mk_list({v}u64, 1u64), '
            f'scoped fn(mut xs{k}: List[u64]) -> u64 {{ list.push(mut xs{k}, {v}u64); list.len(xs{k}) }})\n'
            f'acc = mix(acc, u{k})'
        )
    return (
        f'let mut rc{k} = rc.new(ShareBox(n: {v}u64))\n'
        f'let rc_dup{k} = rc.share(rc{k})\n'
        f'let miss{k} = rc.with_unique(mut rc{k}, scoped fn(mut sb{k}: ShareBox) -> u64 {{ sb{k}.n = sb{k}.n + 1u64; sb{k}.n }})\n'
        f'let _ = take rc_dup{k}\n'
        f'let hit{k} = rc.with_unique(mut rc{k}, scoped fn(mut sb{k}: ShareBox) -> u64 {{ sb{k}.n = sb{k}.n + 5u64; sb{k}.n }})\n'
        f'acc = mix(acc, match miss{k} {{ Some(x) => x, None => 1u64 }} + match hit{k} {{ Some(x) => x, None => 0u64 }})'
    )


def valid_move_stmt(stream: Stream, k: int) -> str:
    choice = stream.below(5)
    v = 1 + stream.below(19)
    if choice == 0:
        return (
            f'{{ let mut ms{k} = string.from("dropflag")\n'
            f'  if flag {{ let b{k} = string.into_bytes(take ms{k}); acc = mix(acc, list.len(b{k})) }}\n}}'
        )
    if choice == 1:
        trips = 1 + stream.below(4)
        return (
            f'let mut cur{k} = string.from("loop")\n'
            f'let mut i{k}: u64 = 0u64\n'
            f'while i{k} < {trips}u64 {{\n'
            f'  let b{k} = string.into_bytes(take cur{k})\n'
            f'  acc = mix(acc, list.len(b{k}) + i{k})\n'
            f'  cur{k} = string.from("next")\n  i{k} = i{k} + 1u64\n}}\n'
            f'acc = mix(acc, string.byte_len(cur{k}))'
        )
    if choice == 2:
        return (
            f'let tp{k} = mk_pair({v}u64)\n'
            f'let Pair(left: dl{k}, right: dr{k}, label: ds{k}) = take tp{k}\n'
            f'let prev{k} = replace(mut outer.inner.label, take ds{k})\n'
            f'acc = mix(acc, list.len(dl{k}) + list.len(dr{k}) + string.byte_len(prev{k}))'
        )
    if choice == 3:
        return (
            f'let owned{k} = mk_list({v}u64, 3u64)\n'
            f'let cl{k}: own fn(u64) -> u64 = fn(x: u64) -> u64 {{ list.sum(owned{k}) + x }}\n'
            f'acc = mix(acc, cl{k}({v}u64) + cl{k}({v + 1}u64))'
        )
    return (
        f'let es{k} = string.from("early")\n'
        f'let er{k} = try_keep_or_move(take es{k}, flag)\n'
        f'acc = mix(acc, match er{k} {{ Ok(x) => x, Error(e) => e }})'
    )


def valid_escape_stmt(stream: Stream, k: int) -> str:
    choice = stream.below(4)
    v = 1 + stream.below(23)
    if choice == 0:
        return (
            f'{{ let c{k} = make_cursor(outer.inner.label, {v}u64)\n'
            f'  let c2_{k} = c{k}\n'
            f'  acc = mix(acc, string.byte_len(c2_{k}.input) + c2_{k}.pos)\n}}\n'
            f'string.push(mut outer.inner.label, "k")'
        )
    if choice == 1:
        return (
            f'let mut words{k}: List[String] = list.new()\n'
            f'list.push(mut words{k}, string.from("a"))\n'
            f'list.push(mut words{k}, string.from("bc"))\n'
            f'{{ let mut found{k}: Option[View[String]] in words{k} = None\n'
            f'  for w{k} in words{k} {{ if string.byte_len(w{k}) > 1u64 {{ found{k} = Some(w{k}) }} }}\n'
            f'  acc = mix(acc, match found{k} {{ Some(w) => string.byte_len(w), None => 0u64 }})\n}}\n'
            f'list.clear(mut words{k})'
        )
    if choice == 2:
        return (
            f'let mut ln{k}: u64 = {v}u64\n'
            f'let bump{k} = scoped fn(d: u64) -> u64 {{ ln{k} = ln{k} + d; ln{k} }}\n'
            f'acc = mix(acc, apply_scoped(3u64, bump{k}))\n'
            f'let snap{k} = isolate(fn() -> u64 {{ ln{k} + list.len(outer.inner.left) }})\n'
            f'acc = mix(acc, match snap{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    return (
        f'let (tx1_{k}, rx1_{k}): (Sender[u64], Receiver[u64]) = channel.new(2u64)\n'
        f'let (tx2_{k}, rx2_{k}): (Sender[String], Receiver[String]) = channel.new(2u64)\n'
        f'let _ = channel.try_send(tx1_{k}, {v}u64)\n'
        f'let _ = channel.try_send(tx2_{k}, string.from("msg"))\n'
        f'let g1_{k} = match channel.try_receive(rx1_{k}) {{ Ok(x) => x, Error(_) => 0u64 }}\n'
        f'let g2_{k} = match channel.try_receive(rx2_{k}) {{ Ok(take s) => string.byte_len(s), Error(_) => 0u64 }}\n'
        f'acc = mix(acc, g1_{k} + g2_{k})'
    )


def valid_capability_stmt(stream: Stream, k: int) -> str:
    choice = stream.below(4)
    v = 1 + stream.below(97)
    if choice == 0:
        return (
            f'let (clk{k}, _hand{k}) = provide.clock({v * 10}u64, {v * 20}u64)\n'
            f'let cb{k}: scoped[view] fn(Clock) -> u64 = scoped fn(c: Clock) -> u64 {{ clock_read(c) }}\n'
            f'let iv{k} = isolate(fn() -> u64 {{ apply_view_cb(clk{k}, cb{k}) }})\n'
            f'acc = mix(acc, match iv{k} {{ Ok(Ok(x)) => x, _ => 0u64 }})'
        )
    if choice == 1:
        return f'let rng{k} = provide.random({v}u64)\nacc = mix(acc, rand_byte(rng{k}, 8u64))'
    if choice == 2:
        return (
            f'let (d{k}, _ds{k}) = provide.dir(bytes 4096)\n'
            f'let _ro{k} = host.read_only(d{k})\n'
            f'let (ex{k}, _exs{k}) = provide.exec(bytes 4096)\n'
            f'let _ = exec.narrow(ex{k}, Programs(names: "echo"))\n'
            f'acc = mix(acc, {v}u64)'
        )
    return (
        f'let (n{k}, _ns{k}) = provide.net(bytes 4096)\n'
        f'let dn{k} = net.denials(n{k})\n'
        f'let _cls{k} = net.class(V4(2130706433u32))\n'
        f'acc = mix(acc, dn{k}.not_public + dn{k}.own_address + {v}u64)'
    )


def valid_interaction_stmt(stream: Stream, k: int) -> str:
    """Multi-feature interactions: closures x regions x isolate x units x spawn/channel x Held."""
    choice = stream.below(7)
    v = 1 + stream.below(41)
    fail_kind = stream.below(6)
    if choice == 6:
        # match mut variant payload x scoped fn x region
        return (
            f'let mri{k} = region mr{k} {{\n'
            f'  let rt{k} = text.copy_in(mr{k}, outer.inner.label)\n'
            f'  match choice {{\n'
            f'    Text(mut ms{k}) => {{\n'
            f'      let cb{k} = scoped fn(d: u64) -> u64 {{ string.push(mut ms{k}, "!"); string.byte_len(ms{k}) + text.len(rt{k}) + d }}\n'
            f'      apply_scoped({v}u64, cb{k})\n    }},\n'
            f'    _ => text.len(rt{k}),\n  }}\n}}\n'
            f'acc = mix(acc, mri{k})'
        )
    if choice == 0:
        # closures x nested regions x isolate x unwinding
        return (
            f'let mut reg_acc{k}: u64 = {v}u64\n'
            f'let r_out{k} = region r1_{k} {{\n'
            f'  let t1_{k} = text.copy_in(r1_{k}, outer.inner.label)\n'
            f'  let own_c{k} = mk_list({v}u64, 2u64)\n'
            f'  let ofn{k}: own fn(u64) -> u64 = fn(x: u64) -> u64 {{ list.sum(own_c{k}) + x }}\n'
            f'  let sfn{k} = scoped fn(d: u64) -> u64 {{ reg_acc{k} = reg_acc{k} + text.len(t1_{k}) + d; reg_acc{k} }}\n'
            f'  let step1_{k} = apply_scoped(ofn{k}(2u64), sfn{k})\n'
            f'  let inner_{k} = region r2_{k} {{\n'
            f'    let t2_{k} = text.copy_in(r2_{k}, string.from("nested"))\n'
            f'    let iso{k} = isolate(fn() -> u64 {{\n'
            f'      let tr{k} = result.unwrap(Track(id: {v}u64, note: string.from("r2")))\n'
            f'      let base{k} = text.len(t1_{k}) + text.len(t2_{k}) + ofn{k}(tr{k}.id)\n'
            f'      base{k} + trigger_failure({fail_kind}u64, outer.inner.left)\n    }})\n'
            f'    match iso{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs) + step1_{k}, Error(_) => 77u64 }}\n  }}\n'
            f'  inner_{k} + text.len(t1_{k})\n}}\n'
            f'acc = mix(acc, r_out{k} + reg_acc{k})'
        )
    if choice == 1:
        # units (spawn_unit + conn) x regions x scoped_fn x isolate
        should_fail = 'true' if stream.below(2) == 0 else 'false'
        return (
            f'let (ucli{k}, ubox{k}): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(1u64, bytes 0))\n'
            f'let ujob{k} = UnitJob(reply: take ucli{k}, seed: {v}u64, should_fail: {should_fail})\n'
            f'let uh{k} = result.unwrap(task.spawn_unit(tasks, bytes 65536, take ujob{k}, unit_worker))\n'
            f'let urep{k} = match conn.receive(ubox{k}) {{ Ok(x) => x, Error(_) => 0u64 }}\n'
            f'let ujoin{k} = match task.join(take uh{k}) {{ Ok(x) => x, Error(_) => 11u64 }}\n'
            f'acc = mix(acc, urep{k} + ujoin{k})'
        )
    if choice == 2:
        # spawn x channel x Arc x Held[send] x child failure unwinding
        use_u64 = stream.below(2) == 0
        held_init = f'code.hold({v}u64)' if use_u64 else 'code.hold(string.from("held_send"))'
        return (
            f'let (ctx{k}, crx{k}): (Sender[u64], Receiver[u64]) = channel.new(2u64)\n'
            f'let carc{k} = arc.new(ShareBox(n: {v}u64))\n'
            f'let cheld{k}: Held[send] = {held_init}\n'
            f'let cwork{k} = ChildWork(tx: take ctx{k}, arc: arc.share(carc{k}), held: take cheld{k}, fail_kind: {fail_kind}u64)\n'
            f'let ch{k} = task.spawn(tasks, bytes 65536, take cwork{k}, child_worker)\n'
            f'let cmsg{k} = match channel.receive(crx{k}) {{ Ok(x) => x, Error(_) => 0u64 }}\n'
            f'let cjoin{k} = match task.join(take ch{k}) {{\n'
            f'  Ok(x) => x,\n'
            f'  Error(Failed(cs)) => cause_mask(cs) + 500u64,\n'
            f'  Error(Cancelled(cs)) => cause_mask(cs) + 900u64,\n}}\n'
            f'acc = mix(acc, cmsg{k} + cjoin{k} + carc{k}.n)'
        )
    if choice == 3:
        # Held custody x wrong-type recovery x match-mut x scoped callback
        return (
            f'let hp{k}: Held = code.hold(mk_pair({v}u64))\n'
            f'let hres{k} = match (code.take_as(take hp{k}) : Result[u64, (code.TakeError, Held)]) {{\n'
            f'  Ok(x) => x,\n'
            f'  Error((_, take back{k})) => match (code.take_as(take back{k}) : Result[Pair, (code.TakeError, Held)]) {{\n'
            f'    Ok(take p{k}) => {{ let mut mp{k} = take p{k}; two_mut(mut mp{k}.left, mut mp{k}.right, {v}u64) + string.byte_len(mp{k}.label) }},\n'
            f'    Error((_, take _d{k})) => 0u64,\n  }},\n}}\n'
            f'acc = mix(acc, hres{k})'
        )
    if choice == 4:
        # virtual capabilities x region x scoped[view] fn x isolate
        return (
            f'let (iclk{k}, _ih{k}) = provide.clock({v * 7}u64, {v * 13}u64)\n'
            f'let irng{k} = provide.random({v}u64)\n'
            f'let ireg{k} = region ir{k} {{\n'
            f'  let it{k} = text.copy_in(ir{k}, outer.inner.label)\n'
            f'  let vcb{k}: scoped[view] fn(Clock) -> u64 = scoped fn(c: Clock) -> u64 {{ host.mono_ns(c) + text.len(it{k}) }}\n'
            f'  let iiso{k} = isolate(fn() -> u64 {{ apply_view_cb(iclk{k}, vcb{k}) + rand_byte(irng{k}, 4u64) }})\n'
            f'  match iiso{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }}\n}}\n'
            f'acc = mix(acc, ireg{k})'
        )
    # Multi-channel + two spawned tasks + isolate + resource cleanup
    return (
        f'let (mx1_{k}, mr1_{k}): (Sender[u64], Receiver[u64]) = channel.new(2u64)\n'
        f'let (mx2_{k}, mr2_{k}): (Sender[u64], Receiver[u64]) = channel.new(2u64)\n'
        f'let mt1_{k} = task.spawn(tasks, bytes 65536, take mx1_{k}, fn(take tx: Sender[u64]) -> u64 waits {{ let _ = channel.send(tx, {v}u64); {v + 1}u64 }})\n'
        f'let mt2_{k} = task.spawn(tasks, bytes 65536, take mx2_{k}, fn(take tx: Sender[u64]) -> u64 waits {{ let _ = channel.send(tx, {v + 2}u64); {v + 3}u64 }})\n'
        f'let mv1_{k} = match channel.receive(mr1_{k}) {{ Ok(x) => x, Error(_) => 0u64 }}\n'
        f'let mv2_{k} = match channel.receive(mr2_{k}) {{ Ok(x) => x, Error(_) => 0u64 }}\n'
        f'let mj1_{k} = match task.join(take mt1_{k}) {{ Ok(x) => x, Error(_) => 0u64 }}\n'
        f'let mj2_{k} = match task.join(take mt2_{k}) {{ Ok(x) => x, Error(_) => 0u64 }}\n'
        f'acc = mix(acc, mv1_{k} + mv2_{k} + mj1_{k} + mj2_{k})'
    )


def valid_unwinding_stmt(stream: Stream, k: int) -> str:
    """Failure-path unwinding (Overflow/Bounds/Division/Unwrap/fail/Capacity) across moved/maybe/lent slots."""
    choice = stream.below(8)
    fail_kind = 1 + stream.below(5)  # 1=Overflow, 2=Bounds, 3=Division, 4=Unwrap, 5=fail()
    v = 1 + stream.below(31)
    if choice == 6:
        # T3.1: replace RHS failure while LHS place is reserved.
        return (
            f'let uw{k} = isolate(fn() -> u64 {{\n'
            f'  let mut rp{k} = mk_pair({v}u64)\n'
            f'  let _ = replace(mut rp{k}.label, {{ let _ = trigger_failure({fail_kind}u64, rp{k}.left); string.from("x") }})\n'
            f'  string.byte_len(rp{k}.label)\n}})\n'
            f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    if choice == 7:
        # Partial-arm match move (`Some(take t)`) drop-flag unwinding on Option[Track].
        return (
            f'let uw{k} = isolate(fn() -> u64 {{\n'
            f'  let opt{k}: Option[Track] = if flag {{ Some(result.unwrap(Track(id: {v}u64, note: string.from("o")))) }} else {{ None }}\n'
            f'  let got{k} = match opt{k} {{ Some(take t) => t.id, None => 0u64 }}\n'
            f'  got{k} + trigger_failure({fail_kind}u64, outer.inner.left)\n}})\n'
            f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    if choice == 0:
        # Unwind past moved-from slot + live resource + live List/String (tests wf::own R1/R2).
        return (
            f'let uw{k} = isolate(fn() -> u64 {{\n'
            f'  let live_tr{k} = result.unwrap(Track(id: {v}u64, note: string.from("live")))\n'
            f'  let moved_s{k} = string.from("gone")\n'
            f'  let sink{k} = string.into_bytes(take moved_s{k})\n'
            f'  let r{k} = trigger_failure({fail_kind}u64, outer.inner.left)\n'
            f'  r{k} + live_tr{k}.id + list.len(sink{k})\n}})\n'
            f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    if choice == 1:
        # Unwind past partially initialized slot + maybe-moved drop-flag slot.
        return (
            f'let uw{k} = isolate(fn() -> u64 {{\n'
            f'  let mut maybe_s{k} = string.from("maybe")\n'
            f'  let mut uninit_s{k}: String\n'
            f'  let mut tr{k} = result.unwrap(Track(id: {v}u64, note: string.from("df")))\n'
            f'  if flag {{\n'
            f'    let _ = string.into_bytes(take maybe_s{k})\n'
            f'    uninit_s{k} = string.from("now_live")\n  }}\n'
            f'  let r{k} = trigger_failure({fail_kind}u64, outer.inner.left)\n'
            f'  r{k} + tr{k}.id\n}})\n'
            f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    if choice == 2:
        # Unwind through lent mut helper place (caller's Pair survives with partial mutation).
        return (
            f'let uw{k} = isolate(fn() -> u64 {{\n'
            f'  let mut local_p{k} = mk_pair({v}u64)\n'
            f'  let _ = fail_while_mut(mut local_p{k}, {fail_kind}u64, flag)\n'
            f'  list.len(local_p{k}.left)\n}})\n'
            f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    if choice == 3:
        # Unwind through active `match` mut variant payload loan + local resource.
        return (
            f'let uw{k} = isolate(fn() -> u64 {{\n'
            f'  let mut lc{k}: Choice = Text(string.from("lent_variant"))\n'
            f'  let tr{k} = result.unwrap(Track(id: {v}u64, note: string.from("var")))\n'
            f'  match lc{k} {{\n'
            f'    Text(mut s{k}) => {{ string.push(mut s{k}, "!"); trigger_failure({fail_kind}u64, outer.inner.left) }},\n'
            f'    _ => tr{k}.id,\n  }}\n}})\n'
            f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    if choice == 4:
        # Capacity unwinding inside `bound bytes` holding moved-from + maybe-live + resource.
        return (
            f'let uw{k} = isolate(fn() -> u64 {{\n'
            f'  let br{k} = bound bytes 256 {{\n'
            f'    let tr{k} = result.unwrap(Track(id: {v}u64, note: string.from("cap")))\n'
            f'    let mut ms{k} = string.from("cap_s")\n'
            f'    if flag {{ let _ = string.into_bytes(take ms{k}) }}\n'
            f'    let big{k}: List[u64] = list.with_capacity(2048u64)\n'
            f'    list.len(big{k}) + tr{k}.id\n  }}\n'
            f'  match br{k} {{ Ok(x) => x, Error(_) => 44u64 }}\n}})\n'
            f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
        )
    # Loop mid-move unwinding + Held custody unwind.
    return (
        f'let uw{k} = isolate(fn() -> u64 {{\n'
        f'  let mut cur{k} = string.from("loop_uw")\n'
        f'  let held_uw{k}: Held = code.hold(result.unwrap(Track(id: {v}u64, note: string.from("h"))))\n'
        f'  let mut idx{k}: u64 = 0u64\n'
        f'  while idx{k} < 3u64 {{\n'
        f'    let bytes{k} = string.into_bytes(take cur{k})\n'
        f'    if idx{k} == 1u64 {{ let _ = trigger_failure({fail_kind}u64, outer.inner.left) }}\n'
        f'    cur{k} = string.from("reinit")\n'
        f'    idx{k} = idx{k} + 1u64 + (list.len(bytes{k}) & 0u64)\n  }}\n'
        f'  let _ = take held_uw{k}\n'
        f'  string.byte_len(cur{k})\n}})\n'
        f'acc = mix(acc, match uw{k} {{ Ok(Ok(x)) => x, Ok(Error(cs)) => cause_mask(cs), Error(_) => 0u64 }})'
    )


VALID_BUILDERS = {
    'aliasing': valid_aliasing_stmt,
    'move': valid_move_stmt,
    'escape': valid_escape_stmt,
    'capability': valid_capability_stmt,
    'interaction': valid_interaction_stmt,
    'unwinding': valid_unwinding_stmt,
}

# Provably unsound mutations injected into complex multi-feature bodies.
UNSOUND_MUTATIONS = {
    'aliasing': (
        ('mut_alias_mut_mut', ('OWN-9',), 'acc = mix(acc, two_mut(mut outer.inner.left, mut outer.inner.left, 1u64))'),
        ('mut_alias_mut_view', ('OWN-9',), 'acc = mix(acc, mut_and_view(mut outer.inner.left, outer.inner.left))'),
        ('mut_alias_view_freeze', ('OWN-10',), 'let fv_m = view outer.inner.left\nlist.push(mut outer.inner.left, 1u64)\nacc = mix(acc, list.len(fv_m))'),
        ('mut_alias_reborrow_freeze', ('OWN-10',), 'let rv_m = grow_view(mut outer.inner.label)\nstring.push(mut outer.inner.label, "!")\nacc = mix(acc, string.byte_len(rv_m))'),
        ('mut_alias_match_retag', ('OWN-9',), 'match choice { Text(mut ms_m) => { string.push(mut ms_m, "a"); choice = Num(9u64) }, _ => {} }'),
        ('mut_alias_map_capture', ('OWN-9',), 'let _ = map.update(mut m, 1u64, fn(mut mv_m: List[u64]) { list.push(mut mv_m, map.len(m)) })'),
        ('mut_alias_index_overlap', ('OWN-9',), 'list.append(mut grid[0u64], grid[1u64])'),
    ),
    'move': (
        ('mut_move_after_take', ('OWN-5',), 'let moved_m = take local_s\nacc = mix(acc, string.byte_len(moved_m) + string.byte_len(local_s))'),
        ('mut_move_branch_maybe', ('OWN-16',), 'if flag { let _ = string.into_bytes(take local_s) }\nacc = mix(acc, string.byte_len(local_s))'),
        ('mut_move_loop_backedge', ('OWN-17',), 'let mut lk_m = 0u64\nwhile lk_m < 2u64 { let _ = string.into_bytes(take local_s); lk_m = lk_m + 1u64 }'),
        ('mut_move_closure_then_read', ('OWN-5',), 'let cf_m: own fn() -> u64 = fn() -> u64 { string.byte_len(local_s) }\nacc = mix(acc, cf_m() + string.byte_len(local_s))'),
        ('mut_move_partial_field', ('OWN-14',), 'let pf_m = take outer.inner.label\nacc = mix(acc, string.byte_len(pf_m))'),
    ),
    'escape': (
        ('mut_escape_block_view', ('OWN-10',), 'let ev_m = { let tmp_m = string.from("x"); view tmp_m }\nacc = mix(acc, string.byte_len(ev_m))'),
        ('mut_escape_own_closure_view', ('LANG-14',), 'let ev_m = view outer.inner.label\nlet bf_m: own fn() -> u64 = fn() -> u64 { string.byte_len(ev_m) }\nacc = mix(acc, bf_m())'),
        ('mut_escape_isolate_mut', ('FAIL-16',), 'let _ = isolate(fn() -> u64 { acc = acc + 1u64; acc })'),
        ('mut_escape_channel_view', ('OWN-19',), 'let (_tx_m, _rx_m): (Sender[View[String]], Receiver[View[String]]) = channel.new(2u64)'),
        ('mut_escape_spawn_rc', ('CONC-2',), 'let rc_m = rc.new(ShareBox(n: acc))\nlet _ = task.spawn(tasks, bytes 4096, take rc_m, fn(take x: Rc[ShareBox]) -> u64 { x.n })'),
        ('mut_escape_region_text', ('OWN-19', 'MEM-6'), 'let rt_m: Text = region r_m { text.copy_in(r_m, outer.inner.label) }\nacc = mix(acc, text.len(rt_m))'),
        ('mut_escape_held_non_send', ('CONC-2',), 'let hm: Held = code.hold(string.from("ns"))\nlet _ = task.spawn(tasks, bytes 4096, take hm, fn(take x: Held) -> u64 { let _ = take x; 1u64 })'),
    ),
    'capability': (
        ('mut_cap_closure_clock', ('LANG-14',), 'let (clk_m, _hm) = provide.clock(1u64, 1u64)\nlet bf_m: own fn() -> u64 = fn() -> u64 { host.mono_ns(clk_m) }\nacc = mix(acc, bf_m())'),
        ('mut_cap_closure_dir', ('LANG-14',), 'let (dir_m, _sm) = provide.dir(bytes 4096)\nlet bf_m: own fn() -> u64 = fn() -> u64 { let _ = host.read_only(dir_m); 1u64 }\nacc = mix(acc, bf_m())'),
        ('mut_cap_closure_net', ('LANG-14',), 'let (net_m, _sm) = provide.net(bytes 4096)\nlet bf_m: own fn() -> u64 = fn() -> u64 { net.denials(net_m).not_public }\nacc = mix(acc, bf_m())'),
        ('mut_cap_closure_random', ('LANG-14',), 'let rnd_m = provide.random(3u64)\nlet bf_m: own fn() -> u64 = fn() -> u64 { rand_byte(rnd_m, 4u64) }\nacc = mix(acc, bf_m())'),
        ('mut_cap_closure_exec', ('LANG-14',), 'let (ex_m, _sm) = provide.exec(bytes 4096)\nlet bf_m: own fn() -> u64 = fn() -> u64 { let _ = exec.narrow(ex_m, Programs(names: "x")); 1u64 }\nacc = mix(acc, bf_m())'),
        ('mut_cap_missing_arg', ('CAP-1',), 'acc = mix(acc, host.mono_ns())'),
        ('mut_cap_isolate_mut', ('FAIL-16',), 'let p1_m = provide.clock(1u64, 1u64)\nlet mut c1_m = p1_m.0\nlet (c2_m, _h2m) = provide.clock(2u64, 2u64)\nlet _ = isolate(fn() -> u64 { c1_m = c2_m; host.mono_ns(c1_m) })'),
        ('mut_cap_spawn_capture', ('LANG-14',), 'let (clk_m, _hm) = provide.clock(1u64, 1u64)\nlet _ = task.spawn(tasks, bytes 4096, 1u64, fn(take x: u64) -> u64 { x + host.mono_ns(clk_m) })'),
    ),
}

# Weighted surface schedule: heavily bias toward multi-feature interactions and failure-path unwinding.
SURFACE_WEIGHTS = (
    'interaction', 'interaction', 'interaction',
    'unwinding', 'unwinding', 'unwinding',
    'aliasing', 'move', 'escape', 'capability',
)


def generate_program(seed: int, draws=None, forced_surface: str | None = None,
                     forced_kind: str | None = None) -> GeneratedProgram:
    stream = Stream(seed, draws)
    # Consume surface and kind draws unconditionally so draw-list replay stays aligned.
    rolled_surface = stream.pick(SURFACE_WEIGHTS)
    surface = forced_surface or rolled_surface
    # ~80% frontend-accepted multi-feature/unwinding programs, ~20% unsound soundness probes.
    kind_roll = stream.below(20)
    kind = forced_kind or ('valid' if kind_roll < 16 else 'mutated_unsound')
    stmt_count = 2 + stream.below(4)
    stmts = []
    for k in range(stmt_count):
        secondary = stream.pick(SURFACE_WEIGHTS)
        s = surface if (k == 0 or stream.below(3) != 0) else secondary
        stmts.append(VALID_BUILDERS[s](stream, k))
    mutation = None
    expected_rules = None
    if kind == 'mutated_unsound':
        mut_surface = surface if surface in UNSOUND_MUTATIONS else stream.pick(CHECKER_SURFACES)
        mut_name, rules, mut_stmt = stream.pick(UNSOUND_MUTATIONS[mut_surface])
        mutation = mut_name
        expected_rules = rules
        pos = stream.below(len(stmts) + 1)
        stmts.insert(pos, mut_stmt)
    return GeneratedProgram(
        seed=seed, surface=surface, kind=kind, mutation=mutation,
        expected_rules=expected_rules, stmts=stmts,
        draws=list(stream.drawn), bounds=list(stream.bounds),
    )
