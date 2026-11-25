# Programs

### PROG-1: Packages

A **package** has a name and a major version and consists of modules; each source file is
one module, with the module path its package manifest assigns. A definition's identity is
its package, version, module path and name. Two versions of a package are two packages,
and their types are distinct. A package names each dependency at one exact version; the
manifest format is outside the language, and the `hirsel` tool's is
[`hirsel.toml`](packages.md). A package states the charge policy it was
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
`args.len(a)` and `args.get(a, i) -> Option[View[String]] in a`. Index zero is the
first program argument supplied after `--`. The executable name is outside `Args`.
Its result is `Result[(), E]` for any `E`. The run's configuration supplies the root limit ([MEM-19](budgets.md#mem-19-limits)). The run
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
| `Error(e)`, `E` not `Exit` | 1 | one line: `error: ` and the text of `e` |
| failure | 101 | the cause chain, one line per cause, primary first: the cause and its site |
| run stop ([FAIL-15](failure.md#fail-15-run-stops)), other than termination by the host | 102 | `run stop: ` and the stop's name |
| not started ([CAP-4](capabilities.md#cap-4-entry-grants), [MEM-19](budgets.md#mem-19-limits)) | 103 | one line naming what could not be given |

So 101 to 103 never come from an `Exit`. The stderr writes are the run's, not the
program's, and come after cleanup; a failure in that cleanup is the run's outcome instead
(101) and the error's line is not written. An `Exit` carries only a status, so it writes
nothing.

A `Capacity(c)` cause's line is `Capacity(requested: N, available: A) at SITE`, with `c`'s
two fields ([MEM-21](budgets.md#mem-21-admission)). When the refusing limit was
the run's root limit `L`, the line goes on with
` (root limit L bytes; raise it with --limit or the manifest)`.

The text of an error value is the value written as Hirsel source, from `E`'s declarations:

- A record is `Name(field: v, ..)`, a tuple `(v, ..)` and the unit `()`.
- A variant case is its name, with its payload in parentheses when it has one, labelled
  (`Bind(port: 80)`) when the case's fields are.
- A list or array is `[v, ..]`. A `Box[T]` is its `T`.
- A string is a string literal and a `char` a character literal. Each uses only the escapes
  `\\`, `\n`, `\r`, `\t`, `\0`, its own delimiter's, and `\u{..}` for any other control
  character.
- An integer is in decimal. A float is the shortest decimal that reads back as the same
  value, with a `.` or an exponent, or `NaN`, `inf` or `-inf`.
- A `Secret` is `[secret]` ([TLS-5](tls.md#tls-5-secrets)). Any other capability or handle, and any opaque,
  compact or region-carrying type, is `<Name>`. A function value is `<fn>`.

```hirsel run
type ServeError = Configuration | Bind(port: u16)
type AppError = Serve(ServeError) | Config(file: String, line: u32)

pub fn main() -> Result[(), AppError] { Error(Config(file: "app \"a\".conf", line: 3)) }
```

Output:

```text
```

Outcome: error, stderr `error: Config(file: "app \"a\".conf", line: 3)`.

The run then exits with status 1, and `Error(Serve(Configuration))` writes
`error: Serve(Configuration)`.

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
[MEM-19](budgets.md#mem-19-limits)), and a test whose run does not start fails. The package
declares that configuration beside the manifest that names `main` (PROG-3): one root limit
and one set of grants for its tests, any part of which a named test's own configuration
replaces, and its test sources. Test sources join the package's modules for `hirsel check`
and `hirsel test`. Only test runs build those sources into an executable. A configuration naming a test the package does not have is refused. Of the
declared grants a test is granted only those its parameters take, so a test without
parameters is granted nothing and no test's configuration changes another test's run. Each
test run starts in a working directory of its own, empty when the run starts, and a relative
directory grant resolves against it as a launch's does
([launch](launch.md#release-recipe-and-host-assembly)). `Ok(())`
passes the test; `Error(e)` fails it, `e` cleaned as its run ends whatever `E` is, an `Exit`
included; a failure fails it; a run stop fails the test it stops and ends no other test's
run. A test that takes no parameters has its outcome fixed by its program and its run's
configuration, so an implementation may report a pass it recorded for the same program and
configuration in place of the run, unless its reach holds a foreign function, whose pass is
never reused ([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)), and then says that test's pass was reused; it never
reuses any other outcome. A test function can be named, called or taken as a value only by
another test function, and a value so taken may be passed on and called anywhere, as any function value;
otherwise it is an ordinary function of its module (PROG-2), so it may call that module's
private functions.

With `--property-corpus DIR`, the runner uses the
[runner-owned property failure corpus](../../core/std/api/prop.md#runner-owned-failure-corpus)
for opted-in properties. It verifies a saved minimized case in a fresh run
before searching and bypasses pass reuse for the invocation. The corpus
directory is outside the test's grants.

With `--laws` ([law table](../laws.md)), the runner also reports a strength row per
test beside this outcome: how strong the evidence of the test's `std.prop` and `std.model`
calls was (held, truncated, vacuous, closed, to depth, …). A row never changes the outcome
above, and no program reads it (R28). Under `--laws` a pass is reused only together with the
rows recorded with it; otherwise the test is executed.

Version note (2026-11-24, verify-observer-2037): adds the `--laws` strength row
beside the PROG-4 outcome and its reuse condition. The outcome rules are unchanged.

In `hirsel.toml`, use plural `grants` and `dirs` in `[tests]` or an individual
test's run table. The `test` command accepts `--grant` and `--dir` when the package
has no manifest test configuration.

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

Executable identity is separate immutable [launch metadata](launch.md), with
native, evaluator, embedded and unavailable cases. It is diagnostic metadata;
it does not participate in function identity or dispatch.

Each top-level function has a **code identity**: its definition identity (PROG-1) and its
signature. A call through a function value keeps the callee's identity; a closure has
none of its own. An instance of a generic function is identified by its definition, its
closed type arguments and the dictionaries chosen for its bounds; sizes are not part of
it. A program reads the identity of a named function through `code` (PROG-9). No
construct dispatches on a function's name or signature at run time, and nothing
observable depends on a function's address or on the order of declarations
([SEM-8](semantics.md#sem-8-determinism)), so two bodies with the same checked meaning
are interchangeable, except to `code.revision` (PROG-9) and the code address (PROG-10),
which name the checked IR.

### PROG-9: Code descriptors

`code.describe(f)` returns a `StaticFn[F]`. It is accepted only where `f` is the name of a
top-level function; any other argument, a local, a parameter, a field or a closure, is
refused here, at the argument. When `f` has no type parameters (region parameters are
allowed), `F` is its callable type ([LANG-13](language.md#lang-13-callables)). When it has
type parameters, `code.describe` names one **instance** of it (PROG-5): its closed type
arguments are found as below, and `F` is `f`'s callable type at them. A `StaticFn[F]` is
plain, `shareable`, `send` and `key`, 8 bytes, and making one admits nothing.
`code.callable(s)` returns the function. Nothing turns bytes into a function, and a
`StaticFn` grants no authority: the function takes its capabilities as arguments.

**Type arguments.** A generic `f`'s type arguments are found from the expected type of the
`code.describe` call alone, at the point LANG-11 types it, and from nothing else: not from
`f`'s body, its bounds, a later argument or a later statement. That expected type must be
`StaticFn[G]`, and `f`'s callable type must fit `G` as a function item fits an expected
callable type (LANG-13). Every type parameter of `f` must be fixed by a part of `G` that is
already fixed there. A part of `G` that is still open, such as a type argument of an
enclosing call that a later argument would fix, fixes nothing, and a type parameter that
only such a part would fix is open. A type parameter of the enclosing definition is fixed;
its body is checked once with it rigid (LANG-12) and describes, in each of its own instances,
the instance at that instance's arguments. When every type parameter is fixed, `f`'s callable
type at them fixes the open parts of `G`, as any argument does (LANG-11). A type parameter
named in neither `f`'s parameters nor its result is never fixed. A type argument may name no
region: no anchor, region argument or region type at any depth, since a root has no name
outside the function that holds it. A type argument that lacks a bound its parameter declares
is refused under LANG-12, at the argument, with LANG-12's message. Every other refusal here is
PROG-9's, at the argument, with one of these messages, where `T` is the declared name of the
first such type parameter in declared order and `f` the function's name:

- the argument is not the name of a top-level function: `code.describe requires the name
  of a top-level function`;
- a type parameter is open: `code.describe leaves type parameter T of f open: the expected
  StaticFn type must fix it`;
- a type argument names a region: `code.describe cannot name an instance whose type
  parameter T of f names a region`.

`code.identity(s)` returns a view of static bytes, the function's **identity bytes**. Two
`StaticFn` values are equal exactly when their identity bytes are, and nothing else about a
function takes part in that equality. The bytes are produced once for each function or instance, by the front end, from
the checked declarations, and are the same on every engine and build
([SEM-8](semantics.md#sem-8-determinism)). They are one version byte, `0x01`, which versions
this encoding and nothing else, followed by the UTF-8 text `D A S`:

- `D` is the definition identity (PROG-1): `package@major/module/../name`, the package's
  name, its major version in decimal without leading zeros, each component of the module
  path, and the function's name, joined by `/`. In each component, every byte other than
  an ASCII letter, digit or `_` is written `%` and two upper-case hex digits.
- `A` is empty for a function without type parameters. For an instance of a generic
  function it is `[`, the closed type arguments, each written as `S` writes a type and
  separated by `,`, then `]`, followed, when any type parameter declares a bound, by `{`,
  the **dictionary texts**, separated by `,`, then `}`. Type parameter `i` is the one `S`
  writes `$i`, so the arguments come in that order. There is one dictionary text for each
  bound each type parameter declares, in that order of the parameters and, within one, in
  the order of the `bound-name` production; it is the bound's name, `[`, the type argument
  as `S` writes it, and `]`. A bound only implied by another, as `key` is by `integer`, has
  no text. In v1 a bound has exactly one implementation for each type that has it
  ([OWN-4](ownership.md#own-4-bounds), [SEM-10](semantics.md#sem-10-codecs)), so the text
  names the dictionary that the checker selects. These texts are this section's spelling;
  PROG-10's instance identity has its own dictionary texts, and neither is computed from
  the other.
- `S` is the function's header (`fn-header`, [syntax](syntax.md)) with its whole meaning
  and no spelling: parameter names and the generics list are left out; a type parameter
  is written `$0`, `$1`, .. in the order each is first named in the parameters and then the
  result, read left to right, never as its argument, so `S` is the generic header and not
  the instance's callable type; the region
  generics ([MEM-6](memory.md#mem-6-roots)) are written `^0`, `^1`, .. in the order each is
  first named in the parameters and then the result, read left to right, and any named in
  neither take the next numbers in their declared order; an anchor, a region argument or
  an outlives clause names a region generic as `^i` and a parameter, `Region` parameters
  included, as `#i` by its position in the function's own parameter list, since a callable
  type's parameters have no names ([LANG-13](language.md#lang-13-callables)) and no anchor
  is a projection ([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)); every
  type path is replaced by the definition identity of the type it names, aliases expanded
  first, and keeps its type arguments; the builtin type names of
  [LANG-10](language.md#lang-10-built-in-types) are written as themselves; a missing result
  is written `->()`; `waits` stays; the outlives clauses are written as `where x:y`
  pairs, one for each `x` that the declared clauses make outlive a different `y`, directly
  or by transitivity, sorted by `x` then `y`, every `^i` before every `#i` and each by `i`;
  bounds in `own[..]` are written in the order of the `bound-name` production.
- No whitespace appears except one space between two adjacent words, a word being a
  keyword, an identifier, a number, a definition identity, a root name `^i` or `#i` or a
  type parameter `$i`, and no list has a trailing comma.
- `D`'s escape writes every `[`, `{`, `(` and `$` in a component as `%XX`, so `D` ends at
  the first `[` or `(`; a type and a dictionary text each close every bracket they open, so
  `A` ends at the `]` or `}` that closes it. The text is therefore read back to one
  `(D, arguments, dictionaries, header)`, and two descriptors have equal bytes exactly
  when those four are equal.

So `fn double(b: Bytes) -> Bytes` in module `main` of package `app`
version 1 has the identity bytes `0x01` followed by
`app@1/main/double(core@1/core/List[u8])->core@1/core/List[u8]`.
And `fn edges[in b, in a](s: Region, r: Region, t: Text in a, u: Text in b) -> u64 where
a: r, r: s` there, or the same header with `[in a, in b]`, has `0x01` followed by
`app@1/main/edges(core@1/core/Region,core@1/core/Region,core@1/core/Text in ^0,core@1/core/Text in ^1)->u64 where ^0:#0,^0:#1,#1:#0`.
A change to a function's package, version, module path, name, parameter modes or types,
result, anchors, `waits` or outlives clauses changes its identity; a change to its
parameter names, to the order of its region generics or to its body does not. A rename or a move to another module is a new
identity, and no alias connects the two.

For an instance, a change to its type arguments, to the bounds `f` declares or to the
definition identity of a type an argument names changes its identity; a change to a type
parameter's name or declared order, to the fields or body of a type an argument names, or to
the call site, the enclosing instance or the package that describes it does not. The bytes
hold no closure hash, layout name, source position, ordinal or build product, so the same
source gives the same bytes in every build. So `fn pick[T: plain](a: T, b: T) -> T` in module
`main` of package `app` version 1, described at `u64` and at `u32`, has `0x01` followed by
`app@1/main/pick[u64]{plain[u64]}($0,$0)->$0` and by
`app@1/main/pick[u32]{plain[u32]}($0,$0)->$0`: two instances, two descriptors, each calling
its own instance. `fn swap[B, A: send](take p: (A, B)) -> (B, A)` at `A` = `u64` and `B` =
`Text` writes `A` as `$0`, since the parameters name it first, whatever the declared order:
it has `0x01` followed by
`app@1/main/swap[u64,core@1/core/Text]{send[u64]}(take($0,$1))->($1,$0)`. A
function without type parameters has an empty `A` and no `$i`, so its bytes are the ones
this section gave before generic descriptors.

`code.revision(s)` returns `Option[View[Bytes]] in s`: the function's **revision**, 32 bytes
that name the code a call through `s` runs. It is derived from the function's **code
address** (PROG-10), which covers the checked core IR of every instance (PROG-5) the function
reaches, std's and core's included, taken through each instance's slots (PROG-10): the calls,
function values and descriptors its retained code makes, the core functions the compiler
implements that it uses, and the finalizers its values need, among them the functions the
checker selects from types, such as a codec maker the code calls. The revision
is `H("hirsel.code-revision.v2", address)`, where the address is that of the function's
revision root: the function itself, the described instance of a generic function, the
adapted function for a `$callable` adapter, and the
builtin address (PROG-10) for a core function the compiler implements, such as `float.widen`.
Equal revisions mean equal addresses, so Hirsel has one identity for code; the revision only
keeps the bytes a program holds apart from the keys of a code store. Nothing else enters: not
the backend, the runtime, the target (except, once retention is built, through the arms
PROG-6 retains for it), the optimisation level, the root limit, source
positions or optimisation hints. So:

- an edit to comments, layout, local or parameter names, item order, or an item the
  function does not reach leaves the revision equal;
- an edit that changes the checked IR of any instance the function reaches changes it,
  including one that changes how a source is parsed, such as a line break before `-`;
- the same program gives the same revision on every engine and optimisation level, and a
  compiler that lowers a reached body differently gives a different one.

The revision is never part of equality: two `StaticFn` values with equal identity bytes are
equal whatever their revisions. It is `None` only when the program was assembled without its
revisions, which a program `hirsel build` links never is; a core function the compiler
implements has its revision in any program. A store, registry or recording that keeps only
identity bytes matches by identity alone, so a changed body under an unchanged identity is
not detected; one that keeps the revision as well detects it.

Version note (2026-11-21, code addresses): the revision was a hash over the reach's
`(identity, IR)` rows under `hirsel.code-revision.v1`. It is now derived from the address,
under `hirsel.code-revision.v2`. Every revision value, and so every release digest
([launch](launch.md#release-manifest)), changes once; its size and every format that carries
it do not. No migration is provided before 1.0.

Version note (2026-11-24, generic descriptors; not yet built): `code.describe` was refused for
a function with type parameters, and the identity text was `D S`. It now describes an instance
at closed type arguments and the text is `D A S`. The version byte stays `0x01`: for a function
without type parameters `A` is empty and `S` writes no `$i`, so every identity a program,
store catalog, runner key or recording holds today is unchanged, and no byte string that
names an instance equals one that names a function without type parameters, because `D` is
then followed by `[`, which no earlier text had there. One encoding covers both, so there is
no second version for a reader to accept. Reversible: the spec only.

```hirsel
type Handler = fn(Bytes) -> Bytes
type Entry { code: StaticFn[Handler] }
fn double(b: Bytes) -> Bytes { let mut out: Bytes = list.new(); list.append(mut out, b); list.append(mut out, b); out }
fn register() -> Entry { Entry(code: code.describe(double)) }
fn serve(reg: List[Entry], wire: Bytes, arg: Bytes) -> Option[Bytes] {
  for e in reg { if code.identity(e.code) == wire { return Some((code.callable(e.code))(arg)) } }
  None
}
```
```hirsel
fn d(f: fn(u64) -> u64) -> StaticFn[fn(u64) -> u64] { code.describe(f) }   // refused: PROG-9, f names no top-level function
```
```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
fn d() -> u64 { let _ = code.describe(same); 0 }                             // refused: PROG-9, T of same is open
```
```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
fn d() -> StaticFn[fn(u64, u64) -> bool] { code.describe(same) }            // same at u64
fn e[T: key]() -> StaticFn[fn(T, T) -> bool] { code.describe(same) }         // same at each e's T
fn f() -> bool { d() == e() }                                                // e at u64: true
```
```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
fn d() -> StaticFn[fn(f64, f64) -> bool] { code.describe(same) }            // refused: LANG-12, f64 is not key
```
```hirsel
fn left[A: plain, B](a: A) -> A { a }
fn d() -> StaticFn[fn(u64) -> u64] { code.describe(left) }                  // refused: PROG-9, B of left is open
```
```hirsel
fn same[T: key](a: T, b: T) -> bool { a == b }
fn keep[F](s: StaticFn[F]) -> u64 { 0 }
fn d() -> u64 { keep(code.describe(same)) }                                  // refused: PROG-9, T of same is open
```

### PROG-10: Code addresses

Every instance (PROG-5) that a checked program reaches has a **code address**: 32 bytes,
computed from its checked core IR and the addresses of the code it calls. The address names
code only. Public names stay path-based: identity bytes (PROG-9), `StaticFn` equality, entry
keys, client matching and link symbols never use it, so an edit to a body never makes a new
public name. Write `H(d, x)` for SHA-256 over the length-framed domain text `d` followed by the
length-framed bytes `x`.

**Instance identities.** An instance's identity is its definition, the canonical text of the
implementation chosen for each of its constraints, in order, and the closure hash of each of
its type arguments, in order. A type's **closure hash** is `H("hirsel.core-closure.v1", c)`,
where `c` is the closure wire encoding of the rows the type reaches, in first-visit order from
the type, over fields, then held parts, then hook type arguments, then hook signatures. That
encoding is frozen as it stands at this version of the section: a change to it, including one
that comes from reordering the compiler's own declarations, takes a new domain. Identities are
ordered by comparing, in turn, the package name, the package's major version, the module path
component by component, the item name, the dictionary texts one by one, and the closure hashes
one by one. Texts and hashes compare bytewise, and a list that is a prefix of another comes
first. The **identity encoding** of an instance is, in order: its definition identity `D`
(PROG-9) as a text, the dictionary count and each dictionary text, then the closure-hash count
and each 32-byte hash. A text is its byte length as unsigned LEB128 followed by its UTF-8
bytes. Bodies, groups and builtin addresses use this encoding, never a hash of it and never the
header-bearing bytes of PROG-9.

A function that the compiler generates beside a source item, such as a closure, a task-entry
wrapper or a flow transform, is named by its owner item, its kind and its ordinal among the
owner's generated functions of that kind, in the order the checker emits them. No syntax-node
number enters the name, so wrapping an earlier expression in parentheses never renames a later
closure. A generated adapter that several owners share keeps its one shared identity.

**Retained code** (macOS decision D1). An instance's **retained code** is its
checked IR with the arms that a constant expression rules out removed
([PROG-6](#prog-6-one-meaning)); a body the checker keeps is one that retained code holds.
Only retained code makes slots, calls and reach. Because the rule covers every constant, not
only `target`'s, it took normal form 4: a body whose function branches on any constant
changes. Retained code is computed for one target at a time, before the body is encoded:
the build's `--target`, and for a build without one and for an in-process run (`hirsel run`
on the evaluator, the checker corpus) `x86_64-unknown-linux-gnu`, the toolchain's default.

**Body.** An instance's **body** is the normal form of its retained code: its own function, the
functions generated beside it, the shapes, signatures, statics and instance rows they use, and
its **slots**, the functions it uses but does not define. It holds:

- every operation and terminator, in its order within its block, so parse decisions,
  type-selected calls, failure order and the order of charges are kept;
- every instruction's failure target, at the instruction;
- each shape's canonical name, representation, carried and held parts, field order, cases,
  grants, `send`, and its hooks as metadata (each hook's identity and signature); signatures
  and loans; statics, with each byte static's alignment and each aggregate's field order;
  layout operands as the checker wrote them: symbolic layout queries, explicit byte steps,
  allocation alignment, the charge, overflow and refund operands, and case indices; compact
  roots, as a set; and which functions take vector-width variants (SEM-11);
- the instance's own identity, so `ident[u64]` and `ident[u32]` have different bodies, and
  each slot's identity and signature.

The slots are exactly the functions the retained code can run: the target of each call, each
function value and each descriptor (`code.describe`) in a block the body keeps, each core
function the compiler implements that such a block uses (below), and the finalizer of every
type that a value in that code can hold, through fields and cases included. A hook that only
describes a type, such as a codec maker no retained code calls, is metadata in its shape and
never a slot. One inventory serves every consumer: the slots a body lists are the ones its
group binds, the ones its program links, and the only places a revision root of a described
function is looked for.

It holds no source position, no `cold` hint, no optimisation-only parameter facts, no
generated code variants, no physical byte size or offset that native layout resolves later,
and no number a producer chose: functions, blocks, values, addresses, calls, edges, switch arms,
rows and pool entries are numbered in one fixed traversal. That traversal starts at the
instance's function. It walks each function's blocks depth-first from the entry block, taking
a block's successors in the IR's own order: each instruction's failure target, in instruction
order, then the terminator's targets in operand order. It numbers each value at its definition,
and each row and function at its first reference. When a walk ends, the finalizers the walked
code needs and that are not yet numbered take the next numbers in ascending order of their
types' closure hashes, and those with bodies are walked in that order, until none is left.
The types of compact roots not yet numbered then take the next numbers in ascending order of
closure hash, and the roots are written sorted by type number and width, without repeats.
Blocks the entry does not reach are left out, with every call and reference in them. A body's
slots are its bodiless functions in number order.

Every enumeration is written with a tag from a fixed, append-only table, and every count and
index as unsigned LEB128. The body starts with the seven ASCII bytes `HIRBODY`, a zero byte
and its normal-form version as a 16-bit little-endian number. The current version is 4, whose bodies hold
retained code ([PROG-6](#prog-6-one-meaning)); a reader accepts version 4 only.
Each value has exactly one encoding: a reader refuses a LEB128 number with a redundant byte,
an unknown tag, a list out of its required order or with a repeat, or bytes left over at the
end. Decoding and re-encoding a body gives the same bytes. Its
**body hash** is `H("hirsel.code-body.v4", body)`. Positions and hints go to the body's
**annex** instead, in the body's order: each entry names a position of that body, and an annex
whose entries do not match its body's instructions, terminators and calls in number and kind is
malformed. The annex is hashed under `hirsel.code-annex.v1` and never enters an address.

**Groups.** Addresses are computed for each build from the whole reach of its program, after
the reach walk, from each instance's body hash and the ordered identities of its slots. They are
never stored with one instance's compiled form, since a callee's edit changes them without
changing that instance. The instances the program reaches are partitioned into the strongly
connected components of their slot graph, one **group** per component, so a function and the
functions it calls back form one group. A function that calls nothing back is a group of one.
Groups are hashed dependency-first: a group's hash is computed after the hash of every group it
binds. A group lists its members in ascending identity order, which never ties, so the order
does not depend on how they were found. For each member it gives the body hash and, for each of
the member's slots in order, a **binding**: `Internal(k)`, the group's member k;
`External(address)`; or `Builtin(n)`. The **group hash** is `H("hirsel.code-group.v1", group)`,
and member k's address is `H("hirsel.code-address.v1", group hash ‖ k)`, with `k` a 32-bit
little-endian number. An edit to a callee therefore changes its callers' addresses, through
their groups, but not their bodies.

**One address per instance.** In one program, every instance identity has exactly one address.
Every slot that names an identity is bound to that one address, every group in the program
agrees on it, and the program links the identity's one symbol to the code at that address. A
slot's signature equals the signature of the function it is bound to. A generated function that
two owners both define is one instance: both definitions have equal bodies and equal bindings,
and a producer that would give them different ones refuses the program. A set of groups in which
one identity has two addresses, or a slot's signature differs from its target's, is not a
program, and is refused before anything is assembled from it.

**Builtins.** A core function the compiler implements has no checked IR. Its address is
`H("hirsel.code-builtin.v1", e ‖ n)`, where `e` is its identity encoding and `n` its semantics
number, a 32-bit little-endian number that starts at 1 and rises whenever the compiler changes
what the builtin does. Every reachable use of such a function is a slot bound `Builtin(n)`:
a call the compiler lowers inline, as for `float.widen`, a function value of it, and a
descriptor of it alike. The lowered operations stay in the body as well. An engine implements
each builtin at the numbers it lists and refuses a program that binds any other; it never runs
a builtin under a number other than the one bound. Engine operations inside a body are written
as tags, and a change in the meaning of one gets a new tag, under the same rule.

So equal addresses mean the same checked code everywhere it reaches, on every engine and
build. The driver's reuse keys (pre-check keys, record and instance keys, image roots) decide
only whether stored work can be reused. They never enter a body, a group, an address, a
revision or a release recipe ([launch](launch.md#release-recipe-and-host-assembly)), so a
build from source and a build from an image give equal addresses. An address says nothing
about inputs: two runs of equal code with different arguments, or with different arrival
orders between units ([CONC-10](tasks.md#conc-10-event-order)), may differ. A cache keyed by
an address keys those as well. A cache whose outcomes depend on source sites, such as a
failure's reported site or a `Cause.site` value a program compares, also keys the site maps of
every annex the outcome can read and the site context of its caller, or does not cache those
outcomes. A cache of one function's results keys each function argument by its target's address
and its captured values, since two function values with equal identity bytes can run different
code.

The domains `hirsel.code-annex.v1`, `hirsel.code-group.v1`, `hirsel.code-address.v1`,
`hirsel.code-builtin.v1` and `hirsel.code-revision.v2` are added by the 2026-11-21 version of
this section, and `hirsel.code-body.v3` by the 2026-11-22 version, which retires
`hirsel.code-body.v2`; `hirsel.code-body.v4` by the 2026-11-25 version retires
`hirsel.code-body.v3`. A change to the normal form or to a group's encoding takes a new version
and a new domain. It never reuses an old one.

Version note (2026-11-21, code-address attacks): the traversal now follows every instruction's
failure target; slots are the functions retained code can run, and type hooks are metadata;
every use of a builtin, lowered inline or not, is a `Builtin(n)` slot over the identity
encoding; addresses are computed per build after the reach walk; compact-root types are ordered
by closure hash; the identity order, the identity encoding and the closure encoding are fixed;
generated functions are named by ordinal; and a program has one address per identity. Earlier
text took slots from every bodiless row, wrote lowered builtins as instructions only, and stored
an address at materialisation. No address had been published, so nothing migrates.

Version note (2026-11-22, Map index): normal form 3 replaces the linear-probe index of words
with the grouped index of PROG-11, adds the fast placement operations, and changes the meaning of
every private Map builtin that reads or writes a Map's block. Bodies of version 2 are refused,
not converted.

Version note (2026-11-22, Map index corrections): before any NF3 body was published, PROG-11
gained a strong placement key derived from the run key, and the strong-empty carrier mark. It
also gained one writer with a staged commit, promotion that abandons a partial rebuild, a
capacity bound of 2^59, unaligned group loads, the entry recipe's own field order, and the
private builtin table with its numbers. The body header is `HIRBODY`, a zero byte, then 3 as a
16-bit little-endian number, and both the writer and the reader use it.

Version note (2026-11-25, macos-s9-target-consts-2508): normal form 4 encodes retained code,
so a branch on a constant expression keeps only its taken arm, per target. The header carries
4, the body domain is `hirsel.code-body.v4`, and bodies of version 3 are refused, not
converted.

### PROG-11: Map index

A Map's physical layout, its index, its placement and its private builtins are an agreement
between the compiler and the engines under one normal-form version (PROG-10), kept in the
implementation document [Map index](../internal/map-index.md). They are not part of the
language. What a program sees of a Map is [SEM-5](semantics.md#sem-5-maps) and
[SEM-6](semantics.md#sem-6-iteration) only: iteration in insertion order, the charges of
[MEM-24](budgets.md#mem-24-logical-charges), and no hash word. The Map hash key is engine
state drawn from host entropy by every run, a replay included; it is not an input at the
replay point ([CAP-8](capabilities.md#cap-8-the-replay-point)) and no recording holds it.

### PROG-6: One meaning

A program has one meaning. Whether it is accepted, and what each accepted program does,
does not depend on the engine, the build mode, optimisation, flags, the host, or the order
in which definitions are checked. There are no conditional compilation forms and no
language switches. The one named input is the **target** of a build
([SEM-8](semantics.md#sem-8-determinism)), chosen with `--target`: it is the only flag that
reaches a program, and it does so only as below. The one exception is an implementation's resource refusal: a program
whose checking exceeds the implementation's resources (for example unbounded generic
instantiation, or instantiation past a depth or count) may be refused with a message that
names no rule; such a refusal is not a meaning of the program. Likewise, where a step bound
runs out and which admission an over-tight byte bound refuses are a build's resource
outcomes, fixed for that build but not the same on every engine
([SEM-8](semantics.md#sem-8-determinism)); they are not meanings of the program either.
A package's dependencies are fixed before the compiler runs ([packages](packages.md)): a
published library by its root, a workspace member by its captured files, and a toolchain
package by the toolchain, so `--locked` and `--frozen` decide only whether a command
proceeds.

Version note (2026-11-25, macos-s0-spec-2495; macOS decision D1; built by
macos-s9-target-consts-2508 except `target.unsupported`): the target
becomes a named input through core module `target`, whose contract is
[target](../../core/std/api/target.md). Nothing is conditionally compiled out of checking,
and the host stays invisible.

A program is checked identically for every target. The target of a build enters a program
only as the values of the constants of core module `target`, `target.os` and `target.arch`,
which the toolchain supplies (LANG-20). These rules hold:

- **Every arm is checked on every target.** Every arm of a branch on such a constant is
  parsed, resolved, typed and ownership-checked on every target, and `hirsel check`, which
  has no target, checks them all. Types never differ by target: a foreign module's interface
  is one text for all targets, and only its archive rows differ
  ([FFI-3](foreign.md#ffi-3-archives-are-admitted-by-their-pin-before-linking)).
- **A constant that depends on `target` is evaluated for every target the module names**,
  reserved ones included. A
  failure on any one is a refusal on all, so acceptance never depends on the target
  ([LANG-20](language.md#lang-20-constants)).
- **Retention is decided by form.** A `match` whose scrutinee, or an `if` whose condition,
  is a constant expression keeps only its taken arm in retained code (PROG-10). A constant
  expression is exactly what LANG-20 admits in a plain constant's initialiser (literals,
  operators, constructions, other constants and `bytes[T]`), so it has no call and no local. So `match target.os { ... }` keeps one
  arm, and `let o = target.os` followed by `match o { ... }` keeps both, on every engine.
  How much an engine folds never decides what is retained. The rule holds for every
  constant, not only `target`'s. Reach ([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused))
  and FFI-3 are computed over retained code.
- **`match` on `target.os` and `target.arch` is exhaustive**, as every `match` is, so a
  target added later is a refusal at every branching site that names no arm for it.
- **Reaching `target.unsupported(why)` refuses the build for that target.** It is never a
  run-time failure and is not a meaning of the program. The refusal names the target and
  the reach chain from the entry:
  `PROG-6: main reaches target.unsupported("batched submit needs io_uring") for target aarch64-apple-darwin, through app.run → queue.submit`.

```hirsel unbuilt
fn batched(n: u64) -> u64 { n + 1 }
fn submit(n: u64) -> u64 {
    match target.os {
        Linux => batched(n),
        Macos => target.unsupported("batched submit needs io_uring"),
    }
}
```

Built for `x86_64-unknown-linux-gnu`, `submit` retains the `Linux` arm only. Built for
`aarch64-apple-darwin`, a program whose entry reaches `submit` is refused with the PROG-6
line above, naming the chain. Both arms are typed for both targets: an arm that fails to type
is refused on every target, with the build's target playing no part.

```hirsel unbuilt
fn pick() -> u64 {
    let o = target.os
    match o {
        Linux => 1,
        Macos => target.unsupported("needs Linux"),          // refused: PROG-6, on every target
    }
}
```

`o` is a local, so the scrutinee is not a constant expression, both arms are retained, and
every build whose entry reaches `pick` reaches `target.unsupported`.

**Constant-form branches, exactly** (macos-s9-target-consts-2508). Only a `match` scrutinee and
an `if` condition qualify; `if let`, `while` and `loop` never do. The expression qualifies when
it evaluates as a constant on every target the module names. One that fails on any target, or
names a local, a call or a type parameter, keeps every arm and is refused for nothing here; so
does a `match` scrutinee that is an unsuffixed literal, which has no type of its own. Each arm
is decided on each target: an arm whose pattern the value matches is taken and the arms after
it are dropped, and an arm whose pattern no constant decides (a float pattern) stays an
ordinary run-time test. A guard stays a run-time test. `target.Os` and `target.Arch` are
`key`, so `target.arch == (Aarch64 : target.Arch)` is a constant expression.

`target` describes the artifact, never the machine a run is on: one binary has one value
on every run and every replay, so it passes no replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)) and is not an authority
([CAP-1](capabilities.md#cap-1-authority-is-explicit)). A run-time description of the host
is not provided. A value of `target.Os` is not a claim that Hirsel supports that target:
macOS stays in [reserved](reserved.md) until its native gate is green.
