# Ownership

## Checker state

A **slot** is a local binding, a parameter or a statement temporary. A **place** is a slot
followed by zero or more field, index, variant-payload and `Box`-payload steps
(`m.items[i]`). Checking a definition reads its body and the declarations it names
([LANG-8](language.md#lang-8-signatures)) in one pass in source order, and keeps exactly
this state; every rule of this specification that refuses a body is decided from it:

- the **slot table**: each slot's type, mode, writability, availability
  ([OWN-5](#own-5-availability)) and whether it is flagged ([OWN-18](#own-18-drop-flags));
- the **scope stack**: the open lexical scopes, each with its slots in declaration order,
  and the open regions, limits and labels;
- the **freeze records**: for each live view, its anchor places and extent
  ([OWN-10](#own-10-freezes)), and for each reservation its place and end
  ([OWN-11](#own-11-call-arguments), [OWN-12](#own-12-assignment));
- the **anchor sets**: for each scoped value, the places and regions it reads
  ([OWN-19](#own-19-scoped-values));
- the **outlives relation**: the reflexive transitive relation over the region parameters
  of the signature, closed once from its `where` clauses, and extended by the lexical scope
  tree and parameter-outlives-local edges as regions open
  ([MEM-6](memory.md#mem-6-roots)); root and placement checks read it and the declared type
  roots;
- the **typing context**: the signature, the expected type at each point
  ([LANG-11](language.md#lang-11-typing-is-local)), and at each open call the type and
  region arguments found so far and its unsuffixed literal arguments, pending until all
  the call's arguments are seen;
- the **accumulators**: for the definition and each open closure body, whether it has made a
  `waits` call ([LANG-8](language.md#lang-8-signatures)), and for each open closure its
  captures in first-naming order ([LANG-14](language.md#lang-14-captures));
- the **capture descriptors**: for each scoped callable value, the whole slots or formal
  anchors it captures, each with its mode (view or `mut`) and lexical scope, and the loans
  each activation holds ([OWN-21](#own-21-scoped-callbacks));
- the **construction frames**: for each open `emit` literal, its type and next field, and
  whether the current expression is a `?` operand or in tail position
  ([MEM-4](memory.md#mem-4-sized-destinations));
- the published **type summaries** ([OWN-2](#own-2-type-summaries)) and **constant values**
  ([LANG-20](language.md#lang-20-constants)), which declaration preprocessing computes once
  ([index](index.md)) and checking reads and never recomputes.

At a join ([OWN-16](#own-16-joins)), availability joins by its table, freeze records,
reservations, anchor sets and capture descriptors join by union, `mut` dominating view, and the accumulators by
"or" and concatenation. How an implementation represents this state, and how it emits drop
flags, is its choice; neither changes what is accepted.

### OWN-1: Representation is declared once

A nominal type's declaration fixes the representation of every value of that type, and
every use inherits it. `type T` is **own**, or plain (OWN-2); `stable type T` is **stable**
(region data, [MEM-1](memory.md#mem-1-regions)); `resource type T` is a **resource** (affine,
with a finalizer, [FAIL-11](failure.md#fail-11-resources)); `scoped type T` is scoped
([OWN-22](#own-22-scoped-types)). A use never writes a representation; it writes only a
mode (`view`, `mut`, `take`, `copy`) and a place (`in r`). A type needed in two
representations is two types, such as `String` and `Text`.

```hirsel
type Inbox { items: List[String] }            // own
stable type Name { text: Text }               // stable
fn first(inbox: Inbox) -> u64 { list.len(inbox.items) }
```
```hirsel
fn bad(x: own Inbox) {}                        // refused: LANG-5, no representation at a use
```

### OWN-2: Type summaries

Each type declaration publishes one **summary**. This is its whole content:

- its kind (OWN-3) and declared bounds (OWN-4), with the type parameters that condition
  each;
- its region parameters and anchors ([MEM-6](memory.md#mem-6-roots), OWN-22);
- its logical size ([MEM-24](budgets.md#mem-24-logical-charges)), its layout obligations
  ([MEM-9](memory.md#mem-9-compact-types)), whether it is `packed` and `streamable`
  ([MEM-12](memory.md#mem-12-stable-sequences)), and its declared twin
  ([MEM-17](memory.md#mem-17-clone));
- its **holds** set: which of resource, capability, scoped value, stable reference and
  **opaque owner** (a builder, `Causes` or an owning closure: an owner with no copy
  contract) it contains at any depth.

Kind, size and layout read only the fields held inline. The indirections `Box`, `List`,
`Map`, `Rc`, `Arc`, `Task`, `Sender`, `Receiver`, callables and stable references have a
fixed size and layout that does not read their payload's; inside a compact type, a
`SeqSpan` or `IndexedSpan` ends the layout the same way, by its header. These parts are computed in
dependency order and never iterated. A declaration whose computation re-enters a type
still being computed contains itself inline and is refused; the refusal is reported at the
field, continuing the cycle, of the cycle member whose definition identity (package, module
path, byte offset) is least. A logical size beyond `2^64 − 1` is refused too, at the field
or type argument that exceeds it; a smaller size that an engine cannot lay out is a resource
refusal ([PROG-6](programs.md#prog-6-one-meaning)). The holds set reads every field, through indirections too:
types that name each other through fields, at any depth, form a group, and every member of
a group holds the union of what the group's fields hold directly and what the other types
they name hold. Where a rule needs a type to hold nothing, a type parameter counts as
holding every category its bounds do not exclude: `plain` excludes resource, scoped value,
stable reference and opaque owner, but not capability, so `Wrap[T: plain] { item: T }` may hold a
capability. A generic declaration's summary is stated
in terms of its parameters and instantiated from the arguments' summaries at each use. A
parameter that no field **mentions** contributes nothing to it: a field mentions `T` when its
type is `T`, or names a type whose summary lets that argument position condition something,
at any depth. So a phantom parameter holds nothing and conditions no bound. A
`type` whose fields are all plain is **plain**: it is copied by `let`, by passing and by
capture, and has no cleanup. Scalars, `bool`, `char`, unit, plain callables, and tuples,
`Option` and `Result` of plain types are plain. The summaries of built-in types are in
[core](core.md#built-in-types). The checker reads summaries and never recomputes them.

```hirsel
type Point { x: i64, y: i64 }
fn twice(p: Point) -> (Point, Point) { let q = p; (p, q) }    // both copies are live
type Tree { label: String, kids: List[Tree] }                 // through List: no cycle
```
```hirsel
type A { b: B }
type B { a: A }                                          // refused: OWN-2, at A's field b
```

### OWN-3: Kinds

Every type has exactly one kind. A value is **copyable** when `let` copies it.

| kind | members | `let` | cleanup |
|---|---|---|---|
| plain | OWN-2 | copies | none |
| own | `type` with a non-plain field; `Box`, `List`, `String`, `Map`, the builders, `own fn` | moves | fields in reverse declaration order, then its blocks |
| stable | `stable type`; `Text`, `FixedSpan`, `SeqSpan`, `IndexedSpan`, `SeqIndex` | copies | freed with its region |
| resource | `resource type`; `Sender`, `Receiver`, `Task`; capabilities that hold state | moves | finalizer, then fields |
| counted | `Rc[T]`, `Arc[T]` | moves | release ([MEM-16](memory.md#mem-16-counting)) |
| scoped | `View[T]`, `Region`, `Dest`, `Writer`, `scoped fn`, `scoped type` | copies when every component is copyable, otherwise moves | cleans each owned component |

A tuple, `Option` or `Result` is scoped when a component is; otherwise it is plain when every
component is, and own otherwise. Any of them copies exactly when every component is
copyable, and its cleanup cleans each component. `View[T]` and `Region` are copyable;
`Dest`, `Writer` and `scoped fn` are not, and have no cleanup. A generic `T` is none of these until
instantiated; [OWN-23](#own-23-generic-bodies) says how a body treats it.

```hirsel
fn pair(a: String, b: String) -> u64 { let p = (view a, view b); let q = p; string.byte_len(q.0) }
```
```hirsel
scoped type Cursor { input: View[Bytes], owned: String }
fn f(c: Bytes, take s: String) { let x = Cursor(input: view c, owned: take s); let y = x; let z = x }
                                                          // refused: OWN-5, x moved
```

### OWN-4: Bounds

The bounds are:

- `plain`: exactly the types of plain kind (OWN-2, OWN-3). An own type, `List[T]` and
  `Box[T]` included, never has it, whatever its arguments.
- `shareable`: plain types, counted types, and own types that declare it and whose fields
  are shareable; never resources, scoped or stable types, or owning closures. Required of
  `Rc` and `Arc` payloads.
- `send`: [CONC-2](tasks.md#conc-2-the-send-bound).
- `key`: equality and hashing ([SEM-7](semantics.md#sem-7-equality-and-order)).
- `ordered`: total order (SEM-7).
- `integer`: exactly the integer types; `float`: exactly `f32` and `f64`. They are written
  only on the type parameters of core declarations ([core](core.md)), and are refused
  anywhere else: numeric generics are not in v1
  ([reserved](reserved.md#not-in-v1)). Each implies every bound that all the types it admits
  have, so a body may rely on those and an instance needs no further check: `T: integer` is
  also `plain`, `shareable`, `send`, `key`, `ordered`, `packed` and `streamable`, and `T:
  float` is `plain`, `shareable`, `send`, `packed` and `streamable` but neither `key` nor
  `ordered`. So a core body `fn max[T: integer](a: T, b: T) -> T { if a < b { b } else { a }
  }` copies and compares, and `fn same[T: float](a: T, b: T) -> bool { a == b }` is refused
  by [LANG-21](language.md#lang-21-operators).
- `packed`, `streamable`: compact encodings
  ([MEM-12](memory.md#mem-12-stable-sequences)), computed like `plain`; `streamable` implies
  `packed`, as MEM-12 defines it.

Plain types are `shareable` and `send` without declaring it. Otherwise a nominal type
declares the bounds it promises after its name (`type Node: shareable + send { .. }`), and
the declaration is checked once against its fields; a field naming a type, the declared
type included, reads that type's declared bounds and does not re-derive them, so there is
no fixpoint. A generic type's declared bounds are conditional, as a built-in's are: the
declaration is checked as if each type parameter had those bounds as well, and an
instance has a declared bound exactly when each type argument that the fields mention
(OWN-2) has it. So `type Pair[T]: shareable { a: T, b: T }` makes `Pair[u64]` shareable and
`Pair[HostFile]` not, while `type Id[T]: key { slot: u64 }` makes every `Id[X]` key. The
same rule gives the built-in rows of [core](core.md#built-in-types) their "when T", and an
opaque type's published summary carries which parameters condition each bound, without
its fields. A refusal is reported at the field whose type breaks the bound. A type without a declaration of `shareable`, `send`, `key` or `ordered` does
not have it, plain types' `shareable` and `send` excepted. `plain`, `packed` and `streamable` are never declared.

```hirsel
type Leaf: shareable + send { n: List[u64] }
type Pair[T]: shareable { a: T, b: T }
type Holder { r: Rc[Pair[u64]] }
```
```hirsel
type Entry { name: String }
type Id[T]: key { slot: u64, generation: u64 }
fn same(a: Id[Entry], b: Id[Entry]) -> bool { a == b }      // T is phantom: Id[Entry] is key
```
```hirsel
type Entry { name: String }
type Item[T]: key { item: T }
fn same(a: Item[Entry], b: Item[Entry]) -> bool { a == b }  // refused: LANG-21, Entry is not key
```
```hirsel
type Tree: shareable { kids: List[Rc[Tree]], log: HostFile }  // refused: OWN-4, at log
```
```hirsel
fn pick[T: integer](a: T) -> T { a }                      // refused: OWN-4, integer outside core
```

### OWN-5: Availability

Each owning slot is in one of four states at each point of a body: **uninit**, **live**,
**moved** or **maybe**. `let x: T` without an initializer is uninit; an initialization or
assignment makes it live; a move makes it moved; [OWN-16](#own-16-joins) makes maybe.
Reading, viewing, lending `mut`, moving and destructuring need live, and so does assigning
a part of a slot (a field, component or element); only a whole slot is assigned while it is
not live. A slot is
**writable** when it is declared `let mut` or is a `mut` or `take` parameter; assigning,
replacing or lending `mut` any place requires its slot to be writable.

```hirsel
fn f(take s: String) -> String { let t = s; t }
```
```hirsel
fn f(take s: String) -> String { let t = s; s }        // refused: OWN-5, s moved at 1:39
```
```hirsel
type P { a: u64, b: u64 }
fn f(take p: P) { let q = p; p.a = 1 }                    // refused: OWN-5, at p.a: p moved
```

### OWN-6: let, take and copy

`let y = x` moves `x` when its type moves (OWN-3), and copies it otherwise. `take p` moves
the place `p`, which must be a whole slot, at an argument, field or `replace` position;
`take` is written exactly where a place of a moving type is moved, and a fresh value (a
call, a construction, a literal) is passed bare. `take` of an expression that is not a
place is refused. `copy x` copies a value whose type is plain or has the `plain` bound.
Moving a place that is not a whole slot is [OWN-14](#own-14-no-partial-moves).

```hirsel
fn keep(take xs: List[u64]) -> List[u64] { let ys = xs; ys }
fn dup[T: plain](x: T) -> (T, T) { (copy x, copy x) }
```
```hirsel
fn dup[T](x: T) -> (T, T) { (copy x, copy x) }          // refused: OWN-6, T is not plain
```

### OWN-7: Parameter modes

A parameter is a view for the call when unmarked, lent for the call when marked `mut`,
and owned by the callee when marked `take`. An argument for a `mut` parameter is written
`mut p`; an argument for a `take` parameter is written `take p` when it is a place of a
moving type and bare otherwise; an argument for a `bytes` parameter is written `bytes e`;
every other argument is bare. A constructor's payloads are `take` parameters. An argument
whose mark does not match its parameter (a missing or an extra `mut`, `take` or `bytes`)
is refused here, at the argument. `view p` and `copy p` are expressions, not marks, and
`e in r` is a placement; like any argument they are checked against the parameter's type
([LANG-11](language.md#lang-11-typing-is-local)). A bare place argument for
an unmarked parameter is a view; a copyable value is copied. An unmarked parameter of type
`T` also takes a `View[T]`, which passes as that same view: a view of a view is the view
itself, so `list.len(string.as_bytes(s))` passes the `View[Bytes]` unchanged. `mut p` is written nowhere
but as such an argument, and a view parameter of a moving type is never moved; either is
refused here, at the `mut p` or at the move.

```hirsel
fn len(xs: List[u64]) -> u64 { list.len(xs) }
fn add(mut xs: List[u64], x: u64) -> Result[(), (Capacity, u64)] { list.push(mut xs, x) }
fn eat(take xs: List[u64]) {}
fn use_all(take xs: List[u64]) { let n = len(xs); let _ = add(mut xs, n); eat(take xs) }
```
```hirsel
fn eat(take xs: List[u64]) {}
fn f(take xs: List[u64]) { eat(xs) }                      // refused: OWN-7, needs take xs
```
```hirsel
fn f(s: String) -> String { s }                           // refused: OWN-7, at s: a view
```
```hirsel
fn g(bytes n: u64) -> u64 { n }
fn f() -> u64 { g(8) }                                    // refused: OWN-7, needs bytes 8
```
```hirsel
type Size = Exact(u64) | Unknown
fn f() -> Size { Exact(bytes 3) }                         // refused: OWN-7, a payload is not bytes
```
```hirsel
fn f(mut x: u64) { let y = mut x }                        // refused: OWN-7, mut outside an argument
```

### OWN-8: Views

`view p` makes a read-only **view** of the live place `p`, of type `View[T]`. A view lasts
for a lexical **extent**:

- bound by `let`, `let .. else` included: to the end of the enclosing block;
- bound by a match arm or `if let`: to the end of the arm;
- bound by `while let` or a `for` pattern: to the end of the body, in each iteration
  ([OWN-17](#own-17-loops));
- otherwise a temporary: to the end of its statement; in an `if` or `while` condition or
  an operand of `&&` or `||`, to the end of that condition or operand; in a match arm's
  expression, to the end of the arm unless the arm's value reaches it; in a block's tail, to the block's exit unless the block's
  value reaches it ([LANG-6](language.md#lang-6-evaluation-order));
- a parameter view: for the call.

A view of a record reads its fields as views, and a view of a `Box`, `Rc` or `Arc` reads as
a view of its payload, with the same anchors. Such a **holder** is used as itself wherever
its own type fits, and wherever it does not, through its payloads, outermost first, as far
as the first type that fits: for an argument or value whose expected type is fixed
([LANG-11](language.md#lang-11-typing-is-local)), that type; for a field or index step on it
and for a match arm whose pattern is a constructor, literal or tuple pattern, the first
payload that is not a holder. A bare name or `_` binds the holder itself, and `view b` views
the holder, which reads the same way. A match has no expected type, so its
patterns decide. Ownership stays with the holder, which is viewed for the binding's extent
(OWN-15), and a `Box`'s payload is a writable place where the `Box` is (OWN-12), so `mut b`
lends it where `mut T` is expected; a payload reached through an `Rc` or `Arc` on the way is
not one. A core operation may return a view of part
of an argument's storage that its signature names (a sub-range, `string.slice`; the same
bytes at another type, `string.as_bytes`): such a **sub-view** is a view with the anchors
and freezes of a view of the whole argument (OWN-20). There is no last-use end and no
mutable view. Nothing that a view exposes is written, moved or freed while the view lives,
through the view or through any other alias; an operation given a view may still change
state that no view exposes: a counted holder's count, a channel's queue, a region's
bookkeeping, the host. Read tracking (R18 in [reserved](reserved.md)) is such state. Where a plain `T` is expected, a `View[T]` of a
plain `T` reads as the value. A stable value used where a view is expected is that view
([OWN-25](#own-25-stable-values-are-read-as-views)).

```hirsel
type Inbox { items: List[String] }
fn sum(m: Inbox) -> u64 {
  let items = view m.items
  list.len(items)
}
```
```hirsel
type Inbox { items: List[String] }
fn f(mut m: Inbox) {
  let items = view m.items
  list.clear(mut m.items)                       // refused: OWN-10, items lasts to block end
}
```
```hirsel
type N = Lit(i64)
fn ev(n: N) -> i64 { match n { Lit(x) => x } }
fn f() -> i64 { let b = result.unwrap(box.new(Lit(6i64))); ev(b) }
fn g(b: Box[Option[u64]]) -> u64 { match b { Some(x) => x, None => 0 } }   // Some reads the payload
fn h(b: Box[u64]) -> u64 { match b { x => x } }                            // x is the Box, read as u64
fn grow(mut b: Box[List[u64]]) -> Result[(), (Capacity, u64)] { list.push(mut b, 1) }
fn deep(b: Box[Box[N]]) -> i64 { ev(b) }                    // reads through both holders
fn mixed(b: Box[Rc[N]]) -> i64 { match b { Lit(x) => x } }  // Lit reads the N
```
```hirsel
type P { n: u64 }
fn set(mut b: Box[Box[P]]) { b.n = 1 }
fn bump(mut b: Box[Rc[P]]) { b.n = 1 }                      // refused: OWN-12, P is reached through an Rc
```
```hirsel
fn total(mut xs: List[String]) -> u64 {
  let mut n: u64 = 0
  let mut i: u64 = 0
  while let Some(s) = list.get(xs, i) { n += string.byte_len(s); i += 1 }
  list.clear(mut xs)
  n
}
```
```hirsel
fn last_len(xs: List[String]) -> u64 {
  let mut last: Option[View[String]] = None
  let mut i: u64 = 0
  while let Some(s) = list.get(xs, i) {
    last = Some(s)
    i += 1
  }                                             // refused: OWN-17, last keeps s past its iteration
  0
}
```

### OWN-9: Overlap

Two places overlap when one is a prefix of the other, when they differ first at two
indexes of the same sequence, or when they differ first at two variant payloads of the
same value. Two different fields of one record do not overlap. Index operands are
evaluated left to right into temporaries before the places are compared. This one
relation governs views, reservations, `mut` loans, moves, assignments, replacements,
arguments of one call and captures.

```hirsel
type Pair { left: List[u8], right: List[u8] }
fn swap_halves(mut p: Pair) { list.append(mut p.left, p.right) }   // distinct fields
```
```hirsel
fn f(mut xs: List[List[u8]], i: u64, j: u64) {
  list.append(mut xs[i], xs[j])                   // refused: OWN-9, xs[i] and xs[j] overlap
}
```

### OWN-10: Freezes

A live view **freezes** every place in its anchor set, whatever the place's kind, for the
view's extent: moving, lending `mut`, assigning, replacing, destructuring or dropping an
overlapping place is refused. Several views of overlapping places may coexist. Copying a
view into another slot or aggregate extends its anchors' freezes to that slot's extent,
which must end before every anchor's scope does: a view whose extent outlives the scope of
an anchor is refused at the closing `}` of the block whose cleanup would drop the frozen
place, with the view as a secondary site. A freeze ends on every exit edge of its
extent, before the owners of the exited blocks are cleaned
([FAIL-8](failure.md#fail-8-exit-edge-cleanup)). A view of a stable value is anchored to its
region, not to a place, and freezes nothing: stable storage is never written.

```hirsel
type Inbox { items: List[String] }
fn f(mut m: Inbox) -> u64 {
  let n = { let items = view m.items; list.len(items) }
  list.clear(mut m.items)
  n
}
```
```hirsel
fn g() -> bool { let b = { let xs: List[u64] = list.new(); match list.get(xs, 0) { Some(_) => true, None => false } }; b }
```
```hirsel
type T { name: String }
fn h(take t: T) { if t.name == "x" { let T(name: _) = take t } }
```
```hirsel
type Inbox { items: List[String] }
fn f(take m: Inbox) -> Inbox { let v = view m.items; m }     // refused: OWN-10
```
```hirsel
fn f(take o: Option[String]) -> u64 {
  let v = match o { Some(take s) => view s, None => return 0 }   // refused: OWN-10, s ends with its arm
  string.byte_len(v)
}
```
```hirsel
fn f() -> u64 { let n = string.byte_len({ let s = string.new(); view s }); n }
                                                // refused: OWN-10, at the inner block's }
```

### OWN-11: Call arguments

Each argument of a call is **reserved** from its evaluation until the call completes. A
`mut p` argument lends the live place `p`: no later argument of the call may read, view,
move or lend a place overlapping it. A view argument freezes its place from its evaluation
(OWN-10). The callee holds a `mut` place, not its value: it may read, assign and replace
through its `mut` parameter and lend it on, but `take` of a `mut` parameter or of a place
under it is refused. When the call ends, by return or failure, the caller's place holds
whatever value was last installed ([T2.5](#own-26-the-transition-table)).
A value installed through a `mut` parameter, or into a place under it, reads only anchors
the caller's place already has: the parameter itself, its parts and sub-views of them, as
OWN-20 counts them, and the regions its type names. Anything else is refused here, at the
assignment, so the caller's place keeps its anchor set (OWN-19) across the call.

```hirsel
fn reset(mut xs: List[u64]) { xs = list.new() }
scoped type Cursor { input: View[Bytes], pos: u64 }
fn skip(mut c: Cursor) { c = Cursor(input: c.input, pos: c.pos + 1) }
```
```hirsel
fn put(mut out: Option[View[String]], t: String) {
  out = Some(view t)                                        // refused: OWN-11, t is not an anchor of out
}
```
```hirsel
fn steal(mut xs: List[u64]) -> List[u64] { take xs }       // refused: OWN-11
```

### OWN-12: Assignment

A **writable place** is a slot that is writable ([OWN-5](#own-5-availability)), or a
field, tuple component, `List` element, `Inline` element or `Box` payload of a writable place; nothing
reached through a view, a counted value or a stable value is one. `q` must be a writable
place, except that a `let` slot declared without an initializer is assigned while it is
uninit. Any other assignment is refused here, at `q`.
`q = e` resolves the writable place `q`, evaluates `e`, installs the value and drops the old
one, by rows T3.1 to T3.4 of the [transition table](#own-26-the-transition-table). From the
resolution of `q` until the install, `q` is reserved: `e` may read and view places
overlapping `q`, but may not move, assign, replace, destructure, lend `mut` or drop any of
them, so a failure in `e` leaves `q` unchanged. `q` must not overlap a frozen place.
Compound assignment (`+=`) is plain-only: it resolves `q` once, reads its old value,
evaluates the right side, computes, and installs.

```hirsel
fn f(mut s: String) -> Result[(), Capacity] { s = string.from("new")?; Ok(()) }  // old s dropped at install
```
```hirsel
fn f(mut xs: List[u64]) { xs[0] = { xs = list.new(); 7 } }  // refused: OWN-12, xs is reserved
```
```hirsel
fn f(x: u8) -> u8 { let y = x; y = 3; y }                 // refused: OWN-12, y is not writable
```

### OWN-13: replace

`replace(mut q, take v)` installs `v` in the live place `q` and returns the old value
without finalizing it ([T3.5](#own-26-the-transition-table)). It is the way to move a value
out of a field.

```hirsel
type Person { name: String, age: u64 }
fn pop_name(mut p: Person) -> String { replace(mut p.name, string.new()) }
```

### OWN-14: No partial moves

`take r.f` where `r.f` is not copyable is refused: use `replace` or a whole destructure.
`let T(a, b, ..) = take r` (whole destructure) marks `r` moved, makes each bound field an
owner of the enclosing scope in pattern order, then drops the omitted fields in
declaration order ([T3.7](#own-26-the-transition-table)). A resource is destructured only in
its defining module; a transparent type anywhere it is visible.

```hirsel
type Person { name: String, age: u64 }
fn name_of(take p: Person) -> String { let Person(name: n, ..) = take p; n }
```
```hirsel
type Person { name: String, age: u64 }
fn name_of(take p: Person) -> String { take p.name }      // refused: OWN-14
```

### OWN-15: Match on an owned value

`match p { .. }` on a live own place views it while patterns and guards are tested; a
guard sees only views. An arm whose pattern binds with `take` (`Some(take x)`) moves `p`
when that arm is chosen, and only then: every part the arm binds becomes an owner of the
arm, and the rest is dropped as in a whole destructure (OWN-14). Other arms bind views for
the arm. `let p = e` with an owning `e` is a whole destructure; with a view, its bindings
are views, and `take` under a view is refused. An arm that reads through a holder (OWN-8)
binds views of the payload, plain or own, and `take` in it is refused here, at the `take`:
`box.unbox(take b)` moves a payload out.

```hirsel
fn unwrap_or_empty(take o: Option[String]) -> String {
  match o { Some(take s) => s, None => string.new() }
}
```
```hirsel
fn k(take b: Box[Option[String]]) -> String {
  match b { Some(take s) => s, None => string.new() }  // refused: OWN-15, take through a Box
}
```
```hirsel
fn f(take o: Option[String]) -> String {
  match o { Some(s) if string.byte_len(s) > 0 => take s, _ => string.new() }  // refused: OWN-15
}
```

### OWN-16: Joins

Where control flow merges (after `if`, `match`, a labelled block or a loop exit), a slot's
state is the join of its incoming states. A `break`, `continue` or `return` carries its
state to its own target's join and contributes nothing to the join after the construct it
leaves; so does a call returning `Never`.

| ⊔ | uninit | live | moved | maybe |
|---|---|---|---|---|
| uninit | uninit | maybe | moved | maybe |
| live | maybe | live | maybe | maybe |
| moved | moved | maybe | moved | maybe |
| maybe | maybe | maybe | maybe | maybe |

A maybe slot cannot be read, viewed, lent or moved; to consume conditionally, use an
`Option`. It can be assigned (OWN-12) and is cleaned by its flag (OWN-18).

```hirsel
fn f(take s: String, c: bool) { if c { let _ = string.into_bytes(take s) } }   // s is maybe
```
```hirsel
fn f(take s: String, c: bool) -> String { if c { let _ = string.into_bytes(take s) }; s }
                                                          // refused: OWN-16, s is maybe
```

### OWN-17: Loops

The state of every slot at a loop head is its state on entry; a `for take` source is moved
before the head. Every `continue` and the end of the loop body must leave every slot
declared outside the loop in its entry state, and may not end a view or anchor that was live
at the head: a slot moved in the body must be reassigned before the
back edge. A refusal is reported at the `continue`, or at the closing `}` of the body, whose
edge breaks this; the change it undoes is a secondary site
([PROG-7](diagnostics.md#prog-7-one-rule-one-site)). A scoped slot declared outside the
loop reaches every back edge with the anchor set it had at the head, and a scoped callback
with the same capture descriptor (OWN-21): the same captured places, each in the same
mode. Reassigning it in the body to an equal set is accepted; a different set is refused
at that edge. A `while` condition's false edge and
each `break` merge into the loop's exit state by OWN-16. Nothing is iterated to a fixpoint.

```hirsel
fn drain(mut q: List[String]) {
  let mut cur = string.new()
  while list.len(q) > 0 {
    let _ = string.into_bytes(take cur)
    cur = match list.pop(mut q) { Some(take x) => x, None => string.new() }
  }
}
```
```hirsel
fn f(take s: String) { loop { let _ = string.into_bytes(take s) } }   // refused: OWN-17, at the body's }
```
```hirsel
fn each(xs: List[u64], f: scoped fn(u64)) {}
fn twice(xs: List[u64]) -> u64 {
  let mut total: u64 = 0
  let mut add = scoped fn(x: u64) { total += x }
  let mut i: u64 = 0
  while i < 2 {
    each(xs, add)
    add = scoped fn(x: u64) { total += 2 * x }
    i += 1
  }
  total
}
```
```hirsel
fn each(xs: List[u64], f: scoped fn(u64)) {}
fn split(xs: List[u64]) -> u64 {
  let mut a: u64 = 0
  let mut b: u64 = 0
  let mut add = scoped fn(x: u64) { a += x }
  let mut i: u64 = 0
  while i < 2 {
    each(xs, add)
    add = scoped fn(x: u64) { b += x }
    i += 1
  }                                       // refused: OWN-17, add captures b, not a
  a + b
}
```

### OWN-18: Drop flags

A slot is **flagged** when it reaches maybe at any join, label exit or loop exit. Its flag
is cleared at scope entry, set at each initialization or assignment and cleared at each
move or destructure. Cleanup drops a live slot, tests a maybe slot's flag, and skips a
moved or uninit one ([FAIL-8](failure.md#fail-8-exit-edge-cleanup)). Flags are not
observable except through cleanup.

### OWN-19: Scoped values

A **scoped** value (OWN-3) carries an **anchor set**: the places or regions it reads from.
It may be bound, passed as an argument, matched, compared and `?`-propagated while its
anchors are live and frozen. It may not be:

- a type argument of a user-declared generic (built-in `Option`, `Result` and tuples
  accept it);
- a field of an own, stable or resource type, refused at the field, or an element of a
  collection;
- captured by an owning closure, sent, or returned past its anchor.

A slot whose written type names its anchors, `let mut x: Option[View[T]] in xs = None`, as
a result type does (OWN-20), has exactly that anchor set for its whole extent, whatever it
holds: each anchor is frozen from the declaration on (OWN-10), even while the slot holds
`None`, and a value assigned to it or initialising it may read only those anchors, a part
or sub-view of one included, or it is refused there. Its set is then the same at a loop's
head and at every edge (OWN-17), so a loop may keep a view it finds; a slot without a
written anchor starts with its initialiser's set, which is why `last_len` (OWN-8) is
refused and `last` below is not.

```hirsel
fn last(xs: List[String], n: u64) -> Option[View[String]] in xs {
  let mut found: Option[View[String]] in xs = None
  for s in xs { if string.byte_len(s) == n { found = Some(s) } }
  found
}
```
```hirsel
fn last_local(xs: List[String]) -> u64 {
  let mut found: Option[View[String]] in xs = None
  for s in xs {
    let t = string.new()
    found = Some(view t)                        // refused: OWN-19, t is not an anchor of found
  }
  0
}
```
```hirsel
fn longest(a: String, b: String) -> u64 {
  let pair = (view a, view b)
  if string.byte_len(pair.0) > string.byte_len(pair.1) { string.byte_len(pair.0) } else { string.byte_len(pair.1) }
}
```
```hirsel
fn keep(xs: List[String]) -> List[View[String]] { list.new() }  // refused: OWN-19
```

### OWN-20: Scoped results name their anchor

A function that returns a scoped value writes its anchors in the result type: `View[T] in
p` for one, `R[in a, in b]` for a scoped type with several. Each names a view parameter, or
a region whose stable values the result reads.
The caller's argument for each stays frozen for the result's extent. The result may denote
the anchored argument, a part of it (a field, element or payload, at any depth), or a
sub-view that a call gave for the argument or for such a part (OWN-8), core operations'
included, or a part or sub-view of such a sub-view, at any depth; such a result has the
argument's anchor and keeps the same freeze. It is a scoped value and copies as OWN-3 says. No anchor is inferred from a body. A
missing or wrong anchor is refused at the written result type.

```hirsel
fn first(xs: List[String]) -> Option[View[String]] in xs { list.get(xs, 0) }
fn show(mut xs: List[String]) {
  if let Some(s) = first(xs) { let _ = string.byte_len(s) }
  list.clear(mut xs)
}
```
```hirsel
fn first(xs: List[String]) -> Option[View[String]] { list.get(xs, 0) }  // refused: OWN-20
```

### OWN-21: Scoped callbacks

A callable type written `scoped fn(..) -> U` is a **scoped callback**: it may capture views
and lend `mut` (LANG-14), and it is itself scoped (OWN-19). It may be bound by `let` and
passed on as an argument. Its view parameters last for one invocation, so neither `U` nor
any capture may be anchored to a parameter.

Every scoped callable value carries a static **capture descriptor**, computed once with the
closure body: the slots or formal anchors it captures, each with its mode (view, or `mut`
when the body assigns or lends it `mut`) and lexical scope. The descriptor follows the value
through moves, scoped aggregates, argument substitution, returned anchors and joins, where
descriptors unite and `mut` dominates view. It is **activated** for a direct invocation,
for that invocation, and for a call that receives it as an argument, for that whole call,
except that a field callback of an `emit` literal is active only while its field is written
([MEM-4](memory.md#mem-4-sized-destinations)). At
activation each captured place must be live, and writable for a `mut` capture, and its loan
is checked as an argument of that call against the other arguments, active loans and
freezes (OWN-9 to OWN-12). Forwarding a received callback reborrows its loans and creates no
second loan of the same place. An inactive descriptor grants no access, and every captured
slot outlives the callback. A view of an owning callable used as a scoped callback freezes
that owner for the scoped value's extent.

```hirsel
fn count_if(xs: List[String], keep: scoped fn(String) -> bool) -> u64 { 0 }
fn long_ones(xs: List[String], min: u64) -> u64 {
  count_if(xs, fn(s: String) -> bool { string.byte_len(s) >= min })
}
```
```hirsel
fn each(xs: List[String], f: scoped fn(String)) {}
fn grab(xs: List[String]) -> u64 {
  let mut last: Option[View[String]] = None
  each(xs, fn(s: String) { last = Some(view s) })     // refused: OWN-21, escapes the call
  0
}
```
```hirsel
fn direct(take src: String) {
  let cb = scoped fn() { let _ = string.push(mut src, "x") }
  let seen = view src
  cb()                                                    // refused: OWN-10, src is frozen
  let _ = string.byte_len(seen)
}
```

### OWN-22: Scoped types

`scoped type R { .. }` declares a record that may hold views. Its view fields are anchored
to the value's own anchor, written at uses as `R in p`; a declaration with several anchors
names them as parameters, `scoped type R[in a, in b] { x: View[A] in a, y: View[B] in b }`.
A scoped type is scoped (OWN-19); it may hold own fields, which make it move (OWN-3) and
are cleaned normally.

```hirsel
scoped type Cursor { input: View[Bytes], pos: u64 }
fn start(b: Bytes) -> Cursor in b { Cursor(input: view b, pos: 0) }
```
```hirsel
scoped type Cursor { input: View[Bytes], pos: u64 }
type Holder { c: Cursor }                                 // refused: OWN-19
```

### OWN-23: Generic bodies

A body is checked once with its type parameters rigid: it uses only what their declared
bounds give. A value of type `T` moves in the body whatever `T` is later instantiated
with: `let` moves it, and `copy` needs `T: plain`. Instantiation never re-checks a body. A
type parameter cannot be instantiated with a scoped type (OWN-19); a core signature
accepts scoped arguments only where it writes `View[T]` or a scoped type itself.

```hirsel
fn id[T](take x: T) -> T { let y = x; y }
```
```hirsel
fn two[T](take x: T) -> (T, T) { (x, x) }                 // refused: OWN-5, x moved
```

### OWN-24: Conversions

The only conversions between kinds are:

| conversion | from | to | rule |
|---|---|---|---|
| `view p` | live place | scoped view | OWN-8 |
| `rc.new(take x)`, `arc.new(take x)` | own or plain, `shareable` | counted | [MEM-14](memory.md#mem-14-rc), MEM-15 |
| `rc.share(h)`, `arc.share(h)` | counted | another holder | MEM-16 |
| `rc.try_unwrap(take h)`, `arc.try_unwrap(take h)` | counted | own, if the count is 1 | MEM-16 |
| `clone(x)`, `clone_in(r, x)` | view of own, stable or counted | own, or stable in `r` | [MEM-17](memory.md#mem-17-clone) |

There is no implicit counting, copy-on-write, inferred borrow or inferred uniqueness, and
no conversion from stable to own in place, from a resource to counted, or from scoped to
anything storable.

### OWN-25: Stable values are read as views

A published stable value is immutable for the life of its region and never moves
([MEM-5](memory.md#mem-5-publication)). It is read where a view of its type is expected,
with no `view` written, anchored to its region; a function taking `View[Text]` or an
unmarked `Text` parameter accepts it. `let` copies its locator, never its bytes.

```hirsel
fn width(t: Text) -> u64 { text.len(t) }
fn f(r: Region, s: Text in r) -> u64 { let u = s; width(u) + width(s) }
```

### OWN-26: The transition table

This table is the single authority for what each operation does to its inputs, its slots
and its limits, on success and on failure; other rules cite its rows. Notation: `L` live,
`M` moved, `?` maybe; `chg(n)` and `ref(n)` are admission and refund
([MEM-21](budgets.md#mem-21-admission), MEM-23); `F(x)` runs `x`'s finalizer and `Rel(x)`
releases its fields; `cause+=e` appends a cause ([FAIL-10](failure.md#fail-10-cause-chain));
"prop" is failure propagation. The last column names the one owner of every surviving value
after a failure ([FAIL-7](failure.md#fail-7-one-owner-per-survivor)).

**T1 Construction**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T1.1 | `T(a: take a, b: e)` | fields evaluated left to right into temporaries; value L, stored inline; nothing admitted | `e` fails | earlier temporaries dropped in reverse by statement cleanup; prop |
| T1.2 | `T(..) in r`, a resource literal, an owning closure | as T1.1, then `chg` (commit) | `e` fails; `Capacity` at `chg` | `e`: as T1.1. `Capacity`: nothing admitted, cleaned or run. A placed literal's error is bare `Capacity`, its fields being copies. A resource literal's error is `(Capacity, F)`, owning `F`: its one field, or the tuple of all its fields in declaration order; with no fields, bare `Capacity`. An owning closure's is `(Capacity, F)`, `F` its one capture or the tuple of its captures in environment order |
| T1.3 | `box.new(take x)`, `rc.new(take x)` | as T1.2, then `x` moved into the new node, count 1 for `rc`; no block `x` owns is freed | `Capacity` | the error owns `x` |
| T1.4 | `arc.new(take x)` | as T1.3; a recorded event ([CONC-10](tasks.md#conc-10-event-order)) | `Capacity`, recorded | the error owns `x` |
| T1.5 | `clone(x)`, `clone_in(r, x)` | copy L; `chg` per new block; `share` per counted edge | `Capacity` at block k | own blocks copied so far dropped and shares released in reverse, each `ref`; region bytes stay charged until close; `x` untouched |
| T1.6 | `span.push(mut b, x)` | value moved into the builder | `Capacity` | the error owns the value; the builder unchanged; region bytes stay charged |
| T1.7 | `finish(take b)` | stable value L | cannot fail | |
| T1.8 | `build_in`, `build_stream_in` | width check, then `chg(n)` (commit), then the writer runs once | `Overflow` or `Capacity` before the writer runs; the writer's `Error` | nothing allocated before commit; after it the `n` bytes stay charged until close and nothing is published |
| T1.9 | `finish_trim(take b)` | stable value L; `ref(round8(n) − round8(used))` at the allocation's record | cannot fail | |
| T1.10 | `seq.index_in(r, s)` | `chg` (commit), then the table | `Capacity` | nothing allocated; the source unchanged |

**T2 Bindings and views**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T2.1 | `let y = x`, `x` moves | `y` L, `x` M | | |
| T2.2 | `let y = x`, `x` copyable | both L | | |
| T2.3 | `take x` argument | `x` M; a caller temporary until entry | callee fails | before entry the caller's cleanup, after it the callee's |
| T2.4 | `let v = view p` | `p` frozen to block end | | |
| T2.5 | `mut x` argument | `x` L with the last installed value | callee fails | `x` L with the last installed value; the caller's cleanup |
| T2.6 | exit edge | views end, then owners are cleaned | | |

**T3 Assignment and destructure**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T3.1 | `q = e`: resolve `q` (reserved), evaluate `e` | install | `e` or an index fails | `q` unchanged (OWN-12); moves made by `e` stand; prop |
| T3.2 | install, `q` L | install `v`; `F(old)`, `Rel(old)` | `F(old)` fails | `q` owns `v`; old's fields released; `cause+=e`; prop |
| T3.3 | install, `q` ?, flag set | as T3.2 | as T3.2 | as T3.2 |
| T3.4 | install, `q` ? with flag clear, M or uninit | install `v`; set the flag if flagged | | |
| T3.5 | `replace(mut q, take v)` | install `v`; result owns old, unfinalized | | |
| T3.6 | discarded `replace` result | dropped at statement end | its finalizer fails | `q` owns `v`; `cause+=e`; prop |
| T3.7 | whole destructure | `r` M; bound fields L; omitted fields dropped in declaration order; a resource's whole block `ref`: its field storage, unused cause cell and unused grant; blocks the fields own stay with them | an omitted drop fails | `cause+=e`; next omitted drop; prop cleans bound fields |

**T4 Counted values**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T4.1 | `share(h)` | count + 1 | cannot fail | |
| T4.2 | holder drop, count > 1 | count − 1 (Arc: recorded) | cannot fail | |
| T4.3 | holder drop, count = 1 | payload released by worklist; each node `ref` (Arc: one recorded event) | cannot fail | |
| T4.4 | `try_unwrap(take h)` | count = 1: payload L, the node alone `ref`; Arc: recorded | count > 1 | the error owns `h`; count unchanged |

**T5 Regions and limits**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T5.1 | `region r { .. }` enter | `r` open | cannot fail | |
| T5.2 | region allocation of n bytes | `chg(n)` | `Capacity` before any write | nothing allocated; earlier bytes stay charged |
| T5.3 | region close, any exit edge | views end; owners drop; all bytes `ref` | cannot fail | |
| T5.4 | `bound bytes n { .. }` enter | `chg(n)` to the current limit; a new limit of n is current | `Capacity`: the body does not run | `Error(c)` |
| T5.5 | bound exit, any edge | views end; owners drop; the unused part `ref`; surviving blocks handed to the parent ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)) | cannot fail | |
| T5.6 | admission of n | used + n ≤ size: used += n | `Capacity` | per the operation's row |
| T5.7 | refund of n | used −= n at the block's limit | cannot fail | |

**T6 Resources and cleanup**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T6.1 | `close(take f)` and other consuming APIs | `f` M | per signature | the error owns what its type lists |
| T6.2 | implicit drop | `F(f)`, `Rel(f)`, `ref` | `F` fails | `cause+=e`; remaining drops continue; prop |
| T6.3 | exit edge with a pending value | drops, then the value is delivered | drop k fails | `cause+=e`; the pending value is dropped at once; remaining drops; prop past the target |
| T6.4 | cancellation observed | failure with cause `Cancelled` | | as any failure |

**T7 Tasks**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T7.1 | `task.spawn(.., take x, f)` | `chg` (commit); the child owns `x` | `Capacity` | the error owns `x` |
| T7.2 | `send(tx, take x)` | recorded publication (commit); the queue owns `x` | `Closed` | the error owns `x` |
| T7.3 | cancelled before publication | | `Cancelled` | the sender's cleanup drops `x` |
| T7.4 | `receive(rx)` | the receiver owns the item | `Closed` | |
| T7.5 | `join(take h)` | `Ok(r)`: the joiner owns `r` | `Failed(causes)` or `Cancelled(causes)` | the joiner owns the cause chain |
| T7.6 | handle dropped unjoined | child cancelled; shielded wait; its result or chain dropped | | recorded |
| T7.7 | queue dropped with items | items dropped, last published first | an item's finalizer fails | `cause+=e`; continue |

Every refusal in this chapter is made when the definition is checked; nothing refused
runs.
