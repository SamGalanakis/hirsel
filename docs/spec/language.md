# Language

### LANG-6: Evaluation order

Evaluation is strictly left to right: a call's arguments, then the call; a construction's
fields; an operator's operands; an assignment's place, then its value
([OWN-12](ownership.md#own-12-assignment)). `&&` and `||` skip their right operand when the
left decides. A temporary lives to the end of its statement; in the condition of an `if` or
`while`, it ends when the condition has been evaluated, before a branch or the loop body
runs, except that one an `if let` or `while let` binding reaches lives as long as that
binding ([OWN-8](ownership.md#own-8-views)), and a pattern that does not match releases
its condition's temporaries before the `else` branch runs or the loop exits; in an operand of `&&` or `||`, it ends when that operand's `bool` has been produced; in
a match arm's expression, it ends when the arm's value has been produced, before the match's
own cleanup, unless the arm's value reaches it, and then it lives as long as the match's
value; a scrutinee's temporaries follow the expression the match is in, so they live across
every arm; in a block's tail, a function
body's included, it lives to that block's exit and ends before the block's locals are
cleaned, unless the block's value reaches it; a temporary that a `let` binding's value, or
a block's value, reaches lives as long as that value. A moved argument stays a
caller-owned temporary until the callee is entered
([FAIL-3](failure.md#fail-3-commit-points)).

```hirsel
fn f(d: Dir) -> Result[(HostFile, HostFile), IoError] waits {
  Ok((host.open(d, "a", Read)?, host.open(d, "b", Read)?))   // a is opened first
}
```
```hirsel
type T { name: String }
fn keep(take t: T) -> bool { let T(name: _) = take t; true }
fn f(take a: T) -> bool { a.name == "x" && keep(take a) }   // a.name's view ends at ==
```
```hirsel
fn copy_len(s: String) -> u64 {
  if let Ok(t) = string.from_utf8(string.into_bytes(string.from(s))) { string.byte_len(t) } else { 0 }   // the copy lives in the branch
}
```

### LANG-7: Names

There are no imports. Types and values have separate namespaces, and the grammar decides
which one a name is read in: a name in a type position (LANG-5) is a type, and any other
name is a value. A type name resolves, in order, to a builtin type name (LANG-10), a type
of the current module or a type of the **prelude**, and declaring a type with a builtin
type name is refused; a bare value name, with no dot, resolves to a local, a function or constant of the current
module, a function of the prelude, a module of the current package, a dependency package or
a core module. The prelude is the `core` module of the core library: every `pub`
declaration in `core/core/core.hirsel` ([core](core.md)), and nothing else. The head `a`
of a dotted path `a.b` resolves in this order: a local or parameter in scope, then a module
of the current package, a dependency package or a core module, then a constant of the
current module, and nothing else. If `a` is a local or a constant, `.b` selects a field of
its value; if a module or package, `.b` selects a member, so another module's constant is
read as `m.ORIGIN.x`. A module wins over a constant of its name. A function is never the
head of a path: a head that names only a function, or nothing, is refused here, at the
head, and is never rebound.

A package's **root module** is its module whose path is the package's name: module `store`
of package `store`, module `core` of the core library. A package name is the head of a path
into that package, and `.b` after it selects a member of its root module or a module of
the package: `store.fetch(..)` calls the root module's `fetch`, `store.Tx` names its type, and
`store.wal.append(..)` calls a member of module `wal`. The root module is reached only that
way, so `store.store.fetch` names nothing and is refused here, at the second `store`. Inside
the package the head `store` is the root module itself, as a module of the current package,
so the same path reads the same member there. A root module member that has the name of a
module of its package is refused here, at that member, with the module as the causal site,
so `.b` after a package name is never ambiguous. A package with no root module, as `std`,
is reached only through its modules (`std.order.compare`). So a module may
define a function with the name of a module, as an entry point `main` beside a module
`main`, and in it `main.main(..)` calls the module's `main`. In a type position the last
name of a path is a type (`path.Path`). So `fn f(c: char)` names the builtin type and `char.digit(c)` a module
member.

A package module may have the name of a core module; it then **extends** it. In a module of
package P, the name of a core module `m` reads one merged set: the core members of `m`, and
the `pub` members of every module named `m` in P and in P's dependencies (with the private
members of the module itself, when it is one of them). A member that has the name of a core
member is refused, so an extension never redeclares or shadows one; two extensions that
declare one name are refused where that name is read through `m`. Each member keeps its
definition identity (package, module path, name): `list.push` is the core library's and
`list.sort` the standard library's, and visibility and type identity read that identity.
An extension sees no private member of the core module. A local or parameter may not have
the name of a visible local, a value definition of its own module or a prelude function. It
may have the name of a module or a dependency package: in its scope the local wins, also as
the first component of a dotted path, so adding a module or a dependency never makes an
existing local illegal. `main`'s argument list is written `argv: Args` in this
specification; `args: Args` is equally legal. Two definitions with one name in one module are refused. Each attribute kind occurs at
most once on a declaration; a second occurrence is refused under LANG-7 at that attribute,
with the first occurrence as the causal site. Live fields of a record have distinct names; a
duplicate is refused under LANG-7 at the later field, with the first occurrence as the
causal site. Live variants of a variant type have distinct names; a duplicate is refused
under LANG-7 at the later variant, with the first occurrence as the causal site.

Variants are written bare (`Ok(x)`, `None`). Where a constructor or pattern has an expected
type ([LANG-11](#lang-11-typing-is-local); a pattern's is its scrutinee's), the variant is
selected from that type, whichever module, package or core module declares it, when the type
is visible there and not opaque ([PROG-2](programs.md#prog-2-visibility)). With no expected
type, or one that is still a hole, a bare value name that names no local, function or
constant resolves as a bare type name does: in the current module, then in the
prelude, and nowhere else. The current module includes its private types; a sibling module, a
dependency and a core module other than the prelude are outside, and an opaque type supplies
no constructor outside its module. At the first of those two levels that has a constructor of
that name, it is the record type of that name if that level declares one, and otherwise the
variant of the one type there that declares it. Two types at that level that declare it are
refused here, at the constructor, even when the other level has one too. A name neither level
has is refused here, and when a visible type elsewhere declares the variant the refusal names
that type's path and the fix, an expected type written with it, as `let j: work.Job = Done`
or `(Done : work.Job)`. So a variant added to core or a dependency never takes over or refuses
a constructor the current module declares, and a bare variant without an expected type reads
exactly as a bare type name does (the order at the start of this rule). A dotted path never
names a variant. Core roles (`Result`, `Capacity`, the capability types) are fixed by definition
path, never by spelling.

```hirsel
fn digits(s: String) -> Result[u64, ParseError] { int.parse(s) }
fn list(xs: List[u64]) -> u64 { list.len(xs) }            // list.len is the core module's
```
```hirsel
type Point { x: i64, y: i64 }
const ORIGIN: Point = Point(x: 0, y: 0)
fn left() -> i64 { ORIGIN.x }
```
```hirsel unbuilt
// in a package that depends on package store, whose root module is store/store.hirsel
fn count(db: store.Db, t: store.Table[u64, u64]) -> u64 {
  let n = result.unwrap(store.look(store.snapshot(db), None, fn(tx: store.Tx) -> u64 {
    option.unwrap(result.unwrap(store.fetch(tx, t, 1u64)))
  }));
  n
}
fn stutter(db: store.store.Db) -> u64 { 0 }               // refused: LANG-7, at the second store
```
```hirsel
fn width() -> u64 { 8 }
fn f() -> u64 { width.bits }                              // refused: LANG-7, width is a function
```
```hirsel
fn f(x: u64) -> u64 { let x = x + 1; x }                  // refused: LANG-7, shadows x
```
```hirsel
type Job = Done | Running
fn local() -> Job { let j = Done; j }                     // Job's: the module comes first
fn reply() -> DirReply { let r: DirReply = Done; r }      // selected by the expected type
fn ended(r: DirReply) -> bool { match r { Done => true, _ => false } }
```
```hirsel
fn f() -> u64 { let _ = Done; 0 }                         // refused: LANG-7, several prelude types declare Done
```
```hirsel
fn f() -> u64 { let _ = NotUtf8; 0 }                      // refused: LANG-7, env.EnvError is not in scope
fn g() -> env.EnvError { let e: env.EnvError = NotUtf8; e } // the fix: an expected type
```

### LANG-8: Signatures

Every top-level function writes a complete signature: parameter names, modes and types,
the result type and its anchors ([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)),
`waits` and region outlives clauses ([MEM-6](memory.md#mem-6-roots)). Checking a body reads that body, the signatures and type
summaries ([OWN-2](ownership.md#own-2-type-summaries)) it names. Ordinary calls take their
checking facts from the callee's signature.
The transform-fragment check for a `StaticFn` given to `flow.stage`
([CONC-14](tasks.md#conc-14-flows)) also reads each body in the instance set
[PROG-9](programs.md#prog-9-code-descriptors) defines. Editing a dependency's body
can therefore refuse that call.
A closure writes its parameter names and modes. It writes every parameter type, or none
of them when its expected type gives them ([LANG-11](#lang-11-typing-is-local)); its
result type may be omitted when the expected type gives it.

`waits` says that a call may wait and may observe cancellation
([CONC-7](tasks.md#conc-7-cancellation-points)). The program entry `main` can wait
with or without a written `waits` ([PROG-3](programs.md#prog-3-entry)). A bodiless core
function's or foreign function's
([FFI-2](foreign.md#ffi-2-foreign-modules)) `waits` is as written.
Every other function or closure is declared `waits`
exactly when its body calls a function, or invokes a callable, whose type is `waits`; one
declared `waits` without such a call is refused too. Cleanup never counts: finalizers and the
cleanup of task handles run shielded and observe no cancellation
([FAIL-11](failure.md#fail-11-resources), [CONC-8](tasks.md#conc-8-abandoned-handles)), so a
finalizer may call `waits` operations without being marked, and no implicit cleanup makes a
function `waits`. A flow transform's reach holds no resource, so its implicit cleanup
runs no user code.

A body that does not have its declared result type is refused by LANG-11, at the
expression that has the wrong type.

`cold` before `fn` declares a cold function: one the author expects to run rarely, such
as a slow path or a failure report. It changes no result, charge or failure, and it is not
part of the callable type, so a cold function's value fits the same callable types as an
ordinary one. An engine never inlines a cold function, weights each branch whose arm calls it
directly as unlikely, and places its body with the failure paths; an engine may ignore all
three. `cold` stands only before `fn` in a function item, after any `pub`
([LANG-5](syntax.md#lang-5-grammar)): a type, constant, closure, finalizer or clone body
cannot be cold. Core and std mark their own slow paths the same way.

```hirsel
fn twice(x: u64) -> u64 { x * 2 }
```
```hirsel
fn twice(x: u64) -> u64 waits { x * 2 }                   // refused: LANG-8, waits with no waiting call
```
```hirsel
cold fn report(x: u64) -> u64 { x + 1 }
fn step(x: u64) -> u64 { if x > 1000 { report(x) } else { x } }
```

### LANG-9: Nominal types

A type definition declares a record (`type P { x: i64, y: i64 }`), a variant type
(`type Shape = Circle(f64) | Rect(w: f64, h: f64)`, a single variant `Only()`) or a
transparent alias (`type Ids = List[u64]`), as LANG-5 tells them apart. An alias is its
right-hand side; a chain of aliases that reaches itself is refused, at the reference that
continues the chain in the member whose definition identity is least, as
[OWN-2](ownership.md#own-2-type-summaries) reports a cycle. A variant
needs its parentheses: `type Unit = Unit` has a type on its right, so it is an alias of
itself and refused by that rule, and a one-variant type named like its variant is
`type Unit = Unit()`. Payloads are all
labelled or all positional. A record is constructed with every field labelled, in
declaration order, `P(x: 1, y: 2)`, a field with a default
([SEM-10](semantics.md#sem-10-codecs)) included; a variant with labels exactly when its payloads are
labelled; labels appear in no other call. A labelled argument whose expression is a local of
the same name may be written as that name alone, **punned**: in a call of a record or of a
labelled variant, a bare name `x` is `x: x`, so with locals `x` and `y`, `Point(x, y)` is
`Point(x: x, y: y)` and `Point(x, y: 0)` mixes the two. The pun names a field, so the
declaration-order rule applies to it as written; any other unlabelled argument of a
labelled constructor, `Point(x + 1, y)`, is refused here, at that argument, and a positional
payload is never punned. Patterns pun the same way ([LANG-17](#lang-17-match)). The representation keyword (`resource`,
`scoped`), the `@compact` attribute, or their absence is fixed by the definition
([OWN-1](ownership.md#own-1-representation-is-declared-once)). A nominal type's identity is
its definition path; two definitions with equal fields are different types.

```hirsel
type Shape = Circle(f64) | Rect(w: f64, h: f64)
```
```hirsel
type Bad = A(x: u64, u64)                                 // refused: LANG-9, mixed payload labels
```
```hirsel
pub type OutOfScope = OutOfScope                          // refused: LANG-9, an alias of itself
```
```hirsel
type Point { x: i64, y: i64 }
fn up(x: i64, y: i64) -> Point { Point(x, y: y + 1) }
```
```hirsel
type Point { x: i64, y: i64 }
fn up(x: i64, y: i64) -> Point { Point(x, y + 1) }      // refused: LANG-9, y + 1 is not a pun
```

### LANG-10: Built-in types

The builtin type names are `bool`, `char`, `i8` to `i64`, `u8` to `u64`, `f32`, `f64`,
`Inline` ([SEM-9](semantics.md#sem-9-inline-arrays)) and `Simd` ([SEM-11](semantics.md#sem-11-vectors)); unit `()` and tuples `(A, B, ..)` are
written by syntax. Every other type a program names
without declaring it is declared in the prelude ([core](core.md)), `Option`, `Result`,
`String`, `List` and the capability types included. `Never` is the type of `return`,
`break`, `continue`, `fail` and a loop with no `break`, and converts to every type. Kinds
are in [OWN-3](ownership.md#own-3-kinds).

### LANG-11: Typing is local

Types are checked bidirectionally within one body: an expression is checked against an
expected type where one exists, and its type is synthesised otherwise. These are the
**sources of an expected type**, and nothing else is one: no later use, later operand, later
arm or exit gives an expression its type.

- a `let` with a written type: its initializer;
- an ascription `(e : T)` ([LANG-5](syntax.md#lang-5-grammar)): `e`;
- a call's parameter types and a constructor's declared field types, with what is fixed so
  far filled in, in the order below: each argument or field;
- the declared type of the place `q` in `q = e` ([OWN-12](ownership.md#own-12-assignment)):
  `e`; a compound assignment `q op= e` types as `q op e`;
- the function's declared result type: each `return e` and the body's tail; a closure's
  result type, written or expected: its body's tail;
- a closure's expected callable type ([LANG-13](#lang-13-callables)): the types of
  parameters the closure writes without one;
- the expected type of a block: its last expression; of an `if` or `match`: each branch or
  arm; of a loop: each `break` value;
- with none of those, the type of the first branch, arm or `break` value in source order
  whose type is not `Never`: every later one, so `if c { Some(1u64) } else { None }` types;
- a binary operator's left operand: its right operand; a literal operand takes the type of
  the other operand ([LANG-21](#lang-21-operators)); the expected type of a unary `-` or
  `~`: its operand;
- the expected type `T` of `e?`: `e`'s payload, and nothing else of `e`
  ([LANG-18](#lang-18-errors));
- the expected type of a tuple or an `Inline` array ([SEM-9](semantics.md#sem-9-inline-arrays)):
  each component or element;
- `bool` for a condition, a guard and each operand of `!`, `&&` and `||`; `u64` for an index, which a narrower unsigned index
  converts to ([LANG-21](#lang-21-operators)).

An ascription has the type it writes, and it is checked as `let t: T = e` checks its
initializer: the type is complete, a conversion below applies against it, and an `e` that
does not then have type `T` is refused here, at the innermost expression `T` reached. It is
not a cast: `(x : u64)` with `x: u8` is refused. It changes nothing else, so `(e : T)` is
evaluated, moved, viewed and cleaned as `e` is, and an ascribed place is that place. It
fixes a call's type parameters from its result, the one way to fix them inside an
expression: type arguments are never written at a call ([LANG-12](#lang-12-generics)), so
`f.[T](..)` is not syntax.

A closure omits its parameter types all together or not at all: one that writes some and
omits others is refused here, at the first parameter without a type. It may omit them only
where its expected type is a callable type whose every parameter type is fixed at that
point of the forward visit below, with no hole left in any of them; otherwise it is
refused here, at the first parameter. Each omitted type is the expected parameter type,
exactly. Modes are never omitted: the closure writes `take`, `mut` or `bytes` as the
callable type does, and one whose modes differ does not fit (LANG-13). Omitting the types
changes nothing else: the closure's kind, captures and fit are checked as if it had
written them. A top-level function still writes its whole signature (LANG-8).

An `if` without `else` has type unit, and an arm of type `Never` constrains nothing. An
expression whose type, after the conversions below, is not its
expected type is refused, at the innermost expression the expected type reached: in `if c
{ x } else { 0 }` checked against `u16` with `x: u8`, at `x`. This one rule covers every
source above.
A call has exactly one argument per parameter, in order; a missing argument is refused at
the call's closing parenthesis, and an extra one at the first extra argument. A call's
type parameters and regions are found in one forward visit of that call, in this order:

1. The expected result type, where there is one, fixes what it can and leaves the rest as
   **holes**.
2. Each argument, left to right, is checked against its parameter type with what is fixed
   so far filled in. An argument that is itself a call takes that partly known type as its
   expected type, a hole constraining nothing, and its own result may fill holes.
3. Last, each argument that is an unsuffixed literal, or a tuple or `[..]` array whose
   every component is such an argument, at any depth, is checked against its now fixed
   parameter type, so `f(1, x)` with `x: u8` types `1` as `u8`, and `apply((1, 2, 5),
   transfer)` types the tuple from `transfer`. A tuple or array with any other component is
   checked at step 2.

So `let n: u16 = result.unwrap(int.parse(s))` fixes `unwrap`'s `T`, then
`parse`'s `T`, then `unwrap`'s `E` as `ParseError`. A parameter determined twice
differently is refused at the argument or result that determined it second; a hole still
open when its call has been visited is refused by
[LANG-12](#lang-12-generics) at that call. Nothing is inferred across statements, from a
later use or between definitions. An unsuffixed number needs an
expected type ([LANG-3](syntax.md#lang-3-literals)); operands follow
[LANG-21](#lang-21-operators). There are no implicit conversions except these: `Never` to any type; a
`View[T]` of a plain `T` to `T` ([OWN-8](ownership.md#own-8-views)); a `Box`, `Rc` or `Arc`
to its payload, through nested holders, where its own type does not fit (OWN-8); a region value to its
view ([OWN-25](ownership.md#own-25-region-values-are-read-as-views)); `T in a` to `T in r`
when `a` outlives `r` ([MEM-6](memory.md#mem-6-roots)); an `IndexedSpan[T]` to a
`SeqSpan[T]`, for reading only, keeping its region, `W` and locator
([MEM-12](memory.md#mem-12-region-sequences)), never a `Writer`, a `Child` or a field's
declared encoding; a `u8`, `u16` or `u32` index to `u64`, zero-extended
([LANG-21](#lang-21-operators)); and a callable to a callable type it fits ([LANG-13](#lang-13-callables)).
A conversion applies to an expression whose type is already synthesised, against its
expected type; it never settles a type parameter, which the steps above fix as written. So
a holder nested in a generic call whose result type was fixed from outside is refused, at
the argument that fixes the parameter second: bind the holder with `let` first, or ascribe
it.

```hirsel
fn f() -> u64 { let x: u64 = 7; x }
```
```hirsel
fn f() -> u64 { let x = 7; x }                            // refused: LANG-11, 7 has no expected type
```
```hirsel
type Slot { len: u32 }
fn set(mut s: Slot, n: u64) { s.len = option.unwrap(int.try_narrow(n)) }
fn pick(c: bool) -> u64 { let o = if c { Some(1u64) } else { None }; option.unwrap(take o) }
```
```hirsel
fn pick(c: bool) -> u64 { let o = if c { None } else { Some(1u64) }; option.unwrap(take o) }
                                                          // refused: LANG-12, at None: T is open
```
```hirsel
type N = Num(i64)
fn ev(n: N) -> i64 { match n { Num(x) => x } }
fn f(take o: Option[Box[N]]) -> i64 { ev(option.unwrap(take o)) }  // refused: LANG-11, at take o: T is N
```
```hirsel
fn fits(s: String) -> bool { match (int.parse(s) : Result[u8, ParseError]) { Ok(_) => true, Error(_) => false } }
fn first(s: String) -> Result[u64, ParseError] { let n = (int.parse(s) : Result[u64, ParseError])?; Ok(n + 1) }
fn none_first(v: Option[u64]) -> bool { (None : Option[u64]) == v }
fn empty() -> u64 { list.len((list.new() : List[u64])) }
```
```hirsel
fn widen(x: u8) -> u64 { (x : u64) }                      // refused: LANG-11, at x: u8 is not u64
```
```hirsel
fn apply(f: fn(u64, take String) -> u64, x: u64, take s: String) -> u64 { f(x, take s) }
fn g(take s: String) -> u64 { apply(fn(n, take t) { n + string.byte_len(t) }, 1, take s) }
fn h() -> u64 { let inc: fn(u64) -> u64 = fn(x) { x + 1 }; inc(2) }
```
```hirsel
fn apply(f: fn(u64, u64) -> u64, x: u64) -> u64 { f(x, x) }
fn g() -> u64 { apply(fn(a, b: u64) { a + b }, 1) }      // refused: LANG-11, at a: all or none
```
```hirsel
fn fold[A, T](xs: List[T], f: fn(A, T) -> A, take init: A) -> A { init }
fn total(xs: List[u64]) -> u64 { let sum = fold(xs, fn(acc, x) { acc + x }, 0); sum }   // refused: LANG-11, at acc: A is open
```
```hirsel
fn f(c: bool, x: u8) -> u16 { if c { x } else { 0 } }     // refused: LANG-11, at x: u8 is not u16
```
```hirsel
fn f(x: u8) -> u8 { x }
fn g() -> u8 { f(1, 2) }                                  // refused: LANG-11, at 2: f takes one argument
```

A call's argument-count or type refusal names the expected signature. Types in the
diagnostic use their defining package and module paths, so `std@1/upgrade/Socket`
and `core@1/core/Socket` remain distinct even though both are named `Socket`.

### LANG-12: Generics

A definition may take type parameters with bounds, `[T: key + send]`. A generic body is
checked once and uses only the bounds it declares
([OWN-23](ownership.md#own-23-generic-bodies)). Type arguments are never written at a
call: they are found from that one call, as LANG-11 orders it, or the call is refused here.
A type argument that lacks a bound its
parameter declares is refused when it is fixed, at what fixed it in LANG-11's order: at the
call when the expected result fixed it, otherwise at the argument that did. An argument that
later agrees with it is not a second site. A type argument written in a type, as in
`Rc[HostFile]` for `Rc[T: shareable]`, that lacks a bound its parameter declares is refused
here, at that argument. A record or variant constructor is typed exactly as such a
call, its payloads the arguments: its type arguments are fixed by its expected type and its
own payloads, and one still open when the constructor has been typed is refused here, at the
constructor. Nothing to its right, in its statement or later, fixes it, so the checker emits
each expression as it types it. So `let x = None`, `let y = Ok(1u64)`, `let xs =
list.new()` and `let r = if c { None } else { Some(1u64) }` are refused without a written
type, on the binding or as an ascription ([LANG-11](#lang-11-typing-is-local)), and
`None == v` is refused where `v == None` and `(None : Option[u64]) == v` are accepted. Each use of a
generic type writes its arguments.

A type definition is refused when following its recursive references requires infinitely
many type instances. The refusal is at the field or variant payload that closes a growing
cycle, whether or not the definition is instantiated. A cycle grows when it feeds a
parameter back nested inside a type constructor; one that feeds its parameters back
permuted, closed or projected reaches finitely many instances and is accepted. Removed
fields and variants ([SEM-10](semantics.md#sem-10-codecs)) count.

```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
```
```hirsel
fn same[T](a: T, b: T) -> bool { a == b }                 // refused: LANG-21, T is not key
```
```hirsel
fn keep(r: Option[Rc[HostFile]]) -> u64 { 0 }             // refused: LANG-12, at HostFile: not shareable
```
```hirsel
fn empty() -> u64 { list.len(list.new()) }                // refused: LANG-12, at list.new(): T is open
```
```hirsel
fn f() -> bool { let x: Option[u64] = None; x == Some(1u64) }
fn g(v: Option[u64]) -> bool { v == None }
```
```hirsel
fn f() -> bool { let x = None; x == Some(1u64) }          // refused: LANG-12, at None: T is open
fn h(v: Option[u64]) -> bool { None == v }                // refused: LANG-12, at None: T is open
fn g() -> u64 { let y = Error(1u64); 0 }                  // refused: LANG-12, at Error(1u64): T is open
```
```hirsel
fn f(a: f64, b: f64) -> f64 { int.wrapping_add(a, b) }    // refused: LANG-12, at the call: f64 is not integer
fn g(a: f64, b: f64) -> bool { int.wrapping_add(a, b) == a }  // refused: LANG-12, at a: f64 is not integer
```
```hirsel
type Tree[T] { label: T, kids: List[Tree[T]] }
type Swap[A, B] { kids: List[Swap[B, A]] }               // two instances: Swap[A, B], Swap[B, A]
```
```hirsel
type Nest[T] = Leaf(T) | Deeper(List[Nest[(T, T)]])      // refused: LANG-12, at Deeper's payload: Nest[(T, T)] grows
```

### LANG-13: Callables

A callable type has one of three kinds, written in the type:

| type | admits | kind | size |
|---|---|---|---|
| `fn(A, mut B, take C) -> R` | top-level functions and closures that capture nothing | plain, `send` | 8 |
| `own fn(..) -> R`, `own[send] fn(..) -> R` | the above, and **owning closures** (LANG-14); `own[send]` only those whose captures are all `send` | own; `send` only with `[send]` | 16 |
| `scoped[view] fn(..) -> R` | a scoped closure whose fully transitive capture descriptor (OWN-21), its own entries with those of every scoped callback it invokes, passes on, returns or stores, has view entries only; and plain and owning callables | scoped | 16 |
| `scoped fn(..) -> R` | any closure, and the above | scoped ([OWN-21](ownership.md#own-21-scoped-callbacks)) | 16 |

The bracket of an owning callable type parses `view` or any bounds (LANG-5) but holds
nothing but `send`: `view`, any other bound, or a second bound, is refused here, at the
first name unless that is `send` and otherwise at the second. The bracket of a scoped
callable type holds nothing but `view`, and a bound there is refused here, at the first
bound name. A scoped callback the closure received as an unqualified `scoped fn` has an
unknown descriptor and counts as a `mut` entry; its `mut` and `take` parameters are not
captures, and each activation checks them (OWN-9 to OWN-11). The descriptor is summarized
where the closure is defined, and a closure that does not fit is refused here; the
diagnostic points at the first `mut` entry. A `scoped[view] fn` fits a `scoped fn` of the
same parameters and result, losing its promise; a `scoped fn` never fits a `scoped[view]
fn`. Its result names no anchor (OWN-20). An owning closure is never `shareable`
([OWN-4](ownership.md#own-4-bounds)). A callable type also says whether it is `waits` (LANG-8). A callable fits a callable type
when its parameter modes and types and its result are identical, its kind is admitted, and
it is `waits` only if the type is; one that does not fit is refused here, at the callable.
A result's written anchors ([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor))
are part of it, and a callable type's parameters have no names, so its scoped result can
name only a region, and one that names no anchor is refused at the written result type,
OWN-20; a function whose result is anchored to one of its own parameters fits no callable
type.
A parameter's mode is unmarked, `mut`, `take` or `bytes`, so a function with a `bytes`
parameter has a callable type such as `fn(bytes u64) -> u64`. Calling a callable never
consumes it and follows the modes of a direct call, `bytes e` included
([OWN-7](ownership.md#own-7-parameter-modes)). Any call through a callable may allocate. Cleaning an owning closure
cleans its captures, which hold no resource (LANG-14), so it runs no finalizer and admits
nothing ([FAIL-13](failure.md#fail-13-mechanical-release)).

```hirsel
fn apply(f: fn(u64) -> u64, x: u64) -> u64 { f(x) }
fn main_like() -> u64 { apply(fn(x: u64) -> u64 { x + 1 }, 1) }
```
```hirsel
fn run(take f: own[send] fn() -> u64) -> u64 { f() }
```
```hirsel
fn sized(bytes n: u64) -> u64 { n }
fn f() -> u64 { let h: fn(bytes u64) -> u64 = sized; h(bytes 7) }
```
```hirsel
fn sized(bytes n: u64) -> u64 { n }
fn f() -> u64 { let h: fn(u64) -> u64 = sized; h(7) }     // refused: LANG-13, at sized: bytes differs
```
```hirsel
fn run(take f: own[shareable] fn() -> u64) -> u64 { f() }   // refused: LANG-13, at shareable
```
```hirsel
fn apply(f: fn(u64) -> scoped fn(u64) -> u64, a: u64) -> u64 { let g = f(a); g(1) }
                                                          // refused: OWN-20, the result names no anchor
```
```hirsel
fn keep(take s: String) -> fn() -> u64 { fn() -> u64 { string.byte_len(s) } }
                                                          // refused: LANG-13, captures s
```

### LANG-14: Captures

A closure's captures are the locals of enclosing scopes that its body names; naming
`r.f` captures `r`. A closure written `scoped fn`, or whose expected type is a `scoped fn`,
is scoped: a capture it only reads is a view of the place it reads and a capture it
assigns or lends `mut` is a `mut` loan of that place, held while its capture descriptor is
active ([OWN-21](ownership.md#own-21-scoped-callbacks)), and it never moves a capture. Any other
closure with captures is an **owning closure**: creating it admits one environment block
([MEM-24](budgets.md#mem-24-logical-charges)) and then copies its plain captures into it and
moves the others, whose slots become moved ([OWN-5](ownership.md#own-5-availability)); its
literal has type `own fn(..) -> R`, `own[send]` when every capture is `send`. A refused
environment admission raises the failure `Capacity` ([MEM-21](budgets.md#mem-21-admission))
before any capture is moved, and the captures stay in their slots for the failure's cleanup
([T1.2](ownership.md#own-26-the-transition-table)). Its body reads captures as views and
cannot move, assign or lend them `mut`. An owning closure may capture only a value whose
type holds no resource, no capability, no view, no scoped value and no region reference at
any depth, as its type
summary records ([OWN-2](ownership.md#own-2-type-summaries)): plain values other than
capabilities, own values, counted holders of such values, and nothing whose summary may
hold a capability, such as a type parameter or `Wrap[T]` for `type Wrap[T: plain] { item: T }`,
unless its bounds include `inert` or `codec`, which implies it
([OWN-4](ownership.md#own-4-bounds)): `T: inert` and `Wrap[T]` for `T: codec` may be captured. Resources and capabilities reach a closure as call arguments. The environment
holds the captures in the order the body first names them, and cleaning it cleans them in
reverse.

```hirsel
fn counter(take base: List[u64]) -> own fn(u64) -> u64 {
  fn(x: u64) -> u64 { x + list.len(base) }
}
```
```hirsel
fn counter(base: List[u64]) -> own fn(u64) -> u64 {
  fn(x: u64) -> u64 { x + list.len(base) }                // refused: LANG-14, base is a view
}
```

### LANG-15: Blocks and labels

A block, `loop`, `while` and `for` are expressions. A block's value is its last
expression. A `loop` evaluates to the value of the `break` that leaves it; `while` and `for`
evaluate to unit. The body of a `loop`, `while` or `for` is checked against unit
(LANG-11), so a value it should keep is bound or discarded with `;`. Any of them may carry
a label, `'l:`. `break v` leaves the innermost loop with `v`, `break 'l v` the labelled
block or loop; `continue` selects the innermost loop or the labelled one. A label cannot
shadow an active label, and no exit crosses a closure. These are refused at the exit: a
`break` or `continue` with no loop to leave, a `continue` whose label names a block, and a
`break` with a value out of `while` or `for`, which have no value to give.
Every exit cleans up the scopes it leaves ([FAIL-8](failure.md#fail-8-exit-edge-cleanup)).

```hirsel
fn find(xs: List[u64], k: u64) -> Option[u64] {
  let mut i: u64 = 0
  loop { if i == list.len(xs) { break None }; if xs[i] == k { break Some(i) }; i += 1 }
}
```
```hirsel
fn f(c: bool) -> u64 { 'b: { if c { continue 'b }; 1 } }  // refused: LANG-15, 'b labels a block
```
```hirsel
fn f(c: bool) { while c { break 5 } }                     // refused: LANG-15, while has no value
```

### LANG-16: Loops

`loop` repeats until a `break`. `while c` and `while let p = e` test before each iteration.
`for x in e` iterates exactly the sources of [SEM-6](semantics.md#sem-6-iteration), in its
modes; `for take x in e` consumes an own `List` and `for take (k, v) in e` an own `Map`, and
no other iteration consumes in v1. A `for` pattern is irrefutable. Bindings made by one iteration end with it
([OWN-17](ownership.md#own-17-loops)). To walk a String's UTF-8 scalars, start at byte
offset zero and advance to the byte offset returned by `string.next_scalar` on
each iteration.

```hirsel
fn scalars(s: String) -> u64 {
  let mut n: u64 = 0
  let mut at: u64 = 0
  while let Some((_, next)) = string.next_scalar(s, at) { n += 1; at = next }
  n
}
```

### LANG-17: Match

`match` is exhaustive over the scrutinee's type, and arms are tried in order. A guard `p if g
=> e` is a `bool` expression that may not move, assign or lend `mut`, and a guarded arm
never counts toward exhaustiveness. Patterns are literals, closed ranges `a..=b` of
integers or `char`, names, `_`, tuples and variants with labelled or positional fields, and
`..` for remaining fields. A labelled field's pattern may be its label alone, punned as in
construction ([LANG-9](#lang-9-nominal-types)): `Rect(w, h)` is `Rect(w: w, h: h)` and binds
`w` and `h`, and `Point(x, ..)` binds only `x`. A bare name among labelled fields that is not a
field's label is refused here, at that name. A literal pattern matches exactly when `scrutinee == literal` is
true, and is refused here wherever that `==` would be refused
([LANG-21](#lang-21-operators)). Floats are not `key`, so a float literal is never a
pattern. Use `float.total_order` for total ordering, or `float.eq`, `float.lt`, `float.le`,
`float.gt` and `float.ge` for IEEE 754 comparisons. `let p = e` needs an irrefutable pattern; `let p = e else { .. }`
takes a refutable one, and its `else` block has type `Never`. `if let` and `while let` take
refutable patterns. Arms of an owned scrutinee bind by
[OWN-15](ownership.md#own-15-match-on-an-owned-value). Inside a constructor or tuple
pattern of a `match` arm, an `if let` or a `while let`, a name may be marked `take`
(`Some(take s)`) or `mut` (`File(mut f)`); `mut x` names the matched place of a writable
scrutinee rather than binding a value (OWN-15). A `mut` binding that is an arm's whole
pattern, or that is in a `let`, `let .. else` or `for` pattern, is refused here, at the
binding.

```hirsel
type Shape = Circle(f64) | Rect(w: f64, h: f64)
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r, Rect(w: w, h: h) => w * h } }
```
```hirsel
type Shape = Circle(f64) | Rect(w: f64, h: f64)
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r, Rect(w, h) => w * h } }
```
```hirsel
type Shape = Circle(f64) | Rect(w: f64, h: f64)
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r } }   // refused: LANG-17, Rect missing
```
```hirsel
fn zero(x: f64) -> u64 { match float.total_order(x, 0.0) { Equal => 1, _ => 2 } }
```
```hirsel
fn zero(x: f64) -> u64 { match x { 0.0 => 1, _ => 2 } }   // refused: LANG-17, f64 is not key
```
```hirsel
fn read_some(mut input: Option[HostFile], mut buf: Bytes) -> Result[u64, IoError] waits {
  match input { Some(mut f) => host.read(mut f, mut buf), None => Ok(0) }
}
```
```hirsel
fn whole(mut s: String) { match s { mut t => {} } }      // refused: LANG-17, a mut binding is a payload
```
```hirsel
fn unpack(take o: Option[String]) { let Some(mut s) = o else { return } }  // refused: LANG-17
```

### LANG-18: Errors

Expected failures are `Result[T, E]` values. `e?` on `Result[T, E]` consumes `e` and
evaluates to the `Ok` payload, or exits the function with `Error` of the same payload; the
function's error type must be exactly `E`. `e?` on `Option[T]` in a function returning
`Option` exits with `None`. `?` always exits the innermost enclosing function or closure
body; no bound or region scope stops it.

A different error type is converted at the exit with a **mapping propagate**, `e ?=> m`,
or with `match`. `e` is a `Result[T, E]` and the function returns `Result[_, F]`; `e ?=> m`
consumes `e` and evaluates to the `Ok` payload, or, on `Error(x)`, evaluates `m` applied
to `x` and exits the function with `Error` of that value. The **mapper** `m` is one of:

- a path naming a variant of `F` whose one payload has type `E`, as `?=> Io`;
- a path naming a callable, a function or a local, that applied as `m(take x)` has result
  type `F`, as `?=> json.wrap`;
- an inline arm `|p| v`: `p` an irrefutable pattern over `E`, binding `x` as `let p =
  take x` would, and `v` an expression of type `F`, as `?=> |(c, _)| Full(c)` or
  `?=> |_| Invalid`. The arm is code of the enclosing function, not a closure: it captures
  nothing, may name the function's locals, and its own `?` exits that function.

`m` is checked against the expected type `fn(take E) -> F`, which a variant, callable or arm
must fit; nothing global converts errors, and there is no conversion trait. The mapper runs
on the error edge before it leaves any scope, so an arm sees the locals `e` could see; the
exit then stages `Error(m(x))` as its pending result and cleans the exited scopes
([FAIL-9](failure.md#fail-9-pending-results)). `?=>` applies to a `Result` only, in a
function whose result is a `Result`; on an `Option` it is refused.

Which exit `?` or `?=>` takes is chosen from its operand's type, so nothing gives the operand an expected error type: an expected
type `T` of `e?` reaches `e` only as its payload; whether `e` is a `Result` or an `Option`,
and its error type, are found from `e` alone, and neither the function's result type nor a mapper is an expected type of `e`.
A type argument that only the error would fix is refused by
[LANG-12](#lang-12-generics) at its call; the error type is written on a binding or an
ascription, `(receipt() : Result[u64, ParseError])?`. An expression statement whose
value is a `Result` is refused; `let _ = e` discards it explicitly.

```hirsel
fn half(n: u64) -> Result[u64, ParseError] { if n % 2 == 0 { Ok(n / 2) } else { Error(OutOfRange) } }
fn quarter(n: u64) -> Result[u64, ParseError] { let h = half(n)?; half(h) }
```
```hirsel
fn half(n: u64) -> Result[u64, ParseError] { Ok(n / 2) }
fn quarter(n: u64) -> Result[u64, ParseError] { half(n); Ok(0) }   // refused: LANG-18, unused Result
```
```hirsel
fn receipt[E]() -> Result[u64, E] { loop {} }
fn f() -> Result[u64, ParseError] { let r: Result[u64, ParseError] = receipt(); let n = r?; Ok(n) }
```
```hirsel
fn receipt[E]() -> Result[u64, E] { loop {} }
fn f() -> Result[u64, ParseError] { let n: u64 = receipt()?; Ok(n) }   // refused: LANG-12, at receipt(): E is open
```
```hirsel
type ReadError = Parse(ParseError) | Short(u64)
fn half(n: u64) -> Result[u64, ParseError] { Ok(n / 2) }
fn read(n: u64) -> Result[u64, ReadError] { let h = half(n) ?=> Parse; Ok(h) }
fn mark(n: u64) -> Result[u64, ReadError] { Ok(half(n) ?=> |_| Short(n)) }
```
```hirsel
fn half(n: u64) -> Result[u64, ParseError] { Ok(n / 2) }
fn f(n: u64) -> Option[u64] { Some(half(n) ?=> |_| 0) }   // refused: LANG-18, the function returns an Option
```

### LANG-19: Recursion

Functions may call each other and themselves freely; each call's signature is all the
caller needs. Exhausting the stack is a run stop ([FAIL-15](failure.md#fail-15-run-stops)),
not a failure. Mechanical release ([FAIL-13](failure.md#fail-13-mechanical-release)) uses
constant stack whatever the depth of the value; a finalizer is an ordinary call.

### LANG-20: Constants

`const N: T = e` declares a plain constant, a static string or an assertion. Its initialiser
`e` is a **closed expression**: it names literals, constants, functions and types, and no
local or parameter. Hirsel has no other compile-time code: no marker on a function, no
compile-time parameter and no compile-time block ([reserved](reserved.md#not-in-v1)).

**Two kinds of constant, one keyword.** A **manifest constant expression** uses only
literals, operators, tuple, plain and `Inline` array constructions, manifest constants, the
toolchain constants below and `bytes[T]` ([MEM-33](budgets.md#mem-33-reading-a-logical-charge));
it calls no function. A constant whose initialiser is one is a **manifest constant**. A
**computed constant** is one whose initialiser calls a function or names a computed
constant, and every constant of type `FrozenBytes`, since no manifest constant expression
has that type. Which kind a constant is follows from its type, its initialiser and the
constants it names, without running anything.

- **Manifest constants** are evaluated once, before any body is checked, in dependency
  order, by the rules of [semantics](semantics.md). They belong to the declarations: types,
  retention ([PROG-6](programs.md#prog-6-one-meaning)) and checking may read their values.
- **Computed constants** are evaluated after their initialiser and
  every function it can reach are checked. The initialiser's retained code
  ([PROG-10](programs.md#prog-10-code-addresses)) runs as a call with no argument, with the
  meaning the reference evaluator gives it, under the budget of
  [MEM-34](budgets.md#mem-34-constant-budgets). Nothing decided before that reads a computed
  constant's value: no type, no retention (PROG-6) and no judgment of the checker depends on
  it, so a computed constant is opaque to checking. A body that reads one reads its value at
  run time (PROG-10), and an engine may still fold it.
- **Every computed constant is evaluated.** Whenever a package is checked or built,
  `hirsel check` included, each computed constant it declares is evaluated, whether or not
  any code reads it. So acceptance never depends on the command or on what an entry reaches.

**What an initialiser may call.** It may call any function, with no marker on the function,
except these:

- A function whose type is `waits`. An initialiser is not a `waits` context, so the call is
  refused at the call, before anything runs.
- A function that needs a capability. The initialiser holds none to pass: authority enters a
  program only through its entry ([CAP-1](capabilities.md#cap-1-authority-is-explicit)). So
  the call is a type error, found before anything runs.
- A function on the **closed list** below, or a foreign function
  ([FFI-2](foreign.md#ffi-2-foreign-modules)). These take no capability, so typing admits
  the call. Reaching one while the initialiser runs refuses the constant, with the call
  chain. A foreign function runs on the host's stack
  ([FFI-7](foreign.md#ffi-7-host-stack-and-waits)) and cannot run for another target. The
  closed list holds the core functions that make authority, provider-held keys or task
  objects, or that read the run itself:
  - every function of core module `provide`, which makes virtual providers, and so
    capabilities, from nothing;
  - `secret.seal` and `schedule.quic_initial`, which make a `Secret` and a `Schedule`,
    whose key material a provider holds outside the run;
  - `channel.new`, `channel.sender`, `conn.new`, `conn.client`, `latest.new`,
    `latest.handle` and `flow.new`, which make task objects ([tasks](tasks.md));
  - `runtime.root_limit`, which reads the run's limit; `task.check_cancel`, which reads its
    cancellation; `unique.new`, which makes a fresh identity; and `code.revision`, whose
    value depends on how the compiler lowers code
    ([PROG-9](programs.md#prog-9-code-descriptors)).

  The list is closed, and only core extends it. A core function added later that makes one
  of these things or reads the run joins the list in the change that adds it. Every other
  core function means in an initialiser what it means at run time.

**The value.** A computed constant's type is plain
([OWN-1](ownership.md#own-1-representation-is-declared-once)) and holds no function value
(no `fn` type and no `StaticFn`), or it is `String` or `FrozenBytes`. Any other type is
refused here, at the type, `Bytes` included: a `Bytes` is a `List[u8]`, and no `List` is
static ([core](core.md)), so the static byte type is `FrozenBytes`. The initialiser may
build `List`, `Map` and `String` values while it runs. None of them survives, because a
plain value holds no owned storage, so no check at run time is needed.

**Static values.** A `String` or `FrozenBytes` constant is a **static computed constant**.
Only the bytes its initialiser returns survive the run: the toolchain stores them once, in
read-only static storage, the carrier a literal of that type uses ([core](core.md)), and the
run's own storage ends with the run. Each read of the constant is the literal with the same
bytes:
- a `String` constant reads as the string literal: static, capacity 0, allocating nothing
  and never charged, and `""` when it is empty;
- a `FrozenBytes` constant reads as the byte-string literal where `FrozenBytes` is expected:
  read-only static storage, with the literal's reference charge, which an engine can omit
  ([MEM-28](budgets.md#mem-28-charges-are-an-upper-bound)).

There is no second representation: no program can tell a static computed constant from the
literal with the same bytes, by content, length, capacity or charge. Its size is bounded by
the run that made it: the returned bytes were admitted under the constant's root limit
([MEM-34](budgets.md#mem-34-constant-budgets)), so a value is at most 64 MiB, and no other
limit applies. A `String`'s bytes are UTF-8 because the run built a `String`.

**Refusal.** A computed constant is refused when its run fails
([FAIL-2](failure.md#fail-2-causes)), reaches a function on the closed list or a foreign
function, or exceeds its budget (MEM-34). The refusal names the constant, the cause (a
failure's primary cause with its message, the closed-list or foreign function, or the budget
that ran out), and the chain of calls from the initialiser to the site, innermost first. An
initialiser that reaches a body refused already, or reads a constant whose initialiser does,
is not run: that body's refusal stands for the constant's.

**Cycles.** A constant that needs its own value is refused. For a manifest constant, that
means its constants reach itself. For a computed constant, it means its initialiser can
reach a read of the constant, through any function it can reach and any constant those
read. Reach is PROG-10's slot reach, so this refusal is decided before anything runs.

**Assertions.** A constant named `_`, `const _: T = e`, is an
**assertion**. Its initialiser is evaluated like any other constant's, manifest or computed,
and its value is kept nowhere. `_` names nothing, so a module may declare any number of
assertions, none of them can be read, and a `pub` assertion is refused. An assertion whose
run fails refuses the build, as any constant's failure does.

A **static string** is a constant of type `String` whose
`e` is a string literal or another static string, `const NAME: String = "hirsel"`. A
`String` constant whose `e` calls a function or names a computed constant is a static
computed constant, above. Any other `e` is refused: never a concatenation or a byte
string. Each use of a static string is its literal,
under the literal's contract ([core](core.md)): static, allocating nothing and never
charged. A `String` appears in no other constant's type, so `(String, u64)` and a record
with a `String` field are refused here, at the type, and so is every other non-plain type.

**Toolchain constants**. The constants of core module `target`, `os` and
`arch`, end at their type, by the core library's power to leave a declaration to the
implementation ([core](core.md)): the toolchain supplies each value from the build's target
([PROG-6](programs.md#prog-6-one-meaning)). They are the only constants of that kind, and a
constant without an initialiser anywhere else is refused. A constant **depends on `target`**
when its initialiser names a toolchain constant or a constant that depends on `target`, or,
for a computed constant, when a function its initialiser can reach does. Such a constant is
evaluated once for each target the `target` module names, reserved ones included. Each
evaluation of a computed one runs that target's retained code (PROG-10). A failure on any
target refuses the constant on every target, so the target never decides acceptance. Each
such refusal ends `, for target <triple>`, naming the first target that fails. The checker
evaluates no body for a manifest constant.

```hirsel
const WORDS: u64 = 64
const BYTES: u64 = WORDS * 8
```
```hirsel
const BIG: u8 = 200 + 100                                 // refused: LANG-20, Overflow
```
```hirsel
const NAME: String = "hirsel"
const TITLE: String = NAME
fn title() -> String { TITLE }
```
```hirsel
const PAIR: (String, u64) = ("a", 1)                      // refused: LANG-20, String in a tuple
```
```hirsel
fn square(n: u64) -> u64 { n * n }
const AREA: u64 = square(12)                              // computed: 144
const SIDES: u64 = 4                                      // manifest
```
```hirsel
fn fits(size: u64, limit: u64) {
    if size > limit { core.fail("record is larger than its cache budget") }
}
type Particle { x: f32, y: f32, z: f32, mass: f32 }
const _: () = fits(bytes[Particle], 16)                   // accepted: 16 is not over 16
```
```hirsel
fn grow(n: u8) -> u8 { n + 200 }
const BIG: u8 = grow(100)                                 // refused: LANG-20, Overflow in grow
```
```hirsel
fn fresh() -> u64 { let _ = unique.new(); 1 }
const ID: u64 = fresh()                                   // refused: LANG-20, unique.new is on the closed list
```
```hirsel
fn width() -> u64 { 4 }
fn pick(n: u64) -> u64 { if WIDE { n } else { 0 } }       // both arms retained: WIDE is computed
const WIDE: bool = width() > 2
```
```hirsel unbuilt
fn label() -> String { let mut s = string.new(); string.push(mut s, "page-"); string.push_u64(mut s, 64); s }
const LABEL: String = label()                             // static computed: reads as "page-64"
fn ramp() -> FrozenBytes {
    let mut b: Bytes = list.new()
    let mut i: u64 = 0
    while i < 4 { list.push(mut b, 1u8); i += 1 }
    bytebuf.freeze(take b)
}
const RAMP: FrozenBytes = ramp()                          // static computed: reads as b"\x01\x01\x01\x01"
```
```hirsel
fn raw() -> Bytes { list.new() }
const RAW: Bytes = raw()                                  // refused: LANG-20, a Bytes constant is never static
```

Version note (2026-11-25, comptime-phase1-spec-2686; COMPTIME-DIRECTION decisions 1, 2, 3a, 6
and 11, ruled A; not yet built): a constant's initialiser may call functions. It was "calls no
function". The constants LANG-20 admitted before are now the manifest constants, with the same
meaning, so every program accepted before is accepted with the same values. A computed
constant is evaluated after checking, under MEM-34's budget. Its value decides no type and no
retention. `const _` is an assertion. The closed list was settled from core's operation table
(the runtime lanes of the front's core bindings). Beyond the direction's six functions, it
adds every `provide` function, which mints virtual capabilities without authority, so CAP-1
alone does not keep a provider out of an initialiser. It also adds the key-material makers and
the task-object makers. `charge.latest` is not on the list: it is a MEM-33 recipe, a pure
function. Reversible before 1.0: the spec only. Shrinking the closed list or raising MEM-34's
numbers later turns refusals into acceptances and changes no accepted value.

Version note (2026-11-25, comptime-phase2b-2715): computed constants and assertions are
built. Their examples above are checked. An initialiser that reaches a refused body is not
run, so a constant reports no second refusal for a body refused already; this adds no
refusal and removes none of the program's. A `String` constant whose initialiser calls a
function is refused as a static string, at the initialiser, as before this change; the text
had also said "at the type". Reversible: the spec and the driver.

Version note (2026-11-26, comptime-phase4-bytes-2836; COMPTIME-DIRECTION C4 and decision 7,
ruled A; not yet built): a computed constant may have type `String` or `FrozenBytes`, stored
in the literal's static carrier and read as the literal with the same bytes. A `String`
constant whose initialiser calls a function was refused as a static string; it is now
computed. A `FrozenBytes` constant was refused at its type; it is now always computed. The
direction says "`Bytes` or `String`", but `Bytes` is `List[u8]` and no `List` is static
(core, "Literals"), so the static byte type is `FrozenBytes`, the type a byte-string literal
already takes in static storage, and `Bytes` stays refused. Every program accepted before is
accepted with the same values. Reversible before 1.0: the spec, the front and the driver.

### LANG-21: Operators

An operator applies to the operand types this table lists and to no other; anything else is
refused at the operator. A binary operator's operands have one type, except that a vector
shift takes a scalar count of its lane type. A compound assignment `a op= b` types as
`a op b` and installs the result ([OWN-12](ownership.md#own-12-assignment)).

| operators | operands | result |
|---|---|---|
| `+ - * /` | one integer type, or one float type | the operand type |
| `%` | one integer type | the operand type |
| unary `-` | a signed integer type, or a float type | the operand type |
| `& \| ^`, unary `~` | one integer type, or one vector type ([SEM-11](semantics.md#sem-11-vectors)) | the operand type |
| `<< >>` | an integer left operand and a count of its type ([SEM-1](semantics.md#sem-1-integers)), or a `Simd[T, n]` left operand and a count of type `T` (SEM-11) | the left type |
| unary `!`, `&&`, `\|\|` | `bool` | `bool` |
| `== !=` | one `key` type ([SEM-7](semantics.md#sem-7-equality-and-order)) | `bool` |
| `< <= > >=` | one `ordered` type (SEM-7) | `bool` |
| index `a[i]` | `a` a `List[T]`, an `Inline[T, n]` or a `FixedSpan[T]`, or a view of one; `i` a `u8`, `u16`, `u32` or `u64` | a place of type `T` |

Indexing reads and assigns as [SEM-4](semantics.md#sem-4-lists),
[SEM-9](semantics.md#sem-9-inline-arrays) and [MEM-12](memory.md#mem-12-region-sequences)
say. An index of type `u8`, `u16` or `u32` is zero-extended to `u64` and indexes exactly as
that `u64` does, through parentheses; its type is the index expression's own, synthesised
with `u64` expected, so a branch, arm or call inside it is still checked against `u64`. A
signed, `char` or any other index is refused by LANG-11, and this is the only position that
converts an integer: an argument, a field, an operand or a shift count of a narrower type is
refused. No other type is indexable: an index or index assignment on `String`, `Text`, `Map`,
`SeqSpan`, `IndexedSpan` or any user type is refused here, at the index expression. A
string is read through `string.as_bytes` or `string.next_scalar`, and a map through
`map.get` and `map.insert`, whose results say what a missing key does.

```hirsel
type Node { next: u32 }
fn follow(nodes: List[Node], at: u32) -> u32 { nodes[nodes[at].next].next }
fn byte(b: Bytes, i: u8) -> u8 { b[(i)] }
```
```hirsel
fn at(xs: List[u64], i: i32) -> u64 { xs[i] }              // refused: LANG-11, at i: i32 is not u64
```
```hirsel
fn get(xs: List[u64], i: u32) -> Option[u64] { list.get(xs, i) }   // refused: LANG-11, at i: u32 is not u64
```

A **literal operand** is an unsuffixed number, or a unary `-` or `~`, a binary operator or
parentheses applied only to literal operands; it synthesises no type. It is evaluated when
the definition is checked, in the type it takes, by the rules of SEM-1 and SEM-2; a result
that does not fit, a zero divisor or any other failure is refused there
([LANG-3](syntax.md#lang-3-literals)). It therefore does nothing at run time and has no
place in the evaluation order, which stays left to right for every other operand. A binary operator
checks an operand that is not a literal operand first, and checks a literal operand against
that operand's type; of two operands that are not literal operands, the left is checked
first and gives the right its expected type. When both are literal operands they take the expected type, for an
operator whose result is its operand type, and are refused by LANG-11 otherwise. So `1 + x`
and `(1 + 2) < x` type their literals from `x`, and `1 < 2` has no type.

```hirsel
fn f(x: u8) -> bool { (1 + 2) < x && !(x == 7) }
fn bits(x: u8) -> u8 { x & ~8 }
```
```hirsel
fn f(a: bool, b: bool) -> bool { a + b }                  // refused: LANG-21, + on bool
```
```hirsel
fn f(x: u8, y: u16) -> u16 { x + y }                      // refused: LANG-21, mixed widths
```
```hirsel
fn first(s: String) -> u8 { s[0] }          // refused: LANG-21, use string.as_bytes or string.next_scalar
fn find(m: Map[u64, u64]) -> u64 { m[0] }   // refused: LANG-21, use map.get(m, 0)
fn put(mut m: Map[u64, u64]) { m[0] = 1 }   // refused: LANG-21, use map.insert
```
