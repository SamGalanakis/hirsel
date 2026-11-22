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
  and no spelling: parameter names and the generics list are left out; the region
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
  keyword, an identifier, a number, a definition identity or a root name `^i` or `#i`, and
  no list has a trailing comma.

So `fn double(b: Bytes) -> Bytes` in module `main` of package `app`
version 1 has the identity bytes `0x01` followed by
`app@1/main/double(core@1/core/List[u8])->core@1/core/List[u8]`.
And `fn edges[in b, in a](s: Region, r: Region, t: Text in a, u: Text in b) -> u64 where
a: r, r: s` there, or the same header with `[in a, in b]`, has `0x01` followed by
`app@1/main/edges(core@1/core/Region,core@1/core/Region,core@1/core/Text in ^0,core@1/core/Text in ^1)->u64 where ^0:#0,^0:#1,#1:#0`.
A change to a function's package, version, module path, name, parameter modes or types,
result, anchors, `waits` or outlives clauses changes its identity; a change to its
parameter names, to the order of its region generics or to its body does not. A rename or a move to another module is a new
identity, and no alias connects the two. A later encoding may add an instance's closed type
arguments and dictionaries after `D`; version 1 has none.

`code.revision(s)` returns `Option[View[Bytes]] in s`: the function's **revision**, 32 bytes
that name the code a call through `s` runs. It is derived from the function's **code
address** (PROG-10), which covers the checked core IR of every instance (PROG-5) the function
reaches, std's and core's included, taken through each instance's slots (PROG-10): the calls,
function values and descriptors its retained code makes, the core functions the compiler
implements that it uses, and the finalizers its values need, among them the functions the
checker selects from types, such as a codec maker the code calls. The revision
is `H("hirsel.code-revision.v2", address)`, where the address is that of the function's
revision root: the function itself, the adapted function for a `$callable` adapter, and the
builtin address (PROG-10) for a core function the compiler implements, such as `float.widen`.
Equal revisions mean equal addresses, so Hirsel has one identity for code; the revision only
keeps the bytes a program holds apart from the keys of a code store. Nothing else enters: not
the backend, the runtime, the target, the optimisation level, the root limit, source
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
fn d() -> u64 { let _ = code.describe(same); 0 }                             // refused: PROG-9, same is generic
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

**Body.** An instance's **body** is the normal form of its checked IR: its own function, the
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
and its normal-form version as a 16-bit little-endian number. The current version is 3, which
places Map keys in the grouped index of [PROG-11](#prog-11-map-index); a reader accepts version 3
only.
Each value has exactly one encoding: a reader refuses a LEB128 number with a redundant byte,
an unknown tag, a list out of its required order or with a repeat, or bytes left over at the
end. Decoding and re-encoding a body gives the same bytes. Its
**body hash** is `H("hirsel.code-body.v3", body)`. Positions and hints go to the body's
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
`hirsel.code-body.v2`. A change to the normal form or to a group's encoding takes a new version
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

### PROG-11: Map index

This section fixes how a Map stores and finds its entries, so that `core.map`, code the compiler
emits for a Map and every engine agree on one layout under one normal-form version (PROG-10).
None of it is observable. No value, admission, refund, step, cleanup order or iteration order
depends on it, on the run's Map key, on the keys derived from it, or on which placement mode a
Map is in ([SEM-5](semantics.md#sem-5-maps)).

**Carrier.** A Map's carrier is `{data, used, cap}`: the block, the number of initialized entry
slots, and the capacity. While `cap` is 0 the Map has no block and `used` is 0. Then `data` is not
an address: it is 0 for an ordinary empty Map and 1 for a **strong-empty** Map, one whose index
would be strong once it has one. No engine dereferences, frees or charges `data` while `cap` is
0. `map.new` makes `{0, 0, 0}`. `len`, `get`, `update`, `remove`, `reserve(0)`, cleanup and
iteration of a Map with `cap` 0 read only `cap`. A move copies the carrier, so it keeps the
strong-empty mark. The first allocation reads the mark: the new header's strong bit is set when
the Map was strong-empty or its key type is composite (**Classes**).

**Entries are authoritative.** A Map's block holds `cap` entry slots, then a 32-byte index
header, then its index. An entry is the own tuple `(meta: u64, key: K, value: V)`, laid out by
the one entry recipe of the native layout (MEM-13): its fields are ordered by alignment, then
size, then position, so the meta word is not always first. For `Map[u64, u64]` the meta is at
offset 0, the key at 8 and the value at 16, with stride 24. For `Map[String, u64]` the key is at
0, the meta at 24 and the value at 32, with stride 40. Inline construction, relocation, borrowed
and consuming iteration and every cleanup use that one recipe.

Slots below `used` are the **initialized prefix**. Nothing reads a slot at or above `used`. In
the prefix, an entry is **live** when its whole meta word is not 0, and **dead** when it is 0.
Order, membership, iteration, cleanup, compaction, growth, cloning and consuming iteration read
only the live entries of the prefix, in slot order, and clean each key before its value
([SEM-6](semantics.md#sem-6-iteration)). The index is derived from them, and a rebuild
reconstructs it from them alone.

**Index.** The index has B = max(16, next_pow2(2 × cap)) buckets in G = B / 16 groups. Group g
is 16 control bytes followed by 16 entry indices. An entry index is a zero-based slot number. It
is a `u32` while cap ≤ 2^32, so slot 2^32 − 1 still fits, and a `u64` above that. A group is
therefore 80 or 144 bytes, its stride. Group g's control bytes begin at
cap × S + 32 + g × stride, where S is the entry stride. Bucket j is group j / 16, position
j mod 16. Offsets are computed in 64 bits even where entry indices are `u32`: at cap = 2^32, B
is 2^33.

Group offsets need not be 16-byte aligned. With cap = 5 in `Map[u64, u64]`, the entries end at
120, the header spans 120–152 and group 0 begins at 152, which is 8 mod 16. Every engine reads a
group's 16 control bytes with an unaligned 16-byte load, or byte by byte, and never reads past
them, the last group's included. The block's physical size is
cap × S + 32 + G × stride bytes, computed with checked arithmetic. A size that does not fit in 64 bits is
handled as an allocation the host cannot make. That rule is
independent of the logical charge below.

A control byte is one of:

| byte | meaning |
|---|---|
| `0xFF` | EMPTY: never used since the last rebuild, or erased in a group that holds an EMPTY byte |
| `0x80` | DELETED: erased in a group that held no EMPTY byte |
| `0x00`–`0x7F` | FULL: a tag; the bucket's entry index names a live entry |

When a block is allocated, every control byte is set to EMPTY before any probe. A zeroed page
would read as FULL with tag 0. Every live entry has exactly one FULL byte, and every FULL byte
names a distinct live entry. Non-EMPTY bytes number at most `used`, which is at most cap, which
is at most B / 2, so at least half of every index is EMPTY.

The header holds four words: the live count, the group mask G − 1, the mode, and zero. Mode bit
0 is set in **strong** mode and clear in **fast** mode. Bit 1 is set when entry indices are
`u64`. The width bit follows the capacity. The strong bit is kept independently of it, so growth
that changes the width keeps the mode, and so does a clone that narrows it.

**Capacity bound.** A Map's capacity is at most 2^59, so B ≤ 2^60 and G ≤ 2^56, and bit 56 of a
placement word never selects a group. A grown capacity is clamped to 2^59, and growth or
`reserve` whose need is above 2^59 is refused with `Capacity`, as an admission the current limit cannot make
([MEM-22](budgets.md#mem-22-the-capacity-value)). `requested` is then the charge of the needed
capacity, saturated at 2^64 − 1. An entry charges at least 16 bytes, so 2^59 entries would
charge 2^63 bytes. The bound therefore refuses nothing that a limit below 2^63 bytes admits, and
it is the same on every target.

**Classes.** A key type's class decides how it is placed in fast mode, and it is fixed by the
type:

- The **word class** holds the key types carried as one non-float scalar: the integer types,
  `bool`, `char`, and records and tuples carried as one such scalar. Its word x is the 64-bit
  word that `map.hash` reads for the key. A signed value is truncated to its unsigned type of
  the same width, then zero-extended, so `-1i8` gives x = `0xff`. A `bool` gives 0 or 1, and a
  `char` its code point.
- The **byte class** holds the non-region record types with exactly one field, where that
  field is a `u8` sequence: `String`, `Bytes`, `List[u8]`, and any user record of that same
  shape. A record whose one field is itself such a record, such as
  `type Name { s: String }`, is not in the byte class.
- Every other key type is **composite**: tuples and records of several fields, enums, and
  wrappers that are not in either class.

The key hash glue and the header use this one predicate. A Map of a composite key type is
strong from its first index, and its code is specialized straight to strong placement, with no
fast operation and no mode test. A Map of a word-class or byte-class key type gets a fast first
index unless it is strong-empty.

**Placement word.** Each key has a 64-bit placement word p under the Map's mode. Its tag is
p >> 57, and its home group is p & (G − 1). A live entry's meta is p | 2^56, which is never 0. A
rebuild takes each entry's tag and home group from its meta. Two placement words are compared
by tag and then by the key itself, never as whole meta words.

In fast mode p is the key's **fast placement word**. Write fold(a, b) for the low 64 bits of the
128-bit unsigned product a × b, XORed with its high 64 bits. Write C for `0x9e3779b97f4a7c15`, and
s0 and s1 for the run's fast salts.

- **Word class.** p = fold(x ^ s0, C).
- **Byte class.** Take bytes b[0..n). Start with z = s0 and i = 0. While n − i > 16, set
  z = fold(z ^ le(i, 8), s1 ^ le(i + 8, 8)) and add 16 to i. Here le(j, w) is the
  min(w, n − j) bytes from b[j], read as a little-endian number with the bytes above zero (0
  when j ≥ n). With r = n − i, let w0 = le(i, 8), and w1 = le(i + 8, r − 8) when r > 8, else 0.
  Then t = fold(z ^ w0, (s1 ^ w1) + n mod 2^64) and p = fold(t, C). An engine reads no byte
  outside b[0..n).

In strong mode p is the key's **strong placement word**: the SipHash-1-3, under the strong key
k_s, of the same structural stream that `map.hash` reads for the key (SEM-5). `map.hash` itself
is unchanged. It is the SipHash-1-3 of that stream under the run key, and it does not place
keys.

**Derived keys.** The run's Map key K is acquired or restored at the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)). Three things are derived from it there,
before the first Map operation, with raw SipHash-1-3 under K over ASCII labels, with no length
prefix or padding:

- s0 is the SipHash of the 17 bytes `hirsel.map.fast.0`.
- s1 is the SipHash of `hirsel.map.fast.1`.
- k_s is the 16 bytes le64(a) ‖ le64(b), where a and b are the SipHash of the 19 bytes
  `hirsel.map.strong.0` and `hirsel.map.strong.1`.

They belong to the run, and every task, child and context uses the run's values. A recording
holds only K. Every `map.hash` stream is a whole number of 8-byte words, so no stream equals a
label. Under the PRF assumption for SipHash, k_s, s0, s1 and every `map.hash` output are
therefore independent.

Known answers, with K = `00 01 … 0f`:

| value | answer |
|---|---|
| s0 | `c93577b642b675d3` |
| s1 | `216bdc5358d9c4d5` |
| k_s | `1e7ee605b33f7078d171d0ade994210e` |
| fast word of 0 | `175d5dfbd09e75c7` |
| fast word of 1 | `b08ad43c5024c9bd` |
| fast word of 2^64 − 1 | `d71c61fbd21d7517` |
| fast word of `-1i8` (x = `0xff`) | `488dc5e5cae947bd` |
| fast bytes of the empty sequence | `c601c2084ab71e17` |
| fast bytes of `abcdefgh` | `3251c98ae37d4dbf` |
| fast bytes of `abcdefghi` | `dd8ba4778c150f32` |
| fast bytes of `abcdefghijklmnop` | `441cb7420c58fe33` |
| fast bytes of `abcdefghijklmnopq` | `8d14bb88cfa906fe` |
| strong word of 0 | `e8b70260e432308d` |
| strong word of 1 | `5cd87fd53c86d83a` |
| strong word of 2^64 − 1 | `78ee0e4e0cad36d2` |
| strong word of `-1i8` | `fbe4122ec2e54220` |
| strong bytes of `a` | `9b4f3fea3a32bb74` |
| strong bytes of `abcdefghi` | `687d6af59de6e421` |
| strong bytes of `abcdefghijklmnopq` | `4427af8d97cca0dd` |

The byte class's 8- and 16-byte answers are its boundaries: r = 8 has no w1, and n = 16 takes no
chunk.

**Probing.** From the home group g0, a probe visits the groups g(i+1) = (g(i) + i + 1) & (G − 1).
Because G is a power of two, these are a permutation of the groups in G visits. The group number
is wrapped before its address is computed. In fast mode a probe visits at most min(8, G)
groups: 128 buckets, or 16 × G when G < 8. In strong mode it has no cap. Within a group, buckets
are taken in ascending order.

- A **lookup** compares the key of every bucket whose byte equals p's tag, in probe order, and
  finds the first that is equal ([SEM-7](semantics.md#sem-7-equality-and-order)). Every
  equal-tag bucket of a group is compared before the group's EMPTY bytes end the search. A
  lookup ends absent after a group that holds an EMPTY byte, or at the cap.
- An **insertion probe** is a lookup that also notes the first EMPTY or DELETED bucket it passes.
  It does not stop at that bucket: it goes on looking for an equal key until the lookup would
  end. Its result is **Found**(entry, bucket), **Vacant**(bucket) or, in fast mode only,
  **Crowded**, when no EMPTY or DELETED bucket lies within the cap. A DELETED bucket noted at the
  cap gives Vacant only after the key has failed to match in every group of the window. The
  result carries p. A bucket or p in a result is valid only for the index it was computed on:
  compaction, growth, a rebuild or a mode change discards it.
- A **placement** takes the first EMPTY or DELETED bucket in probe order. In fast mode it is
  Crowded when there is none within the cap. In strong mode the half-empty index always has one.

Filling an EMPTY bucket adds one to the non-EMPTY bytes and to `used`. Reusing a DELETED bucket
adds one to `used` only. An entry always takes the next slot, `used`. A reused bucket never
reuses an old entry's place in the order.

A fast Map places every key within the cap. So a key that an insertion probe does not find
within the cap is absent, and once the Map is strong a probe again for that key gives Vacant.

**Writers.** Only one bodiless function writes the index: `install(m, b, p, i)` writes p's tag
to bucket b's control byte, writes i at the header's width as the bucket's entry index, and
stores p | 2^56 as slot i's meta. It accepts only a resolved vacancy. The only callers are three
core.map functions:

- `place_entry(m, p, i)` places slot i under the Map's mode, for rebuilds.
- `commit(m, r)` installs a Vacant result r for slot `used`, then calls `advance`.
- `remove` erases. It is the only function that does.

Allocation and clearing initialize controls to EMPTY. `kill` and `take_entry` set a dead
slot's meta to 0. Relocation and clone copy the existing meta as part of moving or copying
the live entry. Only `install` publishes a live entry in the index.

**Insertion.** `map.insert`, `update_or_insert` and the compiler's in-place insertion all follow
these steps:

1. **Empty Map.** If cap is 0, the Map has no index to probe and the key is absent. Make room
   (step 3). A Map with no block never promotes because it has zero groups.
2. **Probe.** Run the insertion probe for the key, under the Map's mode.
   - On **Found**, install the incoming value in place, keep the stored key and its place in the
     order, clean the old value and then drop the incoming key (SEM-5). Nothing is admitted.
   - On **Vacant** with `used` < cap, commit (step 4).
   - On **Crowded** with `used` < cap, promote, then probe again under the strong word. The
     result is Vacant. Commit.
3. **Make room.** If `used` = cap, compact or grow, as **Growth and compaction** below says.
   That discards the probe's bucket and word. Then compute p under the Map's mode, which may
   now be strong, and place. If that placement is Crowded, promote and place again. The
   placement then gives a bucket to commit.
4. **Commit.** Write the key and value into slot `used` (`push_entry`) if they are not already
   there. Then `install(m, b, p, used)` and `advance`: `used` and the live count each grow by 1. `update_or_insert` then calls
   its callback exactly once, on the entry it found or committed.

**Staging and custody** ([MEM-31](memory.md#mem-31-construction-in-place)). The compiler's
in-place insertion builds the key, and then the value, directly in slot `used` when that slot
exists before the call (`used` < cap). A built field there is **staged**. Staged fields lie
outside the initialized prefix, so cleanup, iteration and rebuild never read them. They stay
outside it until both operands are complete and the call's step has been taken. Their owner is
the call's pending operand, and each carries an already-in-place flag. Promotion moves no entry
and allocates nothing, so staged fields survive a Crowded probe and its promotion. The single
re-probe then commits them where they are.

When the slot does not exist (`used` = cap), the operands are built in local places and moved
in at commit, after room is made (MEM-31's "any other value"). Making room can move or reallocate
the slots, so the compiler never stages into them.

If the value expression fails after the key is staged, only the staged key is cleaned. If it
fails on the Found path, the previous value is restored. A refused growth leaves the Map, its
old contents and its old owner unchanged. The operands are then still the caller's, and are
cleaned in [FAIL-9](failure.md#fail-9-pending-results)'s order. The compiler's code branches on
the probe and calls `commit`, `promote` and the ordinary insertion's make-room path. It never
writes a control byte, an index or a meta itself.

**Erasure.** `map.remove` finds its entry and erases the bucket before the entry dies. The byte
becomes EMPTY when its group holds an EMPTY byte, and DELETED otherwise. Then the entry's meta
becomes 0 and the live count drops by one, and the key is cleaned and the value returned
(SEM-5). Erasure never adds a non-EMPTY byte. A consuming loop moves entries out in slot order
and sets each meta to 0 without touching the index. The Map has been taken, so its index is
unreachable and may be stale. Its cleanup reads only the prefix: if the loop is interrupted, the
entries it has not reached are cleaned in order, each key before its value.

**Rebuild.** A rebuild sets every control byte to EMPTY, then calls `place_entry` for each live
entry of the prefix, in slot order, with p taken from its meta. A fast rebuild hashes nothing.
If a fast placement during a rebuild is Crowded, the rebuild stops and promotes. The promotion
itself re-places every live entry. The interrupted rebuild is not resumed, and it returns as
complete to whatever called it. A clone, growth, compaction or `reserve` then goes on from a
complete strong index. An insertion that was pending probes again (step 3).

**Promotion.** A fast Map promotes when an insertion probe, or a placement during an insertion
or a rebuild, is Crowded. Promotion:

1. abandons the whole index, including any partial rebuild;
2. sets the strong bit;
3. sets every control byte to EMPTY;
4. for each live entry of the prefix, in slot order, computes its strong word p, then places and
   installs it, which stores p | 2^56 as its meta.

Every live entry is re-placed exactly once, and no entry is published twice. Promotion allocates
nothing, admits nothing, moves no entry and takes no step
([MEM-30](budgets.md#mem-30-step-bounds)). Its time is one clear of G groups, one pass over the
`used` slots, and one strong hash of every live key's bytes. G can be far larger than the live
count after `reserve`.

Promotion happens at most once in a Map's life. Nothing clears the strong bit. Growth,
compaction, `reserve` and removal keep it. A clone of a strong Map is strong, whatever its index
width. If the clone is empty, it is strong-empty.

**Growth and compaction.** A Map's logical charge for capacity c is
charge(c) = ceil8(c × (size(K) + size(V) + 16)), as MEM-24 says. It is computed from the logical
sizes, never from the entry stride or the block size. Growth, relocation, free and cleanup all
use this one operand.

- **Make room when `used` = cap.** If cap ≠ 0 and live ≤ cap / 2, compact in place: move the live
  entries to the front in order, set `used` to live, and rebuild. Compaction admits nothing.
  Otherwise grow to max(4, 2 × cap).
- **Growth from c to c'.** Admit charge(c') from the current limit before refunding charge(c) to
  the old record (MEM-26). Copy the live entries in order with their metas, initialize the new
  index to EMPTY, and rebuild under the Map's mode. A refusal raises or returns `Capacity` with
  `requested` = charge(c') and the limit's `available`, and leaves the old block, its contents,
  its charge and its owner unchanged.
- **`reserve(n)` and `try_reserve(n)`.** If live + n > cap, grow to
  max(live + n, 2 × cap, 4), subject to the capacity bound. Otherwise, if `used` + n > cap,
  compact. That holds even when more than half the entries are live. Otherwise do nothing.
- **Nothing shrinks** on removal.

Worked charges for `Map[u64, u64]`, where charge(c) = c × 32:

- 4 → 8 admits 256 and refunds 128.
- With all 4 slots used, remove two keys and insert an absent one. Live is 2, which is at most
  2, so the Map compacts. The two survivors keep their order, the new entry goes last, and
  nothing is charged.
- Remove only one key instead and insert. Live is 3, which is more than 2, so the Map grows to 8.

The block's physical size is
engine layout and is not charged (MEM-19, MEM-28):

| capacity | block bytes (S = 24, 80-byte groups) | logical charge |
|---|---|---|
| 1 (a one-entry clone) | 24 + 32 + 80 = 136 | 32 |
| 4 | 96 + 32 + 80 = 208 | 128 |
| 5 | 120 + 32 + 80 = 232 | 160 |
| 8 | 192 + 32 + 80 = 304 | 256 |

**Clone** ([MEM-17](memory.md#mem-17-clone)). Cloning a Map with live count 0 makes a Map with
cap 0 that admits nothing. It is strong-empty when the source is strong or strong-empty, and
ordinary otherwise.

Otherwise the clone allocates capacity = live, charging charge(live), and takes the source's
mode. Then it walks the source's live entries in order. For each, it clones the key and then the
value. `copy_entry` moves both fields into the next slot, copies the source's meta (valid
because the modes agree), and advances `used` and the live count. This makes the complete pair
available to cleanup before cloning the next one. After the last entry it rebuilds once.

A fast clone's rebuild may promote. Promotion re-places every entry under the strong word, so a
fast meta is never left in a strong Map. If a clone fails partway, the copy's cleanup cleans the
copied prefix in order and frees the block. The clone never probes the source, and it never
probes the copy before its rebuild.

**Cost.** Before promotion, every individual search visits at most 8 groups and compares at most
128 candidate keys, whatever the keys. Hits, misses and overwrites can keep taking the last group
of the window without promoting, and reusable DELETED buckets allow further fast insertions.
That is bounded work, not a small average. In strong mode a search has the expected cost of a
half-full table under a keyed PRF, for keys chosen without the run key. That is an expectation
over keys, not a bound on one fixed run.

**Private operations.** `core.map` reaches a block only through bodiless private functions. The
compiler lowers each inline from one layout definition, on every engine. The table gives each
function's semantics number (PROG-10, Builtins) in normal form 3:

| function | number | meaning |
|---|---|---|
| `place(k)` | 1 (new) | The fast placement word: engine operation `MapPlaceWord` (runtime tag 60) for the word class, and `MapPlaceBytes` (61) for the byte class; never lowered for a composite type. The word form is a pure function of k and the salts, and may be hoisted over the mode test. The byte form reads b[0..n) and the salts: it may be reused across Maps only while no write can reach those bytes, and is not computed eagerly in strong mode. Only its salt loads are invariant for the task. |
| `strong(k)` | 1 (new) | The strong placement word: `MapStrongWord` (62), `MapStrongBytes` (63), or for a composite key `MapStrongStart` (64) followed by the unchanged `MapHashAddWord` (44), `MapHashAddBytes` (45) and `MapHashFinish` (46). |
| `fast_class(m)` | 1 (new) | A constant from the Map's key type: 0 for composite, 1 for word, 2 for byte. This lets word placement precede the mode test while byte placement stays on the fast branch. |
| `mode(m)`, `group_mask(m)` | 1 (new) | The header's strong bit and group mask. Not called when cap is 0. |
| `strong_empty(m)`, `set_strong_empty(mut m)` | 1 (new) | Read and set the carrier's strong-empty mark (cap 0 only). |
| `group_match(m, g, t)` | 1 (new) | A 16-bit mask whose bit j is set when group g's byte j equals t. |
| `group_empty(m, g)`, `group_vacant(m, g)` | 1 (new) | The same mask for EMPTY bytes, and for EMPTY or DELETED bytes. |
| `bucket_entry(m, b)` | 1 (new) | Bucket b's entry index. |
| `install(mut m, b, p, i)` | 1 (new) | See **Writers**. |
| `erase(mut m, b)` | 1 (new) | Erase bucket b; called only by `remove`. |
| `clear_index(mut m)`, `set_strong(mut m)` | 1 (new) | Set every control byte to EMPTY; set the strong bit. |
| `slot_meta(m, i)` | 1 (new) | Slot i's meta. |
| `hash(k)` | 2 (kept) | `map.hash`, as in version 2. NF3's `core.map` does not call it. |
| `capacity(m)`, `slots(m)` | 1 (kept) | Carrier reads, unchanged. |
| `new` | 1 (kept) | Makes `{0, 0, 0}`, as in version 2. |
| `advance(mut m)` | 1 (new) | Add 1 to `used` and to the live count; called only by `commit`. |
| `compact`, `kill`, `len`, `regrow`, `slot_key`, `slot_value`, `swap_value`, `take_entry`, `with_value` | 2 | Same names, new block layout. |
| `push_entry(mut m, take k, take v)` | 2 | Move k and v into slot `used`; it no longer takes a hash, and does not change `used`. |
| `copy_entry(mut m, meta, take k, take v)` | 1 (new) | Clone's completed pair and copied meta, appended to the initialized prefix before rebuilding. |
| `try_regrow` | 2 | A first explicit entry. Under version 2 it had none, and so ran at 1. |

The functions `slot_hash`, `slot_live`, `index_len`, `index_get` and `index_set` are gone.
Source-defined `core.map` functions (`get`, `insert`, `probe_insert`, `place_entry`, `commit`,
`promote`, `rebuild`, `clone` and the rest) have bodies, and so take ordinary body hashes.

Each mask is exact on every target. SSE2, NEON and the portable word-at-a-time form give equal
masks, with no false positives, at lanes 0 and 15 and on either byte order. The probe result is a
core-private enum with the cases `Found(entry, bucket)`, `Vacant(bucket)` and `Crowded`, and it
carries p.

The runtime tags 60–64 are appended to the engine-operation table. Each engine lists them among
the operations it supports, so admission of a published NF3 body checks them like any other tag.
`MapPlaceWord` and `MapStrongWord` take a `u64`. `MapPlaceBytes` and `MapStrongBytes` take a
pointer and a length. Each of those four returns a `u64`. `MapStrongStart` initializes a hash
state, as `MapHashStart` does, from k_s.

### PROG-6: One meaning

A program has one meaning. Whether it is accepted, and what each accepted program does,
does not depend on the engine, the build mode, optimisation, flags, the host, or the order
in which definitions are checked. There are no conditional compilation forms and no
language switches. The one exception is an implementation's resource refusal: a program
whose checking exceeds the implementation's resources (for example unbounded generic
instantiation, or instantiation past a depth or count) may be refused with a message that
names no rule; such a refusal is not a meaning of the program.
A package's dependencies are fixed before the compiler runs ([packages](packages.md)): a
published library by its root, a workspace member by its captured files, and a toolchain
package by the toolchain, so `--locked` and `--frozen` decide only whether a command
proceeds.
