# Ownership

This chapter says what each value is, who owns it, and what each operation does to the
slots it touches. The [transition table](#own-26-the-transition-table) at the end is the
one authority for operation outcomes; the other chapters state rules and cite its rows.

A **slot** is a local binding, a parameter, a field path of one, or a statement temporary.
A **place** is a slot followed by zero or more field, index or variant-payload steps
(`m.items[i]`). Checking a definition reads its body and the declarations it names
([LANG-8](language.md#lang-8-signatures)); every rule here is decided in one pass over the
body in source order.

### OWN-1: Representation is declared once

A nominal type's declaration fixes the representation of every value of that type, and
every use inherits it. `type T` is **own**; `stable type T` is **stable** (region data,
[MEM-1](memory.md#mem-1-regions)); `resource type T` is a **resource** (affine, with a
finalizer, [FAIL-11](failure.md#fail-11-resources)). A use never writes a representation;
it writes only a mode (`view`, `mut`, `take`, `copy`) and a place (`in r`). A type needed
in two representations is two types, such as `String` and `Text`.

```hirsel
type Inbox { items: List[String] }            // own
stable type Name { text: Text }               // stable
fn first(inbox: Inbox) -> u64 { list.len(inbox.items) }
```
```hirsel
fn bad(x: own Inbox) {}                        // refused: LANG-5, no representation at a use
```

### OWN-2: Plain is structural

A `type` whose fields are all plain is **plain**: it is copied by `let`, by passing and by
capture, and has no cleanup. The scalars, `bool`, `char`, unit, and tuples, `Option` and
`Result` of plain types are plain. A `stable type`, a `resource type` and a type with any
non-plain field are not plain.

```hirsel
type Point { x: i64, y: i64 }
fn twice(p: Point) -> (Point, Point) { let q = p; (p, q) }    // both copies are live
```
```hirsel
type Named { name: String }
fn twice(take n: Named) -> (Named, Named) { let m = n; (n, m) } // refused: OWN-5, n moved
```

### OWN-3: Kinds

Every type has exactly one kind:

| kind | members | `let` | cleanup |
|---|---|---|---|
| plain | OWN-2 | copies | none |
| own | `type` with a non-plain field; `List`, `String`, `Map`, `Bytes` | moves | drops fields in reverse declaration order |
| stable | `stable type`; `Text`, the spans | copies | freed with its region |
| resource | `resource type`; capabilities that hold state; task handles | moves | finalizer, then fields |
| counted | `Rc[T]`, `Arc[T]` | moves | release ([MEM-16](memory.md#mem-16-counting)) |
| scoped | `View[T]`; `scoped fn`; `scoped type`; `Option`, `Result` and tuples with a scoped component | copies | none |

A generic `T` is none of these until instantiated; [OWN-23](#own-23-generic-bodies) says how
a body treats it.

### OWN-4: Bounds

The bounds are:

- `plain`: OWN-2.
- `shareable`: plain, counted, stable-free own types whose fields are shareable; excludes
  resources, scoped and stable types. Required of `Rc` and `Arc` payloads.
- `send`: [CONC-2](tasks.md#conc-2-the-send-bound).
- `key`: equality and hashing ([SEM-7](semantics.md#sem-7-equality-and-order)).
- `ordered`: total order (SEM-7).

A nominal type declares the bounds it promises after its name (`type Node: shareable + send
{ .. }`), and the declaration is checked once against its fields. A recursive reference
reads the declared bounds and is not re-derived, so there is no fixpoint. A type without
a declaration of `shareable`, `send`, `key` or `ordered` does not have it. `plain` is never
declared; it is OWN-2.

```hirsel
type Leaf: shareable + send { n: u64 }
type Tree: shareable + send { kids: List[Rc[Tree]] }  // refused: OWN-4, Rc is not send
```

### OWN-5: Availability

Each owning slot is in one of four states at each point of a body: **uninit**, **live**,
**moved** or **maybe**. A declaration without an initializer is uninit; an initialization
or assignment makes it live; a move makes it moved; [OWN-16](#own-16-joins) makes maybe.
Reading, viewing, lending `mut`, moving and destructuring need live. Plain slots are always
live once initialized.

```hirsel
fn f(take s: String) -> String { let t = s; t }
```
```hirsel
fn f(take s: String) -> String { let t = s; s }        // refused: OWN-5, s moved at 1:39
```

### OWN-6: let, take and copy

`let y = x` moves `x` when its kind is own, resource or counted, and copies it otherwise.
`take x` moves `x` at an argument or field position. `copy x` copies a value whose type is
plain or has the `plain` bound. Moving a place that is not a whole slot is
[OWN-14](#own-14-no-partial-moves).

```hirsel
fn keep(take xs: List[u64]) -> List[u64] { let ys = xs; ys }
fn dup[T: plain](x: T) -> (T, T) { (copy x, copy x) }
```
```hirsel
fn dup[T](x: T) -> (T, T) { (copy x, copy x) }          // refused: OWN-6, T is not plain
```

### OWN-7: Parameter modes

A parameter is borrowed as a view for the call when unmarked, lent for the call when
marked `mut`, and owned by the callee when marked `take`. The caller writes `mut x` and
`take x` at the argument; an unmarked argument is a view. Plain arguments are copied
whatever the mode, except `mut`.

```hirsel
fn len(xs: List[u64]) -> u64 { list.len(xs) }
fn add(mut xs: List[u64], x: u64) -> Result[(), Capacity] { list.push(mut xs, x) }
fn eat(take xs: List[u64]) {}
fn use_all(take xs: List[u64]) { let n = len(xs); add(mut xs, n); eat(take xs) }
```
```hirsel
fn f(take xs: List[u64]) { eat(xs) }                      // refused: OWN-7, needs take xs
```

### OWN-8: Views

`view p` makes a read-only **view** of the live place `p`, of type `View[T]`. A view lasts
for a lexical **extent**:

- bound by `let`: to the end of the enclosing block;
- bound by a match arm or `if let`: to the end of the arm;
- otherwise a temporary: to the end of its statement;
- a parameter view: for the call.

There is no last-use end and no mutable view. A stable value used where a view is expected
is that view ([OWN-25](#own-25-stable-values-are-read-as-views)).

```hirsel
fn sum(m: Inbox) -> u64 {
  let items = view m.items
  list.len(items)
}
```
```hirsel
fn f(mut m: Inbox) {
  let items = view m.items
  list.clear(mut m.items)                       // refused: OWN-10, items lasts to block end
}
```

### OWN-9: Overlap

Two places overlap when one is a prefix of the other, when they differ first at two
indexes of the same sequence, or when they differ first at two variant payloads of the
same value. Two different fields of one record do not overlap. Index operands are
evaluated left to right into temporaries before the places are compared. This one
relation governs views, `mut` loans, moves, assignments, replacements, arguments of one
call and captures.

```hirsel
fn swap_halves(mut p: Pair) { list.append(mut p.left, p.right) }   // distinct fields
```
```hirsel
fn f(mut xs: List[List[u8]], i: u64, j: u64) {
  list.append(mut xs[i], xs[j])                   // refused: OWN-9, xs[i] and xs[j] overlap
}
```

### OWN-10: Freezes

A live view of an own place **freezes** it for the view's extent: moving, lending `mut`,
assigning, replacing, destructuring or dropping an overlapping place is refused. Several
views of overlapping places may coexist. A freeze ends on every exit edge of its extent,
before the owners of the exited blocks are cleaned ([FAIL-8](failure.md#fail-8-exit-edge-cleanup)).
Stable values are never frozen; they are never written.

```hirsel
fn f(mut m: Inbox) -> u64 {
  let n = { let items = view m.items; list.len(items) }
  list.clear(mut m.items)
  n
}
```
```hirsel
fn f(take m: Inbox) -> Inbox { let v = view m.items; m }     // refused: OWN-10
```

### OWN-11: mut arguments

`mut x` lends the live place `x` to one call. During the call no other argument of the
call may overlap it. The callee holds the place, not the value: it may read, assign and
replace through its `mut` parameter and lend it on, but `take` of a `mut` parameter or of
a place under it is refused. When the call ends, by return or failure, the caller's place
holds whatever value was last installed ([T3.2](#own-26-the-transition-table)).

```hirsel
fn reset(mut xs: List[u64]) { xs = list.new() }
```
```hirsel
fn steal(mut xs: List[u64]) -> List[u64] { take xs }       // refused: OWN-11
```

### OWN-12: Assignment

`q = e` runs in three phases:

1. Resolve the place `q` and evaluate `e`. A failure here consumes nothing.
2. Install, which cannot fail, by `q`'s state: live: the old value is staged and the new
   one installed; maybe: if the slot's flag is set, stage the old value; either way install
   and set the flag; moved or uninit: install, and set the flag if the slot is flagged.
3. Drop the staged old value now. If its finalizer fails, the new value stays installed,
   the old value's fields are released, and the failure propagates from this statement
   ([FAIL-12](failure.md#fail-12-finalizer-failure)).

`q` must not overlap a frozen place. Compound assignment (`+=`) is plain-only and is a
read followed by assignment.

```hirsel
fn f(mut s: String) { s = string.from("new") }       // the old String is dropped
```
```hirsel
fn f(mut s: String) { let v = view s; s = string.new() }  // refused: OWN-10
```

### OWN-13: replace

`replace(mut q, take v)` installs `v` in the live place `q` and returns the old value
without finalizing it. A discarded result is dropped at the end of its statement. It is the
way to move a value out of a field.

```hirsel
fn pop_name(mut p: Person) -> String { replace(mut p.name, string.new()) }
```

### OWN-14: No partial moves

`take r.f` where `r.f` is not plain is refused: use `replace` or a whole destructure.
`let T(a, b, ..) = take r` (whole destructure, allowed in the module that declares `T`)
marks `r` moved, makes every bound field an owner of the enclosing scope, then drops the
omitted fields in declaration order. A failing omitted drop appends a cause, the
remaining omitted drops run, then the failure propagates and cleans the bound fields
([FAIL-10](failure.md#fail-10-cause-chain)).

```hirsel
fn name_of(take p: Person) -> String { let Person(name: n, ..) = take p; n }
```
```hirsel
fn name_of(take p: Person) -> String { take p.name }      // refused: OWN-14
```

### OWN-15: Match on an owned value

`match p { .. }` on a live own place views it while patterns and guards are tested. An arm
whose pattern binds with `take` (`Some(take x)`) moves `p` when that arm is chosen, and
only then: the bound parts become owners of the arm and the rest is dropped as in a whole
destructure (OWN-14). Other bindings are views for the arm. A guard is a pure boolean
expression and cannot move or lend.

```hirsel
fn unwrap_or_empty(take o: Option[String]) -> String {
  match o { Some(take s) => s, None => string.new() }
}
```
```hirsel
fn f(take o: Option[String]) -> String {
  match o { Some(s) if string.len(s) > 0 => take s, _ => string.new() }  // refused: OWN-15
}
```

### OWN-16: Joins

Where control flow merges (after `if`, `match`, a labelled block or a loop exit), a slot's
state is the join of its incoming states. A diverging arm (one ending in `return`, `break`,
`continue` or a call returning `Never`) contributes nothing.

| ⊔ | uninit | live | moved | maybe |
|---|---|---|---|---|
| uninit | uninit | maybe | moved | maybe |
| live | maybe | live | maybe | maybe |
| moved | moved | maybe | moved | maybe |
| maybe | maybe | maybe | maybe | maybe |

A maybe slot cannot be read, viewed, lent or moved; to consume conditionally, use an
`Option`. It can be assigned (OWN-12) and is cleaned by its flag (OWN-18).

```hirsel
fn f(take s: String, c: bool) { if c { drop(take s) } }     // s is maybe; cleaned by flag
```
```hirsel
fn f(take s: String, c: bool) -> String { if c { drop(take s) }; s }  // refused: OWN-16
```

### OWN-17: Loops

The state of every slot at a loop head is its state on entry. Every `continue` and the end
of the loop body must leave every slot declared outside the loop in its entry state; a
slot moved in the body must be reassigned before the back edge. Each `break` merges into
the loop's exit state by OWN-16. Nothing is iterated to a fixpoint.

```hirsel
fn drain(mut q: List[String]) {
  let mut cur = string.new()
  while list.len(q) > 0 { consume(take cur); cur = list.pop_or_empty(mut q) }
}
```
```hirsel
fn f(take s: String) { loop { consume(take s) } }           // refused: OWN-17
```

### OWN-18: Drop flags

A slot is **flagged** when it reaches maybe at any join, label exit or loop exit. Its flag
is cleared at scope entry, set at each initialization or assignment and cleared at each
move or destructure. Cleanup drops a live slot, tests a maybe slot's flag, and skips a
moved or uninit one. Flags are not observable except through cleanup.

### OWN-19: Scoped values

A **scoped** value (OWN-3) carries one or more **anchors**: the places or regions it reads
from. It may be bound, passed as an argument, matched, compared and `?`-propagated while
its anchors are live and frozen. It may not be:

- a type argument of a user-declared generic (built-in `Option`, `Result` and tuples
  accept it);
- a field of an own, stable or resource type, or an element of a collection;
- captured by an escaping closure, sent, or returned past its anchor.

```hirsel
fn longest(a: String, b: String) -> u64 {
  let pair = (view a, view b)
  max(string.len(pair.0), string.len(pair.1))
}
```
```hirsel
fn keep(xs: List[String]) -> List[View[String]] { .. }      // refused: OWN-19
```

### OWN-20: Scoped results name their anchor

A function that returns a scoped value writes its anchor as `in p`, naming a view
parameter `p`. The caller's argument for `p` stays frozen for the result's extent. No
anchor is inferred from a body.

```hirsel
fn first(xs: List[String]) -> Option[View[String]] in xs { list.get(xs, 0) }
fn show(mut xs: List[String]) {
  if let Some(s) = first(xs) { print_len(s) }
  list.clear(mut xs)
}
```
```hirsel
fn first(xs: List[String]) -> Option[View[String]] { list.get(xs, 0) }  // refused: OWN-20
```

### OWN-21: Scoped callbacks

A callable type written `scoped fn(..) -> U` is a **scoped callback**: it may capture
views, and it is itself scoped (OWN-19). Its view parameters last for one invocation, so
neither `U` nor any capture may be anchored to a parameter. Captures are checked as if
they were arguments of the call that receives the callback.

```hirsel
fn count_if(xs: List[String], keep: scoped fn(String) -> bool) -> u64 { .. }
fn long_ones(xs: List[String], min: u64) -> u64 {
  count_if(xs, fn(s: String) -> bool { string.len(s) >= min })
}
```
```hirsel
fn grab(xs: List[String]) -> u64 {
  let mut last = none_view()
  each(xs, fn(s: String) { last = Some(view s) })     // refused: OWN-21, escapes the call
  0
}
```

### OWN-22: Scoped types

`scoped type R { .. }` declares a record that may hold views. Its view fields are anchored
to the value's own anchor, written at uses as `R in p`; a declaration with several anchors
names them as parameters, `scoped type R[in a, in b] { x: View[A] in a, y: View[B] in b }`.
A scoped type is scoped (OWN-19); its own fields make it affine and are cleaned
normally.

```hirsel
scoped type Cursor { input: View[Bytes], pos: u64 }
fn start(b: Bytes) -> Cursor in b { Cursor(input: view b, pos: 0) }
```
```hirsel
type Holder { c: Cursor }                                 // refused: OWN-19
```

### OWN-23: Generic bodies

A body is checked once with its type parameters rigid: it uses only what their declared
bounds give. A value of type `T` is affine in the body whatever `T` is later instantiated
with: `let` moves it, and `copy` needs `T: plain`. Instantiation never re-checks a body.
A type parameter cannot be instantiated with a scoped type (OWN-19).

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

A stable value is read where a view of its type is expected, with no `view` written; it is
anchored to its region. A function taking `View[Text]` or an unmarked `Text` parameter
accepts it. `let` copies a stable value; both copies refer to the same published data.

```hirsel
fn width(t: Text) -> u64 { text.len(t) }
fn f(r: Region, s: Text in r) -> u64 { let u = s; width(u) + width(s) }
```

### OWN-26: The transition table

This table is the single authority for what each operation does to its inputs, its slots
and its domain, on success and on failure. Notation: `L` live, `M` moved, `?` maybe;
`chg(n)` and `ref(n)` are admission and refund ([MEM-23](budgets.md#mem-23-admission),
MEM-25); `F(x)` runs `x`'s finalizer and `Rel(x)` releases its fields; `cause+=e` appends a
cause ([FAIL-10](failure.md#fail-10-cause-chain)); "prop" is failure propagation. The last
column names the one owner of every surviving value after a failure
([FAIL-7](failure.md#fail-7-one-owner-per-survivor)).

**T1 Construction**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T1.1 | `T(a: take a, b: e)` own | fields evaluated left to right into temporaries; admission (commit); value L | `e` fails | temporaries dropped in reverse by statement cleanup; prop |
| T1.2 | same | | admission: `Capacity` | as T1.1; the error is bare `Capacity` |
| T1.3 | `rc.new(take x)` | node L, count 1; `chg(node)`; `ref` of `x`'s top block | `Capacity` | the error owns `x` |
| T1.4 | `arc.new(take x)` | as T1.3; a recorded event ([CONC-10](tasks.md#conc-10-event-order)) | `Capacity`, recorded | the error owns `x` |
| T1.5 | `clone(x)`, `clone_in(r, x)` | copy L; `chg` per own node; `share` per counted edge | `Capacity` at node k | copied nodes dropped and shares released in reverse, each refunded; `x` untouched |
| T1.6 | builder append | value moved into the builder | `Capacity` | the error owns the value; the builder unchanged; region bytes stay charged |
| T1.7 | `builder.finish(take b)` | stable value L | cannot fail | |

**T2 Bindings and views**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T2.1 | `let y = x`, non-plain | `y` L, `x` M | | |
| T2.2 | `let y = x`, plain, stable or scoped | both L | | |
| T2.3 | `take x` argument | `x` M | callee fails | the callee's cleanup owns it |
| T2.4 | `let v = view p` | `p` frozen to block end | | |
| T2.5 | `mut x` argument | `x` L with the last installed value | callee fails | `x` L with the last installed value; the caller's cleanup |
| T2.6 | exit edge | views end, then owners are cleaned | | |

**T3 Assignment and destructure**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T3.1 | `q = e`, phase 1 | phase 2 | `e` or an index fails | `q` unchanged; prop |
| T3.2 | `q = take v`, `q` L | install `v`; `F(old)`, `Rel(old)` | `F(old)` fails | `q` owns `v`; old's fields released; `cause+=e`; prop |
| T3.3 | `q = take v`, `q` ?, flag set | as T3.2 | as T3.2 | as T3.2 |
| T3.4 | `q = take v`, `q` ?, flag clear, or `q` M or uninit | install `v`; set the flag if flagged | | |
| T3.5 | `replace(mut q, take v)` | install `v`; result owns old, unfinalized | | |
| T3.6 | discarded `replace` result | dropped at statement end | its finalizer fails | `q` owns `v`; `cause+=e`; prop |
| T3.7 | whole destructure | `r` M; bound fields L; omitted fields dropped in declaration order | an omitted drop fails | `cause+=e`; next omitted drop; prop cleans bound fields |

**T4 Counted values**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T4.1 | `share(h)` | count + 1 | cannot fail | |
| T4.2 | holder drop, count > 1 | count − 1 (Arc: recorded) | cannot fail | |
| T4.3 | holder drop, count = 1 | payload released by worklist; each node `ref` (Arc: one recorded event) | cannot fail | |
| T4.4 | `try_unwrap(take h)` | count = 1: payload L, node `ref`; Arc: recorded | count > 1 | the error owns `h`; count unchanged |

**T5 Regions and limits**

| row | operation | success | failure | owner after failure |
|---|---|---|---|---|
| T5.1 | `region r { .. }` enter | `r` open | cannot fail | |
| T5.2 | region allocation of n bytes | `chg(n)` to the current limit | `Capacity` before any write | nothing allocated; earlier bytes stay charged |
| T5.3 | region close, any exit edge | views end; owners drop; all bytes `ref` | cannot fail | |
| T5.4 | `bound bytes n { .. }` enter | `chg(n)` to the parent limit; the body's limit is n | `Capacity`: the body does not run | `Error(c)` |
| T5.5 | bound exit, any edge | views end; owners drop; unused part `ref` to parent | cannot fail | |
| T5.6 | admission of n | used + n ≤ limit: used += n | `Capacity` | per the operation's commit point |
| T5.7 | refund of n | used −= n | cannot fail | |

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
| T7.1 | `spawn(..)` | captures moved into an environment temporary; admission (commit); the task owns it | `Capacity` | the temporary's captures dropped in reverse capture order; the error is `Capacity` |
| T7.2 | `send(tx, take x)` | recorded publication; the queue owns `x` | `Closed` | the error owns `x` |
| T7.3 | cancelled before publication | | `Cancelled` | the sender's cleanup drops `x` |
| T7.4 | `receive(rx)` | the receiver owns the item | `Closed` | |
| T7.5 | `join(take h)` | `Ok(r)`: the joiner owns `r` | `Failed(causes)` or `Cancelled` | the joiner owns the cause chain |
| T7.6 | handle dropped unjoined | child cancelled; shielded wait; its result dropped | child failed | causes dropped; recorded |
| T7.7 | queue dropped with items | items dropped, highest index first | an item's finalizer fails | `cause+=e`; continue |

Every refusal in this chapter is made when the definition is checked; nothing refused
runs.
