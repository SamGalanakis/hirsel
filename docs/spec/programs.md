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

So `fn double(b: Bytes) -> Result[Bytes, Capacity]` in module `main` of package `app`
version 1 has the identity bytes `0x01` followed by
`app@1/main/double(core@1/core/List[u8])->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]`.
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
implements has its revision in any program. A store, registry or tape that keeps only
identity bytes matches by identity alone, so a changed body under an unchanged identity is
not detected; one that keeps the revision as well detects it.

Version note (2026-11-21, code addresses): the revision was a hash over the reach's
`(identity, IR)` rows under `hirsel.code-revision.v1`. It is now derived from the address,
under `hirsel.code-revision.v2`. Every revision value, and so every release digest
([launch](launch.md#release-manifest)), changes once; its size and every format that carries
it do not. No migration is provided before 1.0.

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
index as unsigned LEB128. The body starts with its normal-form version, which is 1. Each value
has exactly one encoding: a reader refuses bytes that are not that encoding, such as a LEB128
number with a redundant byte, an unknown tag, a list out of its required order or with a
repeat, or bytes left over at the end. Decoding and re-encoding a body gives the same bytes. Its
**body hash** is `H("hirsel.code-body.v1", body)`. Positions and hints go to the body's
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

The domains `hirsel.code-body.v1`, `hirsel.code-annex.v1`, `hirsel.code-group.v1`,
`hirsel.code-address.v1`, `hirsel.code-builtin.v1` and `hirsel.code-revision.v2` are added by
the 2026-11-21 version of this section. A change to the normal form or to a group's encoding
takes a new version and a new domain. It never reuses an old one.

Version note (2026-11-21, code-address attacks): the traversal now follows every instruction's
failure target; slots are the functions retained code can run, and type hooks are metadata;
every use of a builtin, lowered inline or not, is a `Builtin(n)` slot over the identity
encoding; addresses are computed per build after the reach walk; compact-root types are ordered
by closure hash; the identity order, the identity encoding and the closure encoding are fixed;
generated functions are named by ordinal; and a program has one address per identity. Earlier
text took slots from every bodiless row, wrote lowered builtins as instructions only, and stored
an address at materialisation. No address had been published, so nothing migrates.

### PROG-6: One meaning

A program has one meaning. Whether it is accepted, and what each accepted program does,
does not depend on the engine, the build mode, optimisation, flags, the host, or the order
in which definitions are checked. There are no conditional compilation forms and no
language switches. The one exception is an implementation's resource refusal: a program
whose checking exceeds the implementation's resources (for example unbounded generic
instantiation, or instantiation past a depth or count) may be refused with a message that
names no rule; such a refusal is not a meaning of the program.
