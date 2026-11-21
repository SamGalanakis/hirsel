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
consumer. These rules are unbuilt design. Every fence on this page is marked `hirsel unbuilt`
until `hirsel` implements it.

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
call boundary, it is bounded and does not block when it is not `waits`, it does not call
back into Hirsel, and it reaches the outside world only as its package's audit says it does.
A callee that breaks the native contract is outside every guarantee of this specification:
the fixed goals ([index](index.md#fixed-goals)) hold for a run only while its foreign
callees keep that contract. A process abort, a signal, a stack overflow or memory corruption
in native code is **environmental termination**. It is termination by the host
([FAIL-15](failure.md#fail-15-run-stops)): Hirsel promises no diagnostic, no cause, no
cleanup and no finalizer for it, and the exit status is the host's. The archive pin is an
admission rule for exactly the code that was audited. It says nothing about what that code
does.

The reasons are these. A raw pointer in a same-process callee can read, write or keep
anything in the address space. A helper thread in the same process shares the process's
fate. No Hirsel-side check can contain such a callee, so this chapter promises only what the
Hirsel side enforces. Contained execution of untrusted native code, compiled to Wasm or run
out of process, is a separate later design ([reserved](reserved.md), R5).

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
pub fn open_memory(f: Foreign[sqlite.Sqlite], mut e: Bytes) -> Option[foreign.Ptr] waits
pub fn exec(f: Foreign[sqlite.Sqlite], db: foreign.Ptr, sql: Bytes, mut e: Bytes) -> i32 waits
pub fn close(f: Foreign[sqlite.Sqlite], take db: foreign.Ptr, mut e: Bytes) -> i32 waits
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
| no global symbol it defines is also defined by another admitted archive or by the runtime | `FFI-3: symbol S is defined by archive F1 of P1 and by G` (`G` an archive or `the runtime`) |

`P` is the module's definition path (`sqlite@1/native`), `T` the target triple, `F` the
row's `file` and `N` the member's name. The digest is checked first. So a file that is not an
archive but carries a wrong pin is reported as a wrong pin, and the driver parses no bytes that
do not match the pin. The member and symbol checks read the members' own ELF headers and symbol
tables, never the index alone, so an index that names a symbol no member defines is refused.
Two modules whose selected rows have one pin select one archive: it is checked, staged and
linked once, and the symbol check compares distinct archives only. Which object features an
admitted member may hold beyond these, such as initializer sections, is FFI-1's audit rule.

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
leaf holds the module's identity encoding, its symbol prefix as a text, its whole archive
table, and its reached functions. The table is the number of rows and then each row in
ascending order of target, as the target text, the 32-byte pin and the byte length as a
number, which are exactly the bytes FFI-9's archive-set digest is taken over. The reached
functions are their number and then each function, in strictly ascending order of identity
encoding, as that encoding and its PROG-9 header text `S` as a text. So a reader with no
source recomputes every foreign function's address from the leaf, and selects the row for
the recipe's target from it. The **foreign archives** part lists the distinct pins of the
rows the leaves select for the recipe's target, each as its SHA-256 and byte length, in
strictly ascending order of hash. An archive's bytes are an object of the code store, named
by that SHA-256, and they are transferred, reserved, collected and reassembled as the
recipe's other objects are. With these parts the recipe is version 2, and its root is
`R = H("hirsel.release-recipe.v2", recipe)`. The version-1 domain is never reused for the new
encoding. A program that reaches no foreign function has two empty parts, and it still uses
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
release recipe a grant binding of kind `Foreign` holds `D` as its one value.

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
| `f32`, `f64` | `float`, `double` | by value |
| result `()` or no result | `void` | |
| `b: Bytes` | `const uint8_t *b_ptr, uint64_t b_len` | an **immutable loan** |
| `mut b: Bytes` | `uint8_t *b_ptr, uint64_t b_len` | an **exclusive loan** |
| `p: foreign.Ptr` | `void *p` | a handle in use for the call (FFI-6) |
| `take p: foreign.Ptr` | `void *p` | a **release** of the handle (FFI-6) |
| result `Option[foreign.Ptr]` | `void *` | `NULL` is `None`, and any other pointer is a new handle (FFI-6) |

A `Bytes` parameter becomes two C parameters in place, the pointer first. A parameter of type
`Bytes` also takes a `View[Bytes]` ([OWN-7](ownership.md#own-7-parameter-modes)), such as a
`list.slice` or a `string.as_bytes`, and passes that view's range. `bool`, `char`, `String`,
other lists, records, variants, callables, other capabilities, `View` results, `bytes`
parameters, a `foreign.Ptr` result without `Option`, and a `mut` handle are all refused.

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
fn column(f: Foreign[Sqlite], s: foreign.Ptr, i: u32, mut out: Bytes) {
  let n = native.column_len(f, s, i)
  let start = list.len(out)
  list.fill(mut out, n, 0)
  native.column_copy(f, s, i, mut out, start)    // C writes bytes start .. start + n
}
```
```hirsel unbuilt
// native.hirsel, marked foreign
pub fn name(f: Foreign[sqlite.Sqlite], s: String) -> i32       // refused: FFI-5, String is not a foreign type
```

### FFI-6: Handles

`foreign.Ptr` is the core type of a **handle**. It is plain, `send`, and 8 bytes, and it is
not `shareable`, `key` or `codec`. Its value is a run-local ordinal, never the C pointer. The
run keeps a **handle table** that maps each live ordinal to its C pointer, its interface and
its state. Ordinals start at 1 and are never reused within a run. A table that would pass
`2^64 − 1` ordinals is the run stop `HostHandles` ([FAIL-15](failure.md#fail-15-run-stops)).

**Where a handle may live.** `foreign.Ptr` appears only in the owning package: in foreign
function signatures; in the parameters, results and locals of functions that are not visible
outside the package, as itself or inside `Option`, `Result` or a tuple; and as a field of a
`resource type` of that package that declares `drop`. Any other position is refused here, at
the type: a field of a type that is not such a resource, a type argument of anything other
than `Option`, `Result` or a tuple, a constant, a `pub` function of a non-foreign module, and
every position in another package. So outside its package a handle exists only inside an
opaque resource, and that resource's finalizer runs exactly once
([FAIL-11](failure.md#fail-11-resources)). The type is copyable, so a wrapper may hold one handle in two
places, for example a parent's handle beside a child's. A copy is checked like the original,
because the check reads the table, not the value.

**States.** An ordinal is **live**, **in flight** or **consumed**. A new handle is live. A call
moves each handle argument from live to in flight before it enters C. When C returns, a `take`
argument becomes consumed and is removed from the table, and every other handle argument
becomes live again. A release always consumes its handle, whatever the C function returns. A
consumed ordinal is any ordinal below the next one that is not in the table, so no tombstone
is kept. Before any C runs, and in the order of the parameters, the run checks each handle
argument. On the first that fails it raises the failure below, at the call, and changes
nothing. No handle changes state, no C runs and no helper is taken:

| the handle argument | failure |
|---|---|
| is consumed | `Fail("foreign handle released")` |
| is in flight, in this call or in another task's | `Fail("foreign handle in use")` |
| belongs to another interface | `Fail("foreign handle of another interface")` |

`Fail(text)` is the cause ([FAIL-2](failure.md#fail-2-causes)). So passing one handle twice to
one call fails at that call, and releasing a handle twice or using it after its release fails
without the C function being entered. When a pointer result is not `NULL` and equals the C
pointer of a handle that is live or in flight for the same interface, the call fails with
`Fail("foreign pointer already held")` after C returns. No new handle is made and the pointer
is not released. This catches a C function that hands back a pointer the wrapper already owns.
The table slot that a pointer result needs is reserved before the call is issued (FFI-7), so
recording a new handle cannot fail.

**Release.** A release never fails or retries on the Hirsel side. An adapter whose C close can
refuse, for example `sqlite3_close`, which returns `SQLITE_BUSY` while statements remain, binds
a function that always ends the object instead, for SQLite `sqlite3_close_v2`. The report's
"a busy close keeps its owner" state therefore never arises, and one release rule covers every
handle. A release that does I/O is declared `waits` and runs on a helper (FFI-7). A finalizer
may call it, shielded ([CONC-7](tasks.md#conc-7-cancellation-points)), and the resource and
the capabilities it holds stay live until the C call returns, because the finalizer is still
running. Hirsel guarantees that a handle reaches a release at most once and is never used
after one. That the finalizer releases it is the wrapper's audited code. A handle still live
when the run ends is leaked to the process and released by its exit.

```hirsel unbuilt
// in module sqlite of package sqlite
pub opaque resource type Db { f: Foreign[Sqlite], p: foreign.Ptr }
  drop(d) { let mut e = list.new(); let _ = native.close(d.f, d.p, mut e) }
```
```hirsel unbuilt
// in module sqlite of package sqlite
pub type Leak { p: foreign.Ptr }                                  // refused: FFI-6, a handle field outside a resource
```
```hirsel unbuilt
// in module sqlite of package sqlite
pub fn raw(d: Db) -> foreign.Ptr { d.p }                          // refused: FFI-6, a handle in a pub signature
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
   ([CONC-7](tasks.md#conc-7-cancellation-points)). A pending request is observed first, and the
   call fails with `Cancelled` with nothing else done.
2. **Checks.** The handle checks of FFI-6 run, and the handle arguments become in flight.
3. **Admission.** The call takes a free helper and, when its result is `Option[foreign.Ptr]`,
   reserves a handle-table slot. When no helper is free, the task waits for one. Every moment
   it waits is a cancellation point. If it observes cancellation there, its handles return to
   live and it fails with `Cancelled`, and no C has run. Nothing is charged to any limit.
4. **Issue.** The helper enters the C function. From here cancellation does not reach the
   call. A request made during it stays pending and is observed at the task's next
   cancellation point. No deadline applies.
5. **Delivery.** When C returns, the handle states change as FFI-6 says, the pointer result
   becomes a handle or `None`, the helper returns to the pool and the task resumes with the
   result.

A task parked at steps 4 and 5 is in an **issued** foreign call. It is waiting on the host, so
it is an outstanding operation under [FAIL-15](failure.md#fail-15-run-stops)'s `Deadlock`
rule. A task at step 3 waits only while every helper runs an issued call, so the run is never
deadlocked while one waits. While a helper runs, the
other tasks keep running. **Shutdown with a call in flight**: a run ends only after its tasks
end, and a task parked in a `waits` call ends only after delivery. Its own cancellation and
cleanup wait for C as well ([CONC-8](tasks.md#conc-8-abandoned-handles)), so an orderly end
never abandons an issued call. A run stop, a second SIGTERM or SIGINT, a test timeout or
any other termination by the host ends the process with the call still running. No release
runs then, and what the native code had not yet written is lost. This is the same outcome as
a killed process.

**Native work is accounted for separately.** A foreign call takes the one step of its call
expression. The native code's time takes no steps, a step bound does not bound it, and a slice
end cannot fall inside it ([MEM-30](budgets.md#mem-30-step-bounds)). The memory the native code
allocates is host memory outside every limit, as the host memory behind a provider operation
is ([CAP-5](capabilities.md#cap-5-provider-operations)). A native allocation failure is a
result of the C function, and the wrapper maps it to its own error. A program that needs
native time or memory bounded relies on its package's audited bounds, such as SQLite's heap
limit. Hirsel charges for neither.

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
of `G` in the reach, the one in the instance of least identity. `E` is the entry.

| command | outcome |
|---|---|
| `hirsel build --recording` and `hirsel run --record PATH` | the build is refused: `FFI-8: a recording build cannot reach foreign code: E reaches G` |
| `hirsel replay app --tape PATH`, with or without `--diagnose` | exits 2, before reading the tape, with `replay: refused: FFI-8: a recording build cannot reach foreign code: E reaches G` |
| `hirsel test --replay`: each selected foreign-reaching test | fails before its live recording half is built, with that test's line ending in `refused: FFI-8: a recording build cannot reach foreign code: E reaches G`. The other tests run as usual. |
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
**archive-set digest**: SHA-256 over the module's archive table, each row written in ascending
order of target as the target text, the 32-byte pin and the file's byte length as an unsigned
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
`pub opaque type Sqlite {}` in module `sqlite`. The first archive row is
`x86_64-unknown-linux-gnu`. The package's directory also holds the adapter source, the
amalgamation's version and SHA3-256 as sqlite.org publishes them, and the exact build command,
so that the pinned archive can be rebuilt and checked.

**Build configuration.** The archive is built with `SQLITE_OS_OTHER=1`,
`SQLITE_THREADSAFE=1`, `SQLITE_TEMP_STORE=3`, `SQLITE_OMIT_LOAD_EXTENSION`,
`SQLITE_OMIT_DESERIALIZE`, `SQLITE_USE_URI=0`, `SQLITE_DQS=0` and
`SQLITE_DEFAULT_FOREIGN_KEYS=1`. It is built with no other `SQLITE_` option and no extension.
`SQLITE_THREADSAFE=1` (serialized) is chosen because tasks move between threads, helpers are
threads, and a statement and its connection are separate handles that two tasks may use at
once. Under `SQLITE_OS_OTHER=1` that setting alone selects SQLite's no-op mutex, so the adapter
supplies real ones: before its first `sqlite3_initialize`, run once per process with
`pthread_once`, it calls `sqlite3_config(SQLITE_CONFIG_MUTEX, &m)`, where `m` implements the
`sqlite3_mutex_methods` over `pthread_mutex_t`, with `PTHREAD_MUTEX_RECURSIVE` for
`SQLITE_MUTEX_RECURSIVE` and the static mutexes allocated once. SQLite then serializes any two
calls on one connection. When the adapter reads an error message, it holds
`sqlite3_db_mutex` across the failing call and `sqlite3_errmsg`, so another call on the same
connection cannot replace the message in between.

**OS layer.** With `SQLITE_OS_OTHER` the adapter supplies `sqlite3_os_init` and
`sqlite3_os_end`, and it registers its VFSes itself. The OS layer gives SQLite no ambient
input:

- `xRandomness` fills its buffer with zero bytes. SQLite's PRNG then always starts from the
  same seed, so SQL `random()` and `randomblob()` are a fixed sequence, not a source of
  randomness. A program that needs random bytes binds bytes from `Random`.
- `xCurrentTime` and `xCurrentTimeInt64` return `SQLITE_ERROR`, so `'now'` in a date
  function gives `NULL`. A program that needs the time binds a value from `Clock`.
- `xSleep` returns at once, having slept 0 microseconds. A busy handler therefore never waits
  on the clock.
- `xDlOpen` and the other loader hooks are null.

### Slice 1: in-memory databases

The only VFS is `hirsel-none`. Its `xOpen` refuses every name with `SQLITE_CANTOPEN`, its
`xAccess` reports every name as absent, and its `xDelete` returns `SQLITE_IOERR_DELETE`. A
connection is opened with `sqlite3_open_v2(":memory:", &db, SQLITE_OPEN_READWRITE |
SQLITE_OPEN_CREATE | SQLITE_OPEN_EXRESCODE, "hirsel-none")`, with no URI flag. Right after
opening, the adapter installs an authorizer that denies `SQLITE_ATTACH` and `SQLITE_DETACH`
whatever their arguments, so a computed attach name is denied too. Temporary storage is in
memory (`SQLITE_TEMP_STORE=3`), and any other file that SQLite tries to open, `VACUUM INTO`
included, reaches `hirsel-none` and is refused.

The public API, in module `sqlite`:

```hirsel unbuilt
pub opaque type Sqlite {}
pub type SqliteError { code: i32, message: String }
pub type Step = Row | Done
pub opaque resource type Db { f: Foreign[Sqlite], p: foreign.Ptr }
  drop(d) { let mut e = list.new(); let _ = native.close(d.f, d.p, mut e) }
pub opaque resource type Stmt { f: Foreign[Sqlite], p: foreign.Ptr }
  drop(s) { let mut e = list.new(); let _ = native.finalize(s.f, s.p, mut e) }
pub fn open_memory(f: Foreign[Sqlite]) -> Result[Db, SqliteError] waits
pub fn exec(mut db: Db, sql: String) -> Result[(), SqliteError] waits
pub fn prepare(mut db: Db, sql: String) -> Result[Stmt, SqliteError] waits
pub fn bind_i64(mut s: Stmt, i: u32, v: i64) -> Result[(), SqliteError]
pub fn bind_f64(mut s: Stmt, i: u32, v: f64) -> Result[(), SqliteError]
pub fn bind_bytes(mut s: Stmt, i: u32, v: Bytes) -> Result[(), SqliteError]
pub fn bind_text(mut s: Stmt, i: u32, v: String) -> Result[(), SqliteError]
pub fn bind_null(mut s: Stmt, i: u32) -> Result[(), SqliteError]
pub fn step(mut s: Stmt) -> Result[Step, SqliteError] waits
pub fn column_kind(s: Stmt, i: u32) -> Kind
pub fn column_i64(s: Stmt, i: u32) -> i64
pub fn column_f64(s: Stmt, i: u32) -> f64
pub fn column_bytes(s: Stmt, i: u32, mut out: Bytes)
pub fn reset(mut s: Stmt) -> Result[(), SqliteError] waits
pub fn finalize(take s: Stmt) -> Result[(), SqliteError] waits
pub fn close(take db: Db) -> Result[(), SqliteError] waits
pub type Kind = Integer | Float | Text | Blob | Null
```

- **Contract.** `open_memory`, `exec`, `prepare`, `step`, `reset`, `finalize` and `close` are
  `waits` in both slices. They can touch disk once slice 2 lands, and `step` and `exec` can
  run for as long as a query does. Declaring them `waits` now keeps one API across both slices.
  The `bind_` and `column_` functions run on the host stack. A statement call passes the
  statement's handle only, and the adapter reaches its connection with `sqlite3_db_handle`. The adapter binds every text and
  blob with `SQLITE_TRANSIENT`, so SQLite copies them before the bind returns and keeps no
  loaned pointer. `column_bytes` appends the column's bytes to `out`, through
  `column_len`/`column_copy` as in FFI-5, before any other call on the statement. An empty
  blob and `NULL` are told apart by `column_kind`.
- **Errors.** Every native function that can fail takes a last parameter `mut e: Bytes`, an
  **error buffer** that the wrapper's private `error_buffer()` makes as 260 zero bytes with
  `list.fill`. A native function with an `i32` result returns the extended result code. On
  failure the adapter writes that code as 4 little-endian bytes at offset 0, then the bytes of
  `sqlite3_errmsg` from offset 4. It writes at most 256 of them, cut at a UTF-8 character
  boundary, and leaves the rest zero. `open_memory`'s `None` means failure, and its code is
  read from the buffer. The adapter writes nothing into a buffer shorter than 260 bytes, so a
  finalizer passes an empty `list.new()` and allocates nothing. The wrapper reads the code with `list.word_le[i32](e, 0)` and the
  message as the bytes from offset 4 up to the first zero byte, converted by
  `string.from_utf8`, in its private `read_error(e)`. A `SqliteError` is that code and message.
- **Release.** `close` binds `sqlite3_close_v2` and `finalize` binds `sqlite3_finalize`.
  Each consumes its handle whatever it returns (FFI-6). The public `close` and `finalize` end
  their resource by a whole destructure, so its finalizer does not run
  ([FAIL-11](failure.md#fail-11-resources)), and then release the handle once:

  ```hirsel unbuilt
  pub fn close(take db: Db) -> Result[(), SqliteError] waits {
    let Db(f: f, p: p) = take db          // db ends without its finalizer
    let mut e = error_buffer()
    let rc = native.close(f, p, mut e)
    if rc == 0 { Ok(()) } else { Error(read_error(e)) }
  }
  ```

  A `Db` or `Stmt` dropped without these is released by its finalizer, and the result is
  discarded. A statement may outlive its connection's `close`. `sqlite3_close_v2` then returns
  `SQLITE_OK` and keeps the connection as a zombie until the last statement is finalized, so
  every later call on the statement is a valid C call and returns SQLite's own code.
- **A refused admission.** When a `Db` or `Stmt` literal returns `Error((c, (f, p)))`, the
  wrapper releases `p` with `native.close` or `native.finalize`. It then returns
  `Error(SqliteError(code: 7, message: "sqlite: no capacity for the handle"))`, where 7 is
  `SQLITE_NOMEM`.
- **Native bounds.** A query's native time and SQLite's heap are not charged (FFI-7).
  `sqlite3_interrupt`, progress-handler budgets and a heap limit are later work
  ([reserved](reserved.md), R5).

### Slice 2: databases in a granted directory

Slice 2 adds persistent databases under a granted `Dir`. Every file that SQLite opens, deletes
or checks for the database goes through that directory: the database, its rollback journal,
its WAL, its `-shm` and any super-journal. Temporary files never reach a directory.

- **A new ABI form.** A foreign parameter `d: Dir` passes to C two parameters,
  `int d_fd, uint32_t d_flags`. `d_fd` is a new `O_PATH | O_DIRECTORY | O_CLOEXEC`
  descriptor for the directory, which the callee owns from then on and must close exactly
  once. Bit 0 of `d_flags` is set when the `Dir` permits writing. This is the one ownership
  transfer in the ABI, and its release contract is the adapter's: it closes the descriptor when
  the last SQLite file that uses it is closed. A `Dir` made by virtual construction, or by the
  model world, has no descriptor, and the call fails with
  `Fail("foreign call needs a host Dir")` before C runs. The run makes the descriptor at the
  call's issue. A failure to make it, such as the descriptor table being full, is delivered as
  `d_fd = -1`, and the callee returns its own error.
- **API.** `pub fn open(f: Foreign[Sqlite], d: Dir, name: String) -> Result[Db, SqliteError] waits`.
  `name` is 1 to 200 bytes of `A`–`Z`, `a`–`z`, `0`–`9`, `.`, `_` and `-`. It does not start
  with `.`, and it does not end with `-journal`, `-wal` or `-shm`. Any other name gives
  `SqliteError(code: 14, message: "sqlite: name is not one leaf in the directory")`
  (`SQLITE_CANTOPEN`) before C runs. A read-only `Dir` opens with `SQLITE_OPEN_READONLY`.
- **The `hirsel-dir` VFS.** The adapter registers one VFS, `hirsel-dir`, and passes its name to
  `sqlite3_open_v2`. `xFullPathname` maps a leaf to `/hirsel-dir/<k>/<leaf>`, where `k` is the
  adapter's index of the root descriptor, which is reference-counted by each file that SQLite
  opens under it. `xOpen`, `xAccess` and `xDelete` take `k` and the leaf from that name, check
  the leaf by the rule above extended with exactly the suffixes `-journal`, `-wal` and `-shm`,
  and act through `openat2(root, leaf, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
  RESOLVE_NO_MAGICLINKS)`, `faccessat` and `unlinkat` on the root descriptor. A name that fails
  the check, including a super-journal name read from a journal on disk, gives
  `SQLITE_CANTOPEN`, so recovery fails and never escapes the directory. Only regular files are
  opened. The file methods are real: offset reads that zero-fill short reads, writes,
  `ftruncate`, `fsync` for `xSync` and `fsync` of the root descriptor after creating or deleting
  a journal, POSIX advisory locks for the five lock levels, and `SQLITE_NOTFOUND` for unhandled
  file controls. On a failed open `pMethods` stays null after the partial state is cleaned. WAL
  uses real `xShmMap`, `xShmLock`, `xShmBarrier` and `xShmUnmap` over the `-shm` file opened
  under the same root, mapped shared, and locked with `fcntl` byte-range locks, so that two
  connections, in one process or two, see one WAL index. `xShmUnmap` with delete removes the
  `-shm` file with `unlinkat` on the root. A null-name open, which SQLite makes for temporary
  and sort files that `SQLITE_TEMP_STORE=3` did not keep in memory, gets an in-memory file of
  the adapter, never a host file. Hard links and bind mounts inside the directory are files of
  the directory. A rename of the directory while it is open does not move the descriptor.
- **Refused selection.** URI filenames are off globally (`SQLITE_USE_URI=0`, no
  `SQLITE_CONFIG_URI`) and in every open, so `file:` and `vfs=` have no meaning and name a
  leaf, which the leaf rule refuses. The authorizer denies `ATTACH` and `DETACH` as in slice 1.
  `VACUUM INTO` names a leaf of the same directory through `hirsel-dir`, the only VFS a
  connection uses.
- **Durability.** A commit is durable as SQLite's own contract makes it, over `xSync` that
  really syncs. The sync barriers are SQLite's, not `host.sync`'s, and a SQLite database is not
  the store's durability contract ([CAP-9](capabilities.md#cap-9-recording-is-outside-the-program)).

## Laws

Each law's expected value comes from this chapter, from a published test vector or from the
fixture's own counters. None comes from a run's output. Laws FL1 to FL7 use the fixture
package `ffi_laws` in the program tests. Its adapter `laws.c` defines, with prefix `hfl_`:
`make` (returns a fresh `malloc(1)` and increments `made`), `free` (frees, increments `freed`),
`freed` (returns `freed`), `mark` (writes `0xAB` to each of its `len` bytes and returns `len`)
and `touch` (writes `touched\n` to file descriptor 2 with `write(2)` and returns 0). Its
interface is `Laws`.

| law | bug | oracle and setup |
|---|---|---|
| **FL1** a wrong archive digest | a pin checked after linking, or not at all | `ffi_laws`' x86_64 row names a file whose bytes are exactly `abc`, pinned to the SHA-256 of the empty string. `hirsel build` of an entry that calls `native.make` is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu does not match its pin: expected sha256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855, found sha256 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad` (FIPS 180-2 vectors), and the linker is never started. With the pin changed to `ba78…15ad`, the same build is refused with `… is not an indexed ar archive`, so the digest is checked before the bytes are parsed. |
| **FL2** a missing target row | a missing row passed through as a linker error, or a row required when nothing is reached | With only the x86_64 row, `hirsel build --target aarch64-unknown-linux-gnu` of the FL1 entry, with its real pin, is refused with `FFI-3: ffi_laws@1/native has no archive for target aarch64-unknown-linux-gnu; its archives are for x86_64-unknown-linux-gnu`, before the linker starts. An entry of the same package that never names `native` gets no FFI-3 refusal for aarch64. |
| **FL3** double release | a second release that reaches C | A test takes `f: Foreign[Laws]`, makes `p` with `native.make`, calls `native.free(f, p)`, then calls `native.free(f, p)` again inside an `isolate`. The `isolate` returns `Ok(Error(causes))`, with primary cause `Fail("foreign handle released")` at the second `free`, and then `native.freed(f)` returns 1. |
| **FL4** stale use after release | a released handle's pointer passed to C, or a copy that escapes the check | As FL3, but the second call after the release is `native.free(f, q)`, where `q` is a copy of `p` that a resource field held. The cause is the same, and `freed` returns 1. Releasing one handle from two tasks at once makes one call fail with `Fail("foreign handle in use")` or `Fail("foreign handle released")`, and `freed` returns 1. |
| **FL5** a `mut Bytes` loan that would expose spare capacity | capacity passed as the length | `b = list.with_capacity(16)`, then `list.fill(mut b, 4, 0)`. `native.mark(f, mut b)` returns 4. Afterwards `list.len(b)` is 4 and every byte of `b` is `0xAB`. Then `list.push(mut b, 0)` admits nothing, because the capacity is still 16. An empty `b` gives `mark` 0, and the adapter checks that `ptr` is not `NULL`. |
| **FL6** an FFI-reaching test refused a cached pass | a pass key made for a test whose reach holds C | A parameterless test calls `probe(None)`, where `fn probe(f: Option[Foreign[Laws]]) -> u64 { match f { Some(f) => native.freed(f), None => 0 } }`, and passes. Run `hirsel test` twice with one cache: both runs report it executed, and neither reports it reused. A parameterless test beside it that does not call `probe` is reported reused on the second run. The same holds when the only path to `native` is a resource's finalizer. |
| **FL7** `--replay` refused before C runs | a live recording half that enters native code | `hirsel test --replay` of a test that calls `native.touch` fails with a line ending `refused: FFI-8: a recording build cannot reach foreign code: ffi_laws@1/tests/touches reaches ffi_laws@1/native/touch`. Its stderr holds no `touched`, and no recording file exists. `hirsel replay` of an entry that reaches `touch`, given any tape path, exits 2 with `replay: refused: FFI-8: …` and does not open the tape. `hirsel build --recording` is refused the same way. |
| **FL8** SQLite round trip | a loan, `SQLITE_TRANSIENT` or `NULL`/empty mistake | In `sqlite`'s own tests, with grant `Foreign=sqlite@1/sqlite/Sqlite`: create `t(b blob)`, insert `x''`, `NULL` and `x'00ff'` with `bind_bytes` and `bind_null`, after which the bound list is cleared and refilled. Selecting in insertion order gives `Blob` with 0 bytes, `Null`, and `Blob` with `00 ff`. |
| **FL9** SQLite release paths | an explicit `close` or `finalize` whose finalizer runs as well, or a busy close | Four tests, each preparing `s` from `select 1` on a fresh connection `db`: (a) `finalize(take s)` then `close(take db)`; (b) `close(take db)` then `finalize(take s)`; (c) `close(take db)` with `s` dropped at scope end; (d) both dropped at scope end, in either order. Every explicit call returns `Ok(())`, because `sqlite3_close_v2` returns `SQLITE_OK` with a statement pending. Every test passes with no cause, so no finalizer met a consumed handle. |
| **FL10** no ambient input | the OS layer reading the clock or the host's randomness | `select random()` returns the same value in two runs of a fresh process. `select date('now')` returns `NULL`. `attach ':memory:' as x` fails with `SQLITE_AUTH` (23). |
| **FL11** directory confinement (slice 2) | a sidecar, temp file or attach outside the grant | In a granted scratch `Dir` whose parent holds a sentinel directory: open `a.db`, run a WAL transaction from two connections, then a rollback-journal transaction. The files created are exactly `a.db`, `a.db-wal` and `a.db-shm` (WAL), and `a.db-journal` only while the journal transaction runs. The parent and the sentinel are unchanged. `open(f, d, "../x")` and `open(f, d, "file:a.db?vfs=unix")` give code 14 before C runs. A journal whose super-journal name is `/tmp/evil` fails recovery with `SQLITE_CANTOPEN`. A `sort` over 10⁶ rows creates no file. |
| **FL12** an archive whose code is outside its pin | a thin archive, or a member that is not a relocatable object, admitted because its digest matches | `ffi_laws`' x86_64 row names an indexed thin archive made with `ar --thin` from the adapter's `laws.o`, which lies beside it, pinned to the thin archive's true SHA-256. `hirsel build` of the FL1 entry is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu is a thin archive; its members are not inside the pinned bytes`, and the linker is never started. With the row naming a regular archive whose one member is `laws.o` compiled for `aarch64`, again with its true pin, the refusal is `… member laws.o is not a relocatable object for x86_64-unknown-linux-gnu`. With a regular x86_64 archive whose index lists `hfl_make` but whose member does not define it, the refusal is `… does not define hfl_make, declared at SITE`. |
| **FL13** a recipe-only build of a foreign program | a recipe from which a reader cannot recompute the foreign address, or one that links archives the leaves do not select | `ffi_laws` gets a second row, `aarch64-unknown-linux-gnu`, naming any file with its true pin. `hirsel publish` of the FL1 entry for x86_64 prints R. Delete the package's directory: `hirsel build --recipe R --code-store DIR` succeeds, and the executable's `native.make` call works. Change only the aarch64 row's bytes and pin and publish again: the new R differs from R (FFI-9), and it builds from the code store alone the same way. A recipe made from R's bytes by removing the x86_64 archive's entry from its foreign archives part, with its objects in the store, is refused as `Missing`, and one made by adding the aarch64 archive's entry as `Extra`. |

## Build slices

1. FFI-1 to FFI-9 with the in-memory `sqlite` package (slice 1), and laws FL1 to FL10, FL12 and FL13.
2. The `Dir` ABI form and the `hirsel-dir` VFS (slice 2), and law FL11.
