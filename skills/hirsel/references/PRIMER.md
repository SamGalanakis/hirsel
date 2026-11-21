# Hirsel primer

Hirsel is a systems language with value semantics: loops and local mutation, no garbage
collector, no raw pointers, no lifetimes to write. This primer is for a programmer who
knows Rust or OCaml and has the `hirsel` binary. Each section names the specification rules
behind it, such as `OWN-7`. Run `hirsel explain OWN-7` to print a rule, and
`hirsel explain --list` to list them all. Every `hirsel` block below is checked by CI, and
a `// refused: RULE` comment marks the line the checker refuses and the rule it cites.

## 1. A first program

```hirsel run
pub fn main(out: Stdout) -> Result[(), WriteError] waits {
  let mut names: List[String] = list.new()
  list.push(mut names, "ada")
  list.push(mut names, "grace")
  let mut line = string.new()
  for n in names {
    string.push(mut line, n)
    string.push(mut line, " ")
  }
  string.push_u64(mut line, list.len(names))
  string.push(mut line, "\n")
  stdout.write(out, line)
}
```

```text
ada grace 2
```

Outcome: success.

Every outside effect comes in as a parameter: `main` writes only because it receives
`Stdout`. It is `waits` because it calls a waiting operation. `main` returns
`Result[(), E]`, and `Ok(())` exits 0 (PROG-3, CAP-1, CAP-4).

## 2. What is not there

Hirsel has **no imports, no methods, no traits and no turbofish** (LANG-7).

- `list`, `string`, `map`, `int`, `option`, `rc` and the rest are core modules. You call
  their functions by path, `list.push(mut xs, 1)`, never `xs.push(1)`. Another module of
  your package is `wal.append(..)` and a std module is `std.json.parse(..)`, all with no
  `use`. A std module that extends a core module merges into it, so `list.sort` is std's.
- A dotted name after a value reads a field. A method call is a field read, so it is
  refused:

```hirsel
fn count(xs: List[u64]) -> u64 { xs.len() }               // refused: PROG-2, use list.len(xs)
```

- Names never shadow. A local may not reuse a visible local's name, a function of its
  module or a prelude function:

```hirsel
fn f(x: u64) -> u64 { let x = x + 1; x }                  // refused: LANG-7, shadows x
```
```hirsel
fn width() -> u64 { 8 }
fn f() -> u64 { let width: u64 = 1; width }               // refused: LANG-7, width is a function
```

- Generics take bounds from a fixed set: `plain`, `shareable`, `send`, `key`, `ordered`,
  `codec` and `inert` (OWN-4). No user-defined traits, and no operator overloading
  (LANG-21).
- Nothing converts implicitly. `int.widen`, `int.narrow` and `int.try_narrow` change
  widths, and `x + y` needs one type (LANG-21).
- Variants are written bare (`Some(x)`, `Ok(())`), and the expected type picks them.

## 3. Types and local typing

```hirsel
type Point { x: i64, y: i64 }
type Shape = Circle(f64) | Rect(w: f64, h: f64)
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r, Rect(w: w, h: h) => w * h } }
fn origin() -> Point { Point(x: 0, y: 0) }
```

A record is built with every field labelled, in order, and `match` is exhaustive
(LANG-9, LANG-17). Every top-level function writes its whole signature (LANG-8). Inside a
body a type flows only forward, from a written type, a parameter or the result type. It
never flows back from a later use (LANG-11). An unsuffixed literal needs an expected type,
and a generic call needs its type parameters fixed by that one call (LANG-12):

```hirsel
fn total(xs: List[u64]) -> u64 {
  let mut t: u64 = 0
  for x in xs { t += x }
  t
}
fn empty() -> u64 { list.len((list.new() : List[u64])) }  // (e : T) ascribes in place
fn small(n: u64) -> u8 { option.unwrap(int.try_narrow(n)) }
```
```hirsel
fn f() -> u64 { let x = 7; x }                            // refused: LANG-11, 7 has no expected type
```
```hirsel
fn g() -> u64 { list.len(list.new()) }                    // refused: LANG-12, T is open
```

Type arguments are never written at a call. To fix them, write the binding's type
(`let xs: List[u64] = list.new()`) or ascribe `(e : T)`. An ascription is not a cast.

## 4. Kinds: the type decides who frees

A declaration fixes its values' representation once, and every use inherits it (OWN-1,
OWN-3):

| kind | declared as | `let` | freed by |
|---|---|---|---|
| plain | `type` with only plain fields; scalars, `bool`, `char` | copies | nothing to free |
| own | `type` with a non-plain field; `String`, `List`, `Map`, `Box` | moves | its one owner leaving scope |
| region | `@compact(W) type`; `Text`, `FixedSpan` | copies the locator | its region closing |
| resource | `resource type`; `Sender`, `Task`, files | moves | its finalizer |
| counted | `Rc[T]` (task-local), `Arc[T]` (`send`) | moves | the last holder |
| scoped | `View[T]`, `Region`, `scoped fn`, `scoped type` | as its parts | never outlives its anchors |

```hirsel
type Point { x: i64, y: i64 }                             // plain
type Inbox { items: List[String] }                        // own
@compact(u32) type Name { text: Text, len: u32 }          // region
resource type Lease { id: u64 }                           // resource
  drop(l) {}
type Piece: shareable { text: String }                    // may sit in an Rc
scoped type Cursor { input: View[Bytes], at: u64 }        // holds a view
fn pair(take p: Piece) -> (Rc[Piece], Rc[Piece]) {
  let a = rc.new(take p)
  let b = rc.share(a)
  (take a, take b)
}
```
```hirsel
fn f(take s: String) -> String { let t = s; s }           // refused: OWN-5, s moved
```

A counted payload is immutable except through `rc.with_unique` and `arc.with_unique`, so
sharing is safe (MEM-14, MEM-15). A scoped value cannot be stored in an own type's field,
put in a collection or returned past what it reads (OWN-19).

## 5. Views, `mut` and `take`

A parameter's mode says what the callee gets, and the call site repeats it (OWN-7):

```hirsel
fn len(xs: List[u64]) -> u64 { list.len(xs) }             // a view, for the call
fn add(mut xs: List[u64], x: u64) { list.push(mut xs, x) } // lent, for the call
fn eat(take xs: List[u64]) {}                             // owned by the callee
fn use_all(take xs: List[u64]) { let n = len(xs); add(mut xs, n); eat(take xs) }
```
```hirsel
fn eat(take xs: List[u64]) {}
fn f(take xs: List[u64]) { eat(xs) }                      // refused: OWN-7, needs take xs
```

`take` is written where a place of a moving type is moved, and a fresh value is passed
bare. A view is read-only and lasts for a lexical extent: to the end of its block when
bound by `let`, otherwise to the end of the statement (OWN-8). While it lives, nothing may
write the place it views (OWN-10):

```hirsel
type Inbox { items: List[String] }
fn f(mut m: Inbox) {
  let items = view m.items
  list.clear(mut m.items)                                 // refused: OWN-10, items lasts to block end
}
```

A function that returns a view names its anchor:
`fn first(xs: List[String]) -> Option[View[String]] in xs` (OWN-20).

## 6. Errors and failures

Expected failures are `Result` values. `e?` returns the `Error` unchanged, so the error
types must match exactly. `e ?=> m` converts the error on the way out (LANG-18):

```hirsel
type ReadError = Parse(ParseError) | Short(u64)
fn half(n: u64) -> Result[u64, ParseError] { if n % 2 == 0 { Ok(n / 2) } else { Error(OutOfRange) } }
fn read(n: u64) -> Result[u64, ReadError] { let h = half(n) ?=> Parse; Ok(h) }
fn mark(n: u64) -> Result[u64, ReadError] { Ok(half(n) ?=> |_| Short(n)) }
```
```hirsel
fn half(n: u64) -> Result[u64, ParseError] { Ok(n / 2) }
fn quarter(n: u64) -> Result[u64, ParseError] { half(n); Ok(0) }   // refused: LANG-18, unused Result
```

Discard a result explicitly with `let _ = e`. A **failure** is not a value. Integer
overflow, an index out of bounds, `fail("msg")`, `option.unwrap(None)` and a refused
allocation (`Capacity`) each clean every scope and end the task. A failure in `main`'s
task exits 101 and prints the cause chain (FAIL-1, PROG-3). `isolate(fn() -> R { .. })`
catches a failure as `Ok(Error(causes))`, and it undoes nothing (FAIL-16).

## 7. Memory: `bound bytes`, `region` and `@compact`

Every allocation is charged against a byte limit. `bound bytes n { .. }` runs its body
under a new limit of `n` and returns `Result[T, Capacity]`. The `Error` means only that
the `n` itself was refused. An allocation the body cannot fit raises the failure
`Capacity`, which `isolate` catches (MEM-25, MEM-21). `region r { .. }` opens a region:
region data in it is freed at once when the block exits, and no value naming `r` may
leave it (MEM-1, MEM-7, MEM-8).

```hirsel run
fn requested(causes: Causes) -> u64 {
  for c in causes { match c.kind { Capacity(k) => return k.requested, _ => {} } }
  0
}

fn fill(limit: u64) -> Result[u64, Capacity] {
  bound bytes limit {
    let mut xs: List[u64] = list.new()
    let mut i: u64 = 0
    while i < 100 { list.push(mut xs, i); i += 1 }
    i
  }
}

fn attempt(limit: u64) -> u64 {
  match isolate(fn() -> Result[u64, Capacity] { fill(limit) }) {
    Ok(Ok(Ok(n))) => n,
    Ok(Ok(Error(_))) => 0,
    Ok(Error(causes)) => requested(causes),
    Error(_) => 0,
  }
}

fn width(s: String) -> u64 {
  region r { let t = text.copy_in(r, s); text.len(t) }
}

pub fn main(out: Stdout) -> Result[(), WriteError] waits {
  let mut line = string.new()
  string.push_u64(mut line, attempt(4096))
  string.push(mut line, " ")
  string.push_u64(mut line, attempt(64))
  string.push(mut line, " ")
  string.push_u64(mut line, width("hello"))
  string.push(mut line, "\n")
  stdout.write(out, line)
}
```

```text
100 64 5
```

Outcome: success.

The list grows to 4, 8, 16 and more elements, and each growth holds the old and the new
storage at once. Under 4096 bytes all 100 pushes fit. Under 64 bytes, growing from 4 to 8
elements needs 32 + 64 bytes, so that push fails with `requested` 64.

`@compact(W) type` declares region data: one contiguous blob with offsets `W` wide,
built in place by `build_in` and its `Dest` or `Writer`, and immutable once published. Its
fields are scalars, `Text`, spans, links and other compact types, never own values (MEM-9,
MEM-2, MEM-4):

```hirsel
@compact(u32) type Entry { id: u64, name: Text, scores: FixedSpan[u32] }
@compact(u32) type Bad { owner: String }                  // refused: MEM-9, own field
```
```hirsel
fn leak(s: String) -> u64 {
  let t = region r { text.copy_in(r, s) }                 // refused: MEM-7, t names r
  text.len(t)
}
```

## 8. Capabilities and `waits`

Files, directories, network, clocks, randomness, standard streams, environment variables,
processes and tasks are reached only through capability parameters (CAP-1). A function
holds exactly the authority it is passed, and no constant holds any (CAP-1, CAP-3).
`main`'s parameters are granted at launch with `--grant` and `--dir` (CAP-4).

```hirsel
fn stamp(c: Clock) -> u64 { host.mono_ns(c) }
```
```hirsel
fn stamp() -> u64 { host.mono_ns() }                      // refused: CAP-1, no Clock argument
```

A function is `waits` exactly when it calls something that is `waits`, and a
needless `waits` is refused too (LANG-8). A waiting call may observe cancellation
(CONC-7). Tasks share nothing: `task.spawn(tasks, bytes n, take payload, f)` moves a
`send` payload into a new task, and channels carry values between tasks (CONC-2, CONC-3,
CONC-4).

```hirsel
fn twice(x: u64) -> u64 waits { x * 2 }                   // refused: LANG-8, waits with no waiting call
```

## 9. Tests and packages

A `@test` function takes only capabilities and returns `Result[(), E]`. Each test runs as
its own run (PROG-4):

```hirsel
fn twice(n: u64) -> u64 { n + n }
@test
fn doubles() -> Result[(), ()] { if twice(21) == 42 { Ok(()) } else { Error(()) } }
@test
fn greets(out: Stdout) -> Result[(), WriteError] waits { stdout.write(out, "hi\n") }
```

A package is a directory with a `hirsel.toml`. `hirsel new app` writes this one:

```text
[package]
name = "app"
major = 1
charge = "charge-v1"

[dependencies]
std = { major = 1 }

[sources]
"main.hirsel" = "main"

[tests]
limit = 67108864

[tests.sources]
"tests.hirsel" = "tests"
```

Each `[sources]` entry maps a file to a module, and `[tests]` gives every test its root
limit and grants. Test sources see the package's modules, as in `main.twice(21)`
(PROG-1, PROG-2).

## 10. The loop

```sh
hirsel new app && cd app
hirsel check                       # prints file:line:col: RULE message
hirsel explain OWN-7               # the rule a refusal cites, with an example
hirsel test                        # every @test, each in its own process
hirsel run --grant Stdout -- a b   # build and run; grants are explicit
hirsel build --release -o program  # a native executable
hirsel inspect --std --interface   # every public std declaration
hirsel fmt .                       # canonical formatting
```

When `check` refuses, read its rule ID first. The ID names the one rule broken and its
site, and `hirsel explain` gives the rule's exact statement. Fix that site, then check
again. Look up an operation with `hirsel inspect --std --interface`, never by guessing a
method name. `hirsel primer` (or `hirsel explain primer`) prints this document.
