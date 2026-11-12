# Language

Names, types, signatures, control flow and errors. Every rule here is checked from one
body and the declarations it names.

### LANG-6: Evaluation order

Evaluation is strictly left to right: a call's arguments, then the call; a construction's
fields; an operator's operands; an assignment's place, then its value
([OWN-12](ownership.md#own-12-assignment)). `&&` and `||` skip their right operand when the
left decides. A temporary lives to the end of its statement, except that a temporary a
`let` binding's value reaches lives as long as that binding. A `take` argument stays a
caller-owned temporary until the callee is entered.

```hirsel
fn f(d: Dir) -> Result[(Bytes, Bytes), IoError] {
  Ok((file.read(d, path.of("a"))?, file.read(d, path.of("b"))?))   // a is read first
}
```

### LANG-7: Names

There are no imports. A definition in another module is named by its module path,
`list.push` or `json.parse`; a definition in another package by its package path
([PROG-1](programs.md#prog-1-packages)). Unqualified names resolve to locals, then the
current module, then the core library. Variants are written bare (`Ok(x)`, `None`) and
resolved through the expected type. A local may not shadow another local or a definition
of its own module. Core roles (`Result`, `Capacity`, the capability types) are fixed by
definition path, never by spelling.

```hirsel
fn parse(s: String) -> Result[u64, ParseError] { int.parse(s) }
```
```hirsel
fn f(x: u64) -> u64 { let x = x + 1; x }                  // refused: LANG-7, shadows x
```

### LANG-8: Signatures

Every top-level function writes a complete signature: parameter names, modes and types,
the result type and its anchor ([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)),
`waits` ([CAP-5](capabilities.md#cap-5-providers)) and region outlives clauses
([MEM-6](memory.md#mem-6-roots)). Checking a body reads that body, the signatures and type
declarations it names, and nothing else: no fact is inferred from another body, and no
check revisits a definition after its callers are seen. A closure writes its parameter
types; its result type may be omitted when the expected type gives it.

```hirsel
fn twice(x: u64) -> u64 { x * 2 }
```
```hirsel
fn twice(x: u64) { x * 2 }                                // refused: LANG-8, result u64 is not unit
```

### LANG-9: Nominal types

A type definition declares a record (`type P { x: i64, y: i64 }`, built `P(x: 1, y: 2)`),
a variant (`type Shape = Circle(f64) | Rect(w: f64, h: f64)`) or an alias (`type Ids =
List[u64]`). A right-hand side that is a single bare name is an alias; a single variant is
written with parentheses, `Only()`. Payloads are all labelled or all positional. The
representation keyword (`stable`, `resource`, `scoped`) or its absence is fixed by the
definition ([OWN-1](ownership.md#own-1-representation-is-declared-once)). A nominal type's
identity is its definition path; two definitions with equal fields are different types.

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
`Result[T, E] = Ok(T) | Error(E)`. The core library defines `String`, `Text`, `Bytes`,
`Box`, `List`, `Map`, `FixedSpan`, `SeqSpan`, `IndexedSpan`, `Link`, `View`, `Rc`, `Arc`,
`Region`, `Builder`, `Capacity`, `Task`, `Sender`, `Receiver` and the capability types;
their kinds are in [OWN-3](ownership.md#own-3-kinds).

### LANG-11: Typing is local

Types are checked bidirectionally within one body: an expression is checked against an
expected type where one exists, and its type is synthesised otherwise. An unsuffixed
number needs an expected type. There are no implicit conversions except these:
`Never` to any type; a `View[T]` of a plain `T` to `T`
([OWN-8](ownership.md#own-8-views)); a stable value to its view
([OWN-25](ownership.md#own-25-stable-values-are-read-as-views)); `T in a` to `T in r` when
`a` outlives `r` ([MEM-6](memory.md#mem-6-roots)); and a closure to a callable type whose
`uses` includes its own ([CAP-3](capabilities.md#cap-3-callable-authority)).

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
call: they are found from the arguments and the expected type in that one call, and a
parameter that is not determined there is refused. Each use of a generic type writes its
arguments.

```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
```
```hirsel
fn same[T](a: T, b: T) -> bool { a == b }                 // refused: LANG-12, T is not key
```

### LANG-13: Callables

A callable type is `fn(A, mut B, take C) -> R`, optionally `uses` capability types
([CAP-3](capabilities.md#cap-3-callable-authority)). Top-level functions and **escaping**
closures have such types; an escaping closure is own when it captures a non-plain value
and plain otherwise. A callable type marked `scoped` is a scoped callback
([OWN-21](ownership.md#own-21-scoped-callbacks)). A closure literal is scoped exactly when
its expected type is `scoped fn`. Calling a callable never consumes it, and a call through
a callable type follows the same modes as a direct call.

```hirsel
fn apply(f: fn(u64) -> u64, x: u64) -> u64 { f(x) }
fn main_like() -> u64 { apply(fn(x: u64) -> u64 { x + 1 }, 1) }
```

### LANG-14: Captures

An escaping closure owns its captures: plain values are copied and other values are moved
in at creation, and the moved slots become moved
([OWN-5](ownership.md#own-5-availability)). Its body reads captures as views and cannot
move, assign or lend them `mut`. A scoped closure captures as the arguments of the call
that receives it: a capture it only reads is a view, and a capture it assigns or lends
`mut` is a `mut` loan, both for that call. A view, a scoped value or a stable value cannot
be captured by an escaping closure.

```hirsel
fn adder(take base: List[u64]) -> fn(u64) -> u64 { fn(x: u64) -> u64 { x + list.len(base) } }
```
```hirsel
fn adder(base: List[u64]) -> fn(u64) -> u64 { fn(x: u64) -> u64 { x + list.len(base) } }
                                                          // refused: LANG-14, base is a view
```

### LANG-15: Blocks and labels

A block is an expression whose value is its last expression. A block, `loop`, `while` or
`for` may carry a label, `'l:`. `break 'l v` leaves a labelled block with the value `v`;
`break` and `continue` select the innermost loop or the labelled one. A `loop` evaluates to
the value of the `break` that leaves it. A label cannot shadow an active label, and no exit
crosses a closure. Every exit cleans up the scopes it leaves
([FAIL-8](failure.md#fail-8-exit-edge-cleanup)).

```hirsel
fn find(xs: List[u64], k: u64) -> Option[u64] {
  let mut i: u64 = 0
  'scan: loop { if i == list.len(xs) { break 'scan None }; if xs[i] == k { break 'scan Some(i) }; i += 1 }
}
```

### LANG-16: Loops

`loop` repeats until a `break`. `while c` and `while let p = e` test before each iteration.
`for x in e` iterates a range or a view of a sequence, map or span, and `for take x in e`
consumes an own sequence ([SEM-6](semantics.md#sem-6-iteration)). A `for` pattern is
irrefutable. Bindings made by one iteration end with it
([OWN-17](ownership.md#own-17-loops)).

### LANG-17: Match

`match` is exhaustive over the scrutinee's type. A guard `p if g => e` is a `bool`
expression that may not move, assign or lend `mut`, and a guarded arm never counts toward
exhaustiveness. Patterns are literals, closed literal ranges `a..=b`, names, `_`, tuples
and variants with labelled or positional fields, `..` for remaining fields, and `|`
alternatives that bind the same names. `let p = e` needs an irrefutable pattern; `let p =
e else { .. }` takes a refutable one, and its `else` block has type `Never`. `if let` and
`while let` take refutable patterns. Arms of an owned scrutinee bind by
[OWN-15](ownership.md#own-15-match-on-an-owned-value).

```hirsel
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r, Rect(w: w, h: h) => w * h } }
```
```hirsel
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r } }   // refused: LANG-17, Rect missing
```

### LANG-18: Errors

Expected failures are `Result[T, E]` values. `e?` on `Result[T, E]` evaluates to `T` or
exits the function with `Error`: unchanged when the function's error type is `E`, or
injected when the function's error type is a variant type with exactly one arm whose
single payload is `E`; any other case is refused. `e?` on `Option[T]` in a function
returning `Option` exits with `None`. Inside a bound scope, `?` on `Capacity` exits the
scope ([MEM-27](budgets.md#mem-27-the-bound-scope)). An expression statement whose value is
a `Result` is refused; `let _ = e` discards it explicitly.

```hirsel
type LoadError = Io(IoError) | Parse(ParseError)
fn load(d: Dir, p: Path) -> Result[u64, LoadError] { let s = file.read_string(d, p)?; Ok(int.parse(s)?) }
```
```hirsel
fn save(d: Dir, p: Path) -> Result[(), IoError] { file.write(d, p, b"x"); Ok(()) }
                                                          // refused: LANG-18, unused Result
```

### LANG-19: Recursion

Functions may call each other and themselves freely; each call's signature is all the
caller needs. Exhausting the stack is a run stop ([FAIL-15](failure.md#fail-15-run-stops)),
not a failure. Cleanup and counted release never recurse on the stack.
