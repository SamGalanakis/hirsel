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
`Stdout`. The entry `main` may wait with or without the `waits` marker (LANG-8). It returns
`Result[(), E]`: `Ok(())` exits 0, and `Error(e)` exits 1 after writing `error: ` and `e`
as Hirsel source on stderr (PROG-3, CAP-1, CAP-4).

## 2. What is not there

Hirsel has **no imports, no methods, no traits and no turbofish** (LANG-7).

- `list`, `string`, `map`, `int`, `option`, `rc` and the rest are core modules. You call
  their functions by path, `list.push(mut xs, 1)`, never `xs.push(1)`. Another module of
  your package is `wal.append(..)` and a std module is `std.json.parse(..)`, all with no
  `use`. A std module that extends a core module merges into it, so `list.sort` is std's.
  Run `hirsel inspect --core --interface` for core and `hirsel inspect --std --interface`
  for std, including its extensions to core. `hirsel explain clock` points to both
  listings. For sockets, see `hirsel explain NET-1` and look for `host.listen`.
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

- Generics take bounds from a fixed set: `plain`, `shareable`, `send`, `cross`, `key`, `ordered`,
  `codec`, `inert`, `packed` and `streamable` (OWN-4, CONC-15). No user-defined traits, and no operator overloading
  (LANG-21).
- Numbers never convert implicitly. `int.widen`, `int.narrow` and `int.try_narrow` change
  widths, and `x + y` needs one type (LANG-11, LANG-21).
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
bound by `let`, of its arm or loop body when bound by a pattern, and of its statement as a
temporary (OWN-8). While it lives, nothing may
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

The list grows to 4 elements, then doubles, and each growth holds the old and the new
storage at once (MEM-24, MEM-26). Under 4096 bytes all 100 pushes fit. Under 64 bytes, growing from 4 to 8
elements needs 32 + 64 bytes, so that push fails with `requested` 64.

`@compact(W) type` declares region data: one contiguous blob with offsets `W` wide,
built in place through `build_in`'s `Dest` or `build_stream_in`'s `Writer`, and immutable
once published. Its
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

Capability arguments stay explicit: there is no implicit context. Three habits keep them
short. Capture an authority at construction when a value keeps using it: std's
`config.new(source: Env, max_errors: u64)` stores the `Env` in its `Reader`, and every
getter reads through `r.source`, as `store.Db` keeps its `Clock`. When three or more
capabilities travel together, pass one small record of them. Give a leaf only the field it
needs, so its signature still names its whole authority.

```hirsel
type Caps { out: Stdout, clock: Clock, tasks: Tasks }
fn report(caps: Caps) -> Result[(), WriteError] waits {
    let _ = stamp(caps.clock)
    stdout.write(caps.out, "done\n")
}
fn stamp(c: Clock) -> u64 { host.mono_ns(c) }
```

Any function but `main` is `waits` exactly when it calls something that is `waits`, and
a needless `waits` is refused too (LANG-8). A waiting call may observe cancellation
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
limit and grants. A definition is private to its module unless marked `pub`, and test
sources call the package's `pub` definitions by module path, as in `main.twice(21)`
(PROG-1, PROG-2).

## 10. The loop

```sh
hirsel new app && cd app
hirsel check                       # check program and test sources
hirsel explain OWN-7               # the rule a refusal cites, with an example
hirsel test                        # every @test, each in its own process
hirsel run --grant Stdout -- a b   # build and run; grants are explicit
hirsel replay . --tape run.tape    # replay a recording with no host grants
hirsel build --release -o program  # a native executable
hirsel prof run --release .        # sample CPU or replay memory into .hprof
hirsel inspect --core --interface  # public core declarations
hirsel inspect --std --interface   # public std declarations
hirsel fmt .                       # canonical formatting
```

When `check` refuses, read its rule ID first. The ID names the one rule broken and its
site, and `hirsel explain` gives the rule's exact statement. Fix that site, then check
again. Look up an operation with `hirsel inspect --core --interface` or
`hirsel inspect --std --interface`. `hirsel primer` (or `hirsel explain primer`) prints this document.

## 11. A command that reads a file

Replace `main.hirsel` in a package created with `hirsel new` and empty its
generated `tests.hirsel`. This example keeps its test beside `main`. Index zero in
`Args` is the first argument after `--`. The `Dir` grants access to names under
the directory supplied with `--dir`.

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

In `hirsel.toml`, set these keys in the existing `[tests]` table. The test
starts in an empty directory, so `dirs = ["."]` grants that directory.

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

Replace `main.hirsel` in a new package and empty its generated `tests.hirsel`.
The test listens on port zero and connects to the bound address returned by `host.listen`.

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
    let cfg = std.http.Config(ingress: list.new(), sockets_max: 2, http_max: 2, upgrades_max: 0, conns_max: 2, ip_sockets_max: 2, refuse_max: 1, refuse_ms: 500, inbox_cap: 4, head_max: 1024, fields_max: 8, pipeline_max: 1024, body_inline: 1024, body_max: 1024, out_cap: 256, handler_bytes: 4096, first_head_ms: 1500, idle_ms: 1500, head_ms: 1500, body_ms: 1500, handler_ms: 1500, write_ms: 1500, drain_ms: 100, linger_ms: 100, drain_max: 1024, streams_max: 0, state_max: 0, reads_max: 0, heartbeat_ms: 0, lease_ms: 0)
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

In the test manifest, port zero requests an ephemeral port, so the loopback
Listen grant covers the full port range. Connect also covers the range where
the OS can select the server port.

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

A virtual clock starts at a reading you choose. `clock.deadline_after_ms`
takes an absolute monotonic reading in nanoseconds first and a duration in
milliseconds second. For example, `clock.deadline_after_ms(2_000_000, 5)`
returns `7_000_000`. A test can advance the clock to a pending deadline without
sleeping for that duration on the host.

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

Save this as `tests.hirsel` in the package from `hirsel new`. Add `grants =
["Tasks"]` to its `[tests]` table and run `hirsel test`. The test creates its
own `Clock` with `provide.clock`.
