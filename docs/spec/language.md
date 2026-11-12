# Language

### LANG-6: Evaluation order

Evaluation is strictly left to right: a call's arguments, then the call; a construction's
fields; an operator's operands; an assignment's place, then its value
([OWN-12](ownership.md#own-12-assignment)). `&&` and `||` skip their right operand when the
left decides. A temporary lives to the end of its statement, except that a temporary a
`let` binding's value reaches lives as long as that binding. A moved argument stays a
caller-owned temporary until the callee is entered
([FAIL-3](failure.md#fail-3-commit-points)).

```hirsel
fn f(d: Dir) -> Result[(Bytes, Bytes), IoError] waits {
  Ok((file.read(d, path.of("a"))?, file.read(d, path.of("b"))?))   // a is read first
}
```

### LANG-7: Names

There are no imports. A name resolves, in order, to a local, a definition of the current
module, a module of the current package, a dependency package, or a declaration of the
core library ([core](core.md)); locals, definitions, modules and packages share this one
namespace, and a local may not have the name of anything else visible there. A dotted
path `a.b.c` resolves its first name this way and then selects a field, if `a` is a local,
or a member, if `a` is a module or package: a library type of another module is written
`path.Path`. Two definitions with one name in one module are refused. Variants are written
bare (`Ok(x)`, `None`) and resolved through the expected type. Core roles (`Result`,
`Capacity`, the capability types) are fixed by definition path, never by spelling.

```hirsel
fn parse(s: String) -> Result[u64, ParseError] { int.parse(s) }
```
```hirsel
fn f(x: u64) -> u64 { let x = x + 1; x }                  // refused: LANG-7, shadows x
```

### LANG-8: Signatures

Every top-level function writes a complete signature: parameter names, modes and types,
the result type and its anchors ([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)),
`uses`, `waits` ([CAP-5](capabilities.md#cap-5-provider-operations)) and region outlives clauses
([MEM-6](memory.md#mem-6-roots)). Checking a body reads that body, the signatures and type
summaries ([OWN-2](ownership.md#own-2-type-summaries)) it names, and nothing else: no fact is
inferred from another body, and no check revisits a definition after its callers are seen.
A closure writes its parameter types; its result type may be omitted when the expected
type gives it.

```hirsel
fn twice(x: u64) -> u64 { x * 2 }
```
```hirsel
fn twice(x: u64) { x * 2 }                                // refused: LANG-8, result u64 is not unit
```

### LANG-9: Nominal types

A type definition declares a record (`type P { x: i64, y: i64 }`), a variant type
(`type Shape = Circle(f64) | Rect(w: f64, h: f64)`, a single variant `Only()`) or a
transparent alias (`type Ids = List[u64]`), as LANG-5 tells them apart. An alias is its
right-hand side; a chain of aliases that reaches itself is refused. Payloads are all
labelled or all positional. A record is constructed with every field labelled, in
declaration order, `P(x: 1, y: 2)`; a variant with labels exactly when its payloads are
labelled; labels appear in no other call. The representation keyword (`stable`, `resource`,
`scoped`) or its absence is fixed by the definition
([OWN-1](ownership.md#own-1-representation-is-declared-once)). A nominal type's identity is
its definition path; two definitions with equal fields are different types.

```hirsel
type Shape = Circle(f64) | Rect(w: f64, h: f64)
```
```hirsel
type Bad = A(x: u64, u64)                                 // refused: LANG-9, mixed payload labels
```

### LANG-10: Built-in types

The built-in types are `bool`, `char`, `i8` to `i64`, `u8` to `u64`, `f32`, `f64`, unit
`()`, tuples `(A, B, ..)`, `Never` (the type of `return`, `break`, `continue`, `fail` and a
loop with no `break`, which converts to every type), `Option[T] = Some(T) | None` and
`Result[T, E] = Ok(T) | Error(E)`. The core library declares the others ([core](core.md)):
`String`, `Text`, `Box`, `List`, `Map`, `FixedSpan`, `SeqSpan`, `IndexedSpan`, `Link`,
`View`, `Rc`, `Arc`, `Region`, `Builder`, `Dest`, `Child`, `Capacity`, `Task`, `Sender`,
`Receiver`, the capability types, and the alias `Bytes = List[u8]`. Their kinds are in
[OWN-3](ownership.md#own-3-kinds).

### LANG-11: Typing is local

Types are checked bidirectionally within one body: an expression is checked against an
expected type where one exists, and its type is synthesised otherwise. The expected type
reaches a block's last expression, each `if` and `match` arm, each `break` value of a loop
and a closure's result; an `if` without `else` has type unit, and an arm of type `Never`
constrains nothing. At a call, the types of all arguments that synthesise one, and the
expected result type, are collected before an unsuffixed literal argument is checked, so
`f(1, x)` with `x: u8` types `1` as `u8`; a type parameter or region they leave
undetermined, or determine twice differently, is refused. An unsuffixed number needs an
expected type. There are no implicit conversions except these: `Never` to any type; a
`View[T]` of a plain `T` to `T` ([OWN-8](ownership.md#own-8-views)); a stable value to its
view ([OWN-25](ownership.md#own-25-stable-values-are-read-as-views)); `T in a` to `T in r`
when `a` outlives `r` ([MEM-6](memory.md#mem-6-roots)); and a callable to a callable type it
fits ([LANG-13](#lang-13-callables)).

```hirsel
fn f() -> u64 { let x: u64 = 7; x }
```
```hirsel
fn f() -> u64 { let x = 7; x }                            // refused: LANG-11, 7 has no expected type
```

### LANG-12: Generics

A definition may take type parameters with bounds, `[T: key + send]`. A generic body is
checked once and uses only the bounds it declares
([OWN-23](ownership.md#own-23-generic-bodies)). Type arguments are never written at a
call: they are found from that one call (LANG-11). Each use of a generic type writes its
arguments.

```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
```
```hirsel
fn same[T](a: T, b: T) -> bool { a == b }                 // refused: LANG-12, T is not key
```

### LANG-13: Callables

A callable type has one of three kinds, written in the type:

| type | admits | kind | size |
|---|---|---|---|
| `fn(A, mut B, take C) -> R` | top-level functions and closures that capture nothing | plain, `send` | 8 |
| `own fn(..) -> R`, `own[send] fn(..) -> R` | the above, and **owning closures** (LANG-14); `own[send]` only those whose captures are all `send` | own; `send` only with `[send]` | 16 |
| `scoped fn(..) -> R` | any closure, and the above | scoped ([OWN-21](ownership.md#own-21-scoped-callbacks)) | 16 |

Each callable type also carries `uses`, the authority its values may hold
([CAP-3](capabilities.md#cap-3-callable-authority)), and `waits`, whether calling it may
wait ([CAP-5](capabilities.md#cap-5-provider-operations)). These are its declared summaries. A
callable fits a callable type when its parameter modes and types and its result are
identical, its kind is admitted, its `uses` are included in the type's, and it is `waits`
only if the type is. Calling a callable never consumes it and follows the modes of a direct
call. Any call through a callable may allocate; cleaning an owning closure admits nothing
([FAIL-13](failure.md#fail-13-mechanical-release)).

```hirsel
fn apply(f: fn(u64) -> u64, x: u64) -> u64 { f(x) }
fn main_like() -> u64 { apply(fn(x: u64) -> u64 { x + 1 }, 1) }
```
```hirsel
fn keep(take s: String) -> fn() -> u64 { fn() -> u64 { string.byte_len(s) } }
                                                          // refused: LANG-13, captures s
```

### LANG-14: Captures

A closure's captures are the locals of enclosing scopes that its body names; naming
`r.f` captures `r`. A closure written `scoped fn`, or whose expected type is a `scoped fn`,
is scoped: it captures as the arguments of the call that receives it, a capture it only
reads as a view of the place it reads and a capture it assigns or lends `mut` as a `mut`
loan of that place, both for that whole call, and it never moves a capture. Any other
closure with captures is an **owning closure**: creating it admits one environment block
([MEM-26](budgets.md#mem-26-logical-charges)) and then copies its plain captures into it and
moves the others, whose slots become moved ([OWN-5](ownership.md#own-5-availability)); its
literal has type `Result[own fn(..) -> R, Capacity]`, `own[send]` when every capture is
`send`, and on `Capacity` its captures are dropped as a failed literal's fields are
([T1.2](ownership.md#own-26-the-transition-table)). Its body reads captures as views and
cannot move, assign or lend them `mut`. A view, a scoped value or a stable value cannot be
captured by an owning closure.

```hirsel
fn counter(take base: List[u64]) -> Result[own fn(u64) -> u64, Capacity] {
  fn(x: u64) -> u64 { x + list.len(base) }
}
```
```hirsel
fn counter(base: List[u64]) -> Result[own fn(u64) -> u64, Capacity] {
  fn(x: u64) -> u64 { x + list.len(base) }                // refused: LANG-14, base is a view
}
```

### LANG-15: Blocks and labels

A block, `loop`, `while` and `for` are expressions. A block's value is its last
expression. A `loop` evaluates to the value of the `break` that leaves it; `while` and `for`
evaluate to unit. Any of them may carry a label, `'l:`. `break v` leaves the innermost loop
with `v`, `break 'l v` the labelled block or loop; `continue` selects the innermost loop or
the labelled one. A label cannot shadow an active label, and no exit crosses a closure.
Every exit cleans up the scopes it leaves ([FAIL-8](failure.md#fail-8-exit-edge-cleanup)).

```hirsel
fn find(xs: List[u64], k: u64) -> Option[u64] {
  let mut i: u64 = 0
  loop { if i == list.len(xs) { break None }; if xs[i] == k { break Some(i) }; i += 1 }
}
```

### LANG-16: Loops

`loop` repeats until a `break`. `while c` and `while let p = e` test before each iteration.
`for x in e` iterates a range, or a view of a `List`, a `Map` or a span, and `for take x in
e` consumes an own `List` ([SEM-6](semantics.md#sem-6-iteration)); nothing else is a loop
source in v1. A `for` pattern is irrefutable. Bindings made by one iteration end with it
([OWN-17](ownership.md#own-17-loops)).

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
`..` for remaining fields. `let p = e` needs an irrefutable pattern; `let p = e else { .. }`
takes a refutable one, and its `else` block has type `Never`. `if let` and `while let` take
refutable patterns. Arms of an owned scrutinee bind by
[OWN-15](ownership.md#own-15-match-on-an-owned-value).

```hirsel
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r, Rect(w: w, h: h) => w * h } }
```
```hirsel
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r } }   // refused: LANG-17, Rect missing
```

### LANG-18: Errors

Expected failures are `Result[T, E]` values. `e?` on `Result[T, E]` consumes `e` and
evaluates to the `Ok` payload or exits the function with the `Error` payload: unchanged
when the function's error type is `E`, or injected when the function's error type is a
variant type with exactly one arm whose single payload is `E`; any other case is refused.
`e?` on `Option[T]` in a function returning `Option` exits with `None`. Inside a bound
scope of the same function, `?` on an error of type exactly `Capacity` exits the scope
instead ([MEM-27](budgets.md#mem-27-the-bound-scope)). An expression statement whose value
is a `Result` is refused; `let _ = e` discards it explicitly.

```hirsel
type LoadError = Io(IoError) | Parse(ParseError)
fn load(d: Dir, p: path.Path) -> Result[u64, LoadError] waits {
  let s = file.read_string(d, p)?
  Ok(int.parse(s)?)
}
```
```hirsel
fn save(d: Dir, p: path.Path) -> Result[(), WriteError] waits { file.write(d, p, b"x"); Ok(()) }
                                                          // refused: LANG-18, unused Result
```

### LANG-19: Recursion

Functions may call each other and themselves freely; each call's signature is all the
caller needs. Exhausting the stack is a run stop ([FAIL-15](failure.md#fail-15-run-stops)),
not a failure. Mechanical release ([FAIL-13](failure.md#fail-13-mechanical-release)) uses
constant stack whatever the depth of the value; a finalizer is an ordinary call.

### LANG-20: Constants

`const N: T = e` declares a plain constant. `e` may use literals, operators, tuple and plain
constructions, and other constants; it calls no function. It is evaluated when the program
is checked, by the rules of [semantics](semantics.md); a constant that would fail, or whose
constants reach itself, is refused. A string literal is not a constant.

```hirsel
const WORDS: u64 = 64
const BYTES: u64 = WORDS * 8
```
```hirsel
const BIG: u8 = 200 + 100                                 // refused: LANG-20, Overflow
```
