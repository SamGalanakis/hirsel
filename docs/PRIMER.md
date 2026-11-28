# Hirsel primer

Hirsel is a systems language with value semantics: loops and local mutation, no garbage
collector, no raw pointers, no lifetimes to write. This primer is for a programmer who
knows Rust or OCaml and has the `hirsel` binary. Every `hirsel` block below is checked by
CI, and a `// refused: ...` comment marks a line the checker rejects and why. When the
compiler refuses code it prints a short rule code (e.g. `OWN-7`) that `hirsel explain OWN-7`
explains; `hirsel explain --list` lists them all.

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

Every outside effect enters as a parameter: `main` writes only because it receives
`Stdout`. `main` may wait with or without the `waits` marker. It returns `Result[(), E]`:
`Ok(())` exits 0, `Error(Exit(status: n))` exits with status `n` without printing an error
value, and any other `Error(e)` exits 1 after writing `error: ` and `e` as Hirsel source on
stderr.

## 2. What is not there

Hirsel has **no imports, no methods, no traits and no turbofish**.

- `list`, `string`, `map`, `int`, `option`, `rc` and the rest are core modules. Call their
  functions by path — `list.push(mut xs, 1)`, never `xs.push(1)`. Another module of your
  package is `wal.append(..)` and a std module is `std.json.parse(..)`, all with no `use`.
  A std module that extends a core module merges into it, so `list.sort` is std's.
  Run `hirsel inspect --core --interface` for core and `hirsel inspect --std --interface`
  for std, including its extensions to core. `hirsel explain clock` points to both
  listings. For sockets, look for `host.listen` in `hirsel inspect --core --interface`.
- A dotted name after a value reads a field. A method call is a field read, so it is
  refused:

```hirsel
fn count(xs: List[u64]) -> u64 { xs.len() }               // refused: no methods; use list.len(xs)
```

- Names never shadow. A local may not reuse a visible local's name, a function of its
  module or a prelude function:

```hirsel
fn f(x: u64) -> u64 { let x = x + 1; x }                  // refused: shadows x
```
```hirsel
fn width() -> u64 { 8 }
fn f() -> u64 { let width: u64 = 1; 0 }                   // refused: shadows function width
```

- Generics take bounds from a fixed set: `plain`, `shareable`, `send`, `cross`, `key`, `ordered`,
  `codec`, `inert`, `packed` and `streamable`. No user-defined traits, and no operator overloading.
- Numbers never convert implicitly. `int.widen`, `int.narrow` and `int.try_narrow` change
  widths, and `x + y` needs one type.
- Variants are written bare (`Some(x)`, `Ok(())`), and the expected type picks them.

## 3. Types and local typing

```hirsel
type Point { x: i64, y: i64 }
type Shape = Circle(f64) | Rect(w: f64, h: f64)
fn area(s: Shape) -> f64 { match s { Circle(r) => 3.14 * r * r, Rect(w: w, h: h) => w * h } }
fn origin() -> Point { Point(x: 0, y: 0) }
```

A record is built with every field labelled, in order, and `match` is exhaustive.
Every top-level function writes its whole signature. Inside a body a type flows only
forward, from a written type, a parameter or the result type — never backward from a later
use. An unsuffixed literal needs an expected type, and a generic call needs its type
parameters fixed by that one call:

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
fn f() -> u64 { let x = 7; x }                            // refused: 7 has no expected type
```
```hirsel
fn g() -> u64 { list.len(list.new()) }                    // refused: element type T is unknown
```

Type arguments are never written at a call. Fix them with a binding type
(`let xs: List[u64] = list.new()`) or an ascription `(e : T)`. An ascription is not a cast.

Tuples, records and sequences compare lexicographically, so order by two keys
with one comparison rather than nested `if`s:

```hirsel
fn sorted_pairs(ks: List[u64], vs: List[u64]) -> bool {
  let mut i: u64 = 1
  while i < list.len(ks) {
    if (ks[i], vs[i]) < (ks[i - 1], vs[i - 1]) { return false }
    i += 1
  }
  true
}
```

## 4. Kinds: the type decides who frees

A declaration fixes its values' representation once, and every use inherits it:

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
fn f(take s: String) -> String { let t = s; s }           // refused: s was moved
```

A counted payload is immutable except through `rc.with_unique` and `arc.with_unique`, so
sharing is safe. A scoped value cannot sit in an own type's field, enter a collection, or
return past what it reads.

## 5. Views, `mut` and `take`

A parameter's mode states what the callee gets, and the call site repeats it:

```hirsel
fn len(xs: List[u64]) -> u64 { list.len(xs) }             // a view, for the call
fn add(mut xs: List[u64], x: u64) { list.push(mut xs, x) } // lent, for the call
fn eat(take xs: List[u64]) {}                             // owned by the callee
fn use_all(take xs: List[u64]) { let n = len(xs); add(mut xs, n); eat(take xs) }
```
```hirsel
fn eat(take xs: List[u64]) {}
fn f(take xs: List[u64]) { eat(xs) }                      // refused: moving place needs take xs
```

Write `take` when moving from a place of a moving type; pass a fresh value bare. A view is
read-only and lasts for a lexical extent: to the end of its block when bound by `let`, of
its arm or loop body when bound by a pattern, and of its statement as a temporary. While it
lives, nothing may write the place it views:

```hirsel
type Inbox { items: List[String] }
fn f(mut m: Inbox) {
  let items = view m.items
  list.clear(mut m.items)                                 // refused: items view lasts to block end
}
```

A function returning a view names its anchor:
`fn first(xs: List[String]) -> Option[View[String]] in xs`.

## 6. Errors and failures

Expected failures are `Result` values. `e?` returns the `Error` unchanged, so error types
must match exactly. `e ?=> m` converts the error on the way out:

```hirsel
type ReadError = Parse(ParseError) | Short(u64)
fn half(n: u64) -> Result[u64, ParseError] { if n % 2 == 0 { Ok(n / 2) } else { Error(OutOfRange) } }
fn read(n: u64) -> Result[u64, ReadError] { let h = half(n) ?=> Parse; Ok(h) }
fn mark(n: u64) -> Result[u64, ReadError] { Ok(half(n) ?=> |_| Short(n)) }
```
```hirsel
fn half(n: u64) -> Result[u64, ParseError] { Ok(n / 2) }
fn quarter(n: u64) -> Result[u64, ParseError] { half(n); Ok(0) }   // refused: unused Result
```

Discard a `Result` explicitly with `let _ = e`. A **failure** is not a value: integer
overflow, out-of-bounds indexing, `fail("msg")`, `option.unwrap(None)`, and a refused
allocation (`Capacity`) clean every scope and end the task. A failure in `main`'s task
exits 101 and prints the cause chain. `isolate(fn() -> R { .. })` catches a failure in a
non-waiting closure as `Ok(Error(causes))` without undoing prior mutations.

## 7. Memory: `bound bytes`, `bound steps`, `bytes[T]`, `region`, `@compact`, `Inline`, and `Simd`

Every charged program allocation counts against a logical byte limit (provider-side host
buffers stay outside the logical budget unless specified). `bound bytes n { .. }` runs its
body under a sublimit `n` and returns `Result[T, Capacity]`: `Error` means `n` itself could
not be admitted, while an over-budget allocation inside the body raises `Capacity` (caught
by `isolate`). `bound steps n { .. }` caps loop back-edges and calls independently of memory
and returns `Result[T, OutOfTime]`. `bytes[T]` evaluates at compile time to `T`'s logical
byte size under `charge-v1`, usable in `const` and generic code alongside `charge.*`
helpers. `region r { .. }` opens a region whose data frees all at once on exit; no value
naming `r` may escape.

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

The list grows to 4 elements, then doubles, holding old and new storage together during each
growth. Under 4096 bytes all 100 pushes fit; under 64 bytes, growing from 4 to 8 needs
32 + 64 bytes, so that push fails with `requested` 64.

`@compact(W) type` declares region data: one contiguous blob with `W`-wide offsets, built in
place through `build_in`'s `Dest` or `build_stream_in`'s `Writer`, and immutable once
published. Its fields are scalars, `Text`, spans, links, and other compact types — never own
values:

```hirsel
@compact(u32) type Entry { id: u64, name: Text, scores: FixedSpan[u32] }
@compact(u32) type Bad { owner: String }                  // refused: compact type cannot hold an owned field
```
```hirsel
fn leak(s: String) -> u64 {
  let t = region r { text.copy_in(r, s) }                 // refused: t cannot escape region r
  text.len(t)
}
```

Fixed-size values and deterministic budgets avoid heap traffic on hot paths:

- `Inline[T, N]` is a fixed-length inline array of `N` elements (plain when `T` is plain,
  own when `T` is own), indexed with `u64` and zero heap allocation.
- `Simd[T, N]` is a 16- or 32-byte plain vector of integer lanes (local, parameter, or
  result only — never a struct field), operated on through `simd.*`, `list.lanes`, and
  `inline.lanes` with target-independent semantics.
- `bytes[T]` gives the compile-time logical charge of `T`, and `bound steps n { .. }`
  caps loop back-edges and calls to `n` steps.

```hirsel
const MASK: Inline[u8, 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
const MASK_BYTES: u64 = bytes[Inline[u8, 16]]

fn has_byte(b: Bytes, at: u64, needle: u8) -> bool {
  let chunk: Simd[u8, 16] = list.lanes(b, at)
  let target: Simd[u8, 16] = simd.splat(needle)
  simd.any(simd.eq(chunk, target))
}

fn bounded_sum(n: u64) -> Result[u64, OutOfTime] {
  bound steps 1000 {
    let mut i: u64 = 0
    let mut acc: u64 = 0
    while i < n { acc += i; i += 1 }
    acc
  }
}
```

## 8. Capabilities, `waits`, and units

Files, directories, network, clocks, randomness, standard streams, environment variables,
secrets, credentials, foreign interfaces, child processes (`Exec`), and tasks are reached
only through capability parameters (full list in [capabilities](https://github.com/SamGalanakis/hirsel/blob/main/docs/spec/capabilities.md)).
A function holds exactly the authority it is passed, and no constant holds any. `main`'s
parameters are granted at launch with `--grant` and `--dir`, or declared in `[program]` in
`hirsel.toml` (`Args` comes from arguments after `--` and needs no grant flag).

```hirsel
fn stamp(c: Clock) -> u64 { host.mono_ns(c) }
```
```hirsel
fn stamp() -> u64 { host.mono_ns() }                      // refused: missing Clock argument
```

Capability arguments stay explicit: there is no implicit context. Three habits keep them
short: capture an authority at construction when a value keeps using it (`std.config.new`
stores its optional `Env`, `store.Db` keeps its `Clock`), group three or more travelling
capabilities into one small record, and pass each leaf only the field it actually needs.

```hirsel
type Caps { out: Stdout, clock: Clock, tasks: Tasks }
fn report(caps: Caps) -> Result[(), WriteError] waits {
    let _ = stamp(caps.clock)
    stdout.write(caps.out, "done\n")
}
fn stamp(c: Clock) -> u64 { host.mono_ns(c) }
```

Any function except `main` is `waits` exactly when it calls something that is `waits`, and
a needless `waits` is refused. A waiting call may observe cancellation. Tasks share nothing:
`task.spawn(tasks, bytes n, take payload, f)` moves a `send` payload into a new task, and
channels carry values between tasks. To isolate work into a separate **unit** (no shared
mutable memory, charging transferred `List`/`String` bytes to the helper's limit), use
`task.spawn_unit` with `cross` payload and result types. Use `Shared[T]` to share immutable
values between units.

```hirsel
fn unit_len(take xs: List[u64]) -> u64 { list.len(xs) }

fn run_helper(tasks: Tasks, take xs: List[u64]) -> Result[Task[u64], (Capacity, List[u64])] {
  task.spawn_unit(tasks, bytes 4096, take xs, unit_len)
}
```

```hirsel
fn twice(x: u64) -> u64 waits { x * 2 }                   // refused: waits with no waiting call
```

## 9. Tests and packages

A `@test` function takes only capabilities and returns `Result[(), E]`. Each test runs in
its own process; add `grants = ["Stdout"]` to `[tests]` in `hirsel.toml` when a test takes
`Stdout` (such as `greets` below):

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
name = "local/app"
version = "1.0.0"
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

`local/*` names are workspace-only; published packages use scoped names (`HOST/NAME` or
`HOST/TENANT/NAME`, such as `hirsel.ai/sqlite`) with SemVer versions, SHA-256 content roots
pinned in `hirsel.lock` (v2), and per-dependency `authority` ceilings checked by
`hirsel audit`. Each `[sources]` entry maps a file to a module, and `[tests]` gives every
test its root limit and grants. Definitions are private to their module unless marked `pub`,
and test sources call the package's `pub` definitions by module path (e.g. `main.twice(21)`).

## 10. The loop

```sh
hirsel new app && cd app
hirsel check                       # check program and test sources
hirsel explain LANG-11             # explain a compiler rule code, with an example
hirsel test                        # run every @test, each in its own process
hirsel run -- a b                  # build and run (pass --grant KIND when main takes grants)
hirsel run . --record run.tape     # record a run into an owner-only tape
hirsel tape replay . --tape run.tape    # replay a recording with no host grants
hirsel build --release --limit 1048576 -o program  # build a native executable
hirsel prof run --release --limit 1048576 .        # sample CPU into profile.hprof
hirsel prof top profile.hprof      # hottest functions and source lines; profile before tuning
hirsel prof replay . --tape run.tape --mem=sampled --out replay.hprof  # sample replay memory
hirsel inspect --core --interface  # public core declarations
hirsel inspect --std --interface   # public std declarations
hirsel fmt --check .               # check canonical formatting (hirsel fmt . writes in place)
```

When `check` refuses, read its rule code and site first, then run `hirsel explain RULE` for
the exact statement. Look up operations with `hirsel inspect --core --interface` or
`hirsel inspect --std --interface`. `hirsel explain primer` prints this
document.

## 11. A command that reads a file

Replace `main.hirsel` in a package created with `hirsel new` and empty its generated
`tests.hirsel`. This example keeps its test beside `main`. Index zero in `Args` is the first
argument after `--`, and `Dir` confines access to paths under the directory passed to `--dir`.

```hirsel
pub fn main(argv: Args, files: Dir, out: Stdout) -> Result[(), Exit] waits {
    let Some(path) = args.get(argv, 0) else {
        fail("give a file name after --")
    }
    let text = result.unwrap(std.file.read_string(files, path))
    result.unwrap(stdout.write(out, text))
    Ok(())
}

@test
fn reads_utf8(files: Dir, out: Stdout) -> Result[(), ()] waits {
    result.unwrap(std.file.write(files, "note.txt", b"hello\n"))
    let text = result.unwrap(std.file.read_string(files, "note.txt"))
    if text != "hello\n" {
        fail("wrong file contents")
    }
    result.unwrap(stdout.write(out, text))
    Ok(())
}
```

In `hirsel.toml`, set these keys in the existing `[tests]` table. Each test starts in an
empty temporary directory, so `dirs = ["."]` grants that scratch directory:

```text
[tests]
limit = 67108864
dirs = ["."]
grants = ["Stdout"]
```

```sh
printf 'hello\n' > note.txt
hirsel run --dir . --grant Stdout -- note.txt
hirsel test
```

## 12. An HTTP server and client test

Replace `main.hirsel` in a new package and empty its generated `tests.hirsel`. The test
listens on port zero and connects to the bound address returned by `host.listen`.

```hirsel
fn hello(req: std.http.Request, scratch: Region, mut out: std.http.Responder, state: ()) -> Result[std.http.Outcome, std.http.HandlerError] {
    Ok(Reply(std.http.response(200, b"hello", true)?))
}

type Server: send {
    listener: Listener,
    clock: Clock,
    tasks: Tasks
}

fn serve(take job: Server) waits {
    let Server(listener: take listener, clock: c, tasks) = take job
    let mut listeners: List[Listener] = list.new()
    list.push(mut listeners, take listener)
    let mut builder: std.router.Builder[()] = std.router.builder()
    result.unwrap(std.router.add(mut builder, "GET", "/", std.router.raw(hello)))
    let routes = result.unwrap(std.router.build(take builder))
    let cfg = std.http.defaults()
    result.unwrap(std.http.serve(take listeners, c, tasks, take cfg, take routes, ()))
}

pub fn main(n: Net, c: Clock, tasks: Tasks) -> Result[(), Exit] waits {
    let (take listener, _) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433), port: 8080), 8))
    serve(Server(listener: take listener, clock: c, tasks))
    Ok(())
}

@test
fn get_hello(n: Net, c: Clock, tasks: Tasks) -> Result[(), ()] waits {
    let (take listener, addr) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433), port: 0), 8))
    let server = task.spawn(tasks, bytes 16777216, Server(listener: take listener, clock: c, tasks), serve)
    let deadline = clock.deadline_after_ms(host.mono_ns(c), 5000)
    let req = std.http_client.Request(method: "GET", target: "/", headers: list.new(), body: Fixed(0))
    let mut client = result.unwrap(std.http_client.dial(n, addr, "127.0.0.1", c, req, deadline))
    result.unwrap(std.http_client.finish_request(mut client, 1024))
    if std.http_client.status(std.http_client.head(client)) != 200 {
        fail("expected 200")
    }
    let mut body: Bytes = list.with_capacity(16)
    while result.unwrap(std.http_client.read(mut client, mut body)) != 0 {}
    if body != b"hello" {
        fail("wrong response body")
    }
    std.http_client.close(take client)
    let _ = task.cancel_join(take server)
    Ok(())
}
```

In the test manifest, port zero requests an ephemeral port, so the loopback Listen grant
covers `0-65535` and Connect covers `1-65535`:

```text
[tests]
limit = 67108864
grants = ["Clock", "Tasks", "Net=Listen 127.0.0.1/32 0-65535;Connect 127.0.0.1/32 1-65535"]
```

```sh
hirsel test
hirsel run --grant Clock --grant Tasks --grant 'Net=Listen 127.0.0.1/32 8080-8080'
```

## 13. A test with a simulated clock

A virtual clock starts at a reading you choose. `clock.deadline_after_ms` takes an absolute
monotonic reading in nanoseconds first and a duration in milliseconds second — so
`clock.deadline_after_ms(2_000_000, 5)` returns `7_000_000`. A test advances the clock to
the next pending deadline without sleeping on the host.

```hirsel
fn wait(take c: Clock) -> u64 waits {
    host.sleep_until(c, clock.deadline_after_ms(host.mono_ns(c), 5))
    host.mono_ns(c)
}

@test
fn wakes_at_the_deadline(tasks: Tasks) -> Result[(), ()] waits {
    let (c, take control) = provide.clock(2_000_000, 0)
    let mut hand = take control
    let child = task.spawn(tasks, bytes 4096, c, wait)
    let Some(deadline) = clock.idle(mut hand) else {
        fail("expected a waiting task")
    }
    clock.advance_to(mut hand, deadline)
    if result.unwrap(task.join(take child)) != 7_000_000 {
        fail("wrong wake time")
    }
    Ok(())
}
```

Save this as `tests.hirsel` in the package from `hirsel new`, add `grants = ["Tasks"]` to
`[tests]`, and run `hirsel test`. The test creates its own `Clock` with `provide.clock`.
