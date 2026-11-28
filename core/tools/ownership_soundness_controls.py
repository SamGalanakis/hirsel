#!/usr/bin/env python3
"""Curated negative controls for the Hirsel ownership soundness fuzzer.

Every entry is a program the checker must refuse with the named rule: aliasing, move, escape and
capability controls, plus the controls of the six feature families in
ownership_soundness_features.py.
"""
from dataclasses import dataclass

from ownership_soundness_features import FEATURE_CONTROLS


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

NEGATIVE_CONTROLS += tuple(NegativeControl(name, surface, rules, source) for name, surface, rules, source in FEATURE_CONTROLS)
