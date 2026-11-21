# Foreign calls

A package may call native code that it ships as a pinned static archive. The package keeps
the native code behind a **foreign module**. This is a module of signatures only, and no
other package can name it. The package's ordinary modules wrap those signatures in a safe
API, and that API is what other packages use. A call into native code takes a `Foreign[I]`
capability that only an entry grant makes. Arguments and results are fixed-width scalars,
call-scoped byte loans and opaque handles that the run checks before every call. A call
either runs on the host stack or, when declared `waits`, on a helper thread while its task
is parked. A program that can reach native code has no recording, no replay and no reused
test pass.

The rules are FFI-1 to FFI-9. The [`sqlite` package](#the-sqlite-package) is the acceptance
consumer. These rules are unbuilt design. Every fence on this page is marked `hirsel unbuilt` and stays
documentation, because a fence cannot carry a foreign manifest; the laws' refusal cases check
the rules it shows.

## Trust

### FFI-1: Hirsel trusts audited native code and does not contain it

A `Foreign[I]` grant gives the process's whole authority to the archive behind interface `I`.
Hirsel promises exactly three things about foreign code, and nothing else. First, before
every foreign call the run checks on the Hirsel side the loans, handles and releases that
FFI-5 and FFI-6 define, and it refuses a call that breaks them without entering native code.
Second, a pinned archive is admitted only when its bytes are exactly the audited bytes
(FFI-3). Third, a program that can reach native code is never recorded, replayed or given a
reused test pass (FFI-8). The **native contract** is the callee's side of FFI-5 to FFI-7:
it does not keep a loaned pointer after it returns, it reads and writes only inside the
ranges it is lent, it does not unwind, `longjmp` or otherwise transfer control across the
call boundary, it is bounded and does not block when it is not `waits`, an issued `waits`
call finishes without waiting for any foreign call that has not yet been issued (FFI-7), a
pointer it returns as a new handle is a new owner that the kind's release ends exactly once
and is neither a pointer into an argument nor an object that another handle owns (FFI-6), it
does not call back into Hirsel, and it reaches the outside world only as its package's audit
says it does.
A callee that breaks the native contract is outside every guarantee of this specification:
the fixed goals ([index](index.md#fixed-goals)) hold for a run only while its foreign
callees keep that contract. A process abort, a signal, a stack overflow or memory corruption
in native code is **environmental termination**. It is termination by the host
([FAIL-15](failure.md#fail-15-run-stops)): Hirsel promises no diagnostic, no cause, no
cleanup and no finalizer for it, and the exit status is the host's. The archive pin is an
admission rule for exactly the code that was audited. It says nothing about what that code
does.

**No foreign code without a grant.** Native code runs only when a foreign call enters it,
and a foreign call needs a `Foreign[I]` value, which only an entry grant makes (FFI-4). FFI-3
admits no archive that could run code any other way: no member holds an initializer,
finalizer, constructor or destructor section or an indirect-function resolver, and no symbol
it defines can stand in for one of the runtime's, the generated code's or the system
libraries'. So a process with no `Foreign` grant runs no foreign code, before `main`, at exit,
in describe or preview, or in a parameterless test whose reach holds a foreign function.

**The audit and the operator.** The audit unit of a foreign module is its wrapper, its
foreign module's headers (the ABI text of FFI-3), the archive of each target row and
everything that archive links; the pin names exactly the audited bytes. A `Foreign` grant is
explicit at launch: it is given on the command that starts the run, `--grant Foreign=D` for
`hirsel run`, `hirsel test` and `hirsel deploy`, and never only by a package's configuration
or a recipe (FFI-4). Before any process runs with the grant, the launcher writes, for each
`Foreign` grant, one line to stderr (for `hirsel deploy`, to the deploy's output):
`foreign: D: target T, archive sha256 E, recipe R`, with `E` the selected archive's pin and
`R` the recipe's root (launch). The operator who passes the grant has seen what it trusts.

The reasons are these. A raw pointer in a same-process callee can read, write or keep
anything in the address space. A helper thread in the same process shares the process's
fate. No Hirsel-side check can contain such a callee, so this chapter promises only what the
Hirsel side enforces. In-process foreign code is for audited, pinned packages only. A
contained boundary, one restricted worker per interface or package or a Wasm boundary, is
required before any unaudited extension is accepted, and it is a separate later design
([reserved](reserved.md), R5). Moving only the `waits` calls out of process is not that
boundary: the ABI lets one package use a handle from `waits` and host-stack calls alike, so a
statement prepared on a helper could be read by a host-stack call on the calling thread, and neither process could use the other's C pointers or byte loans.

## Declaring foreign code

### FFI-2: Foreign modules

A **foreign module** is a module that its package manifest marks `foreign`. It contains only
bodyless `pub fn` headers, written as a core function's are
([LANG-5](syntax.md#lang-5-grammar)). It has no type, constant, body, attribute or private
item, and each function's first parameter has type `Foreign[I]` for the one interface `I`
of that module (FFI-4). A function of a foreign module is a **foreign function**. It is named
only by modules of the package that owns the foreign module, and only as the callee of a
call expression. Naming it from another package, as a function value, or as the argument of
`code.describe` is refused here, at the name. The owning package's other modules are its
**wrapper**, and they are the only way any other package reaches the native code.

The package's manifest ([packages](packages.md#2-hirseltoml)) marks a module foreign with a
`[foreign."<module path>"]` table. The module must be formed by exactly one `[sources]` file,
and no `[tests.sources]` file may join it.

```text
[sources]
"sqlite.hirsel" = "sqlite"
"native.hirsel" = "native"

[foreign.native]
symbols = "hsq_"
archives = [
  { target = "x86_64-unknown-linux-gnu",
    file = "native/x86_64-unknown-linux-gnu/libhirsel_sqlite.a",
    sha256 = "<64 lower-case hex digits>" },
]
```

- `symbols` is the module's **symbol prefix**. It is 1 to 32 bytes of ASCII letters, digits
  and `_`, and it starts with a letter. Foreign function `f` is the C symbol whose name is the
  prefix followed by `f`, so `native.open_memory` above is `hsq_open_memory`. Two foreign
  modules in one program with the same prefix are refused here, at the second prefix, with
  the first one as the causal site.
- `archives` is the **archive table**. It has one row per full target triple, for example
  `x86_64-unknown-linux-gnu`. The libc is part of the triple, so a later
  `x86_64-unknown-linux-musl` row is a new row with its own archive and digest, and the table's
  format does not change. A target is 1 to 64 bytes of `[a-z0-9_.-]`. Two rows with one target
  are refused, at the second row. The table's order in the manifest means nothing: the rows
  are taken, encoded and listed in diagnostics in ascending order of target bytewise. `file`
  follows the path rules of the packages chapter, ends in `.a`, and is read and captured
  through the same reading rule, for every row whatever the build's target
  ([packages](packages.md#1-canonical-library-content)). One file is named by one row of the
  package, and the source and archive paths of a package form one case-folded tree. `sha256` is
  the SHA-256 of the file's raw bytes.
- A `[foreign]` key that names no module of `[sources]`, a module formed by more than one
  file, or a module that a test-only file joins is refused as a bad field
  ([packages](packages.md#9-diagnostics)), for example
  `hirsel: sqlite/hirsel.toml: foreign.native: module native is formed by 2 files`.

A published library's content `C` holds each foreign module's prefix, its archive table and
the archives' bytes ([packages](packages.md#1-canonical-library-content)), so its root pins
exactly the audited archives. The pin covers every byte the linker reads, because FFI-3 admits
only self-contained archives whose members are inside those bytes. Until the built `hirsel` reads `hirsel.toml`, the same table sits
in `manifest.json` as a `"foreign": {"symbols", "archives": [{"target", "file", "sha256"}]}`
object on the module's source entry, with the same rules.

```hirsel unbuilt
// native.hirsel, module native of package sqlite, marked foreign
pub fn open_memory(f: Foreign[sqlite.Sqlite], mut e: Bytes) -> Option[foreign.Ptr[sqlite.DbHandle]] waits
pub fn exec(f: Foreign[sqlite.Sqlite], db: foreign.Ptr[sqlite.DbHandle], sql: Bytes, mut e: Bytes) -> i32 waits
pub fn close(f: Foreign[sqlite.Sqlite], take db: foreign.Ptr[sqlite.DbHandle]) -> i32 waits
```
```hirsel unbuilt
// native.hirsel, marked foreign
pub fn version(f: Foreign[sqlite.Sqlite]) -> i32 { 3 }          // refused: FFI-2, a foreign function has no body
```
```hirsel unbuilt
// in package app, which depends on sqlite
fn v(f: Foreign[sqlite.Sqlite]) -> i32 { sqlite.native.version(f) }   // refused: FFI-2, native is private to sqlite
```
```hirsel unbuilt
// in module sqlite of package sqlite
fn later(f: Foreign[Sqlite]) -> fn(Foreign[Sqlite]) -> i32 { native.version }   // refused: FFI-2, a foreign function is only called
```

A foreign function's `waits` is as written ([LANG-8](language.md#lang-8-signatures)). A call
of one takes one step, as a core function's call does, whatever the native code does
([MEM-30](budgets.md#mem-30-step-bounds)).

### FFI-3: Archives are admitted by their pin, before linking

A build whose checked reach (FFI-8) holds a foreign function of module `M` needs `M`'s
archive for the build's target. Before the linker runs, and before any object is written
for linking, the driver checks each such module, in ascending order of module definition
identity, and refuses the build at the first failed check:

| check | refused with (site: the archive row, or the `[foreign]` table when no row exists) |
|---|---|
| the table has a row for the build's full target | `FFI-3: P has no archive for target T; its archives are for T1, T2` (all the targets, in ascending order; `none` when the table is empty) |
| its SHA-256 equals the pin | `FFI-3: archive F of P for T does not match its pin: expected sha256 E, found sha256 O` |
| it is not a thin archive: it does not start with `!<thin>\n` | `FFI-3: archive F of P for T is a thin archive; its members are not inside the pinned bytes` |
| it is a self-contained `ar` archive with a symbol index: it starts with `!<arch>\n`, every member's header and data lie inside its bytes, its first member is a GNU or BSD symbol index, and no member name refers to another file | `FFI-3: archive F of P for T is not an indexed ar archive` |
| every member other than the symbol index and the long-name table is an ELF relocatable object (`ET_REL`) whose class, byte order, machine and OS ABI are T's; LLVM bitcode, a nested archive, a linker script or any other content is refused, so nothing in the archive makes the linker look up another file or load a plug-in | `FFI-3: archive F of P for T: member N is not a relocatable object for T` |
| for every foreign function of `M` that the build reaches, its symbol S is defined in one member's own symbol table as a global function (`STB_GLOBAL`, `STT_FUNC`, default visibility, in a section of that member, not common), and the index's entry for S names that member | `FFI-3: archive F of P for T does not define S, declared at SITE` |
| every member holds only admitted object features: each section's type is `SHT_NULL`, `SHT_PROGBITS`, `SHT_NOBITS`, `SHT_SYMTAB`, `SHT_STRTAB`, `SHT_RELA`, `SHT_REL`, `SHT_NOTE`, `SHT_GROUP`, `SHT_LLVM_ADDRSIG`, and for x86-64 `SHT_X86_64_UNWIND` or for AArch64 `SHT_AARCH64_ATTRIBUTES`; no section is named `.init`, `.fini`, `.ctors`, `.dtors`, `.init_array`, `.fini_array` or `.preinit_array`, or one of these followed by `.` and more, and no section's name is a C identifier, for which the linker would define `__start_` and `__stop_` symbols that other code iterates; a note section is `.note.GNU-stack` or `.note.gnu.property`; the member has a `.note.GNU-stack` without `SHF_EXECINSTR`; no symbol has type `STT_GNU_IFUNC`; and no relocation is `R_X86_64_IRELATIVE` or `R_AARCH64_IRELATIVE` | `FFI-3: archive F of P for T: member N has X, which is not an admitted object feature`, `X` one of `section NAME of type TYPE`, `no .note.GNU-stack`, `an executable stack`, `indirect function S`, `an IRELATIVE relocation` (the first in section, then symbol, then relocation order) |
| every symbol a member defines with binding `STB_GLOBAL` or `STB_WEAK` starts with `M`'s symbol prefix | `FFI-3: archive F of P for T: member N defines S, which does not start with the prefix X` |
| its members hold, in all, exactly one section named `.hirsel_abi`, of type `SHT_PROGBITS`, and for every reached foreign function of `M` that section holds exactly one line for its symbol S, equal to the line of S in `M`'s ABI text (below) | `FFI-3: archive F of P for T has no ABI text` (none or more than one section), or `FFI-3: archive F of P for T: the ABI of S is L, but its header declares A, declared at SITE` (`L` the archive's line or `missing`) |
| no global or weak symbol it defines is also defined by another admitted archive, by the runtime, by the program's generated objects or by another link input of the image (the libc and the system libraries the toolchain links, read from their symbol tables and dynamic symbol tables) | `FFI-3: symbol S is defined by archive F1 of P1 and by G` (`G` an archive, `the runtime`, `the generated code` or `link input L`) |

`P` is the module's definition path (`sqlite@1/native`), `T` the target triple, `F` the
row's `file` and `N` the member's name. The digest is checked first. So a file that is not an
archive but carries a wrong pin is reported as a wrong pin, and the driver parses no bytes that
do not match the pin. The member and symbol checks read the members' own ELF headers and symbol
tables, never the index alone, so an index that names a symbol no member defines is refused.
Two modules whose selected rows have one pin select one archive: it is checked, staged and
linked once, and the symbol check compares distinct archives only.

The object-feature, prefix and collision checks are what FFI-1's "no foreign code without a
grant" rests on. An initializer, finalizer, constructor or destructor section, or an indirect
function's resolver, would run archive code when the image loads or exits, with no call and no
grant; a symbol outside the prefix could stand in for `malloc` or a runtime entry. The set of
features is closed: a section type, note or relocation this table does not admit is refused,
not ignored. An archive adds no link input: its undefined symbols resolve only against the
image's other inputs, and the driver passes the linker no library, search path or option for
it. A member that needs another library fails to link; it never pulls one in.

**ABI text.** A foreign module's **ABI text** is derived from its headers alone: one line per
foreign function, in ascending order of C symbol bytewise, each
`S MODE (P1, P2, ...) -> RESULT` and a line feed. `MODE` is `waits` or `stack`. The
parameters are those after the `Foreign` one, in order: a scalar as its type name (`u8` ..
`f64`), `bytes` for an immutable loan, `mut bytes` for an exclusive one, `handle K` for a
handle in use, `take K` for a release, and `dir` for slice 2's `Dir`, where `K` is the handle
kind's definition identity (FFI-6). With no parameters the list is `()`. `RESULT` is `()`, a
scalar's type name, or `new K` for `Option[foreign.Ptr[K]]`. So `sqlite`'s close is the line
`hsq_close waits (take sqlite@1/sqlite/DbHandle) -> i32`. `hirsel inspect --foreign-header P`
prints, for the foreign module `P`, a C header with each function's prototype as FFI-5's
table gives it, and a macro `HIRSEL_ABI_TEXT` whose expansion is one `static`, `__attribute__((used))`
definition, placed in section `.hirsel_abi`, of a `const char` array holding the module's ABI text with no
terminating zero. The adapter includes that header, so its C compiler checks every definition
against the prototype the Hirsel header implies, and it expands `HIRSEL_ABI_TEXT` once. The
FFI-3 check then binds the audited object to the headers that call it: a header changed from
`u64` to `bytes`, or `handle` to `take`, no longer matches the archive it was audited with.
In a build from a code store the check reads each function's header text from its foreign
leaf, and `F` in these diagnostics is `object E`, `E` the archive's SHA-256. Every check of
this table runs wherever an archive enters a link: a build from source, `hirsel build
--recipe` and a host's assembly.

The driver links the bytes it checked, and nothing else. The bytes are taken from the capture
the package resolution made ([packages](packages.md#1-canonical-library-content)): a member's
captured file, the archive bytes of a published package's verified `C`, or, for a build from a
code store, the store's object after its hash is checked. No row's path is opened again. The
driver writes the bytes into a memory file it creates for this build and seals it against every
write, shrink and grow (on Linux a `memfd` with `F_SEAL_WRITE`, `F_SEAL_SHRINK`, `F_SEAL_GROW`
and `F_SEAL_SEAL`). It runs the checks above on the sealed file, hashes it, and gives the
linker that file alone, by its descriptor path. So no process, the build user's own included,
can change what the linker reads after the check. A host that cannot seal a file refuses a
foreign-reaching build with `FFI-3: this host cannot seal staged archives`, before writing
any object.

The **release recipe** ([launch](launch.md#release-recipe-and-host-assembly)) gains two parts.
The **foreign modules** part has one record, a **foreign leaf**, per foreign module that the
entry's closure reaches, in strictly ascending order of the module's identity encoding. A
leaf holds its module's definition path as a text, with the components escaped as in PROG-9,
its symbol prefix as a text, its whole archive
table, and its reached functions. The table is the number of rows and then each row in
ascending order of target, as the target text, the 32-byte pin and the byte length as a
number. The rows, without the count before them, are exactly the bytes FFI-9's archive-set
digest is taken over. The reached
functions are their number and then each function, in strictly ascending order of identity
encoding, as that encoding and its PROG-9 header text `S` as a text. So a reader with no
source recomputes every foreign function's address from the leaf, and selects the row for
the recipe's target from it. The **foreign archives** part lists the distinct pins selected for
the recipe's target by its leaves and by every entry `Foreign` binding, each as its SHA-256
and byte length, in strictly ascending order of hash. A binding selects the pinned archive
of its interface whether or not the entry reaches a call to it. Only reach places functions
in leaves. An archive's bytes are an object of the code store, named
by that SHA-256, and they are transferred, reserved, collected and reassembled as the
recipe's other objects are. With these parts the recipe is version 2, and its root is
`R = H("hirsel.release-recipe.v2", recipe)`. The version-1 domain is never reused for the new
encoding. A program that reaches no foreign function has an empty foreign modules part.
Its foreign archives part holds the pins its entry grants select, if any, and it still uses
version 2. Recipe admission walks the leaves as launch's admission steps 6 and 7 say. So
`hirsel build --recipe R` links exactly the pinned bytes the addresses select, and two
programs that differ only in an archive have different roots.

### FFI-4: The Foreign capability

`Foreign[I]` is a core capability ([CAP-1](capabilities.md#cap-1-authority-is-explicit)). It is
plain, `shareable` and `send`, and 8 bytes, and it designates the authority to call the
foreign functions of interface `I`. An **interface** is a `pub opaque` record with no fields,
declared in a non-foreign module of a package, that exactly one foreign module of that package
names as the type argument of its functions' first parameter. `Foreign[T]` is refused here, at
the type, when `T` is not an interface. Two foreign modules that name one interface are refused
here, at the second module's first function, with the first module as the causal site.

A `Foreign[I]` value is made only by an entry grant ([CAP-4](capabilities.md#cap-4-entry-grants)).
It is not made by attenuation or by virtual construction, and it narrows to nothing. A
parameter of `main` or of a test takes the grant `Foreign=D`, where `D` is the interface's
definition identity ([PROG-9](programs.md#prog-9-code-descriptors)'s `package@major/module/../name`).
On the command line it is written `--grant Foreign=sqlite@1/sqlite/Sqlite`, and in a package's
test configuration it is the grant string `"Foreign=sqlite@1/sqlite/Sqlite"`. A run whose
`Foreign[I]` parameter has no grant naming `I` does not start, and it reports
`not granted: Foreign[D]` ([PROG-3](programs.md#prog-3-entry)). A `Foreign` grant whose `D`
names no interface of the program is refused before the run as an unknown grant. In the
release recipe a grant binding of kind `Foreign` holds `D` as its one value, followed by
the SHA-256 and byte length of its interface's archive for the recipe target. The pin
is in the recipe's foreign archives part even when the entry reaches no call to that
interface. The manifest's `grants` list shows the binding as the string `Foreign=D`.

The grant is explicit at launch (FFI-1). A test configuration's `"Foreign=D"` string says
which tests take it, and `hirsel test` gives it to them only when its own command line also
holds `--grant Foreign=D`; without that, each such test is not started and is reported failed
with `not granted: Foreign[D]: pass --grant Foreign=D to hirsel test`, and the other tests run.
`hirsel deploy R` of a recipe with a `Foreign` binding is refused before admission, with
`deploy: refused: the recipe grants Foreign=D; pass --grant Foreign=D`, unless the request
holds `--grant Foreign=D` for each such binding. Its `foreign:` lines (FFI-1) are written
after admission and before any child of the attempt runs, preview and prepare included, and
those children receive the same `Foreign` bindings as the serving child, which the operator
has approved by that flag. `hirsel run` writes its lines after the build and before the run.

The capability is on every call. Each foreign function's first parameter is `Foreign[I]`, so
no native code runs without a capability argument (CAP-1, [CAP-3](capabilities.md#cap-3-authority-is-received)).
The argument is erased at the C boundary and passes nothing to the callee. A wrapper that
keeps a connection open stores the capability in the resource, and the resource's finalizer
uses it from there ([FAIL-11](failure.md#fail-11-resources)). The report that this chapter
follows also proposed a link-policy opt-in by the root package on top of the capability. It is
not adopted. The grant is already an explicit, per-interface decision by whoever launches the
run, and the archive pin already binds the bytes. A second switch would gate nothing that
these two leave open.

```hirsel unbuilt
pub fn main(out: Stdout, db: Foreign[sqlite.Sqlite]) -> Result[(), sqlite.SqliteError] waits {
  let mut c = sqlite.open_memory(db)?
  sqlite.exec(mut c, "create table t (x integer)")?
  sqlite.close(take c)
}
```
```hirsel unbuilt
type Mine {}
fn f(x: Foreign[Mine]) -> u64 { 0 }                              // refused: FFI-4, Mine is not an interface
```

## The ABI

### FFI-5: Argument and result types

A foreign function's parameters after the first, and its result, have only these forms. Every
other type or mode is refused here, at the parameter or the result.

| Hirsel | C (System V for the target) | rule |
|---|---|---|
| `u8` `u16` `u32` `u64` `i8` `i16` `i32` `i64` | `uint8_t` .. `int64_t` | by value |
| `f32`, `f64` | `float`, `double` | by value; a NaN result becomes SEM-2's one NaN |
| result `()` or no result | `void` | |
| `b: Bytes` | `const uint8_t *b_ptr, uint64_t b_len` | an **immutable loan** |
| `mut b: Bytes` | `uint8_t *b_ptr, uint64_t b_len` | an **exclusive loan** |
| `p: foreign.Ptr[K]` | `void *p` | a handle of kind `K` in use for the call (FFI-6) |
| `take p: foreign.Ptr[K]` | `void *p` | a **release** of the handle (FFI-6) |
| result `Option[foreign.Ptr[K]]` | `void *` | `NULL` is `None`, and any other pointer is a new handle of kind `K` (FFI-6) |

A `Bytes` parameter becomes two C parameters in place, the pointer first. A parameter of type
`Bytes` also takes a `View[Bytes]` ([OWN-7](ownership.md#own-7-parameter-modes)), such as a
`list.slice` or a `string.as_bytes`, and passes that view's range. `bool`, `char`, `String`,
other lists, records, variants, callables, other capabilities, `View` results, `bytes`
parameters, a `foreign.Ptr` result without `Option`, and a `mut` handle are all refused.
A function with a `take` parameter is a **release** of its kind. It has exactly the
`Foreign` parameter and that one `take` parameter, and its result is `()` or a scalar, so the
run can call it itself (FFI-6). A release that also has a handle result, a second `take` or
any other parameter is refused here, at the function, so no call both ends and makes an owner.
Each kind `K` that a function of the module makes, by an `Option[foreign.Ptr[K]]` result, has
exactly one release; a made kind with none, or a kind with two, is refused at the kind's first
use in the module. Each `f32` or `f64` result that is a NaN is replaced by the one NaN bit pattern of
[SEM-2](semantics.md#sem-2-floats) on both backends, and the run restores Hirsel's
floating-point environment (round to nearest, no traps, no flush to zero) on the thread before
Hirsel code runs on it again, after every foreign call on the host stack or on a helper.

**Loans.** `ptr` addresses the first byte of the loaned range, and `len` is its **length**:
the number of initialized elements, `list.len`, never the capacity. For an exclusive loan the
callee may read and write exactly bytes `0 .. len`. It cannot change the length, and the
list's spare capacity is never part of the range. So a callee that fills a buffer is lent a
list whose length the wrapper has first set, with `list.fill`, to the number of bytes it may
write. **Zero length**: when `len` is 0, `ptr` is not `NULL`. It is the address of a static
byte of the runtime, and the callee may neither read nor write through it. So `(ptr, 0)`
always means an empty range, never an absent one. This keeps, for example, SQLite's empty blob
apart from SQL `NULL`.

**What stays pinned.** From the call's entry until the C function returns, the loaned storage
does not move, grow, shrink or get freed. The places it comes from are held as the ownership
rules already hold a call's arguments: a view argument's place is frozen
([OWN-10](ownership.md#own-10-freezes)), a `mut` argument's place is lent, and overlapping
arguments are refused ([OWN-11](ownership.md#own-11-call-arguments)). So the callee never sees
an exclusive range that overlaps another range of the same call. Handles in use stay in flight
(FFI-6), and the `Foreign` argument's designation stays live. For a `waits` call this holds
until the helper's C call returns, and the issuing task stays parked until then (FFI-7). The
loans are passed without copying. Copying into worker-owned buffers, which the report
proposed, is not needed. The only other code that can reach the lent storage while the task
is parked is another task reading through an `Arc` payload, which is immutable
([MEM-15](memory.md#mem-15-arc)), and tasks share nothing mutable
([CONC-12](tasks.md#conc-12-no-shared-mutable-state)). So an immutable loan can only be read
alongside, and nothing else can touch an exclusive one. A loan ends when the C function
returns. Keeping its pointer past that point breaks the native contract (FFI-1).

```hirsel unbuilt
// in the wrapper: copy a column's bytes out through an exclusive loan
fn column(f: Foreign[Sqlite], s: foreign.Ptr[StmtHandle], i: u32, mut out: Bytes) -> i32 waits {
  let mut e: Bytes = error_buffer()
  let n = native.column_len(f, s, i, mut e)       // -1 on failure, the error in e
  if n < 0 { return 1 }
  let start = list.len(out)
  list.fill(mut out, int.narrow(n), 0)
  native.column_copy(f, s, i, mut out, start, mut e)   // C writes bytes start .. start + n
}
```
```hirsel unbuilt
// native.hirsel, marked foreign
pub fn name(f: Foreign[sqlite.Sqlite], s: String) -> i32       // refused: FFI-5, String is not a foreign type
```

### FFI-6: Handles

`foreign.Ptr[K]` is the core type of a **handle** of **kind** `K`. A kind is a `pub opaque`
record with no fields, declared in a non-foreign module of the package that owns the foreign
module, and named as `foreign.Ptr`'s type argument by exactly one interface's foreign module.
`foreign.Ptr[T]` with `T` not such a record is refused here, at the type, and so is a kind
named by the foreign modules of two interfaces, at the second. A database and a statement are
two kinds, so a wrapper that swaps their fields is refused by the type checker, and the run
checks the kind again at every call. The type is plain, `send`, and 8 bytes, and it is not
`shareable`, `key` or `codec`. Its value is a run-local ordinal, never the C pointer. The run
keeps a **handle table** that maps each live ordinal to its C pointer, its interface, its kind
and its state, and a **pointer index** of the C pointers of every live handle and every
in-flight handle that is not being released, across all the run's interfaces and kinds.
Ordinals start at 1 and are never reused within a run. A table that would pass `2^64 − 1`
ordinals is the run stop `HostHandles` ([FAIL-15](failure.md#fail-15-run-stops)). The table
and the index are host memory of the run, outside every limit as a provider's host memory is
([CAP-5](capabilities.md#cap-5-provider-operations)), and they are freed when the run ends,
an embedded run's included. When the host refuses them memory, the run stops with
`HostMemory` before any C of that call runs.

**Where a handle may live.** A type **holds a raw handle** when `foreign.Ptr` appears in it at
any depth once its type arguments are substituted: in a field, a case, a type argument, a
tuple element or the captures of a closure, but not inside an **owning resource**, a resource
type that this paragraph lets hold a handle as a field, which owns it. So `foreign.Ptr[K]` and
`Option[foreign.Ptr[K]]` hold one, and `Db` and `List[Db]` do not. This is OWN-2's raw-handle category
([OWN-2](ownership.md#own-2-type-summaries)). The owning package may hold handles in the
locals of any of its functions; in the parameters and results of its functions that are not
visible outside the package, as itself or inside `Option`, `Result` or a tuple; and as a field
of one of its `resource type`s that declares `drop` and is not visible outside the package or
is `pub opaque`. Every other position of a type that holds a raw handle is refused here, at the
type, whether the type is written, inferred or substituted: a field of a type that is not such
a resource; a `pub` resource that is not `opaque`, whose fields a client could project; a type
argument of anything other than `Option`, `Result` or a tuple, including one a generic call
infers; a constant; the signature of a `pub` function of a non-foreign module; a capture of a
closure, owning or not; and every position in another package. So outside its package a
handle exists only inside an opaque resource, which no client can project, copy or capture
out of, and that resource is cleaned exactly once ([FAIL-11](failure.md#fail-11-resources)).
The type is copyable, so a wrapper's locals may hold one handle in two places. A copy is
checked like the original, because the check reads the table, not the value. Dropping a copy
has no effect: a handle ends only by a release. A wrapper keeps each handle in at most one
resource field, because that resource is the one that releases it (below).

**States and the call protocol.** An ordinal is **live**, **in flight** or **consumed**. A new
handle is live. A consumed ordinal is any ordinal below the next one that is not in the table,
so no tombstone is kept. Every call, on the host stack or on a helper, takes the same three
steps on the table, each one atomic under the run's table lock, which is never held while C
runs:

1. **Preflight.** Before any C runs, the run checks every argument in parameter order: the
   handle checks below, and slice 2's `Dir` check. On the first that fails it raises the
   failure at the call and changes nothing: no handle changes state, no slot is reserved, no
   C runs and no helper is taken. When every check passes, every handle argument becomes in
   flight and, when the result is a handle, a table slot is reserved, at once.
2. **Issue.** C is entered. A `take` argument's pointer leaves the pointer index at that moment.
3. **Delivery.** When C returns, a `take` argument becomes consumed and leaves the table, and
   every other handle argument becomes live again. A release consumes its handle whatever the C
   function returns. A pointer result that is `NULL` is `None`, and the reserved slot is
   returned. A pointer result found in the pointer index fails the call with
   `Fail("foreign pointer already held")`: no handle is made, the pointer is not released and
   the slot is returned. Any other pointer result becomes a new live handle of the result's
   kind in the reserved slot, and joins the index. Everything C wrote, loans included, is
   visible to the task when it resumes: delivery is a release-acquire handoff between the
   thread that ran C and the task.

| the handle argument | failure |
|---|---|
| is consumed | `Fail("foreign handle released")` |
| is another argument of this call too, or is in flight in another call and is not a `take` argument | `Fail("foreign handle in use")` |
| is of another interface or another kind than the parameter's | `Fail("foreign handle of another kind")` |

`Fail(text)` is the cause ([FAIL-2](failure.md#fail-2-causes)). So passing one handle twice to
one call fails at that call, and releasing a handle twice or using it after its release fails
without the C function being entered. A `take` argument that is in flight in another call does
not fail: the release waits until that call is delivered, and then checks it again, so it
finds it live or consumed. The pointer index catches a C function that hands back a pointer
some live handle already owns, under any interface. A released pointer leaves the index at
issue, before C frees the object, so a new object that another helper makes at the recycled
address is a new owner and is accepted. The reserved slot means recording a new handle cannot
fail.

**Release.** A release never fails or retries on the Hirsel side. An adapter whose C close can
refuse, for example `sqlite3_close`, which returns `SQLITE_BUSY` while statements remain, binds
a function that always ends the object instead (for SQLite, the control record of the
[`sqlite` package](#the-sqlite-package)). So one release rule covers every handle. Once
reached, a release is issued: its entry, its wait for a helper and its wait for an in-flight
use are not cancellation points ([CONC-7](tasks.md#conc-7-cancellation-points)), it has no
loan, so nothing is allocated for it, and only its handle checks can stop it before C. Every
live handle is an **obligation** that exactly one release discharges, and the run, not a
value, keeps it. It is discharged by the first of:

- an explicit call of the kind's release;
- the finalizer of the resource whose field holds the handle. When that finalizer ends, by
  returning, by failing or abandoned on entry ([FAIL-12](failure.md#fail-12-finalizer-failure)),
  the run calls the kind's release itself for each handle in the resource's fields that is
  still live, in descending order of ordinal, shielded, with the result discarded. The task's
  cleanup waits for each C return. This is the resource's mechanical cleanup, and it takes no
  steps;
- **run-end release**: when a run ends in an orderly way, after its last task has ended, the
  run calls the kind's release for every handle still live, in descending order of ordinal,
  so a child made after its parent is released first, with the results discarded.

So an allocation refusal, a cancellation, a failed resource literal, an exhausted step bound
or a use in flight never loses the release; at worst it moves it to the resource's cleanup or
to the run's end. A run stop and termination by the host run no release: the process's exit
is the only one then. Hirsel guarantees that a handle reaches a release at most once and is
never used after one.

```hirsel unbuilt
// in module sqlite of package sqlite
pub opaque type DbHandle {}
pub opaque resource type Db: send { f: Foreign[Sqlite], p: foreign.Ptr[DbHandle] }
  drop(d) { let _ = native.close(d.f, d.p) }
```
```hirsel unbuilt
// in module sqlite of package sqlite
pub type Leak { p: foreign.Ptr[DbHandle] }                        // refused: FFI-6, a handle field outside a resource
```
```hirsel unbuilt
// in module sqlite of package sqlite
pub fn raw(d: Db) -> foreign.Ptr[DbHandle] { d.p }                // refused: FFI-6, a handle in a pub signature
```

### FFI-7: Host stack and `waits`

A foreign function not declared `waits` runs on the native stack of the thread that runs the
calling task, with at least 1 MiB of that stack free at entry. It holds that thread until it
returns, so the native contract requires it to be bounded and not to block. It is not a
cancellation point.

A foreign function declared `waits` runs on a **helper**, a thread of the run's helper pool,
with at least 1 MiB of native stack, while the issuing task is parked. The pool has 4
helpers. They are created before `main` runs when the program's reach holds a `waits` foreign
function. When the host refuses one, the run does not start, and it reports
`foreign helpers: R` ([PROG-3](programs.md#prog-3-entry)). A `waits` call goes through these
steps in order:

1. **Entry.** It is a cancellation point, as a provider operation's entry is
   ([CONC-7](tasks.md#conc-7-cancellation-points)), unless the function is a release. A
   pending request is observed first, and the call fails with `Cancelled` with nothing else
   done.
2. **Preflight.** FFI-6's preflight runs: the checks, then the handle arguments in flight and
   the result's slot reserved.
3. **Admission.** The call takes a free helper. When none is free, the task joins the pool's
   one queue, which is first in, first out: a freed helper goes to the task that has waited
   longest. Every moment a call other than a release waits there is a cancellation point. If
   it observes cancellation, it leaves the queue, its handles return to live, its slot is
   returned, and it fails with `Cancelled`, and no C has run. A release waits uncancelled.
   Nothing is charged to any limit.
4. **Issue.** The helper enters the C function, at one atomic moment: a cancellation observed
   before it rolls the whole call back as in step 3, for every argument at once, and none
   after it reaches the call. A request made during it stays pending and is observed at the
   task's next cancellation point. No deadline applies.
5. **Delivery.** When C returns, FFI-6's delivery runs, the helper returns to the pool at
   once, whether or not the task has been scheduled again, and the task resumes with the
   result.

A call on the host stack takes steps 2, 4 and 5 on its own thread, with the same preflight and
the same reserved slot, and it has no entry cancellation point and no queue.

A task parked at steps 4 and 5 is in an **issued** foreign call. It is waiting on the host, so
it is an outstanding operation under [FAIL-15](failure.md#fail-15-run-stops)'s `Deadlock`
rule. A task at step 3, or a release waiting for an in-flight use, waits only on issued
calls, so the run is never deadlocked while one waits. **Progress** is a clause of the native
contract (FFI-1): an issued call finishes without waiting for any foreign call that has not
been issued, its own package's releases included. So four issued calls never wait for a
fifth, and the queue always moves. A package whose native library needs one call to wait for
another unissued one, such as a lock released only by a later call, cannot be audited for v1.
SQLite meets it: two calls on one connection wait only for each other's issued C on the
connection's mutex, its busy handler never sleeps, and its file locks never block (slice 2).
While a helper runs, the other tasks keep running. **Shutdown with a call in flight**: a run
ends only after its tasks end, and a task parked in a `waits` call ends only after delivery.
Its own cancellation and cleanup wait for C as well
([CONC-8](tasks.md#conc-8-abandoned-handles)), so an orderly end never abandons an issued
call, and it waits as long as the host takes to return it, without bound. A run stop, a second
SIGTERM or SIGINT, a test timeout or any other termination by the host ends the process with
the call still running. No release runs then, and what the native code had not yet written is
lost. This is the same outcome as a killed process.

**Native work is accounted for separately.** A foreign call takes the one step of its call
expression. The native code's time takes no steps, a step bound does not bound it, and a slice
end cannot fall inside it ([MEM-30](budgets.md#mem-30-step-bounds)). The memory the native code
allocates is host memory outside every limit, as the host memory behind a provider operation
is ([CAP-5](capabilities.md#cap-5-provider-operations)). A native allocation failure is a
result of the C function, and the wrapper maps it to its own error. A program that needs
native time or memory bounded relies on its package's audited bounds. Hirsel charges for neither.

## Outside replay

### FFI-8: A program that reaches foreign code is never recorded, replayed or reused

An entry is **foreign-reaching** when its checked reach holds a foreign function. The reach
here is the slot closure of [PROG-10](programs.md#prog-10-code-addresses): every call, function
value and descriptor in retained code, and the finalizer of every type that a value in that
code can hold. So indirect calls through a Hirsel function value, generic dictionaries,
closures and implicit finalizers all count. A foreign function is itself only ever called
(FFI-2), so the closure reaches every foreign function that the entry's code could run. The
property is conservative: a call on a path that never runs still counts. The driver keeps the
**foreign summary**, the set of reached foreign functions, in the validated reach result. It
holds the summary both for a fresh walk and for a reach-record hit, and the record's key covers
the summary, so a hit never returns a stale one. Every decision below is taken from that
summary before any build output is written and before any native code runs. Each refusal names
the reached foreign function `G` with the least definition identity, and its site is one call
of `G` in the reach: in the instance of least identity that calls `G`, its first call of `G`
in canonical traversal order, which is source order of the call's byte offset. `E` is the
entry. From a recipe, with no source, the summary is the set of functions of the recipe's
foreign leaves (FFI-3), and the site is `E`'s.

| command | outcome |
|---|---|
| `hirsel build --recording` and `hirsel run --record PATH` | the build is refused: `FFI-8: a recording build cannot reach foreign code: E reaches G` |
| `hirsel replay app --tape PATH`, with or without `--diagnose` | exits 2, before reading the tape, with `replay: refused: FFI-8: a recording build cannot reach foreign code: E reaches G` |
| `hirsel test --replay`: each selected foreign-reaching test | fails before its live recording half is built, with that test's line ending in `refused: FFI-8: a recording build cannot reach foreign code: E reaches G`. The other tests run as usual. |
| `hirsel host --record DIRECTORY`: a deploy whose recipe has a foreign leaf | refused at the recipe's admission, after admission step 1 and before any object is fetched, assembled, published, previewed or launched, with `deploy: refused: FFI-8: a recording build cannot reach foreign code: E reaches G`. The same check runs on every assembly with the recording runtime, and on a cached assembly record before it is used. |
| `hirsel replay --state HOST_STATE --tape TAPE` | reads only the tape's routing header, resolves the recording ID through its pin to the assembly record, and checks that record's recipe. A recipe with a foreign leaf exits 2 with `replay: refused: FFI-8: …` before any record of the tape is read and before the image runs. Since a recording host never assembles such a recipe, this refusal means the host state was changed outside the host. |
| `hirsel test`: a foreign-reaching test | gets no pass key. It neither looks up nor publishes a cached pass ([PROG-4](programs.md#prog-4-tests)). It always runs, and it is never reported as reused. |

There is no recording form that leaves foreign calls out. A recording without a foreign call's
result and its handle transitions could be replayed to `Sealed` while having changed what the
run computed, so a recording build of a foreign-reaching program is refused outright. A test
that reaches foreign code is not fixed by its program and configuration alone, because the
native code may observe anything, so a pass is never reused. Archive pins do not make native
results deterministic. A later modelled exception, a checked provider contract that records a
foreign call's inputs, returned and mutated bytes, handle transitions, issue and delivery, is
reserved ([reserved](reserved.md), R5).

### FFI-9: Foreign code in addresses and revisions

A foreign function has no checked IR. Its code address ([PROG-10](programs.md#prog-10-code-addresses))
is `H("hirsel.code-foreign.v1", e ‖ s ‖ x ‖ a)`. `e` is its identity encoding, and `s` its
PROG-9 header text `S` as a text. `x` is its C symbol as a text. `a` is the module's
**archive-set digest**: SHA-256 over the module's archive table, with no row count, each row
written in ascending order of target as the target text (its length as an unsigned LEB128
number, then its bytes), the 32-byte pin and the file's byte length as an unsigned
LEB128 number. A slot that names a foreign function is bound `External` to that address. So
the address, and each caller's address and revision ([PROG-9](programs.md#prog-9-code-descriptors)),
is the same on every target, and it changes when any target's archive changes, when a row is
added or when the header or prefix changes. A revision names the code a call runs on every
target, so adding a musl row changes the revisions of the functions that reach that module.
The domain `hirsel.code-foreign.v1` is added by this chapter.

## The `sqlite` package

The acceptance consumer is the package `sqlite@1`, at `core/programs/sqlite`. Its foreign
module `native` binds an **adapter**. This is one C file, `hirsel_sqlite.c`, compiled together
with the SQLite amalgamation into one static archive per target. The adapter is the audited
code: it calls the SQLite API and gives each call the FFI-5 shape. The interface is
`pub opaque type Sqlite {}` in module `sqlite`, and the kinds are `DbHandle` and `StmtHandle`
there. The first archive row is `x86_64-unknown-linux-gnu`.

**The pinned build.** The archive is rebuilt from the text below and from nothing else, and
the package's directory holds the adapter source, the build script `build.sh` that runs these
steps, and the pinned archive:

- **SQLite.** Release 3.54.1 (`SQLITE_SOURCE_ID` `2026-10-16 14:45:20
  555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`), as the amalgamation
  `https://sqlite.org/2026/sqlite-amalgamation-3540100.zip`, whose SHA3-256 is
  `92281c86e2957f9b9305890f9ac04204caa7b677cc868dc326d942689df28e30`, as sqlite.org
  publishes it. `build.sh` downloads it, checks that digest before unpacking, and uses its
  `sqlite3.c` and `sqlite3.h`. The amalgamation is not vendored.
- **Tools.** The repository's pinned LLVM 23.1.5 tree (`tools/buck2/bootstrap_llvm.py`): its
  `clang`, `ld.lld`, `llvm-objcopy` and `llvm-ar`, and no host compiler.
- **Options.** `-DSQLITE_OS_OTHER=1 -DSQLITE_THREADSAFE=1 -DSQLITE_TEMP_STORE=3
  -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_DESERIALIZE -DSQLITE_OMIT_LOCALTIME
  -DSQLITE_USE_URI=0 -DSQLITE_DQS=0 -DSQLITE_DEFAULT_FOREIGN_KEYS=1`, and no other
  `SQLITE_` option and no extension.
- **Steps.** For each of `sqlite3.c` and `hirsel_sqlite.c`: `clang --target=T -std=c11 -O2
  -fPIC -fno-ident -ffile-prefix-map=$PWD=. OPTIONS -c F.c -o F.o`. Then `ld.lld -r sqlite3.o
  hirsel_sqlite.o -o hsq.o`; `llvm-objcopy --wildcard --keep-global-symbol='hsq_*' hsq.o`,
  which makes every SQLite symbol local so the one member's global definitions all carry the
  prefix (FFI-3); and `llvm-ar rcs libhirsel_sqlite.a hsq.o`, which writes no timestamps or
  owners. The build is byte-reproducible: two runs give one SHA-256, and the pin is that
  digest. The adapter includes the header that `hirsel inspect --foreign-header
  sqlite@1/native` prints, and expands `HIRSEL_ABI_TEXT` once (FFI-3).

`SQLITE_THREADSAFE=1` (serialized) is chosen because tasks move between threads, helpers are
threads, and a statement and its connection are separate handles that two tasks may use at
once. Under `SQLITE_OS_OTHER=1` that setting alone selects SQLite's no-op mutex, so the adapter
supplies real ones. Once per process, with `pthread_once`, it calls
`sqlite3_config(SQLITE_CONFIG_MUTEX, &m)`, where `m` implements the `sqlite3_mutex_methods`
over `pthread_mutex_t`, with `PTHREAD_MUTEX_RECURSIVE` for `SQLITE_MUTEX_RECURSIVE` and the
static mutexes allocated once, and then `sqlite3_initialize()`. It keeps the first result
that is not `SQLITE_OK`, or `SQLITE_OK`, in a process global, and every open returns that code
without opening when it is not `SQLITE_OK`. SQLite then serializes any two calls on one
connection. Because a call on one connection may wait for another on that mutex, every
public function that passes a handle is `waits` (below), so no scheduler thread waits on it.
When the adapter reads an error message after a call on a live connection, it holds
`sqlite3_db_mutex` across the failing call and `sqlite3_errmsg`, so another call on the same
connection cannot replace the message in between. It never holds that mutex across a call
that can free the connection.

**OS layer and ambient state.** With `SQLITE_OS_OTHER` the adapter supplies `sqlite3_os_init`
and `sqlite3_os_end`, and it registers its VFSes itself. SQLite gets no ambient input:

- `xRandomness` fills its buffer with zero bytes. SQLite's PRNG then always starts from the
  same seed, so SQL `random()` and `randomblob()` are a fixed sequence, not a source of
  randomness. A program that needs random bytes binds bytes from `Random`.
- `xCurrentTime` and `xCurrentTimeInt64` return `SQLITE_ERROR`, so `'now'` in a date
  function gives `NULL`. `SQLITE_OMIT_LOCALTIME` removes the `localtime` and `utc` modifiers,
  which would read the host's time zone through libc, so they give `NULL` too. A program that
  needs the time binds a value from `Clock`.
- `xSleep` returns at once, having slept 0 microseconds, and no busy handler is installed, so
  a busy database returns `SQLITE_BUSY` at once.
- `xDlOpen` and the other loader hooks are null.
- **The authorizer.** Right after a connection opens, the adapter installs, with
  `sqlite3_set_authorizer`, an authorizer that returns `SQLITE_DENY` for `SQLITE_ATTACH` and
  `SQLITE_DETACH` whatever their arguments, and for `SQLITE_PRAGMA` whose name is, in any
  case, `temp_store_directory`, `data_store_directory`, `soft_heap_limit` or
  `hard_heap_limit`, the pragmas that read or change process globals; it allows everything
  else. If installing it fails, the open closes the connection and fails with that code. A
  computed attach name is denied too. `VACUUM` and `VACUUM INTO` attach a database internally
  through the same authorizer, so both fail with `SQLITE_AUTH` (23) when they are stepped. The
  null-name files SQLite opens are the adapter's in-memory files and never read
  `sqlite3_temp_directory`.

**The control record.** A `DbHandle`'s pointer is the adapter's control record, not the
`sqlite3 *`: it holds the connection, a reference count and, in slice 2, the connection's VFS
context. A `StmtHandle`'s pointer is a statement record holding the `sqlite3_stmt *`, a
pointer to its connection's control record, the statement's column count and its **row
state**. A connection's count is one for its `DbHandle` and one for each statement prepared on
it. `close` drops the `DbHandle`'s count, and `finalize` calls `sqlite3_finalize`, reads
its code, and then drops the statement's count. When a count reaches zero, the adapter calls
`sqlite3_close` on the connection, which has no statement left, so it is never busy, and
frees the record. So a statement may outlive its connection's `close`: the connection stays
open, every later call on the statement is a valid C call on a live connection, and no
SQLite zombie connection is ever made. A release reports only a code. The message for it
comes from `sqlite3_errstr(code)`, which reads no connection, through the native function
`errstr` below.

### Slice 1: in-memory databases

The only VFS is `hirsel-none`. Its `xOpen` refuses every name with `SQLITE_CANTOPEN`, its
`xAccess` reports every name as absent, and its `xDelete` returns `SQLITE_IOERR_DELETE`. A
connection is opened with `sqlite3_open_v2(":memory:", &db, SQLITE_OPEN_READWRITE |
SQLITE_OPEN_CREATE | SQLITE_OPEN_EXRESCODE, "hirsel-none")`, with no URI flag. When it fails
with a non-null `db`, the adapter copies `sqlite3_errmsg(db)` into the error buffer and then
calls `sqlite3_close(db)` once. Temporary storage is in memory (`SQLITE_TEMP_STORE=3`).

The public API, in module `sqlite`:

```hirsel unbuilt
pub opaque type Sqlite {}
pub opaque type DbHandle {}
pub opaque type StmtHandle {}
pub type SqliteError { code: i32, message: String }
pub type Step = Row | Done
pub opaque resource type Db: send { f: Foreign[Sqlite], p: foreign.Ptr[DbHandle] }
  drop(d) { let _ = native.close(d.f, d.p) }
pub opaque resource type Stmt: send { f: Foreign[Sqlite], p: foreign.Ptr[StmtHandle] }
  drop(s) { let _ = native.finalize(s.f, s.p) }
pub fn open_memory(f: Foreign[Sqlite]) -> Result[Db, SqliteError] waits
pub fn exec(mut db: Db, sql: String) -> Result[(), SqliteError] waits
pub fn prepare(mut db: Db, sql: String) -> Result[Stmt, SqliteError] waits
pub fn bind_i64(mut s: Stmt, i: u32, v: i64) -> Result[(), SqliteError] waits
pub fn bind_f64(mut s: Stmt, i: u32, v: f64) -> Result[(), SqliteError] waits
pub fn bind_bytes(mut s: Stmt, i: u32, v: Bytes) -> Result[(), SqliteError] waits
pub fn bind_text(mut s: Stmt, i: u32, v: String) -> Result[(), SqliteError] waits
pub fn bind_null(mut s: Stmt, i: u32) -> Result[(), SqliteError] waits
pub fn step(mut s: Stmt) -> Result[Step, SqliteError] waits
pub fn column_kind(mut s: Stmt, i: u32) -> Kind waits
pub fn column_i64(mut s: Stmt, i: u32) -> i64 waits
pub fn column_f64(mut s: Stmt, i: u32) -> f64 waits
pub fn column_bytes(mut s: Stmt, i: u32, mut out: Bytes) -> Result[(), SqliteError] waits
pub fn reset(mut s: Stmt) -> Result[(), SqliteError] waits
pub fn finalize(take s: Stmt) -> Result[(), SqliteError] waits
pub fn close(take db: Db) -> Result[(), SqliteError] waits
pub type Kind = Integer | Float | Text | Blob | Null
```

The foreign module `native`, prefix `hsq_`, has exactly these functions. Every one is `waits`
except `errstr`, and every `e` is an error buffer:

```hirsel unbuilt
// native.hirsel, module native of package sqlite, marked foreign
pub fn open_memory(f: Foreign[sqlite.Sqlite], mut e: Bytes) -> Option[foreign.Ptr[sqlite.DbHandle]] waits
pub fn exec(f: Foreign[sqlite.Sqlite], db: foreign.Ptr[sqlite.DbHandle], sql: Bytes, mut e: Bytes) -> i32 waits
pub fn prepare(f: Foreign[sqlite.Sqlite], db: foreign.Ptr[sqlite.DbHandle], sql: Bytes, mut e: Bytes) -> Option[foreign.Ptr[sqlite.StmtHandle]] waits
pub fn column_count(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle]) -> u32 waits
pub fn bind_i64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: i64, mut e: Bytes) -> i32 waits
pub fn bind_f64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: f64, mut e: Bytes) -> i32 waits
pub fn bind_blob(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: Bytes, mut e: Bytes) -> i32 waits
pub fn bind_text(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: Bytes, mut e: Bytes) -> i32 waits
pub fn bind_null(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, mut e: Bytes) -> i32 waits
pub fn step(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], mut e: Bytes) -> i32 waits
pub fn column_kind(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32) -> i32 waits
pub fn column_i64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32) -> i64 waits
pub fn column_f64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32) -> f64 waits
pub fn column_len(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, mut e: Bytes) -> i64 waits
pub fn column_copy(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, mut out: Bytes, start: u64, mut e: Bytes) -> i32 waits
pub fn reset(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], mut e: Bytes) -> i32 waits
pub fn finalize(f: Foreign[sqlite.Sqlite], take s: foreign.Ptr[sqlite.StmtHandle]) -> i32 waits
pub fn close(f: Foreign[sqlite.Sqlite], take db: foreign.Ptr[sqlite.DbHandle]) -> i32 waits
pub fn errstr(f: Foreign[sqlite.Sqlite], code: i32, mut e: Bytes)
```

- **Contract.** Every public function that passes a handle is `waits`, `bind_` and `column_`
  included: under the serialized mutex a bind can wait for a long query on another statement
  of its connection, and a wait on a helper parks only the task. `Db` and `Stmt` are `send`,
  so two tasks may each hold a statement of one connection; their calls are serialized by
  SQLite, and neither blocks a scheduler thread. A statement call passes the statement's
  handle only, and the adapter reaches the connection through the statement record.
- **SQL text.** `exec` and `prepare` pass `sql` as an immutable loan of exactly its bytes, and
  the adapter never calls a SQLite routine that reads to a terminator (`sqlite3_exec`
  included). Before C runs, the wrapper refuses a `sql` that holds a zero byte with
  `SqliteError(code: 1, message: "sqlite: SQL holds a zero byte")` and one longer than
  `2^31 − 1` bytes with `SqliteError(code: 18, message: "sqlite: SQL is too long")`
  (`SQLITE_TOOBIG`); the adapter makes the same two checks and returns the same codes. `exec`
  loops `sqlite3_prepare_v3(db, p, n, 0, &st, &tail)` over the remaining bytes: it steps each
  statement to `SQLITE_DONE`, finalizes it, and goes on from `tail` until no bytes remain; a
  null statement, from whitespace or comments, is skipped. The first error ends the loop with
  its code. `prepare` prepares the first statement the same way and then prepares the rest:
  if the rest is not empty of statements, it finalizes both and fails with
  `SqliteError(code: 1, message: "sqlite: prepare takes one statement")`. A `sql` with no
  statement at all fails with `SqliteError(code: 1, message: "sqlite: no statement")`, so
  `prepare` never returns a statement record over a null `sqlite3_stmt *`.
- **Indexes and lengths.** A parameter index is 1 or more and a column index is less than the
  statement's column count. The wrapper checks both before C runs: a bind index of 0 or above
  `2^31 − 1` gives `SqliteError(code: 25, message: "sqlite: parameter index out of range")`
  (`SQLITE_RANGE`). Bytes and text are bound with `sqlite3_bind_blob64` and
  `sqlite3_bind_text64` with the `uint64_t` length and `SQLITE_TRANSIENT`, so SQLite copies
  them before the bind returns and keeps no loaned pointer, and a length SQLite does not take
  comes back as its own `SQLITE_TOOBIG`. No length or index is cast to `int` without a check.
- **Row state.** The statement record's row state is `none` after `prepare`, `reset` and a step
  that does not return `SQLITE_ROW`, and `row` after a step that does. The wrapper's `Stmt`
  mirrors it in a private field, with the column count from `column_count` at `prepare`. A
  `column_` call with no current row fails before C with `fail("sqlite: no current row")`, and
  one whose index is not below the column count fails with
  `fail("sqlite: column index out of range")`, so SQLite's undefined column reads are never
  reached. The adapter checks both again.
- **Column values.** `column_kind` reads `sqlite3_column_type` before any conversion, so the
  kind is the stored one. `column_bytes` appends the column's bytes to `out`: `column_len`
  takes the kind first, then calls `sqlite3_column_blob` (or `_text` for text) and then
  `sqlite3_column_bytes`. When the pointer is null for a non-empty value, or the conversion
  ran out of memory (`sqlite3_errcode` is `SQLITE_NOMEM` under the connection's mutex),
  `column_len` returns −1 with the error in `e`, and `column_bytes` returns that error. The
  wrapper then grows `out` by the length with `list.fill`, and `column_copy` writes exactly
  `len(out) − start` bytes at `start`, from the same value, and returns 0, or
  `SQLITE_MISUSE` (21) when the column's length is no longer that. Both run while the task
  holds `mut s`, so no other call on the statement runs between them. An empty blob and `NULL`
  are told apart by `column_kind`.
- **Errors.** Every native function that can fail, other than a release, takes a last parameter
  `mut e: Bytes`, an **error buffer** that the wrapper's private `error_buffer()` makes as 260
  zero bytes with `list.fill`. A native function with an `i32` result returns the extended
  result code. On failure the adapter writes that code's 32 bits as 4 little-endian bytes at
  offset 0, then the bytes of the message from offset 4: at most 256 of them, cut at a UTF-8
  character boundary, and the rest stays zero. It writes nothing into a buffer shorter than
  260 bytes. `open_memory`'s and `prepare`'s `None` means failure, and its code is in the
  buffer. The wrapper's private `read_error(e)` reads the code as `let c: u32 =
  list.word_le(e, 0)` and `int.truncate` to `i32`, and the message as the bytes from offset 4
  up to the first zero byte at an offset from 4 to 259, or to offset 260 when there is none.
  It converts them with `string.from_utf8`, and uses `"sqlite: error text is not UTF-8"`
  when they are not UTF-8 and `"sqlite: no error text"` when there are none. A `SqliteError`
  is that code and message. A release's code `rc` that is not 0 becomes
  `SqliteError(code: rc, message: m)` with `m` from `errstr(f, rc, mut e)`, which writes
  `sqlite3_errstr(rc)` into a fresh error buffer the same way, after the release has been
  issued and delivered; the wrapper's private `release_error(f, rc)` does this.
- **Release.** `close` and `finalize` release through the control record (above). Each
  consumes its handle whatever it returns (FFI-6). The public `close` and `finalize` end their
  resource by a whole destructure, so its finalizer does not run
  ([FAIL-11](failure.md#fail-11-resources)), and then issue the release as their next call, with
  nothing fallible in between; the error buffer comes after:

  ```hirsel unbuilt
  pub fn close(take db: Db) -> Result[(), SqliteError] waits {
    let Db(f: f, p: p) = take db          // db ends without its finalizer
    let rc = native.close(f, p)            // a release: not cancelled, allocates nothing
    if rc == 0 { Ok(()) } else { Error(release_error(f, rc)) }
  }
  ```

  A `Db` or `Stmt` dropped without these is released by its finalizer, and the result is
  discarded. A finalizer that fails or is abandoned leaves the release to the run (FFI-6).
- **A refused admission.** A function that receives a new handle builds its resource as its
  next step, with nothing fallible in between. When a `Db` or `Stmt` literal returns
  `Error((c, (f, p, ...)))`, the wrapper releases `p` with `native.close` or `native.finalize`.
  It then returns `Error(SqliteError(code: 7, message: "sqlite: no capacity for the handle"))`,
  where 7 is `SQLITE_NOMEM`.
- **Native bounds.** A query's native time and SQLite's heap are not charged (FFI-7).
  `sqlite3_interrupt`, progress-handler budgets and a heap limit are later work
  ([reserved](reserved.md), R5).

### Slice 2: databases in a granted directory

Slice 2 adds persistent databases under a granted `Dir`. Every file that SQLite opens, deletes
or checks for the database goes through that directory: the database, its rollback journal,
its WAL and its `-shm`. Temporary files never reach a directory.

- **A new ABI form.** A foreign parameter `d: Dir` passes to C two parameters,
  `int d_fd, uint32_t d_flags`; its ABI text is `dir`. `d_fd` is a new `O_RDONLY |
  O_DIRECTORY | O_CLOEXEC` descriptor for the directory, which can be synced, and which the
  callee owns from then on and must close exactly once. Bit 0 of `d_flags` is set when the
  `Dir` permits writing. This is the one descriptor transfer in the ABI. A `Dir` made by
  virtual construction, or by the model world, has no descriptor, and the call fails in
  FFI-6's preflight with `Fail("foreign call needs a host Dir")`, before any handle is marked.
  The run makes the descriptor at the call's issue. A failure to make it, such as the
  descriptor table being full, is delivered as `d_fd = -1`, and the callee returns its own
  error.
- **API.** `pub fn open(f: Foreign[Sqlite], d: Dir, name: String) -> Result[Db, SqliteError]
  waits`, over `native.open(f, d, name: Bytes, mut e) -> Option[foreign.Ptr[DbHandle]] waits`.
  `name` is 1 to 200 bytes of `A`–`Z`, `a`–`z`, `0`–`9`, `.`, `_` and `-`. It does not start
  with `.`, and it does not end with `-journal`, `-wal` or `-shm`. Any other name gives
  `SqliteError(code: 14, message: "sqlite: name is not one leaf in the directory")`
  (`SQLITE_CANTOPEN`) before C runs. A read-only `Dir` opens with `SQLITE_OPEN_READONLY`.
- **One VFS per connection.** For each `open`, the adapter makes a **VFS context** holding the
  root descriptor, whether it may write, and the database's leaf name, and registers a VFS of
  its own, named `hirsel-dir-<n>` with `n` a process counter, whose `pAppData` is that context.
  It passes that name to `sqlite3_open_v2`. Every method finds the root, its rights and the
  database through `pAppData`, never through a pathname, so two opens with one leaf under two
  grants can never use each other's root. `xFullPathname` returns `/` and the leaf. The context
  belongs to the control record: after the connection's `sqlite3_close` returns, the adapter
  unregisters the VFS and closes the root descriptor, once, whether or not any file ever
  opened. If the open fails, it does the same right after its `sqlite3_close`.
- **Names.** A name SQLite passes to `xOpen`, `xAccess` or `xDelete` is admitted only when it is
  `/` and exactly the context's leaf, or that leaf followed by exactly `-journal`, `-wal` or
  `-shm`, and its open flags' file kind matches: `SQLITE_OPEN_MAIN_DB` for the leaf,
  `MAIN_JOURNAL` for `-journal` and `WAL` for `-wal`. Every other name and kind gives
  `SQLITE_CANTOPEN` (`SQLITE_IOERR_DELETE` for a delete, absent for an access), a
  super-journal open (`SQLITE_OPEN_SUPER_JOURNAL`) and every name read from a journal on disk
  included, so recovery that names any other file fails and never escapes the directory.
  `xOpen` acts through `openat2(root, leaf, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
  RESOLVE_NO_MAGICLINKS)`, `xAccess` through the same `openat2` with `O_PATH` and an `fstat`,
  never `faccessat`, and `xDelete` through `unlinkat` on the root. Only regular files are
  opened, and the main database is refused with `SQLITE_CANTOPEN` when its link count is not 1,
  because SQLite finds a database's journal by its name and two names of one file would have
  two journals. A null-name open, which SQLite makes for temporary and sort files that
  `SQLITE_TEMP_STORE=3` did not keep in memory, gets an in-memory file of the adapter, never a
  host file. A rename of the directory while it is open does not move the descriptor.
- **Read-only.** In a context that may not write, every `xOpen` that asks for
  `SQLITE_OPEN_READWRITE` or `SQLITE_OPEN_CREATE` gives `SQLITE_CANTOPEN` before any system
  call, with no read-only retry (SQLite's own Unix VFS retries; this one does not); files are
  opened `O_RDONLY`; `xDelete` gives `SQLITE_IOERR_DELETE`; and `xShmMap` gives
  `SQLITE_IOERR_SHMOPEN`. SQLite opens a WAL only with `SQLITE_OPEN_READWRITE |
  SQLITE_OPEN_CREATE`, so a WAL database cannot be read through a read-only `Dir` in v1: its
  first statement fails with `SQLITE_CANTOPEN`, and `open` itself succeeds, because SQLite
  reads nothing until then. A rollback database can be read.
- **File methods.** Offset reads that zero-fill short reads, writes, `ftruncate`, `fsync` for
  `xSync`, and `SQLITE_NOTFOUND` for unhandled file controls. After creating or deleting a
  journal or WAL, the adapter calls `fsync` on the root descriptor, and an error of any
  `fsync` is returned as `SQLITE_IOERR_FSYNC` or `SQLITE_IOERR_DIR_FSYNC`, never dropped. On a
  failed open `pMethods` stays null after the partial state is cleaned.
- **Locks.** The five lock levels use open file description locks (`F_OFD_SETLK` with
  `F_RDLCK`, `F_WRLCK` and `F_UNLCK` over SQLite's own byte ranges: the pending byte, the
  reserved byte and the shared range), never `F_SETLK` and never a waiting form. An OFD lock
  belongs to its own open file, so two connections in one process exclude each other exactly
  as two processes do, and closing one connection's descriptor never releases another's
  locks. A lock that is held elsewhere gives `SQLITE_BUSY` at once.
- **WAL index.** Each connection opens the `-shm` of its leaf under the same root through
  `openat2`, maps each region it is asked for with `MAP_SHARED`, and locks with `F_OFD_SETLK`
  on its own descriptor over SQLite's shm lock slots, so two connections, in one process or
  two, see one WAL index and exclude each other. The first opener is the one whose exclusive
  OFD lock on the dead-man byte (offset 128 of the `-shm` file, as SQLite's Unix VFS uses)
  succeeds: it truncates the file to zero, so a stale index from a crash is never read, and
  then holds the byte shared, as every other opener does. `xShmBarrier` is a sequentially
  consistent fence. `xShmUnmap` unmaps, closes, and with its delete flag, which SQLite passes
  only when this connection is the last one, removes the `-shm` with `unlinkat` on the root.
- **Durability.** A commit is durable as SQLite's own contract makes it, over `xSync` that
  really syncs and the directory syncs above. The sync barriers are SQLite's, not
  `host.sync`'s, and a SQLite database is not the store's durability contract
  ([CAP-9](capabilities.md#cap-9-recording-is-outside-the-program)). It holds for a database
  opened always by one leaf name, from any number of connections and processes.
- **Refused selection.** URI filenames are off globally (`SQLITE_USE_URI=0`, no
  `SQLITE_CONFIG_URI`) and in every open, so `file:` and `vfs=` have no meaning and name a
  leaf, which the leaf rule refuses. The authorizer denies `ATTACH` and `DETACH`, so `VACUUM`
  and `VACUUM INTO` fail with `SQLITE_AUTH` as in slice 1.

## Laws

Each law's expected value comes from this chapter, from a published test vector, from SQLite's
documented result codes or from the fixture's own counters. None comes from a run's output.
Every law that runs code runs on both native backends. Laws FL1 to FL7, FL12 to FL18 and FL23
use the fixture package `ffi_laws` in the program tests, with interface `Laws` and handle kind
`Obj`, both in module `laws`. Its adapter `laws.c`, prefix `hfl_`, owns a static pool of 8 one-byte cells, and a
freed cell is the first one reused, so a `make` after a `free` returns the same address. It
defines:

- `make(f) -> Option[foreign.Ptr[Obj]]` and `make_w`, the same declared `waits`: take a cell,
  store in it the count of makes so far plus 1, increment `made`, and return the cell;
- `free(f, take p) -> i32`, declared `waits`, the kind's release: write `freed K` and a line
  feed to file descriptor 2 with `write(2)`, `K` the cell's stored count, return the cell to
  the pool, increment `freed` and return 0;
- `freed(f) -> u64` and `made(f) -> u64`;
- `same(f, p) -> Option[foreign.Ptr[Obj]]`, with `p` a handle in use, declared `waits`: return `p`'s own pointer;
- `mark(f, mut b) -> u64` and `mark_w`, the same declared `waits`: write `0xAB` to each of the
  loan's `len` bytes and return `len`;
- `gate(f, on: u32) -> i32`, on the host stack: sets or clears the gate that FL17 uses;
- `touch(f) -> i32`: write `touched` and a line feed to file descriptor 2 and return 0;
- `hold(f) -> i32`, declared `waits`: increment `held`, then wait on a semaphore; `held(f) ->
  u64`; and `open(f, n: u32) -> i32`, on the host stack, which posts the semaphore `n` times.
  `hold` breaks FFI-7's progress clause on purpose, and only `open`, which needs no helper,
  ends its wait.

A **refusal case** is a multi-package case in the program tests' case harness
(`crates/hirsel-core-program-tests/src/cases.rs`, under `core/front/cases/ffi/`), with a real
foreign manifest, and its diagnostic is checked at the stated site. The `hirsel unbuilt`
fences of this chapter stay documentation.

| law | bug | oracle and setup |
|---|---|---|
| **FL1** a wrong archive digest | a pin checked after linking, or not at all | `ffi_laws`' x86_64 row names a file whose bytes are exactly `abc`, pinned to the SHA-256 of the empty string. `hirsel build` of an entry that calls `native.make` is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu does not match its pin: expected sha256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855, found sha256 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad` (FIPS 180-2 vectors), and the linker is never started. With the pin changed to `ba78…15ad`, the same build is refused with `… is not an indexed ar archive`, so the digest is checked before the bytes are parsed. |
| **FL2** a missing target row | a missing row passed through as a linker error, or a row required when nothing is reached | With only the x86_64 row, `hirsel build --target aarch64-unknown-linux-gnu` of the FL1 entry, with its real pin, is refused with `FFI-3: ffi_laws@1/native has no archive for target aarch64-unknown-linux-gnu; its archives are for x86_64-unknown-linux-gnu`, before the linker starts. An entry of the same package that never names `native` gets no FFI-3 refusal for aarch64. This is a negative test: it gives no AArch64 coverage. |
| **FL3** double release | a second release that reaches C | A test takes `f: Foreign[Laws]`, makes `p` with `native.make`, calls `native.free(f, p)`, then calls `native.free(f, p)` again inside an `isolate`. The `isolate` returns `Ok(Error(causes))`, with primary cause `Fail("foreign handle released")` at the second `free`, and then `native.freed(f)` returns 1. |
| **FL4** stale use and a racing release | a released handle's pointer passed to C, a copy that escapes the check, or two releases that both reach C | As FL3, but the second call is `native.free(f, q)`, where `q` is a copy of `p` that a resource field held: the same cause, and `freed` returns 1. Then two `send` resources `ObjBox` of the fixture each hold a copy of one fresh handle (the fixture breaks FFI-6's one-field rule on purpose), each moves into its own task, and both tasks release it at once with `close_obj`: one returns 0, the other fails with `Fail("foreign handle released")` (it waited for the first and found it consumed, FFI-6), never `in use`, and `freed` returns 2. |
| **FL5** a loan that exposes spare capacity, or moves across a park | capacity passed as the length, or a view whose storage moves while a helper writes it | `b = list.with_capacity(16)`, then `list.fill(mut b, 4, 0)`. `native.mark(f, mut b)` returns 4. Afterwards `list.len(b)` is 4 and every byte of `b` is `0xAB`. Then `list.push(mut b, 0)` admits nothing, because the capacity is still 16. An empty `b` gives `mark` 0, and the adapter checks that `ptr` is not `NULL`. Then, with `c` 12 zero bytes, `native.mark_w(f, mut list.slice(c, 4, 8))` returns 4 while another task runs 10⁴ allocations; afterwards bytes 4 to 7 of `c` are `0xAB` and the other 8 are 0. |
| **FL6** an FFI-reaching test refused a cached pass | a pass key made for a test whose reach holds C | A parameterless test calls `probe(None)`, where `fn probe(f: Option[Foreign[Laws]]) -> u64 { match f { Some(f) => native.freed(f), None => 0 } }`, and passes. Run `hirsel test` twice with one cache: both runs report it executed, and neither reports it reused. A parameterless test beside it that does not call `probe` is reported reused on the second run. The same holds when the only path to `native` is a resource's finalizer, on a fresh and on a warm reach record. |
| **FL7** recording and replay refused before C runs | a live recording half, a recording host or a stored replay that enters native code | `hirsel test --replay` of a test that calls `native.touch` fails with a line ending `refused: FFI-8: a recording build cannot reach foreign code: ffi_laws@1/tests/touches reaches ffi_laws@1/native/touch`. Its stderr holds no `touched`, and no recording file exists. `hirsel replay` of an entry that reaches `touch`, given any tape path, exits 2 with `replay: refused: FFI-8: …` and does not open the tape. `hirsel build --recording` is refused the same way. A deploy of that entry's recipe to `hirsel host --record DIR` is refused with `deploy: refused: FFI-8: a recording build cannot reach foreign code: …`, and the host state gains no assembly record and no image. `hirsel replay --state` given a host state whose assembly record holds that recipe, and a tape whose routing header names it and whose records after the header are not valid records, exits 2 with `replay: refused: FFI-8: …`, not a tape error. |
| **FL8** SQLite round trip | a loan, `SQLITE_TRANSIENT` or `NULL`/empty mistake | In `sqlite`'s own tests, with grant `Foreign=sqlite@1/sqlite/Sqlite`: create `t(b blob)`, insert `x''`, `NULL` and `x'00ff'` with `bind_bytes` and `bind_null`, after which the bound list is cleared and refilled. Selecting in insertion order gives `Blob` with 0 bytes, `Null`, and `Blob` with `00 ff`. |
| **FL9** SQLite release paths | a finalizer that runs after an explicit release, a busy close, or a last release that frees the mutex it reports through | Four tests, each preparing `s` from `select 1` on a fresh connection `db`: (a) `finalize(take s)` then `close(take db)`; (b) `close(take db)` then `finalize(take s)`; (c) `close(take db)` with `s` dropped at scope end; (d) both dropped at scope end, in either order. Every explicit call returns `Ok(())`. In (b), `step(mut s)` between the two calls still returns `Row`, because the connection stays open until its count is zero. With `s` from `select abs(-9223372036854775808)` stepped once, so its last result is an error, `close(take db)` returns `Ok(())` and `finalize(take s)` then returns `Error(SqliteError(code: 1, message: "SQL logic error"))`, `sqlite3_errstr(1)`'s text; each test passes with no cause, so no finalizer met a consumed handle. |
| **FL10** no ambient input | the OS layer, libc or a pragma reading or changing process state | `select random()` returns the same value in two runs of a fresh process. `select date('now')`, `select datetime(0, 'localtime')` and `select datetime(0, 'utc')` each return `NULL`. `attach ':memory:' as x` fails at `prepare` with code 23, and `vacuum` and `vacuum into 'x.db'` fail at `step` with code 23 (`SQLITE_AUTH`). `pragma temp_store_directory = '/tmp'` and `pragma soft_heap_limit = 1` fail with code 23. |
| **FL11** directory confinement (slice 2) | a sidecar, temp file, super-journal or attach outside the grant | In a granted scratch `Dir` whose parent holds a sentinel directory: open `a.db`, run a WAL transaction from two connections, then a rollback-journal transaction. The files created are exactly `a.db`, `a.db-wal` and `a.db-shm` (WAL), and `a.db-journal` only while the journal transaction runs. The parent and the sentinel are unchanged. `open(f, d, "../x")` and `open(f, d, "file:a.db?vfs=unix")` give code 14 before C runs. A hot journal whose super-journal name is `/tmp/evil` fails recovery with `SQLITE_CANTOPEN` and opens no file outside. A `sort` over 10⁶ rows creates no file. Two connections to one leaf under two grants of different directories each see only their own rows. |
| **FL12** an archive whose code is outside its pin | a thin archive, or a member that is not a relocatable object, admitted because its digest matches | `ffi_laws`' x86_64 row names an indexed thin archive made with `ar --thin` from the adapter's `laws.o`, which lies beside it, pinned to the thin archive's true SHA-256. `hirsel build` of the FL1 entry is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu is a thin archive; its members are not inside the pinned bytes`, and the linker is never started. With the row naming a regular archive whose one member is `laws.o` compiled for `aarch64`, again with its true pin, the refusal is `… member laws.o is not a relocatable object for x86_64-unknown-linux-gnu`. With a regular x86_64 archive whose index lists `hfl_make` but whose member does not define it, the refusal is `… does not define hfl_make, declared at SITE`. |
| **FL13** a recipe-only build of a foreign program | a recipe from which a reader cannot recompute the foreign address, or one that links archives the leaves do not select | `ffi_laws` gets a second row, `aarch64-unknown-linux-gnu`, naming any file with its true pin. `hirsel publish` of the FL1 entry for x86_64 prints R. Delete the package's directory: `hirsel build --recipe R --code-store DIR` succeeds, and the executable's `native.make` call works. Change only the aarch64 row's bytes and pin and publish again: the new R differs from R (FFI-9), and it builds from the code store alone the same way. A recipe made from R's bytes by removing the x86_64 archive's entry from its foreign archives part, with its objects in the store, is refused as `Missing`, and one made by adding the aarch64 archive's entry as `Extra`. |
| **FL14** archive code that runs with no grant | an initializer, an indirect function or an out-of-prefix symbol admitted, so C runs before main or with no call | Each archive below has its true pin, and a parameterless test of `ffi_laws` that never names `native` sits beside the FL1 entry. `laws.c` plus `__attribute__((constructor)) static void hfl_c(void) { write(2, "ctor\n", 5); }` makes the build of the FL1 entry refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu: member laws.o has section .init_array of type SHT_INIT_ARRAY, which is not an admitted object feature` before linking. With the same archive, `hirsel test` of the package, whose tests are the parameterless one and one that takes `Foreign[Laws]`, is refused at its build with the same text, and its output holds no `ctor`. An archive defining the global `malloc` is refused with `… member laws.o defines malloc, which does not start with the prefix hfl_`. One defining `hfl_x` as an `ifunc` is refused with `… has indirect function hfl_x, …`. |
| **FL15** a header that disagrees with its archive | a wrapper whose header says `handle` or `u64` where the audited C takes `take` or a loan | `ffi_laws`' header declares `free(f, p: foreign.Ptr[Obj]) -> i32 waits` (no `take`), while the archive's `.hirsel_abi` comes from the true header. `make` still makes `Obj`, so its first use of the kind is refused with FFI-5 because the kind has no release. No archive is admitted. An archive with no `.hirsel_abi` is refused with `… has no ABI text`. A refusal case with two kinds `A` and `B` in one package stores a `foreign.Ptr[A]` in a field of type `foreign.Ptr[B]` and is refused at that field by the type checker. |
| **FL16** a lost or misplaced release | a cancelled close, a failed finalizer or an un-released local that leaks or double-releases | (a) A task with a cancellation already requested calls `close_obj(take o)`, where `o: ObjBox` holds `p` and `close_obj` destructures it and calls `native.free`: `free` is issued and returns, `freed` rises by 1, and the task's next cancellation point fails with `Cancelled`. (b) `ObjBox`'s finalizer variant `drop(o) { fail("x") }`: after the box is dropped, `freed` has risen by 1, by the mechanical release, and the finalizer's cause is reported (FAIL-12). (c) `hirsel run` of an entry that makes three handles with `native.make` into locals and returns: stderr ends with exactly `freed 3`, `freed 2`, `freed 1` on three lines, in descending ordinal order. |
| **FL17** an address reused or handed back | a released pointer that leaves the index only at delivery, refusing a new object made at its address while the release is still in C, or a returned pointer that aliases a live owner | With `gate(f, 1)` set (a host-stack fixture function after which `free` returns its cell to the pool, increments `held` and then waits on the semaphore before returning), a task makes `p` with `make` and calls `free(f, p)`. The test calls `held` until it returns 1, so the release is in C with the cell already free, and then calls `q = make` on the host stack: `q` is accepted though the pool returned `p`'s cell, because `p`'s pointer left the index at issue. `open(f, 1)` ends the release; `made` returns 2 and `freed` 1. With `gate(f, 0)`, `native.same(f, q)` fails with `Fail("foreign pointer already held")`, `made` is unchanged, and `free(f, q)` then returns 0 and `freed` returns 2. A refusal case binds `ffi_laws`' `make` in a second interface's module, naming `Obj` there too, and is refused at the second (FFI-6). |
| **FL18** a full helper pool | four calls holding every helper and starving a fifth, a lost queue entry, or a cancellation in the queue that leaves a handle in flight | Four tasks call `hold`; the test calls `held` on the host stack until it returns 4. In a `hirsel run` entry, a fifth task makes its own `p` with `make` on the host stack, the `K`-th make, and calls `native.same(f, p)`; a sixth calls `make_w`; both wait in the queue. Cancelling the fifth makes it fail with `Cancelled`, whether it was observed at entry or in the queue. Then `open(f, 4)` ends the four holds, every task ends, and the sixth's `make_w` returns a handle with `made` one more than before. The run's stderr then holds `freed K`, written by run-end release, so the cancelled call left `p` live, not in flight. |
| **FL19** a SQLite column read with no row, or an unsafe SQL input | C reading a column with no current row or out of range, a C string reader past the loan, or a second statement prepared silently | `column_i64` right after `prepare` fails with `fail("sqlite: no current row")`; after `step` returns `Done`, the same; with a row of 1 column, `column_i64(mut s, 1)` fails with `fail("sqlite: column index out of range")`; `bind_i64(mut s, 0, 1)` gives `Error(SqliteError(code: 25, message: "sqlite: parameter index out of range"))`. `exec` of the bytes `create table t(x); `, a zero byte and `drop table t` gives `SqliteError(code: 1, message: "sqlite: SQL holds a zero byte")` and `t` does not exist afterwards. `prepare` of `select 1; select 2` gives `Error(SqliteError(code: 1, message: "sqlite: prepare takes one statement"))`, `prepare` of ` -- c` gives `… "sqlite: no statement"`, and `exec` of ` -- c` gives `Ok(())`. |
| **FL20** a SQLite error message at the buffer's edge | a scan past the buffer, a cut inside a character, or a code decoded with the wrong width | In `sqlite`'s own tests through its private `read_error`: a buffer whose bytes 0 to 3 are `01 00 00 00` and bytes 4 to 259 are 256 `a`s decodes to code 1 and a 256-byte message; bytes 0 to 3 `0a 04 00 00` decode to code 1034 (`SQLITE_IOERR_FSYNC`, 10 + 4 · 256) and bytes `ff ff ff ff` to code −1; bytes 4 and 5 `c3 28` decode to `"sqlite: error text is not UTF-8"` and an all-zero payload to `"sqlite: no error text"`. Through the adapter: `prepare` of `select * from ` followed by 240 `a`s and `é` (`c3 a9`) fails with code 1 and the message `no such table: ` and the 240 `a`s, 255 bytes: SQLite's message is 257 bytes, and a cut at 256 would split the `é`. |
| **FL21** SQLite contention from two tasks (`Db` and `Stmt` are `send`) | a bind or column call that blocks a scheduler thread on the connection's mutex, or an error message replaced between the failing call and its read | Two tasks each hold a statement of one connection, one stepping `with recursive c(x) as (select 1 union all select x+1 from c where x < 10⁶) select count(*) from c` and the other binding and stepping `select ?1` 1000 times; both return their expected values (10⁶, and each bound value), and a third task with no SQLite call finishes while they run on a 1-thread scheduler. With `create table u(x unique)` holding 1, a step of `insert into u values (1)` in one task while the other task's statements succeed on the same connection returns `SqliteError(code: 2067, message: "UNIQUE constraint failed: u.x")` (`SQLITE_CONSTRAINT_UNIQUE`, 19 + 8 · 256). |
| **FL22** SQLite in a granted directory under contention (slice 2) | a lock released by a sibling's close, a second writer admitted, a stale `-shm` read, a read-only `Dir` that writes, or a sync error dropped | With `a.db` in a scratch `Dir`: two connections in one process both begin `immediate`; the second gets `SQLITE_BUSY` (5) at once. Closing a third connection to `a.db` while the first holds its write lock leaves the lock held: the second still gets 5. A second process does the same. Opening `a.db` through a read-only grant and running `insert` gives `SQLITE_READONLY` (8), and the directory's bytes are unchanged; with `a.db` in WAL mode, `open` through the read-only grant succeeds and its first `select` fails with `SQLITE_CANTOPEN` (14). A `-shm` file of 32 KiB of `0xFF` left by a killed process is truncated by the next first opener, which reads the database correctly. A hard link `b.db` to `a.db` makes `open(f, d, "a.db")` fail with code 14. |
| **FL23** a raw handle that escapes its owner | a handle reached outside its owner through projection, inference, a generic argument or a capture | Refusal cases in package `ffi_laws`, each refused with FFI-6 at the stated site: `pub resource type Open { f: Foreign[Laws], p: foreign.Ptr[Obj] } drop(o) { let _ = native.free(o.f, o.p) }`, which is not `opaque`, at the type; `let l: List[foreign.Ptr[Obj]] = list.new()`, where the binding supplies the expected type (LANG-11), at the raw handle element type; `fn id[T: plain](x: T) -> T { x }` called as `id(p)`, at the call, for its inferred type argument; and a closure that names `p` in its body, at the capture. Accepted: `fn keep(o: ObjBox) -> List[ObjBox]`, because an owning resource holds no raw handle, and `let q = p` with `q` then unused, after which `freed` is unchanged. |

## Build slices

1. FFI-1 to FFI-9 with the in-memory `sqlite` package (slice 1), and laws FL1 to FL10, FL12 to
   FL21 and FL23. Slice 1 is done only when every one of them passes on both native backends.
2. The `Dir` ABI form and the per-connection `hirsel-dir` VFS (slice 2), and laws FL11 and FL22.
   Slice 2 is enabled only when they pass on both native backends.

A red law whose cause is memory safety, a release (one too few or too many), containment of
foreign code or of SQLite's files, or data integrity blocks its slice, whatever the law's
number. A slice whose law is red for such a cause does not ship.
