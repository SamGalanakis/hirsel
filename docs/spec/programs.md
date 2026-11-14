# Programs

### PROG-1: Packages

A **package** has a name and a major version and consists of modules; each source file is
one module, with the module path its package manifest assigns. A definition's identity is
its package, version, module path and name. Two versions of a package are two packages,
and their types are distinct. A package names each dependency at one exact version; the
manifest format is outside the language. A package states the charge policy it was
written for ([MEM-24](budgets.md#mem-24-logical-charges)); in v1 that is `charge-v1`.

### PROG-2: Visibility

A definition is private to its module unless marked `pub`. A `pub` type exposes its
fields, variants and constructors, and may be wholly destructured anywhere it is visible,
except that a resource is destructured only in its defining module
([OWN-14](ownership.md#own-14-no-partial-moves)). A `pub opaque` type exposes its name and its published summary
([OWN-2](ownership.md#own-2-type-summaries)) only: outside its module its values cannot be
constructed, destructured, matched on their fields or read field by field. Opaque types
are how a library keeps an invariant ([MEM-18](memory.md#mem-18-pools-are-library-code)).

```hirsel
pub opaque type Month { n: u8 }
pub fn month(n: u8) -> Option[Month] { if n >= 1 && n <= 12 { Some(Month(n: n)) } else { None } }
```
```hirsel
fn forge() -> Unique { Unique(n: 1) }                     // refused: PROG-2, Unique is opaque
```

### PROG-3: Entry

A program's entry is a `pub fn main` named by its manifest. Its parameters are capability
types ([CAP-4](capabilities.md#cap-4-entry-grants)) and `Args`, all unmarked. `Args` is an
opaque plain type designating the run's argument strings, which are host memory outside
every limit and live for the whole run ([core](core.md#built-in-types)), read with
`args.len(a)` and `args.get(a, i) -> Option[View[String]] in a`. Its result is `Result[(), E]` for any `E`. The
run's configuration supplies the root limit ([MEM-19](budgets.md#mem-19-limits)). The run
ends when `main` returns and its cleanup is done: `Ok(())` is success, `Error(e)` is an
error outcome carrying `e`, a failure is a failure outcome
([FAIL-14](failure.md#fail-14-failure-boundary)).

A run in a process reports its outcome as the exit status, only after the run has ended, so
every cleanup and finalizer has run; a run stop is the one exception, because it skips
cleanup. A program chooses its status only through `main`'s result. Core's plain
`Exit { status: u8 }` is the error type that carries a status. No operation ends the
process early.

| outcome | exit status | stderr |
|---|---|---|
| `Ok(())` | 0 | nothing |
| `Error(Exit(status: s))`, `s` from 1 to 100 | `s` | nothing |
| `Error(Exit(status: s))`, `s` 0 or above 100 | 1 | nothing |
| `Error(e)`, `E` not `Exit` | 1 | nothing |
| failure | 101 | the cause chain, one line per cause, primary first: the cause and its site |
| run stop ([FAIL-15](failure.md#fail-15-run-stops)), other than termination by the host | 102 | `run stop: ` and the stop's name |
| not started ([CAP-4](capabilities.md#cap-4-entry-grants), [MEM-19](budgets.md#mem-19-limits)) | 103 | one line naming what could not be given |

So 101 to 103 never come from an `Exit`. The stderr writes are the run's, not the
program's. A program writes its own diagnostics before it returns.

```hirsel
pub fn main(out: Stdout, argv: Args) -> Result[(), WriteError] waits { stdout.write(out, "hello\n") }
```
```hirsel
pub fn main(out: Stdout) -> Result[(), Exit] waits {
  match stdout.write(out, "hello\n") { Ok(()) => Ok(()), Error(_) => Error(Exit(status: 2)) }
}
```
```hirsel
pub fn main(take xs: List[u64]) -> Result[(), ()] { Ok(()) }   // refused: PROG-3
```
```hirsel
pub fn main() -> u8 { 3 }                                     // refused: PROG-3, not Result[(), E]
```

### PROG-4: Tests

A function marked `@test` takes no type parameters and no parameters other than
capabilities, unmarked as `main`'s are (PROG-3), so never `Args`; it returns `Result[(), E]`
for any `E`, and may be `waits`. Each test runs as its own run with its own root limit: the
test run's configuration grants its parameters and supplies its root limit as a run's
configuration does for `main` ([CAP-4](capabilities.md#cap-4-entry-grants),
[MEM-19](budgets.md#mem-19-limits)), and a test whose run does not start fails. `Ok(())`
passes the test; `Error(e)` fails it, `e` cleaned as its run ends whatever `E` is, an `Exit`
included; a failure fails it; a run stop fails the test it stops and ends no other test's
run. A test function can be named, called or taken as a value only by another test
function, and a value so taken may be passed on and called anywhere, as any function value;
otherwise it is an ordinary function of its module (PROG-2), so it may call that module's
private functions.

```hirsel
@test
fn adds() -> Result[(), ()] { if 1u64 + 1 == 2 { Ok(()) } else { fail("arithmetic") } }
@test
fn greets(out: Stdout) -> Result[(), WriteError] waits { stdout.write(out, "hi\n") }
fn invoke(f: fn() -> Result[(), ()]) -> Result[(), ()] { f() }
@test
fn leaf() -> Result[(), ()] { Ok(()) }
@test
fn outer() -> Result[(), ()] { invoke(leaf) }
```
```hirsel
@test
fn each[T]() -> Result[(), ()] { Ok(()) }                     // refused: PROG-4, a type parameter
```
```hirsel
@test
fn named(argv: Args) -> Result[(), ()] { Ok(()) }             // refused: PROG-4, Args is not a capability
```

### PROG-5: Code identity

Each top-level function has a **code identity**: its definition identity (PROG-1) and its
signature. A call through a function value keeps the callee's identity; a closure has
none of its own. An instance of a generic function is identified by its definition, its
closed type arguments and the dictionaries chosen for its bounds; sizes are not part of
it. A program reads the identity of a named function through `code` (PROG-9). No
construct dispatches on a function's name or signature at run time, and nothing
observable depends on a function's address or on the order of declarations
([SEM-8](semantics.md#sem-8-determinism)), so two bodies with the same checked meaning
are interchangeable.

### PROG-9: Code descriptors

`code.describe(f)` returns a `StaticFn[F]`. It is accepted only where `f` is the name of a
top-level function without type parameters (region parameters are allowed), and `F` is
then that function's callable type ([LANG-13](language.md#lang-13-callables)); any other
argument, a local, a parameter, a field, a closure or a generic function, is refused here,
at the argument. A `StaticFn[F]` is plain, `shareable`, `send` and `key`, 8 bytes, and
making one admits nothing. `code.callable(s)` returns the function. Nothing turns bytes into
a function, and a `StaticFn` grants no authority: the function takes its capabilities as
arguments.

`code.identity(s)` returns a view of static bytes, the function's **identity bytes**. Two
`StaticFn` values are equal exactly when their identity bytes are, and nothing else about a
function takes part in that equality. The bytes are produced once, by the front end, from
the checked declarations, and are the same on every engine and build
([SEM-8](semantics.md#sem-8-determinism)). They are one version byte, `0x01`, which versions
this encoding and nothing else, followed by the UTF-8 text `D S`:

- `D` is the definition identity (PROG-1): `package@major/module/../name`, the package's
  name, its major version in decimal without leading zeros, each component of the module
  path, and the function's name, joined by `/`. In each component, every byte other than
  an ASCII letter, digit or `_` is written `%` and two upper-case hex digits.
- `S` is the function's header (`fn-header`, [syntax](syntax.md)) with its whole meaning
  and no spelling: parameter names are left out; region parameters keep their order and
  are written `^0`, `^1`, ..; an anchor or region argument names its parameter by position
  as `#0`, `#1`, .., or its region as `^i`, followed by any field names as written; every
  type path is replaced by the definition identity of the type it names, aliases expanded
  first, and keeps its type arguments; the builtin type names of
  [LANG-10](language.md#lang-10-built-in-types) are written as themselves; a missing result
  is written `->()`; `waits` stays; the outlives clauses are written as `where ^i:^j`
  pairs, sorted by `i` then `j`, each once; bounds in `own[..]` are written in the order
  of the `bound-name` production.
- No whitespace appears except one space between two adjacent words, a word being a
  keyword, an identifier, a number or a definition identity, and no list has a trailing
  comma.

So `fn double(b: Bytes) -> Result[Bytes, Capacity]` in module `main` of package `app`
version 1 has the identity bytes `0x01` followed by
`app@1/main/double(core@1/core/List[u8])->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]`.
A change to a function's package, version, module path, name, parameter modes or types,
result, anchors, `waits` or outlives clauses changes its identity; a change to its
parameter names or to its body does not. A rename or a move to another module is a new
identity, and no alias connects the two. A later encoding may add an instance's closed type
arguments and dictionaries after `D`; version 1 has none.

`code.revision(s)`, a check of the body, is reserved ([reserved](reserved.md), D) and is
never part of equality. An identity stored without a revision was matched by identity
alone, so a changed body under an unchanged identity is not detected.

```hirsel
type Handler = fn(Bytes) -> Result[Bytes, Capacity]
type Entry { code: StaticFn[Handler] }
fn double(b: Bytes) -> Result[Bytes, Capacity] { let mut out: Bytes = list.new(); list.append(mut out, b)?; list.append(mut out, b)?; Ok(out) }
fn register() -> Entry { Entry(code: code.describe(double)) }
fn serve(reg: List[Entry], wire: Bytes, arg: Bytes) -> Option[Result[Bytes, Capacity]] {
  for e in reg { if code.identity(e.code) == wire { return Some((code.callable(e.code))(arg)) } }
  None
}
```
```hirsel
fn d(f: fn(u64) -> u64) -> StaticFn[fn(u64) -> u64] { code.describe(f) }   // refused: PROG-9, f names no top-level function
```
```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
fn d() -> u64 { let _ = code.describe(same); 0 }                             // refused: PROG-9, same is generic
```

### PROG-6: One meaning

A program has one meaning. Whether it is accepted, and what each accepted program does,
does not depend on the engine, the build mode, optimisation, flags, the host, or the order
in which definitions are checked. There are no conditional compilation forms and no
language switches. The one exception is an implementation's resource refusal: a program
whose checking exceeds the implementation's resources (for example unbounded generic
instantiation, or instantiation past a depth or count) may be refused with a message that
names no rule; such a refusal is not a meaning of the program.
