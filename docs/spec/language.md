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
fn f(d: Dir) -> Result[(HostFile, HostFile), IoError] waits {
  Ok((host.open(d, "a", Read)?, host.open(d, "b", Read)?))   // a is opened first
}
```

### LANG-7: Names

There are no imports. Types and values have separate namespaces, and the grammar decides
which one a name is read in: a name in a type position (LANG-5) is a type, and any other
name is a value. A type name resolves, in order, to a builtin type name (LANG-10), a type
of the current module or a type of the **prelude**, and declaring a type with a builtin
type name is refused; a value name resolves to a local, a function or constant of the current
module, a function of the prelude, a module of the current package, a dependency package or
a core module. The prelude is the `core` module of the core library: every `pub`
declaration in `core/core/core.hirsel` ([core](core.md)), and nothing else. A dotted path
`a.b` reads its first name as a value; if `a` is a local, `.b` selects a field, and if `a` is
a module or package, `.b` selects a member; in a type position the last name is a type
(`path.Path`). So `fn f(c: char)` names the builtin type and `char.digit(c)` a module
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
specification; `args: Args` is equally legal. Two definitions with one name in one module are refused. Variants are
written bare (`Ok(x)`, `None`) and selected by the expected type; a dotted path never names
a variant. Core roles (`Result`, `Capacity`, the capability types) are fixed by definition
path, never by spelling.

```hirsel
fn digits(s: String) -> Result[u64, ParseError] { int.parse(s) }
```
```hirsel
fn f(x: u64) -> u64 { let x = x + 1; x }                  // refused: LANG-7, shadows x
```

### LANG-8: Signatures

Every top-level function writes a complete signature: parameter names, modes and types,
the result type and its anchors ([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)),
`waits` and region outlives clauses ([MEM-6](memory.md#mem-6-roots)). Checking a body reads that body, the signatures and type
summaries ([OWN-2](ownership.md#own-2-type-summaries)) it names, and nothing else: no fact is
inferred from another body, and no check revisits a definition after its callers are seen.
A closure writes its parameter types; its result type may be omitted when the expected
type gives it.

`waits` says that a call may wait and may observe cancellation
([CONC-7](tasks.md#conc-7-cancellation-points)). `main` is always `waits`
([PROG-3](programs.md#prog-3-entry)), and a core declaration's `waits` is as written.
Every other function or closure is declared `waits`
exactly when its body calls a function, or invokes a callable, whose type is `waits`; one
declared `waits` without such a call is refused too. Cleanup never counts: finalizers and the
cleanup of task handles run shielded and observe no cancellation
([FAIL-11](failure.md#fail-11-resources), [CONC-8](tasks.md#conc-8-abandoned-handles)), so a
finalizer may call `waits` operations without being marked, and no implicit cleanup makes a
function `waits`.

A body that does not have its declared result type is refused by LANG-11, at the
expression that has the wrong type.

```hirsel
fn twice(x: u64) -> u64 { x * 2 }
```
```hirsel
fn twice(x: u64) -> u64 waits { x * 2 }                   // refused: LANG-8, waits with no waiting call
```

### LANG-9: Nominal types

A type definition declares a record (`type P { x: i64, y: i64 }`), a variant type
(`type Shape = Circle(f64) | Rect(w: f64, h: f64)`, a single variant `Only()`) or a
transparent alias (`type Ids = List[u64]`), as LANG-5 tells them apart. An alias is its
right-hand side; a chain of aliases that reaches itself is refused, at the reference that
continues the chain in the member whose definition identity is least, as
[OWN-2](ownership.md#own-2-type-summaries) reports a cycle. Payloads are all
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

The builtin type names are `bool`, `char`, `i8` to `i64`, `u8` to `u64`, `f32`, `f64` and
`Inline` ([SEM-9](semantics.md#sem-9-inline-arrays)); unit `()` and tuples `(A, B, ..)` are
written by syntax. Every other type a program names
without declaring it is declared in the prelude ([core](core.md)), `Option`, `Result`,
`String`, `List` and the capability types included. `Never` is the type of `return`,
`break`, `continue`, `fail` and a loop with no `break`, and converts to every type. Kinds
are in [OWN-3](ownership.md#own-3-kinds).

### LANG-11: Typing is local

Types are checked bidirectionally within one body: an expression is checked against an
expected type where one exists, and its type is synthesised otherwise. The expected type
reaches a block's last expression, each `if` and `match` arm, each `break` value of a loop
and a closure's result; an `if` without `else` has type unit, and an arm of type `Never`
constrains nothing. An expression whose type, after the conversions below, is not its
expected type is refused, at the innermost expression the expected type reached: in `if c
{ x } else { 0 }` checked against `u16` with `x: u8`, at `x`. This one rule covers a body
and its result type, an arm, a `let` with a written type, an argument and a `break` value.
A call has exactly one argument per parameter, in order; a missing argument is refused at
the call's closing parenthesis, and an extra one at the first extra argument. At a call, the types of all arguments that synthesise one, and the
expected result type, are collected before an unsuffixed literal argument is checked, so
`f(1, x)` with `x: u8` types `1` as `u8`; a type parameter or region they leave
undetermined, or determine twice differently, is refused. An unsuffixed number needs an
expected type ([LANG-3](syntax.md#lang-3-literals)); operands follow
[LANG-21](#lang-21-operators). There are no implicit conversions except these: `Never` to any type; a
`View[T]` of a plain `T` to `T` ([OWN-8](ownership.md#own-8-views)); a stable value to its
view ([OWN-25](ownership.md#own-25-stable-values-are-read-as-views)); `T in a` to `T in r`
when `a` outlives `r` ([MEM-6](memory.md#mem-6-roots)); an `IndexedSpan[T]` to a
`SeqSpan[T]`, for reading only, keeping its region, `W` and locator
([MEM-12](memory.md#mem-12-stable-sequences)), never a `Writer`, a `Child` or a field's
declared encoding; and a callable to a callable type it fits ([LANG-13](#lang-13-callables)).

```hirsel
fn f() -> u64 { let x: u64 = 7; x }
```
```hirsel
fn f() -> u64 { let x = 7; x }                            // refused: LANG-11, 7 has no expected type
```
```hirsel
fn f(c: bool, x: u8) -> u16 { if c { x } else { 0 } }     // refused: LANG-11, at x: u8 is not u16
```
```hirsel
fn f(x: u8) -> u8 { x }
fn g() -> u8 { f(1, 2) }                                  // refused: LANG-11, at 2: f takes one argument
```

### LANG-12: Generics

A definition may take type parameters with bounds, `[T: key + send]`. A generic body is
checked once and uses only the bounds it declares
([OWN-23](ownership.md#own-23-generic-bodies)). Type arguments are never written at a
call: they are found from that one call (LANG-11). A type argument that lacks a bound its
parameter declares is refused, at the argument or result that determined it. Each use of a generic type writes its
arguments.

```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
```
```hirsel
fn same[T](a: T, b: T) -> bool { a == b }                 // refused: LANG-12, T is not key
```
```hirsel
fn f(a: f64, b: f64) -> f64 { int.wrapping_add(a, b) }    // refused: LANG-12, at a: f64 is not integer
```

### LANG-13: Callables

A callable type has one of three kinds, written in the type:

| type | admits | kind | size |
|---|---|---|---|
| `fn(A, mut B, take C) -> R` | top-level functions and closures that capture nothing | plain, `send` | 8 |
| `own fn(..) -> R`, `own[send] fn(..) -> R` | the above, and **owning closures** (LANG-14); `own[send]` only those whose captures are all `send` | own; `send` only with `[send]` | 16 |
| `scoped fn(..) -> R` | any closure, and the above | scoped ([OWN-21](ownership.md#own-21-scoped-callbacks)) | 16 |

A callable type also says whether it is `waits` (LANG-8). A callable fits a callable type
when its parameter modes and types and its result are identical, its kind is admitted, and
it is `waits` only if the type is. Calling a callable never consumes it and follows the
modes of a direct call. Any call through a callable may allocate. Cleaning an owning closure
cleans its captures, which hold no resource (LANG-14), so it runs no finalizer and admits
nothing ([FAIL-13](failure.md#fail-13-mechanical-release)).

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
is scoped: a capture it only reads is a view of the place it reads and a capture it
assigns or lends `mut` is a `mut` loan of that place, held while its capture descriptor is
active ([OWN-21](ownership.md#own-21-scoped-callbacks)), and it never moves a capture. Any other
closure with captures is an **owning closure**: creating it admits one environment block
([MEM-24](budgets.md#mem-24-logical-charges)) and then copies its plain captures into it and
moves the others, whose slots become moved ([OWN-5](ownership.md#own-5-availability)); its
literal has type `Result[own fn(..) -> R, Capacity]`, `own[send]` when every capture is
`send`, and on `Capacity` its captures are dropped as a failed literal's fields are
([T1.2](ownership.md#own-26-the-transition-table)). Its body reads captures as views and
cannot move, assign or lend them `mut`. An owning closure may capture only a value whose
type holds no resource, no capability, no view, no scoped value and no stable reference at
any depth, as its type
summary records ([OWN-2](ownership.md#own-2-type-summaries)): plain values other than
capabilities, own values, counted holders of such values, and nothing whose summary may
hold a capability, such as a type parameter or `Wrap[T]` for `type Wrap[T: plain] { item: T }`. Resources and capabilities reach a closure as call arguments. The environment
holds the captures in the order the body first names them, and cleaning it cleans them in
reverse.

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
modes, and `for take x in e` consumes an own `List`; no other iteration consumes in v1. A `for` pattern is irrefutable. Bindings made by one iteration end with it
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
type Shape = Circle(f64) | Rect(w: f64, h: f64)
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r, Rect(w: w, h: h) => w * h } }
```
```hirsel
type Shape = Circle(f64) | Rect(w: f64, h: f64)
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r } }   // refused: LANG-17, Rect missing
```

### LANG-18: Errors

Expected failures are `Result[T, E]` values. `e?` on `Result[T, E]` consumes `e` and
evaluates to the `Ok` payload, or exits the function with `Error` of the same payload; the
function's error type must be exactly `E`. A different error type is converted explicitly,
with `match`. `e?` on `Option[T]` in a function returning `Option` exits with `None`. Inside
a bound scope of the same function, `?` on an error of type exactly `Capacity` exits the
scope instead ([MEM-25](budgets.md#mem-25-the-bound-scope)). An expression statement whose
value is a `Result` is refused; `let _ = e` discards it explicitly.

```hirsel
fn half(n: u64) -> Result[u64, ParseError] { if n % 2 == 0 { Ok(n / 2) } else { Error(OutOfRange) } }
fn quarter(n: u64) -> Result[u64, ParseError] { let h = half(n)?; half(h) }
```
```hirsel
fn half(n: u64) -> Result[u64, ParseError] { Ok(n / 2) }
fn quarter(n: u64) -> Result[u64, ParseError] { half(n); Ok(0) }   // refused: LANG-18, unused Result
```

### LANG-19: Recursion

Functions may call each other and themselves freely; each call's signature is all the
caller needs. Exhausting the stack is a run stop ([FAIL-15](failure.md#fail-15-run-stops)),
not a failure. Mechanical release ([FAIL-13](failure.md#fail-13-mechanical-release)) uses
constant stack whatever the depth of the value; a finalizer is an ordinary call.

### LANG-20: Constants

`const N: T = e` declares a plain constant. `e` may use literals, operators, tuple, plain and
`Inline` array constructions, and other constants; it calls no function. Constants are evaluated once, before any body is checked, in
dependency order, by the rules of [semantics](semantics.md); a constant that would fail, or whose
constants reach itself, is refused. A string literal is not a constant.

```hirsel
const WORDS: u64 = 64
const BYTES: u64 = WORDS * 8
```
```hirsel
const BIG: u8 = 200 + 100                                 // refused: LANG-20, Overflow
```

### LANG-21: Operators

An operator applies to the operand types this table lists and to no other; anything else is
refused at the operator. Both operands of a binary operator have one type, so there are no
mixed-width or mixed-kind operators. A compound assignment `a op= b` types as `a op b` and
installs the result ([OWN-12](ownership.md#own-12-assignment)).

| operators | operands | result |
|---|---|---|
| `+ - * /` | one integer type, or one float type | the operand type |
| `%` | one integer type | the operand type |
| unary `-` | a signed integer type, or a float type | the operand type |
| `& \| ^`, unary `~` | one integer type | the operand type |
| `<< >>` | an integer left operand and a count of its type ([SEM-1](semantics.md#sem-1-integers)) | the left type |
| unary `!`, `&&`, `\|\|` | `bool` | `bool` |
| `== !=` | one `key` type ([SEM-7](semantics.md#sem-7-equality-and-order)) | `bool` |
| `< <= > >=` | one `ordered` type (SEM-7) | `bool` |

A **literal operand** is an unsuffixed number, or a unary `-` or `~`, a binary operator or
parentheses applied only to literal operands; it synthesises no type. It is evaluated when
the definition is checked, in the type it takes, by the rules of SEM-1 and SEM-2; a result
that does not fit, a zero divisor or any other failure is refused there
([LANG-3](syntax.md#lang-3-literals)). It therefore does nothing at run time and has no
place in the evaluation order, which stays left to right for every other operand. A binary operator
checks an operand that is not a literal operand first, and checks a literal operand against
that operand's type. When both are literal operands they take the expected type, for an
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
