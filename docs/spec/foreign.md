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

FFI-1 to FFI-9 govern native archives. The [`sqlite` package](#the-sqlite-package) is the
acceptance consumer. FFI-10 to FFI-16 govern [contained foreign code](#contained-foreign-code).
The contained module runs as WebAssembly in the embedded Wasmtime and stays inside replay.
The foreign examples keep the `` ```hirsel unbuilt `` marker because the fence checker
cannot supply their manifest. The foreign laws check the examples' refusal cases.

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
contained boundary is required before any unaudited extension is accepted. The Wasm boundary
is FFI-10 to FFI-16 ([contained foreign code](#contained-foreign-code)); FFI-1's trust and
FFI-4's launch rules apply only to native modules. A restricted worker process per interface
or package stays a later design ([reserved](reserved.md), R5). Moving only the `waits` calls out of process is not that
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
only self-contained archives whose members are inside those bytes.

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
| every member holds only admitted object features: each section's type is `SHT_NULL`, `SHT_PROGBITS`, `SHT_NOBITS`, `SHT_SYMTAB`, `SHT_STRTAB`, `SHT_RELA`, `SHT_REL`, `SHT_NOTE`, `SHT_GROUP`, `SHT_LLVM_ADDRSIG`, and for x86-64 `SHT_X86_64_UNWIND` or for AArch64 `SHT_AARCH64_ATTRIBUTES`; no section is named `.init`, `.fini`, `.ctors`, `.dtors`, `.init_array`, `.fini_array` or `.preinit_array`, or one of these followed by `.` and more, and no section's name is a C identifier, for which the linker would define `__start_` and `__stop_` symbols that other code iterates; a note section is `.note.GNU-stack` or `.note.gnu.property`; the member has a `.note.GNU-stack` without `SHF_EXECINSTR`; no symbol has type `STT_GNU_IFUNC` or binding `STB_GNU_UNIQUE`; and no relocation is `R_X86_64_IRELATIVE` or `R_AARCH64_IRELATIVE` | `FFI-3: archive F of P for T: member N has X, which is not an admitted object feature`, `X` one of `section NAME of type TYPE`, `no .note.GNU-stack`, `an executable stack`, `indirect function S`, `symbol S with binding STB_GNU_UNIQUE`, `an IRELATIVE relocation` (the first in section, then symbol, then relocation order) |
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
it. A member that needs another library never pulls one in: the import check below refuses
it before the linker runs.

The member, symbol, feature, prefix and ABI rows of the table are written for an ELF target
(`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`). The other rows, and the import
check, apply to every target.

**Mach-O members** (`aarch64-apple-darwin`; macOS decision D1). For a Mach-O
target the ELF rows are replaced by these, with the same diagnostics. Names are LLVM's
`BinaryFormat/MachO.h`. A symbol `S` in a diagnostic is its C name, without the leading `_`,
and a feature `X` is the first in load-command order (a segment's sections where its command
stands), then in symbol order. The target's **floor** is macOS 26.0 (macOS decision D2), the first
release whose kernel enforces `O_RESOLVE_BENEATH`; a Mac law that shows an earlier release
enforcing it lowers the floor to that release. The floor and the platform number come from
the target's Apple record ([EMB-2](embed.md#emb-2-apple-target-records)). For the iOS
targets these rows apply with that record's platform (iOS 2 for `aarch64-apple-ios`, iOS
simulator 7 for `aarch64-apple-ios-sim`) and floor in place of macOS and 26.0
([EMB-8](embed.md#emb-8-refusals)).

| check | ELF row it replaces |
|---|---|
| every member is a 64-bit Mach-O relocatable object (`MH_MAGIC_64`, `MH_OBJECT`) with `CPU_TYPE_ARM64` and `CPU_SUBTYPE_ARM64_ALL` (arm64, never arm64e), whose `LC_BUILD_VERSION` names platform macOS and a minimum version no later than the target's floor, macOS 26.0 | the `ET_REL` row |
| every load command is `LC_SEGMENT_64`, `LC_SYMTAB`, `LC_DYSYMTAB`, `LC_BUILD_VERSION`, `LC_DATA_IN_CODE` or `LC_LINKER_OPTIMIZATION_HINT`. `LC_LINKER_OPTION`, which asks the linker to load a library or framework, is refused like every other command | none; the feature `load command NAME` |
| each reached symbol S is defined in one member as an external (`N_EXT`) symbol of type `N_SECT` in a section with `S_ATTR_SOME_INSTRUCTIONS` or `S_ATTR_PURE_INSTRUCTIONS`, under its C name with the platform's leading `_`, and the index's entry names that member | the `STB_GLOBAL`/`STT_FUNC` row |
| each section's type (the low byte of its flags) is `S_REGULAR`, `S_ZEROFILL`, `S_CSTRING_LITERALS`, `S_4BYTE_LITERALS`, `S_8BYTE_LITERALS`, `S_16BYTE_LITERALS`, `S_LITERAL_POINTERS`, `S_COALESCED`, `S_THREAD_LOCAL_REGULAR`, `S_THREAD_LOCAL_ZEROFILL`, `S_THREAD_LOCAL_VARIABLES` or `S_THREAD_LOCAL_VARIABLE_POINTERS`. So `S_MOD_INIT_FUNC_POINTERS`, `S_MOD_TERM_FUNC_POINTERS`, `S_INIT_FUNC_OFFSETS`, `S_THREAD_LOCAL_INIT_FUNCTION_POINTERS`, `S_INTERPOSING` and `S_DTRACE_DOF` are refused, and so is a section in segment `__LLVM` (bitcode). No symbol has `N_SYMBOL_RESOLVER`, the Mach-O indirect function | the object-feature row; `X` adds `section SEG,SECT of type TYPE` and `symbol resolver S` |
| every external or weak (`N_WEAK_DEF`) symbol a member defines starts with `_` followed by `M`'s symbol prefix | the prefix row |
| exactly one section `__TEXT,__hirsel_abi` holds the ABI text | the `.hirsel_abi` row |

A section-boundary symbol (`section$start$SEG$SECT`, `segment$start$SEG`) is the linker's
way of letting code iterate a section, as `__start_` is on ELF. A member that imports one is
refused by the import check, because no admitted input defines it.

**Imports** (every target). Before the linker runs, every symbol a member leaves undefined
(an ELF global or weak symbol in `SHN_UNDEF`; a Mach-O external `N_UNDF` symbol with no
size, or an `N_PBUD` one), weak or not, must be defined by another member of the same archive, another admitted
archive, the runtime, the program's generated objects, or another link input of the image
(the libc and the system libraries the toolchain links, as the collision row reads them),
and nothing else. Otherwise: `FFI-3: archive F of P for T: member N imports S, which no
admitted input defines`, for the first such import in ascending order of module definition
identity, then member, then symbol table order. On `aarch64-apple-darwin` the only system library is the libSystem
stub, generated from the Hirsel-owned libSystem allowlist
(macOS decision D3). The Security and CoreFoundation stubs that the runtime links for
`Trust.System` define nothing an archive may import.
That allowlist leaves out `fsync` and `fdatasync`, which on macOS return before the drive
has flushed. A C library that calls them is refused for Darwin with the symbol named. Its
recipe answers by defining the call itself, for example with a package function that calls
`fcntl(F_FULLFSYNC)`, so a database inside a foreign archive never has weaker durability on
macOS than the Store. Package authors build a Darwin row on any host with the pinned clang
and the pinned Zig's Darwin headers at build time. Hirsel redistributes no Apple file.

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
definition, placed in section `.hirsel_abi` (`__TEXT,__hirsel_abi` on Mach-O), of a `const char` array holding the module's ABI text with no
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
recipe's other objects are. The recipe's root is `R = H("hirsel.release-recipe.v4", recipe)`
([launch](launch.md#release-recipe-and-host-assembly)); the version-1, version-2 and version-3 domains are never reused. A program that reaches no foreign function has an empty foreign modules part.
Its foreign archives part holds the pins its entry grants select, if any. Recipe admission walks the leaves as launch's admission steps 6 and 7 say. So
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
`not granted: Foreign[D]`, one line for each such interface in parameter order ([PROG-3](programs.md#prog-3-entry)). A `Foreign` grant whose `D`
names no interface of the program is refused before the run as an unknown grant. In the
release recipe a grant binding of kind `Foreign` holds `D` as its one value, followed by
the SHA-256 and byte length of its interface's archive for the recipe target. The pin
is in the recipe's foreign archives part even when the entry reaches no call to that
interface. The manifest's `grants` list shows the binding as the string `Foreign=D`.

The grant is explicit at launch (FFI-1). A test configuration's `"Foreign=D"` string says
which tests take it, and `hirsel test` gives it to them only when its own command line also
holds `--grant Foreign=D`; without that, each such test is not started and is reported failed
with `not granted: Foreign[D]: pass --grant Foreign=D to hirsel test`, one line for each grant
it is not given (`Exec` lines first, then `Foreign`, each in parameter order), and the other tests run.
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
```hirsel
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
checks the kind again at every call. The type is plain, `send` (except for a contained kind,
FFI-12), and 8 bytes, and it is not `shareable`, `key` or `codec`. Its value is a run-local ordinal, never the C pointer. The run
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
connection's mutex, it has no busy handler, its only sleep is the finite WAL read retry on
the Directory VFS, and its file locks never block (slice 2).
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

An entry is **foreign-reaching** when its checked reach holds a function of a native foreign
module. A function of a contained module does not make it foreign-reaching (FFI-14). The reach
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
| A recording group (`hirsel register --record DIRECTORY`): a registration or deploy whose recipe has a foreign leaf | `register` refuses it before it creates DIRECTORY or connects, with `register: refused: FFI-8: a recording build cannot reach foreign code: E reaches G`. A deploy is refused at the recipe's admission, after admission step 1 and before any object is fetched, assembled, published, previewed or launched, as `Failed(Denied)`, printed `deploy: denied: FFI-8: a recording build cannot reach foreign code: E reaches G`. The same check runs on every assembly with the recording runtime, and on a cached assembly record before it is used. |
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
The domain `hirsel.code-foreign.v1` is added by this chapter. A contained function's address
is FFI-16's.

## The `sqlite` package

The acceptance consumer is the package `sqlite@1`, at `core/programs/sqlite`. Its foreign
module `native` binds an **adapter**. This is the C file `hirsel_sqlite.c`, with its OS layer
(the mutexes and the memory VFS) in `hirsel_sqlite_os.c` and `hirsel_sqlite_os.h`, compiled
together with the SQLite amalgamation into one static archive per target. The adapter is the audited
code: it calls the SQLite API and gives each call the FFI-5 shape. The interface is
`pub opaque type Sqlite {}` in module `sqlite`, and the kinds are `DbHandle` and `StmtHandle`
there. Its archive rows are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and
`aarch64-apple-darwin`.

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
  `clang`, `ld.lld`, `llvm-objcopy` and `llvm-ar`, and no host compiler. The
  `aarch64-unknown-linux-gnu` row reads the pinned Arm sysroot's glibc headers
  (`HIRSEL_AARCH64_SYSROOT`); the `aarch64-apple-darwin` row reads the pinned Zig's Darwin
  headers (`lib/libc/include/any-darwin-any` at the link kit's Zig commit,
  `ZIG_DARWIN_INCLUDE`).
- **Options.** `-DSQLITE_OS_OTHER=1 -DSQLITE_THREADSAFE=1 -DSQLITE_TEMP_STORE=3
  -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_DESERIALIZE -DSQLITE_OMIT_LOCALTIME
  -DSQLITE_USE_URI=0 -DSQLITE_DQS=0 -DSQLITE_DEFAULT_FOREIGN_KEYS=1`, and no other
  `SQLITE_` option and no extension.
- **Steps.** For each ELF row `T`, and each of `sqlite3.c`, `hirsel_sqlite.c` and `hirsel_sqlite_os.c`: `clang --target=T -std=c11 -O2
  -fPIC -fno-ident -ffile-prefix-map=$PWD=. OPTIONS -c F.c -o F.o`. Then `ld.lld -r sqlite3.o
  hirsel_sqlite.o hirsel_sqlite_os.o -o hsq.o`; `llvm-objcopy --wildcard --keep-global-symbol='hsq_*' hsq.o`,
  which makes every SQLite symbol local so the one member's global definitions all carry the
  prefix (FFI-3); and `llvm-ar rcs libhirsel_sqlite.a hsq.o`, which writes no timestamps or
  owners. The `aarch64-unknown-linux-gnu` row is `libhirsel_sqlite-aarch64-unknown-linux-gnu.a`,
  built alike with `--sysroot`. ld64 has no relocatable link that makes symbols local, so the
  `aarch64-apple-darwin` row compiles one unit that includes the three files with
  `SQLITE_API`, and the OS layer's shared names, `static`: `clang
  --target=arm64-apple-macos26.0 -nostdlibinc -isystem DARWIN -std=c11 -O2 -fno-ident
  -D_FORTIFY_SOURCE=0 -DSQLITE_WITHOUT_ZONEMALLOC OPTIONS`, then `llvm-ar --format=darwin rcs
  libhirsel_sqlite-aarch64-apple-darwin.a`. On Darwin the adapter syncs with
  `fcntl(F_FULLFSYNC)`, never `fsync`, and opens beneath its directory with `openat` and
  `O_RESOLVE_BENEATH | O_NOFOLLOW_ANY`. The build is byte-reproducible: two runs give one SHA-256, and the pin is that
  digest. The adapter includes the header that `hirsel inspect --foreign-header
  sqlite@1/native` prints, and expands `HIRSEL_ABI_TEXT` once (FFI-3).
- **Native laws.** `build.sh --test DIR` builds no archive: it links `native_laws.c` against
  the checked-in archive, with the SQLite symbols the laws call made global again and `fcntl`
  wrapped to time a peer's release, and runs it in a fresh directory under `DIR`. Its cases
  are the `-shm` allocation law (it needs `DIR` on a filesystem with 32 to 64 KiB free, such
  as a `tmpfs` mounted with `size=64k`), the three dead-man opener cases, the trusted-schema
  and persistent-prepare settings, `POWERSAFE_OVERWRITE` clear, and the two `xSleep`s.
  `//:core-sqlite-native-laws` runs it on the host, in a 64 KiB tmpfs the test launcher
  mounts, with `SQLITE_HEADER` naming 3.54.1's `sqlite3.h` in place of the fetched amalgamation.

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
- No busy handler is installed, so a busy database returns `SQLITE_BUSY` at once. The memory
  VFS's `xSleep` returns at once, having slept 0 microseconds. The Directory VFS's `xSleep`
  (slice 2) sleeps the requested microseconds with `nanosleep` and returns them: its only
  caller is SQLite's finite WAL read retry, whose delays total under 10 seconds before it
  returns `SQLITE_PROTOCOL`, and it runs on the call's helper thread, never a scheduler
  thread. File locks stay nonblocking.
- `xDlOpen` and the other loader hooks are null.
- **The authorizer.** Right after a connection opens, the adapter installs, with
  `sqlite3_set_authorizer`, an authorizer that returns `SQLITE_DENY` for `SQLITE_ATTACH` and
  `SQLITE_DETACH` whatever their arguments, and for `SQLITE_PRAGMA` whose name is, in any
  case, `temp_store_directory`, `data_store_directory`, `soft_heap_limit` or
  `hard_heap_limit`, the pragmas that read or change process globals, and for
  `busy_timeout` with a value, which would install a sleeping busy handler (reading it is
  allowed); it allows everything else. If installing it fails, the open closes the connection and fails with that code. A
  computed attach name is denied too. `VACUUM` and `VACUUM INTO` attach a database internally
  through the same authorizer, so both fail with `SQLITE_AUTH` (23) when they are stepped. The
  null-name files SQLite opens are the adapter's in-memory files and never read
  `sqlite3_temp_directory`.
- **Untrusted schema.** Every open, in memory or in a `Dir`, then sets
  `SQLITE_DBCONFIG_TRUSTED_SCHEMA` to 0 and reads it back; if the call fails or the value is
  not 0, the open closes the connection and fails, so a database file's schema cannot run
  functions or virtual tables that are not marked innocuous.

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
  its code. `prepare` prepares the first statement the same way, but with
  `SQLITE_PREPARE_PERSISTENT`, since the statement it returns is held and reused, and then
  prepares the rest:
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
  two, see one WAL index and exclude each other. An opener first queries the dead-man byte
  (offset 128 of the `-shm` file, as SQLite's Unix VFS uses) with `F_OFD_GETLK`, as SQLite's
  `unixLockSharedMemory` does. If another descriptor holds it shared, the opener is admitted
  without truncating. If one holds it exclusive, the open gives `SQLITE_BUSY`, because that
  first opener may have died before its truncation finished, and SQLite retries. If it is
  free, the opener takes it exclusive (`SQLITE_BUSY` if it loses that race) and truncates the
  file to zero, so a stale index from a crash is never read. Every admitted opener then holds
  the byte shared. To extend the file for a new region, the adapter writes the last byte of
  each new 4096-byte page, as SQLite's Unix VFS does, never `ftruncate`, so the file's size
  never runs ahead of the pages written: a full disk gives `SQLITE_IOERR_SHMSIZE` there, before
  the region is mapped, rather than `SIGBUS` on a later store. Like SQLite's, this does not
  make every later fault of the mapping impossible. `xDeviceCharacteristics` reports 0, so
  `SQLITE_IOCAP_POWERSAFE_OVERWRITE` stays clear for an arbitrary granted directory. `xShmBarrier` is a sequentially
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
- `loan_length(f, b: Bytes) -> u64` returns the length of the immutable loan, and
  `loan_sum(f, b: Bytes) -> u64` returns the sum of its bytes;
- `gate(f, on: u32) -> i32`, on the host stack: sets or clears the gate that FL17 uses;
- `touch(f) -> i32`: write `touched` and a line feed to file descriptor 2 and return 0;
- `hold(f) -> i32`, declared `waits`: increment `held`, then wait on a semaphore; `held(f) ->
  u64`; and `open(f, n: u32) -> i32`, on the host stack, which posts the semaphore `n` times.
  `hold` breaks FFI-7's progress clause on purpose, and only `open`, which needs no helper,
  ends its wait.

A **refusal case** is a multi-package case in the program tests' case harness
(`crates/hirsel-core-program-tests/src/cases.rs`, under `core/front/cases/ffi/`), with a real
foreign manifest, and its diagnostic is checked at the stated site. The `` ```hirsel unbuilt ``
fences of this chapter stay documentation.

| law | bug | oracle and setup |
|---|---|---|
| **FL1** a wrong archive digest | a pin checked after linking, or not at all | `ffi_laws`' x86_64 row names a file whose bytes are exactly `abc`, pinned to the SHA-256 of the empty string. `hirsel build` of an entry that calls `native.make` is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu does not match its pin: expected sha256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855, found sha256 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad` (FIPS 180-2 vectors), and the linker is never started. With the pin changed to `ba78…15ad`, the same build is refused with `… is not an indexed ar archive`, so the digest is checked before the bytes are parsed. |
| **FL2** a missing target row | a missing row passed through as a linker error, or a row required when nothing is reached | With only the x86_64 row, `hirsel build --target aarch64-unknown-linux-gnu` of the FL1 entry, with its real pin, is refused with `FFI-3: ffi_laws@1/native has no archive for target aarch64-unknown-linux-gnu; its archives are for x86_64-unknown-linux-gnu`, before the linker starts. An entry of the same package that never names `native` gets no FFI-3 refusal for aarch64. This is a negative test: it gives no AArch64 coverage. |
| **FL3** double release | a second release that reaches C | A test takes `f: Foreign[Laws]`, makes `p` with `native.make`, calls `native.free(f, p)`, then calls `native.free(f, p)` again inside an `isolate`. The `isolate` returns `Ok(Error(causes))`, with primary cause `Fail("foreign handle released")` at the second `free`, and then `native.freed(f)` returns 1. |
| **FL4** stale use and a racing release | a released handle's pointer passed to C, a copy that escapes the check, or two releases that both reach C | As FL3, but the second call is `native.free(f, q)`, where `q` is a copy of `p` that a resource field held: the same cause, and `freed` returns 1. Then two `send` resources `ObjBox` of the fixture each hold a copy of one fresh handle (the fixture breaks FFI-6's one-field rule on purpose), each moves into its own task, and both tasks release it at once with `close_obj`: one returns 0, the other fails with `Fail("foreign handle released")` (it waited for the first and found it consumed, FFI-6), never `in use`, and `freed` returns 2. |
| **FL5** byte loans preserve length, storage and range | capacity passed as the length, storage that moves while a helper writes it, or a read beyond an immutable slice | `let mut b: Bytes = list.with_capacity(16)`, then `list.fill(mut b, 4, 0)`. `native.mark(f, mut b)` returns 4. Afterwards `list.len(b)` is 4 and every byte of `b` is `0xAB`. Then `list.push(mut b, 0)` admits nothing, because the capacity is still 16. An empty `b` gives `mark` 0, and the adapter checks that `ptr` is not `NULL`. In a separate test, fill an owned `Bytes` list `d` with four zero bytes. `native.mark_w(f, mut d)` returns 4 while another task runs 10⁴ allocations. Afterwards every byte of `d` is `0xAB`. For the immutable range test, fill `c: Bytes` with the twelve values 0 through 11 and pass `list.slice(c, 4, 8)` to `native.loan_length` and `native.loan_sum`. They return 4 and 22, the length and sum of bytes 4 through 7. |
| **FL6** an FFI-reaching test refused a cached pass | a pass key made for a test whose reach holds C | A parameterless test calls `probe(None)`, where `fn probe(f: Option[Foreign[Laws]]) -> u64 { match f { Some(f) => native.freed(f), None => 0 } }`, and passes. Run `hirsel test` twice with one cache: both runs report it executed, and neither reports it reused. A parameterless test beside it that does not call `probe` is reported reused on the second run. The same holds when the only path to `native` is a resource's finalizer, on a fresh and on a warm reach record. |
| **FL7** recording and replay refused before C runs | a live recording half, a recording host or a stored replay that enters native code | `hirsel test --replay` of a test that calls `native.touch` fails with a line ending `refused: FFI-8: a recording build cannot reach foreign code: ffi_laws@1/tests/touches reaches ffi_laws@1/native/touch`. Its stderr holds no `touched`, and no recording file exists. `hirsel replay` of an entry that reaches `touch`, given any tape path, exits 2 with `replay: refused: FFI-8: …` and does not open the tape. `hirsel build --recording` is refused the same way. A deploy of that entry's recipe to a group registered with `--record DIR` exits 2 with `deploy: denied: FFI-8: a recording build cannot reach foreign code: …`, the group keeps serving its old image, and the host's code store gains no copy of the recipe; `hirsel register --record DIR` of it exits 2 with `register: refused: FFI-8: …` and creates neither DIR nor any host state. `hirsel replay --state` given a host state whose assembly record holds that recipe, and a tape whose routing header names it and whose records after the header are not valid records, exits 2 with `replay: refused: FFI-8: …`, not a tape error. |
| **FL8** SQLite round trip | a loan, `SQLITE_TRANSIENT` or `NULL`/empty mistake | In `sqlite`'s own tests, with grant `Foreign=sqlite@1/sqlite/Sqlite`: create `t(b blob)`, insert `x''`, `NULL` and `x'00ff'` with `bind_bytes` and `bind_null`, after which the bound list is cleared and refilled. Selecting in insertion order gives `Blob` with 0 bytes, `Null`, and `Blob` with `00 ff`. |
| **FL9** SQLite release paths | a finalizer that runs after an explicit release, a busy close, or a last release that frees the mutex it reports through | Four tests, each preparing `s` from `select 1` on a fresh connection `db`: (a) `finalize(take s)` then `close(take db)`; (b) `close(take db)` then `finalize(take s)`; (c) `close(take db)` with `s` dropped at scope end; (d) both dropped at scope end, in either order. Every explicit call returns `Ok(())`. In (b), `step(mut s)` between the two calls still returns `Row`, because the connection stays open until its count is zero. With `s` from `select abs(-9223372036854775808)` stepped once, so its last result is an error, `close(take db)` returns `Ok(())` and `finalize(take s)` then returns `Error(SqliteError(code: 1, message: "SQL logic error"))`, `sqlite3_errstr(1)`'s text; each test passes with no cause, so no finalizer met a consumed handle. |
| **FL10** no ambient input | the OS layer, libc or a pragma reading or changing process state | `select random()` returns the same value in two runs of a fresh process. `select date('now')`, `select datetime(0, 'localtime')` and `select datetime(0, 'utc')` each return `NULL`. `attach ':memory:' as x` fails at `prepare` with code 23, and `vacuum` and `vacuum into 'x.db'` fail at `step` with code 23 (`SQLITE_AUTH`). `pragma temp_store_directory = '/tmp'` and `pragma soft_heap_limit = 1` fail with code 23. |
| **FL11** directory confinement (slice 2) | a sidecar, temp file, super-journal or attach outside the grant | In a granted scratch `Dir` whose parent holds a sentinel directory: open `a.db`, run a WAL transaction from two connections, then a rollback-journal transaction. The files created are exactly `a.db`, `a.db-wal` and `a.db-shm` (WAL), and `a.db-journal` only while the journal transaction runs. The parent and the sentinel are unchanged. `open(f, d, "../x")` and `open(f, d, "file:a.db?vfs=unix")` give code 14 before C runs. A hot journal whose super-journal name is `/tmp/evil` fails recovery with `SQLITE_CANTOPEN` and opens no file outside. A `sort` over 10⁶ rows creates no file. Two connections to one leaf under two grants of different directories each see only their own rows. |
| **FL12** an archive whose code is outside its pin | a thin archive, or a member that is not a relocatable object, admitted because its digest matches | `ffi_laws`' x86_64 row names an indexed thin archive made with `ar --thin` from the adapter's `laws.o`, which lies beside it, pinned to the thin archive's true SHA-256. `hirsel build` of the FL1 entry is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu is a thin archive; its members are not inside the pinned bytes`, and the linker is never started. With the row naming a regular archive whose one member is `laws.o` compiled for `aarch64`, again with its true pin, the refusal is `… member laws.o is not a relocatable object for x86_64-unknown-linux-gnu`. With a regular x86_64 archive whose index lists `hfl_make` but whose member does not define it, the refusal is `… does not define hfl_make, declared at SITE`. |
| **FL13** a recipe-only build of a foreign program | a recipe from which a reader cannot recompute the foreign address, or one that links archives the leaves do not select | `ffi_laws` gets a second row, `aarch64-unknown-linux-gnu`, naming any file with its true pin. `hirsel publish` of the FL1 entry for x86_64 prints R. Delete the package's directory: `hirsel build --recipe R --code-store DIR` succeeds, and the executable's `native.make` call works. Change only the aarch64 row's bytes and pin and publish again: the new R differs from R (FFI-9), and it builds from the code store alone the same way. A recipe made from R's bytes by removing the x86_64 archive's entry from its foreign archives part, with its objects in the store, is refused as `Missing`, and one made by adding the aarch64 archive's entry as `Extra`. |
| **FL14** archive code that runs with no grant | an initializer, an indirect function or an out-of-prefix symbol admitted, so C runs before main or with no call | Each archive below has its true pin, and a parameterless test of `ffi_laws` that never names `native` sits beside the FL1 entry. `laws.c` plus `__attribute__((constructor)) static void hfl_c(void) { write(2, "ctor\n", 5); }` makes the build of the FL1 entry refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu: member laws.o has section .init_array of type SHT_INIT_ARRAY, which is not an admitted object feature` before linking. With the same archive, `hirsel test` of the package, whose tests are the parameterless one and one that takes `Foreign[Laws]`, is refused at its build with the same text, and its output holds no `ctor`. An archive with a prefixed symbol `hfl_unique` bound as `STB_GNU_UNIQUE` is refused with `… member laws.o has symbol hfl_unique with binding STB_GNU_UNIQUE, which is not an admitted object feature`. An archive defining the global `malloc` is refused with `… member laws.o defines malloc, which does not start with the prefix hfl_`. One defining `hfl_x` as an `ifunc` is refused with `… has indirect function hfl_x, …`. |
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

## Contained foreign code

FFI-10 to FFI-16 add a second kind of foreign module, a **contained module**. Its native
code is shipped as one pinned WebAssembly module, not as a static archive. The run executes it
inside an embedded Wasmtime, and the module has no access to the process or the OS. The
wrapper syntax does not change: the module's functions are still bodyless headers, the
capability is still `Foreign[I]`, and handles are still `foreign.Ptr[K]`. What changes is what
the run promises. FFI-1 trusts an archive with the whole process. A contained module is trusted
with nothing. Its time is bounded by the program's own `bound steps`, and its linear memory and
tables are charged to the program's own limits (`bound bytes` or the root limit). Its stack and
the engine's state for it are host memory outside every limit, as FFI-6's handle table is
(FFI-13). It cannot touch the process, so it needs no audit and no launch-time trust line. Its
results are a function of its inputs, so a program that reaches only contained code keeps
SEM-8, recording and replay. A test that calls contained code takes `Foreign[I]`, so PROG-4
never reuses its pass (FFI-14).

**Status: slice W1 in progress.** A manifest's `containment` key is parsed (FFI-10), a build
that reaches a contained module admits it (FFI-11) and compiles it for Linux targets, and its
calls run in the calling task's own instance with loans, handles and traps (FFI-12, FFI-14,
FFI-15), metered by fuel into steps, slice ends and byte charges (FFI-13). A program that
reaches only contained code is recorded and replayed (FFI-14). A test can take its
contained `Foreign[I]` grant from its test configuration alone. Deploy accepts the
contained binding, and the driver emits no `foreign:` line for it (FFI-14). A build
keeps each artifact in its part cache (FFI-15 Caching), and a recipe pins the module
and rebuilds it from the code store (FFI-16). Darwin images remain planned for W2.

Amended 2026-11-26 (contained-foreign-spec-fix-2923) for the soundness review
contained-foreign-review-2903: the artifact is loaded in place from the image's executable
segment through a custom code memory (FFI-15 Loading); one fuel-granule mechanism gives exact
steps at return, a bounded charge at a trap, a bound enforced before each bulk operation, and
slice ends that follow the build (FFI-13 Steps); the cache key holds the toolchain image
(FFI-15 Caching); contained tests are not reused (FFI-14 Replay); the fixture and laws FW3,
FW7 to FW12, FW16 and FW17 are rebuilt, and FW21 to FW28 added (contained laws); the runtime
links no compiler (FFI-15 The pin); the admitted feature set is one named constant (FFI-11);
and the opening claim on memory is narrowed (above). The open items O1 to O17 are ruled
(Rulings).

The section has one acceptance consumer, the fixture package `wasm_laws` (laws FW1 to FW28).
It is not a port of `sqlite`. SQLite's slice 2 needs a `Dir`, and a contained module has none
(FFI-12).

### FFI-10: Contained modules and their build

A foreign module is contained when its `[foreign."<module path>"]` table holds
`containment = "wasm"`. With that key, the table holds a `module` row in place of `archives`:

```text
[foreign.native]
symbols = "hpng_"
containment = "wasm"
module = { file = "native/png.wasm", sha256 = "<64 lower-case hex digits>" }
```

- `containment` takes the one value `"wasm"`. When the key is absent, the module is native
  (FFI-2 and FFI-3). Any other value is refused as a bad field, and so are a contained table
  that also holds `archives` and one that has no `module`. In
  `hirsel: P/hirsel.toml: foreign.native: containment: expected "wasm"`, `P` is the package.
- `module` is one row, with no target. A Wasm module runs unchanged on every target, so one pin
  covers them all. `file` ends in `.wasm`, follows the packages chapter's path rules, and is
  captured by its reading rule. `sha256` is the SHA-256 of the file's raw bytes.
- **Package identity.** A published library's content `C` holds the module's bytes and its
  pin, so its library root pins exactly that module. At W1, `C`'s encoding version becomes 2
  ([packages](packages.md#1-canonical-library-content)): in item 6, each foreign module's
  symbol prefix is followed by a containment tag. Tag `0` is followed by the archive rows as
  version 1 writes them. Tag `1` is followed by the module row: the file's path (a text), the
  32-byte pin, and the module's bytes (their length as a number, then the raw bytes). Readers
  accept only version 2 from then on, and libraries published under version 1 are republished.
  There is no dual reader.
- `symbols` stays the module's symbol prefix. Foreign function `f` is the **export** whose
  name is the prefix followed by `f`.

**The build pipeline.** The module is a WebAssembly 1.0 core module in the binary format, never
a component. The package's directory keeps the sources and a `build.sh`, and it rebuilds the
`.wasm` from them byte for byte, as `sqlite`'s pinned build does. The supported pipelines are
these:

- **C or C++** with the repository's pinned LLVM (`tools/buck2/bootstrap_llvm.py`):
  `clang --target=wasm32-unknown-unknown -nostdlib -O2 -fno-ident
  -ffile-prefix-map=$PWD=.`, with the freestanding parts of a libc built in, then `wasm-ld
  --no-entry --export=<each symbol> --export=<prefix>loan_area --strip-debug` (exports are
  only the named ones, since `--no-export-dynamic` <!-- doc-ref: external --> is wasm-ld's default). A library that needs a hosted libc takes `--target=wasm32-wasip1` and a
  pinned wasi-libc sysroot. Its WASI imports are then held to the null imports below.
- **Rust**: `cargo build --target wasm32-unknown-unknown --release` with `panic = "abort"`, or
  `wasm32-wasip1` held to the null imports.

The brief asked for "wasm32-wasip-none". No such triple exists. The two triples above are the
real ones: `wasm32-unknown-unknown`, which has no imports, and `wasm32-wasip1`, which uses
WASI preview 1 and has no capabilities here.

The adapter includes the header that `hirsel inspect --foreign-header P` prints. For a
contained module, that header declares every pointer as a 32-bit linear-memory address and
defines no `HIRSEL_ABI_TEXT`: a C section attribute does not make a Wasm custom section. With
clang and wasm-ld 21, `__attribute__((section("hirsel_abi")))` lands in a data segment of
that name. The build's last step adds the ABI text instead, as `llvm-objcopy
--add-section=hirsel_abi=abi.txt`, which writes a Wasm custom section (checked with LLVM 21's
`llvm-objcopy`). `abi.txt` holds exactly the bytes that `hirsel inspect --foreign-abi P` <!-- doc-ref: planned -->
prints. A Rust module can instead use `#[link_section = "hirsel_abi"]`
on a byte array. FFI-11 then binds the module to its headers, as FFI-3 binds an archive.

**What containment promises.** A contained module runs in the program's process, beside its
secrets (`Secret`, TLS keys). Containment promises that the module reads and writes only its
own linear memory, its loans and the null imports' fixed answers. It does not promise that
the module cannot learn anything through timing: the guest has no clock, no threads and no
shared memory, so it has no timer of its own, and Cranelift's heap Spectre mitigations stay on
(FFI-15), but timing channels are not a promise (O4). A module that must not run beside the
program's secrets is not a case this section serves.

### FFI-11: A contained module is admitted by its pin and its shape

A build whose checked reach holds a function of contained module `M` checks `M` before it
compiles or links anything. The checks run in this order, and the first that fails refuses the
build. Sites and `P`, `F` and `S` are as in FFI-3.

| check | refused with |
|---|---|
| its SHA-256 equals the pin (first, before any byte is parsed) | `FFI-11: module F of P does not match its pin: expected sha256 E, found sha256 O` |
| it starts with `\0asm` and has version 1: a core module, not a component (layer 1) | `FFI-11: module F of P is not a core WebAssembly module` |
| it validates under exactly the **admitted features** (below), with Wasmtime's `wasmparser` at the pinned version (FFI-15) | `FFI-11: module F of P is not valid with the admitted features: D`, `D` being the validator's first error |
| it defines exactly one memory and imports none, and the memory is exported as `memory` (validation has already refused a second memory, a shared memory and a 64-bit memory) | `FFI-11: module F of P: X`, where `X` is `no memory`, `an imported memory` or `a memory not exported as memory` |
| it imports no table, global or tag, and each function it imports is a **null import** (FFI-12) with the type that FFI-12 gives | `FFI-11: module F of P imports MOD.NAME, which is not a null import` |
| it has no start function, exports no `_start`, and exports `_initialize` only as `[] -> []` | `FFI-11: module F of P has a start function` (or `exports _start`) |
| every function it exports, other than `_initialize`, starts with `M`'s prefix | `FFI-11: module F of P exports NAME, which does not start with the prefix X` |
| for each reached foreign function, `S` is exported as a function whose core type is the one FFI-12 derives from the header; when any reached function has a `Bytes` parameter, `<prefix>loan_area` is exported as `[i32] -> [i32]` | `FFI-11: module F of P does not export S, declared at SITE`, or `… exports S as T, but its header gives U` |
| it has exactly one custom section `hirsel_abi`, and for each reached function its line equals the ABI text line, with `MODE` set to `contained` | `FFI-11: module F of P has no ABI text`, or `… the ABI of S is L, but its header declares A` |

**Admitted features.** The admitted features are one constant of the pinned `wasmparser`
(0.254, the version Wasmtime 48.0.10 requires): `WASM2 | TAIL_CALL | EXTENDED_CONST` with
`GC_TYPES` removed. `WASM2` is the WebAssembly 2.0 core set: mutable globals, bulk memory,
reference types, sign-extension operators, non-trapping float-to-int conversions, multi-value
and fixed-width SIMD (`WasmFeatures::WASM2` in wasmparser's `src/features.rs` <!-- doc-ref: external -->). With `GC_TYPES` off,
`funcref` is the only reference type: `externref` needs `GC_TYPES` (wasmparser's `MVP` constant
holds it), so a module with an `externref` table, global or parameter is refused by
validation, whatever reference types otherwise admit. The engine's `Config` first turns every
feature off by name (`Config::wasm_features(WasmFeatures::all(), false)`) and then turns on
exactly this constant, so nothing comes from Wasmtime's defaults (Wasmtime builds its set from
`WASM3` and its cargo features, then applies the `wasm_*` calls: `Config::features`,
`crates/wasmtime/src/config.rs` <!-- doc-ref: external --> lines 2516-2578). The admission
check's validator is given the same constant. Everything else is refused by validation:
threads, with shared memory and atomics; relaxed SIMD; memory64; multi-memory; exception
handling; GC; function references; stack switching; wide arithmetic; custom page sizes; and
the component model. A pin move that adds a proposal to Wasmtime's defaults admits nothing
new. (Checked on Wasmtime 48.0.10 with this `Config`: a module with one `externref` table is
refused with "gc types are disallowed but found type which requires gc".)

A contained module adds no link input and is never handed to the system linker. The driver
reads its bytes from the package resolution's capture, checks them in a sealed memory file, as
FFI-3 says for archives, and passes only the sealed bytes to the compiler (FFI-15).

### FFI-12: The contained ABI

**Calls.** A contained foreign function is called with Wasmtime's typed call on the calling
task's own instance (FFI-13). Parameters after `Foreign` and the result map as follows. A
pointer is a 32-bit address in the instance's linear memory.

| Hirsel | Wasm core type | C in the generated header | rule |
|---|---|---|---|
| `u8` `u16` `u32` | `i32` | `uint8_t` .. `uint32_t` | passed zero-extended; a result takes the low bits, as the C ABI does |
| `i8` `i16` `i32` | `i32` | `int8_t` .. `int32_t` | passed sign-extended; a result takes the low bits |
| `u64` `i64` | `i64` | `uint64_t`, `int64_t` | by value |
| `f32` `f64` | `f32` `f64` | `float`, `double` | by value; every NaN on either side is SEM-2's one NaN (FFI-14) |
| result `()` | no result | `void` | |
| `b: Bytes` | `i32 i32` | `const uint8_t *b_ptr, uint32_t b_len` | an immutable loan, **copied in** |
| `mut b: Bytes` | `i32 i32` | `uint8_t *b_ptr, uint32_t b_len` | an exclusive loan, **copied in and back** |
| `p: foreign.Ptr[K]` | `i32` | `void *p` | a handle in use (FFI-6) |
| `take p: foreign.Ptr[K]` | `i32` | `void *p` | a release (FFI-6) |
| result `Option[foreign.Ptr[K]]` | `i32` | `void *` | 0 is `None` |

`Dir` is refused in a contained module, at the parameter: `FFI-12: a contained function takes
no Dir`. So is `waits`, at the header: `FFI-12: a contained function is never waits`. A
contained call blocks on nothing. It runs on the calling task's thread, and when it computes
for long it yields at slice ends (FFI-13), so `waits` would mean nothing. Leaving it out keeps
the caller's signatures honest.

**Loans are copies.** Linear memory is the module's, so a loan is never a pointer into Hirsel
storage. When a call has `Bytes` parameters, the run first calls `<prefix>loan_area(n)` on the
instance. `n` is the sum, over the loans, of each length rounded up to 8, and it is at most
`2^32 − 8`. A larger total fails the call before anything runs, with `Fail("contained loans
exceed linear memory")`. The returned address `a` must be nonzero, a multiple of 8, and satisfy
`a + n` ≤ the memory's current size. Otherwise the call fails with `Fail("contained loan area
out of bounds")` and the function is never entered. The loans are laid out in parameter order
from `a`, each at an offset that is a multiple of 8. Each loan's bytes are copied in, and the
function is called with each loan's address and length. An empty loan gets the address of its
own place in the layout, which is nonzero, with length 0. When the function returns normally,
each exclusive loan's `len` bytes are copied back into its Hirsel list. The list's length never
changes. When the call traps or runs out of steps, nothing is copied back, so an exclusive loan
keeps its bytes from before the call. `loan_area` is the module's own code, and its fuel
counts toward the call (FFI-13). Because a loan is a copy, a module that keeps a loan's address
after the call reads only its own memory. The FFI-1 native contract on loans has nothing left
to protect, and a contained module is not bound by it.

**Handles.** The handle table, its states and the call protocol of FFI-6 apply unchanged, with
two differences. First, the pointer index is per instance. An address names memory in one
instance only, so two instances may each return the address 16 for different objects, and both
are accepted. Second, each handle records the instance generation that made it (FFI-13). A handle of a
discarded generation is **orphaned** (FFI-13).

**A contained handle is not `send`.** For a contained kind `K`, `foreign.Ptr[K]` is plain but
not `send`, and so is not `cross`. A resource that holds one may not declare `send` (CONC-2,
OWN-4). This is the one change the same wrapper syntax costs. A contained `Db` cannot move
between tasks. Its instance belongs to the task, and a handle is an address in that task's
memory. `Foreign[I]` itself stays plain, `shareable` and `send`, so every task can call `I`, on
its own instance.

**Null imports.** A `wasm32-wasip1` module may import functions only from
`wasi_snapshot_preview1`, and only these, each with its preview-1 core type. Each one gives a
fixed answer that reads nothing outside the instance:

| import | answer |
|---|---|
| `args_sizes_get`, `environ_sizes_get` | writes `0, 0`; returns 0 |
| `args_get`, `environ_get` | writes nothing; returns 0 |
| `fd_write`, `fd_read`, `fd_close`, `fd_seek`, `fd_fdstat_get`, `fd_prestat_get`, `fd_prestat_dir_name` | returns `EBADF` (8), writes nothing |
| `clock_time_get`, `clock_res_get`, `random_get`, `poll_oneoff` | returns `ENOSYS` (52), writes nothing |
| `sched_yield` | returns 0 |
| `proc_exit(c)` | ends the call as the trap `exit c` (FFI-14) |

A pointer argument of a null import is never read, and the only writes are the two `0` words
of the `*_sizes_get` calls. They are bounds-checked like any guest store, and an out-of-bounds
address traps as `memory out of bounds`. Every other import is refused by FFI-11. So a
contained module has no clock, no randomness, no file, no environment and no output.

### FFI-13: Instances, bounds and steps

**One instance per task and interface.** A task's first call of a function of contained
interface `I` instantiates `I`'s module in a new Wasmtime `Store` that belongs to the task.
Every later call by that task uses this **instance**, and no other task ever reaches it.
Instantiation runs `_initialize` when the module exports it. Its memory and fuel are charged to
that first call, under the limit and step bound current at the call. A trap during
instantiation (an active data or element segment out of range, or a trap in `_initialize`)
fails that first call with FFI-14's text for it, and no instance is made: the task's next call
instantiates again. A flow stage (CONC-14) never calls contained code, because a stage body
holds no capability and so no `Foreign[I]`. Because the instance is
task-private and its handles are not `send`, the order of calls into any instance is its task's
program order. No event is added to CONC-10, and nothing here depends on the order between
tasks. When the task ends, after its finalizers have run, the run drops the instance and
refunds its memory charge. It runs no release for the handles still live: a release can only
change the instance's own memory, which is dropped with it, so it would observe nothing.
Those handles become consumed. FFI-6's run-end release therefore does not apply to contained
kinds. A resource's mechanical release (FFI-6) still runs while the task lives.

**Discarding.** An instance is **discarded** when a call into it traps (FFI-14) or stops with
`OutOfTime`. Its state may then be partway through an update, so it is never entered again.
Its memory charge is refunded. Its live and in-flight handles become **orphaned**, and no
release runs for them, since their memory is gone. Every call that passes an orphaned handle,
an explicit release included, fails in FFI-6's preflight with `Fail("foreign instance
discarded")` and enters nothing; a release passing it also consumes it, so a second release
fails with `Fail("foreign handle released")`. FFI-6's mechanical release, after a resource's
finalizer, skips an orphaned handle and consumes it without a cause. So a finalizer written as
`drop(o) { let _ = free(o.f, o.p) }` fails with that cause, reported as every finalizer failure
is (FAIL-12), and the handle is still consumed exactly once. The task's next call of `I` makes a new instance of the next **generation**. So
one bad input costs that task one instance. It never costs a later caller a wrong answer.

**Bytes.** Linear memory is charged to the program's limits. At instantiation, the module's
initial memory, of 65,536 bytes per page, is admitted against the limit current at the call,
along with its initial tables at 8 bytes per element. A refusal raises `Capacity(c)` at the
call (MEM-21). Nothing is instantiated and no module code runs. Each `memory.grow` and
`table.grow` admits its growth against the limit current at the contained call that executes
it. A refusal answers the guest exactly as WebAssembly's own failure does: `memory.grow`
returns −1 (Wasmtime's `ResourceLimiter::memory_growing` returning `Ok(false)`, which "will
cause the `memory.grow` instruction in a module to return -1 (failure)"). The C allocator then
returns `NULL`, and the wrapper maps that to its own error. This is not a hidden cap. The limit
is the program's own `bound bytes` or root limit, the refusal is a refused admission, which is
already a CONC-10 event, and the guest sees an explicit failure. The other answer, `Err`, is a
trap that Wasmtime's docs call "not necessarily compliant with the WebAssembly specification",
and it is not taken. Growth past the memory's or table's own declared maximum fails the same
way, but before any admission: nothing is charged and there is no CONC-10 event, since no
limit refused it. A charge outlives the bound scope that made it, as any surviving charge
does, and is handed over to the parent on the scope's exit (MEM-20). It is refunded when the
instance is discarded or dropped. Memory never shrinks while the instance lives. A host that
refuses the pages a granted grow needs stops the run with `HostMemory` (FAIL-15), as it does
for any engine memory. The run reserves the address space before the call, so the guest
never sees that refusal.

Only linear memory and tables are charged. The call's fiber stack (below), the instance's
context and its address-space reservation are host memory outside every limit, as FFI-6's
handle table is. Each instance reserves 4 GiB plus a 32 MiB guard of address space, so a
process holds about 30,000 live instances in a 47-bit user address space; the next
instantiation's reservation is refused by the host and stops the run with `HostMemory`. A
program that needs more live instances spawns fewer tasks that call contained code.

**Slot reuse.** A discarded instance's linear-memory reservation and its fiber stack go back to
a per-process idle cache, and the next instantiation or call takes one before it maps a new
one. A returned linear-memory slot is reset by mapping fresh anonymous `PROT_NONE` pages over
the whole range it opened (`mmap` with `MAP_FIXED`), so the next instance sees zero bytes, its
own initial size and nothing accessible past it; its grows are charged from its own grant, as a
fresh reservation's are. Tables, globals and the instance's context are never cached: Wasmtime
makes them new for every instance. A returned fiber stack keeps its bytes, as Wasmtime's own
stack pool does without `async_stack_zeroing`: guest code cannot address the native stack. Each cache holds at
most `min(root limit / 2 MiB, 1024)` idle slots; a slot past that is unmapped. The cache never
holds a live instance's slot, so the live-instance ceiling above is unchanged. A refused
reservation, page grant or grow on a cached slot stops the run with `HostMemory`, as on a fresh
one.

**Steps.** A contained call takes one step, as every foreign call does (FFI-2). Before anything
is copied in, it takes one step for each 16 KiB, or part, of each loan it copies in, and one
more for each 16 KiB, or part, of each exclusive loan it will copy back, as `crc32c.update`
does (MEM-30). So a 4 GiB loan costs what a 4 GiB checksum costs, not one step. Then it takes
⌈`u` / 16⌉ steps, where `u` is the Wasmtime fuel that the call consumes in its **entries**:
instantiation with `_initialize`, `loan_area`, and the function itself. The fuel cost of each
operator is the **pinned cost table**, which is Wasmtime 48.0.10's `default_operator_cost`.
`nop`, `drop` and the structural control operators (`block`, `loop`, `unreachable`, `return`,
`else`, `end`) cost 0, and every other operator costs 1. On top of that flat cost, the table's
`VariableOperatorCost::new()` charges 1 per byte of `memory.copy`, `memory.fill` and
`memory.init`, and 1 per element of `table.copy`, `table.fill`, `table.init` and `table.grow`.
It charges nothing per page of `memory.grow`, whose growth is admitted against the bytes limit
(`crates/environ/src/tunables.rs` <!-- doc-ref: external --> in
Wasmtime 48.0.10). The driver passes it with `Config::operator_cost`, so a later Wasmtime
default cannot move it. The rate of 16 fuel per step keeps a step close in size to a Hirsel
loop iteration.

**Fuel checks.** The Wasmtime source cited in this paragraph and the next is
`crates/cranelift/src/func_environ.rs` <!-- doc-ref: external --> and
`crates/wasmtime/src/runtime/` <!-- doc-ref: external --> at tag v48.0.10. Compiled code adds
each operator's cost to a counter held in a register and compares it with the fuel only at a
**fuel check**: at each function entry (`fuel_function_entry`), at each loop header
(`translate_loop_header`), and before each bulk operation whose cost is not a constant of at
most 128 (`pre_translate_bulk_op`, func_environ.rs:5002-5066). A bulk operation's whole cost
is added before its check, so an operation the fuel cannot pay never runs. Nothing is checked
at a return: a function with no loop and no call adds its cost when it returns
(`fuel_function_exit`, func_environ.rs:481-486). The counter is written to the store only
before a call, a return or `unreachable`, and on the way into the out-of-fuel path
(func_environ.rs:514-525 and 627-660).

**Granules.** Each entry runs as a Wasmtime async call on a store configured with
`Store::fuel_async_yield_interval(G)`, where the **granule** `G` is 16,384 fuel, 1,024 steps.
Before each entry the run sets the store's fuel with `Store::set_fuel` to `F + 1`, where `F` is
16 × the steps left in the current bound (at most `2^64 − 2`), or to `2^64 − 1` with no bound
current. So each entry starts a fresh granule, whatever earlier entries consumed. Wasmtime
injects at most `G` fuel at a time and keeps the rest in a reserve. When a fuel check finds the
injected fuel spent, `out_of_gas` refills it from the reserve with the smaller of `G` and what
the reserve holds, then yields (`out_of_gas`, vm/libcalls.rs:676-687; `refuel` and `set_fuel`,
store.rs:1506-1538). A **granule end** is each point at which the entry's future returns
pending. Nothing else in an entry returns pending: the resource limiter (FFI-13 Bytes) and the
null imports are synchronous. Between an entry's start and its first granule end, and between
two granule ends, at least `G` fuel is consumed. At a bulk operation it can be far more,
because one check pays the whole operation and the entry returns pending once.

**Bounds.** With a bound current, the store holds exactly one unit of fuel more than the bound
pays for. A call **runs out** in three cases:

- an entry reaches a fuel check after consuming more than `F`. The reserve cannot refill it,
  and Wasmtime raises `Trap::OutOfFuel` at that check;
- an entry returns normally and `Store::get_fuel` reads 0. It then consumed at least `F + 1`,
  because `get_fuel` saturates at 0 for any overdraw (store.rs:1501). When it reads more than
  0, the entry consumed exactly `F + 1 − get_fuel`;
- an entry traps and `Store::get_fuel` reads 0. The stored consumption has reached at least
  `F + 1`, so the call runs out. A trap whose stored fuel is above 0 is reported as the trap
  even when the entry had passed `F` in the straight-line run that led to it. The charge is
  capped at the steps remaining in the bound (FFI-14 Fuel at a trap).

A call that runs out raises `OutOfTime` at its call site (MEM-30). Its result is dropped,
nothing is copied back, the instance is discarded, and the call takes exactly the steps its
bound had left. An `isolate` does not catch it there. The bound's scope does. A call that does
not run out takes ⌈`u` / 16⌉ steps for its exact `u`, and the next entry's `F` is computed
after them. So a contained call never takes more steps than its bound has left, and its work
past the bound is at most one straight-line run: the operators after its last fuel check, up to
the next check or the return. A bulk operation that would pass the bound is stopped before it
runs. With no bound current, the store holds `2^64 − 1` fuel. A refused `table.grow` is charged
for the elements requested, so a call can spend that fuel. `Trap::OutOfFuel` with no bound current
fails the call with `Fail("contained trap: out of fuel")` and discards the instance (FFI-14).

**Slice ends.** The entry's future holds the store, so the run cannot read the fuel at a
granule end. It counts each granule end as exactly `G` fuel, 1,024 steps, toward the task's
step count. While a bound is current, if that count has crossed a multiple of 65,536 since the
task's last slice end, the task takes exactly one slice end there (MEM-30), and the scheduler
may run another task before it polls the entry again. With no bound current, it polls again at
once. When the entry ends, the run adds the steps that the granule ends did not count: at a
return, the exact steps less those counted; at a trap, the trap's steps (FFI-14) less those
counted. The task's next slice end then follows MEM-30 from the new count. So inside a
contained call a slice end can fall late, by the fuel that bulk operations consumed past
their granule's `G`, and there is at most one slice end per granule end. Where a slice end
falls is a function of the code path alone, so it is the same on every run of one build. A
bound's exhaustion is never late, because the store's fuel decides it.

This mechanism was checked with a scratch program against Wasmtime 48.0.10 on x86-64 Linux,
with `G` = 16,384 and fuel reset before each entry. A loop of 10⁶ iterations consumed 9,000,006
fuel and returned pending 549 times (⌊9,000,006 / `G`⌋ = 549). A 128 KiB `memory.fill`
consumed 131,077 fuel and returned pending once. A function of 4,000 straight-line operators
(2,000 adds), called with fuel 33, returned normally and `get_fuel` read 0. The same `memory.fill`, called
with fuel 1,601, raised `Trap::OutOfFuel`, and the first byte of memory was unchanged. A loop
of 10⁵ iterations that then divided by zero left a stored consumption of 885,007 against
884,736 counted from 54 granule ends (true consumption about 900,000).

Wasmtime's `consume_fuel` docs say fuel "can be used to deterministically prevent
infinitely-executing WebAssembly code", and its `epoch_interruption` docs say epoch
interruptions "are not deterministic" and that fuel "should be used when _deterministic_
yielding or trapping is needed". Epoch interruption is not used.

**Stack.** A contained call runs on a Wasmtime async fiber stack of 2 MiB
(`Config::async_stack_size`), with `Config::max_wasm_stack` set to 1 MiB. A guest that recurses
past that traps as `stack overflow`. It does not touch the task's thread stack, so the docs'
warning that exhausting the thread stack "typically leads to an abort of the process" does not
apply. The fiber stack is host memory outside every limit, like FFI-6's handle table (FFI-13
Bytes). The overflow point is the stack pointer at the entry less `max_wasm_stack`
(`crates/wasmtime/src/runtime/func.rs` <!-- doc-ref: external --> lines 1578-1579), so the depth at which a guest
overflows, and the steps it has taken there, depend on the frames below it. On the fiber those
are Wasmtime's own frames, the same in every run of one build. Two targets may differ, which
SEM-8's per-build scope allows.

### FFI-14: Determinism, traps and replay

**Determinism.** Every contained call is a function of these values: the module's bytes, the
pinned Wasmtime and its configuration (FFI-15), the instance's state, and the call's arguments.
Nothing else enters it. The engine is configured to make that true:

- `cranelift_nan_canonicalization` is on. Wasmtime then canonicalizes NaNs, its docs say, "into
  a particular, canonical NaN value", covering `f32`, `f64` and `v128`. In Wasmtime 48.0.10
  that value is Cranelift's `Ieee32::NAN` and `Ieee64::NAN`: `0x7fc00000` and
  `0x7ff8000000000000`, which are SEM-2's NaNs (`cranelift/codegen/src/nan_canonicalization.rs`,
  `ir/immediates.rs`). A NaN result is still made canonical at the boundary, as FFI-5 says.
- Relaxed SIMD and threads are refused by FFI-11, not made deterministic. Wasmtime's
  deterministic-execution guide offers `relaxed_simd_deterministic` "at the cost of
  performance" or turning the proposal off. Off is the smaller trusted surface.
- Nothing is imported except the null imports, so there is no clock, randomness or host
  input. That follows the guide's first rule: "Do not give Wasm programs access to
  non-deterministic host functions."
- Fuel, not epochs, bounds and slices the work (FFI-13). `memory_init_cow` is off, because the
  guide warns that otherwise "instantiation can consume a different amount of fuel depending on
  the host".
- Growth is decided only by the program's limits. The guide warns that `memory.grow` may
  otherwise "non-deterministically succeed or fail". Here a host refusal is a run stop, not an
  answer (FFI-13).

**Traps.** A trap ends the call, discards the instance (FFI-13) and fails the call with
`Fail("contained trap: X")` at the call site. `X` is taken from this closed list of Wasmtime
48.0.10's `Trap`:

| Wasmtime `Trap` | `X` |
|---|---|
| `UnreachableCodeReached` | `unreachable` (a Rust panic under `panic = "abort"`, or a C `abort()`) |
| `MemoryOutOfBounds` | `memory out of bounds` |
| `TableOutOfBounds` | `table out of bounds` |
| `IndirectCallToNull` | `indirect call to null` |
| `BadSignature` | `bad signature` |
| `IntegerOverflow` | `integer overflow` |
| `IntegerDivisionByZero` | `integer division by zero` |
| `BadConversionToInteger` | `bad conversion to integer` |
| `StackOverflow` | `stack overflow` |
| `proc_exit(c)` (FFI-12) | `exit c`, with `c` printed as an unsigned 32-bit number in decimal |
| `OutOfFuel` | with a bound current, `OutOfTime` (FFI-13 Bounds); with none, `out of fuel` |

Every other variant belongs to a feature that FFI-11 refuses, so a module that reaches one
means Wasmtime broke its own feature set. That is a failed runtime invariant, not a property of
the guest, and the process aborts as on any failed invariant of the runtime. No run stop name
and no `Fail` text is given to it. `Trap::OutOfFuel` belongs to no refused feature, so the listed
`out of fuel` trap takes precedence over O15's invariant rule. FAIL-2's `Fail` row
lists these texts, and FFI-12's and FFI-13's. A trap is a failure, not environmental
termination. It runs every cleanup that FAIL-8 runs, and an `isolate` catches it. This is the
difference from FFI-1, where a native crash takes the process.

**Fuel at a trap.** Wasmtime's docs say "fuel accounting is not precise when WebAssembly
traps." The store's value lags the code: it holds the counter as last written, at a call, a
return, `unreachable` or a granule end (FFI-13 Fuel checks). So a loop with no call writes it
only at its granule ends. Let `s` be the fuel the store reports consumed at the trap entry
(`F + 1 − get_fuel`, or `2^64 − 1 − get_fuel` with no bound). The entry is charged `s + G`
fuel, and with a bound current at most `F`: the trapping call takes ⌈(`s` + `G`) / 16⌉ steps
for that entry, capped at the steps its bound has left, beside its other entries' exact steps
and its copy steps. Since `s` is at least the fuel counted at the entry's granule ends, a
trapping call gets less than one granule plus one straight-line run of work free. That run
can span one 7,654,321-byte function body, about 478,000 steps. A caller that catches the trap
and calls again under `bound steps n` does `O(n)` work.
`s` is a function of the code path alone. It is the same on every run of one build (law FW8),
and nothing compares it across builds (SEM-8).

**Replay.** A program is **native-reaching** when its checked reach holds a function of a
native foreign module (FFI-8 then applies, and nothing below does). A program that reaches only
contained modules is recorded, replayed and given reused test passes like any program with no
foreign code:

- a contained call is not a replay point (CAP-8). It reads no input that the run did not
  compute, so replay recomputes it and nothing is recorded for it;
- its steps, slice ends, charges, refusals and failures are SEM-8 observables, and they are
  the same on every run of one build;
- a test that calls contained code takes `Foreign[I]` (Grants, below), so it is not a test
  that takes no parameters, and PROG-4 never reuses its pass. The driver agrees: it keeps a
  pass only for a test with no parameters whose reach holds no foreign function. A test that
  reaches contained code is run every time.

A program that reaches both kinds is native-reaching, and FFI-8 refuses its recording build in
full.

**Grants.** A contained interface's `Foreign[I]` gives no process authority. So FFI-1's and
FFI-4's launch rules do not apply to it: no `--grant` is needed on the command line, no
`foreign:` line is written, and no deploy refusal applies. Its grant `Foreign=D` is an ordinary
entry grant (CAP-4). It is given by the run's configuration, a test configuration's grant
string, or a recipe binding, like every other grant that is not `Foreign`. The grant is kept
(O6): one rule for `Foreign[I]` treats like things alike, and an ambient contained call would be
the only call without a capability.

### FFI-15: Wasmtime, compilation and caching

**The pin.** Hirsel embeds the `wasmtime` crate at exactly `=48.0.10`, with default features
off, in two builds. The driver builds it with `cranelift` and `std`, so it can validate and
precompile. The runtime builds it with `runtime`, `async` and `std` only, so it can load,
instantiate and call. `Engine::precompile_module` exists only with `cranelift` (or `winch`) on
(`crates/wasmtime/src/engine.rs` <!-- doc-ref: external --> line 675), so an image holds no code generator, and "the run never compiles" is a fact
of its link, not a promise. Wasmtime calls every release that is a multiple of 12 an LTS,
"supported for 24 months", and security fixes are "guaranteed to be backported to all supported
releases". A pin moves only to a later patch of 48, or to the next LTS. The pin is a toolchain
input, so a change moves the toolchain image identity, and with it every contained object key
(below). The native backend's Cranelift (`=0.136.3`, the Wasmtime 49 line) is a separate pin.
Wasmtime 48 brings its own Cranelift (`0.135`), so the driver builds two Cranelifts. The runtime
builds none of Wasmtime's. Both pins stay (O9).

Containment rests on Wasmtime's validator, its Cranelift lowering and its runtime (O12). An
advisory against this configuration moves the pin, and every image that holds a contained
artifact is rebuilt and redeployed.

**Configuration.** One function in the toolchain writes the `Config`, and both builds call it.
Its settings are of two kinds.

- **Compile settings**, which shape the artifact and which only the driver's engine reads:
  `cranelift_nan_canonicalization(true)`; and `target(T)` for the build's target, with the CPU
  features named in full, never taken from the host. Wasmtime's `target` docs say
  "target-specific flags (such as CPU features) will not be inferred by default". The x86-64
  features are the native backend's baseline for `T`, and AArch64 uses the base Armv8.0-A set.
  Cranelift's NaN canonicalization is a shared flag that a load does not check
  (`check_compatible_with_shared_flag`, engine.rs:436-520, lists `enable_nan_canonicalization`
  among the flags that "don't change semantics" for loading). So the artifact's code carries it,
  and the runtime need not repeat it.
- **Engine settings**, which both engines set identically: `consume_fuel(true)` with the pinned
  cost table (`Config::operator_cost`); `epoch_interruption(false)`; the admitted features of
  FFI-11, set by name; `memory_init_cow(false)`; `signals_based_traps(true)` with the default
  4 GiB reservation and 32 MiB guard; `max_wasm_stack(1 MiB)` and `async_stack_size(2 MiB)`.
  A load checks the artifact's triple, its shared and ISA flags, its tunables (fuel, the cost
  table, the reservation, the guard, signals-based traps and `memory_init_cow` among them) and
  its features against the runtime's engine, and refuses a mismatch
  (`check_compatible`, `crates/wasmtime/src/engine/serialization.rs` <!-- doc-ref: external -->
  lines 204-211 and 278-445). The two stack sizes are runtime settings that no artifact
  records. The runtime's engine also sets the custom code memory (Loading, below).

**Signals.** Signals-based traps stay on. Turning them off makes code "up to 2x slower", and it
also needs Cranelift's heap Spectre mitigations off, since those "rely on faults from loading
from the null address". On Linux, Wasmtime installs handlers for `SIGSEGV`, `SIGBUS` and
`SIGILL`, and `SIGFPE` on x86-64. It saves each previous action and passes a fault outside Wasm
code to it (`crates/wasmtime/src/runtime/vm/sys/unix/signals.rs` <!-- doc-ref: external -->,
48.0.10), so the runtime installs its own handlers first and Wasmtime's chain to them. On macOS
it uses Mach exception ports, Wasmtime's default there, which forward "any non-guest faults" to
the process-level handlers. That the two meet correctly on Linux is law FW28 (O2): a guest
out-of-bounds trap on one scheduler thread while another overflows a Hirsel stack, each
reported as its own rule says. The macOS case, and Mach ports across `fork()`, belong to W2.

**Compiling and cross-compiling.** The driver compiles each contained module once per target,
with `Engine::precompile_module` on an engine configured for that target. The same code builds
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and `aarch64-apple-darwin` artifacts on
any host, because Wasmtime's `target` "can be used, for example, to cross-compile from one
platform to another". A cross build never creates a `Module` for a foreign target: it only
precompiles.

**What the image ships, and loading.** The artifact is one object of the link: the bytes that
`precompile_module` returned, unchanged, placed in a read-and-execute segment of the image
(`PT_LOAD` with `R E` on ELF, `__TEXT` on Mach-O) and aligned to 64 KiB. The artifact's own
text section is aligned within it to the target's page size as Wasmtime sees it: 4 KiB on
x86-64, 64 KiB on AArch64 Linux, 16 KiB on AArch64 macOS (`page_size_align`,
`crates/environ/src/compile/mod.rs` <!-- doc-ref: external --> lines 370-394). So the text
lands on an executable page boundary in the image. The runtime's `Config` holds a custom code
memory (`Config::with_custom_code_memory`, config.rs:1731) whose `required_alignment` is that
page size and whose `publish_executable` changes no protection, because the pages are already
executable; on AArch64 it performs the instruction-cache maintenance the target needs, and
`unpublish_executable` is never called. At the task's first call, the run loads the artifact in
place with `Module::deserialize_raw`, passing the object's address and length. Wasmtime wraps
that memory as externally owned, which supports no virtual-memory operation, so without a
custom code memory its publish step fails with "this target requires virtual memory to be
enabled" (`CodeMemory::publish`, `crates/wasmtime/src/runtime/code_memory.rs` <!-- doc-ref: external -->
lines 415-475, the bail at 452; `MmapVec::supports_virtual_memory`, vm/mmap_vec.rs:157-164). With it, publish
calls the custom memory and writes nothing to the text. An artifact whose text has relocations
is refused at load (code_memory.rs:178-192), and `precompile_module` makes none. So no
executable memory is mapped or written at run time, on any target (O3).

This loader was checked with a scratch program on x86-64 Linux only: a runtime-only build
(`runtime`, `async`, `std`) loaded a 11,168-byte artifact from a 64 KiB-aligned object in the
image's `.text`, instantiated it and ran it. The AArch64 rows are checked at W1 and W2.

Wasmtime marks `deserialize_raw` `unsafe` because a modified artifact can run arbitrary code,
and says it may be "considered safe" only for the unmodified output of `precompile_module`.
The image is the toolchain's own output, as its native code is. The run never deserializes
bytes from a cache, a package or the network.

**Caching.** A compiled artifact is a build part. It is kept in the driver's part cache, under
the key `H("hirsel.contained-object.v1", m ‖ t ‖ k ‖ i)`. `m` is the module's 32-byte pin. `t`
is the target triple as a text. `k` is the compiler identity (`docs/building.md`, "Compiler
identity"): the driver's source closure and pins, which fix the engine configuration
`compile::engine` builds for `t`, with its Cranelift and ISA flags, tunables, cost table and
features. `i` is the toolchain image identity. The test pass key holds both. Wasmtime's own
`Engine::precompile_compatibility_hash` is not used: it is built only with the `runtime`
feature, which the driver leaves off. A toolchain rebuilt with a patched Cranelift, still at
version 48.0.10, changes `k` or `i`, so its key misses. Since the pin is in the key, the cache
follows the package identity: two packages with one module share an entry, and a changed
module misses. The cache is trusted as every compiled part is. It is the build user's own
output, and nothing guards it against that user ("trust our own implementation"). A contained
artifact comes only from the build user's own part cache, or is compiled in place. Kiln RE and
the code store carry module bytes, never artifacts, and compiled artifacts are never moved
between hosts. In a recipe build, the module's bytes come from the code store, checked by hash,
and are compiled there. No signature binds an artifact (O1).

### FFI-16: Contained code in addresses and recipes

A contained foreign function's code address is `H("hirsel.code-contained.v1", e ‖ s ‖ x ‖ m)`.
`e`, `s` and `x` are as in FFI-9, and `m` is the module's 32-byte pin. So a caller's address
and revision change when the module's bytes, its header or its prefix change. The address is
the same on every target, because one module serves every target. The domain is new, so a
contained function never shares an address with a native one.

At W1, the release recipe's foreign leaf ([FFI-3](#ffi-3-archives-are-admitted-by-their-pin-before-linking),
[launch](launch.md#release-recipe-and-host-assembly)) gains a containment byte between its
symbol prefix and its archive table. The byte is 0 for native, followed by the table as today,
and 1 for wasm, followed in place of the table by the module row: its 32-byte pin and its byte
length as a number. The reached functions follow as today. The foreign archives part lists that
pin as it lists an archive's. The bytes are an object of the code store, named by the pin, and
are transferred and collected like the recipe's other objects. The leaf's format changes, so
at W1 the recipe root's domain becomes `hirsel.release-recipe.v5`, and v4 is never read or
written again
([launch](launch.md#release-recipe-and-host-assembly)). The recipe is not target-specific for
this module, and every host assembly compiles the module for its own target (FFI-15).

### Contained laws

Each law's expected value comes from this section, the WebAssembly specification, Wasmtime's
documented behaviour, or the fixture's own source. None comes from a run's output. Each law runs
on both native backends, on x86-64 and AArch64 Linux. The fixture package `wasm_laws`, with
interface `Box` and kind `Cell`, has a module `box.wasm` built by its `build.sh` from `box.c`
with prefix `hwl_`. Its `build.sh` compiles `box.c` with clang 21 as FFI-10 says, and
assembles `box_traps.s`, written in LLVM's WebAssembly assembly, with the same clang. The
repository's pinned LLVM tree builds only the X86 and AArch64 targets, so `build.sh` takes an
LLVM 21 clang, `wasm-ld` and `llvm-objcopy` that have the WebAssembly target from the
environment. `wasm-ld` links both with `--initial-memory=131072` <!-- doc-ref: external -->,
`-z stack-size=16384` <!-- doc-ref: external -->, `--stack-first` <!-- doc-ref: external --> and
`--table-base=3` <!-- doc-ref: external -->, so the memory's 2 pages and the table's 4 elements
are stated link inputs, and the stack and static data lie in the first page.
The functions whose operators a law counts are written in `box_traps.s`, so no optimizer
folds a loop, removes a recursion or chooses the trapping operator: at `-O2`, a C loop that
returns `n` folds to `return n`, and signed overflow, a wild load and a mistyped indirect call
are undefined behaviour in C. The module imports exactly two null imports,
`wasi_snapshot_preview1.clock_time_get` and `wasi_snapshot_preview1.proc_exit` (declared in
`box.c` with clang's `import_module` and `import_name` attributes), defines one table of 4
`funcref` elements (slots 0 to 2 null, slot 3 a function of type `[] -> []`), and
exports:

- `make(f) -> Option[foreign.Ptr[Cell]]` and `free(f, take p) -> i32`, which use a static pool
  of 8 cells, and `live(f) -> u64`, the number of cells in use (`box.c`);
- `spin(f, n: u64) -> u64`, which loops `n` times and returns `n` (`box_traps.s`);
- `straight(f) -> i32`, which runs 2,000 `i32.const 1; i32.add` pairs, 4,000 straight-line
  operators with no loop and no call, from 0, and returns 2,000 (`box_traps.s`);
- `straight_trap(f) -> i32`, which runs the same operators and ends in `unreachable`
  (`box_traps.s`);
- `loop_div(f, n: u64) -> i32`, which loops `n` times with no call and then executes
  `i32.div_u` of 1 by 0 (`box_traps.s`);
- `fill_mem(f, n: u32) -> u32`, which executes one `memory.fill` of `n` bytes of `0xAB` at
  address 65,536 and returns the byte there (`box_traps.s`);
- `grow(f, pages: u32) -> i32`, which returns `memory.grow`'s result;
- `tgrow(f, n: u64) -> i32`, which `n` times asks `table.grow` for `2^32 − 1` null elements,
  each refused with −1 but charged its requested count, and returns the last answer
  (`box_traps.s`);
- `fill(f, mut b: Bytes) -> u64`, which writes `0xAB` to each byte, and `sum(f, b: Bytes) ->
  u64`;
- `nan32(f) -> f32`, which returns `f32.reinterpret_i32(0x7fa00001)`, a NaN that no float
  operator made (`box_traps.s`);
- `div_bits(f, a: f64, b: f64) -> u64`, which executes `f64.div` of `a` by `b` in the guest,
  stores the quotient to memory and returns its bits read back as an `i64` (`box_traps.s`);
- `trap(f, k: u32) -> i32`, which for `k` from 1 to 10 traps as row `k` of FFI-14's table, in
  its order (`box_traps.s`): `unreachable`; an `i32.load` at `0xFFFFFFF0`; `call_indirect` of
  slot 4; of slot 0; of slot 3 with type `[] -> [i32]`; `i32.div_s` of `-2^31` by `-1`;
  `i32.div_u` by 0; `i32.trunc_f32_s` of a NaN; a call of `recurse` with depth `2^32 − 1`; and
  `proc_exit(7)`;
- `clock(f) -> i32`, which calls `clock_time_get(0, 1, a)` with an address `a` in its static
  data and returns the answer;
- `count(f) -> u64`, the number of entries so far into the instance's exported functions other
  than `loan_area`, this one included;
- `fill_trap(f, mut b: Bytes) -> u64`, which writes `0xAB` to each byte and then executes
  `unreachable`;
- `set_area(f, mode: u32) -> i32`: with mode 1, `loan_area` from then on returns the memory's
  size in bytes minus 8, which is a multiple of 8 but leaves no room for 16 bytes;
- `recurse(f, d: u32) -> u32`, which recurses `d` frames deep and returns `d`, holding its
  depth in Wasm locals only (`box_traps.s`);
- `loan_area(n)`, which in mode 0 returns the address of a static 4,096-byte area, and traps
  when `n` is larger.

`box.wasm` declares an initial memory of 2 pages and no maximum, and one table of 4 elements.
At instantiation they are charged 131,072 + 32 = 131,104 bytes (FFI-13 Bytes).

Refusal cases live under `core/front/cases/ffi/` in the existing case harness, with real
manifests.

| law | bug | oracle and setup |
|---|---|---|
| **FW1** a wrong module digest | a pin checked after parsing or not at all | The row names a file of exactly `abc` pinned to the empty string's SHA-256. The build is refused with `FFI-11: module F of wasm_laws@1/native does not match its pin: expected sha256 e3b0…b855, found sha256 ba78…15ad` (FIPS 180-2). With the true pin, it is refused as `not a core WebAssembly module`. |
| **FW2** a component, or a disabled proposal | a validator run with Wasmtime's default features | A component-layer file with its true pin is refused as not a core module. A module using one `i32.atomic.rmw.add` on a shared memory is refused with `not valid with the admitted features`. So are one `f32x4.relaxed_madd`, one `memory64` memory, and one `externref` table, which is valid WebAssembly 2.0 but needs `GC_TYPES` (FFI-11). |
| **FW3** an OS import | a module that reaches a clock, a file or randomness | A module importing `wasi_snapshot_preview1.path_open` is refused with `imports wasi_snapshot_preview1.path_open, which is not a null import`. So is `env.getenv`. `box.wasm`, which imports `clock_time_get`, is admitted, and `clock(f)` returns 52 (`ENOSYS`, WASI preview 1). |
| **FW4** a header that disagrees with its module | an export of the wrong type, or ABI text that does not bind | Exporting `hwl_fill` as `[i32] -> [i64]` is refused with `… exports hwl_fill as [i32] -> [i64], but its header gives [i32 i32] -> [i64]`. A module with no `hirsel_abi` section is refused with `has no ABI text`. A header with `waits` is refused with FFI-12's text. |
| **FW5** loans are copies | a loan passed by pointer, copied back after a trap, or measured by capacity | `b = list.with_capacity(16)` filled to 4 zeros: `fill(f, mut b)` returns 4, `list.len(b)` is 4, and every byte is `0xAB`. `c` is the bytes 0..11: `sum(f, list.slice(c, 4, 8))` returns 22. An empty `b` gives `fill` 0. With `d` of four zeros, `fill_trap(f, mut d)` fails with `Fail("contained trap: unreachable")`, and `d` is still four zeros. |
| **FW6** a loan area outside memory | a write past linear memory into host memory | In a fresh task, `set_area(f, 1)`, then `sum(f, b)` with a 16-byte `b` fails with `Fail("contained loan area out of bounds")`. Then `count(f)` returns 2 (`set_area` and itself): `sum` was never entered, and the instance was not discarded. |
| **FW7** a step bound around contained code | fuel not tied to `bound steps`, or tied to wall time | `bound steps 1000 { spin(f, 10⁹) }` returns `Error(OutOfTime(steps: 1000))`. `bound steps 10⁶ { spin(f, 10) }` returns `Ok(10)`. In one build, `spin(f, n)` for n = 10⁴ takes the same steps on 3 runs: read with a step bound searched by bisection, and compared only within the build. |
| **FW8** fuel and traps are deterministic per build | a count read from leftover fuel that varies by run at a trap | `trap(f, 7)` (integer division by zero) after a fixed prologue: the bisected smallest `n` with `bound steps n { trap(f, 7) }` failing with the trap, not `OutOfTime`, is the same on 3 runs of one build. |
| **FW9** slice ends inside a contained call | a contained call that holds a scheduler thread past a slice end | On a 1-thread scheduler, task A runs `bound steps 10⁹ { spin(f, 10⁸) }`, and task B prints a line and ends. B's line appears before A's result. With no bound current, the same order is not required. |
| **FW10** bytes charged to the program's limit | linear memory outside every limit, or growth past a refused admission | In a task that has already called `count(f)`, `bound bytes 2²⁰ { (grow(f, 32), grow(f, 1)) }` returns `Ok((-1, 2))`: 32 × 65,536 > 2²⁰ is refused and the guest sees −1, and one page is admitted, the old size being 2 pages. In a fresh task, `isolate { bound bytes 65536 { count(f) } }` returns `Ok(Error(causes))` with primary cause `Capacity`, because 2 initial pages and a 4-element table are 131,104 bytes. A following `count(f)` outside it returns 1, so no instance was made. A task that grows by 16 pages and ends returns its charge: its spawner's limit then admits `bound bytes` of what it had before the spawn. |
| **FW11** NaN bits | a non-canonical NaN reaching Hirsel or linear memory | `float.to_bits32(nan32(f))` is `0x7fc00000` (SEM-2): the guest returns `0x7fa00001` unchanged by any float operator, so the boundary rule (FFI-5) is what makes it canonical. `div_bits(f, 0.0, 0.0)` divides inside the guest by `f64.div` and returns `0x7ff8000000000000`, on x86-64 and on AArch64. x86-64's own NaN for that division is `0xfff8000000000000`, so the law is red without NaN canonicalization. |
| **FW12** the trap map | a trap that aborts the process, is caught as the wrong text, or leaves the instance live | For each `k` from 1 to 10 (rows 1 to 10 of FFI-14's table; row 10 is `proc_exit(7)`, so its `X` is `exit 7`), `isolate { trap(f, k) }` returns `Ok(Error(causes))` with primary cause `Fail("contained trap: X")` and the listed `X`. Then `count(f)` returns 1, because the instance was discarded and the new generation's counter starts again. |
| **FW13** a discarded instance's handles | a use of a handle into freed linear memory, or a release run on a discarded instance | `p = make(f)`, then `trap(f, 1)`, then `isolate { free(f, p) }` returns `Ok(Error(causes))` with `Fail("foreign instance discarded")`, and a second `isolate { free(f, p) }` gives `Fail("foreign handle released")`. An `ObjBox` holding a handle made before a trap, when dropped, reports its finalizer's cause `Fail("foreign instance discarded")`, and `count(f)` shows no entry into `free`. |
| **FW14** instances are per task | state shared between tasks, or a handle crossing tasks | Two tasks each call `count(f)` 3 times, and each sees 1, 2, 3. A refusal case declares `pub opaque resource type CellBox: send { f: Foreign[Box], p: foreign.Ptr[Cell] }` and is refused at the type (FFI-12, CONC-2). |
| **FW15** a resource's mechanical release | a contained handle that a dropped resource leaks or releases twice | `ObjBox { f, p }` with `drop(o) { let _ = free(o.f, o.p) }`. Make two boxes: `live` is 2. Drop one: `live` is 1. Destructure the other, call `free` on its handle, then drop nothing more: `live` is 0, and the test passes with no cause. A box whose finalizer fails (`fail("x")`) still leaves `live` one lower, through the mechanical release. |
| **FW16** guest stack overflow | a guest overflow that aborts the process | `recurse(f, 10⁷)` fails with `Fail("contained trap: stack overflow")`, and the run goes on to print a following line. |
| **FW17** replay of a contained program | a contained call that changes replay, or a contained test's pass reused | `hirsel test --replay` of a test calling `spin`, `fill`, `grow` under `bound bytes`, and `trap` inside `isolate` replays `Sealed`. `hirsel test` twice with one cache runs the test both times and reports no reuse: it takes `Foreign[Box]`, so PROG-4 never reuses its pass. A test that reaches both `wasm_laws` and `ffi_laws` is refused as in FL7. |
| **FW18** addresses and recipes | a recipe that does not pin the module, or an address that ignores it | Changing one byte of `box.c`'s output and its pin changes the caller's revision (FFI-16). `hirsel build --recipe R --code-store DIR` with the package deleted builds and runs `count`. A recipe with the module's object missing is refused as `Missing`. |
| **FW19** cross-compiled artifacts | a host-inferred CPU feature, or an artifact that differs by host | The `aarch64-unknown-linux-gnu` artifact precompiled on an x86-64 host and on an AArch64 host is byte-identical. The image built on x86-64 runs FW5 and FW11 on AArch64. |
| **FW20** an unbuilt key refuses | a parsed and ignored `containment` | Until slice W1, a manifest with `containment = "wasm"` is refused with `FFI-10: containment = "wasm" is not built yet`. After W1, `containment = "native"` is refused as a bad field. |
| **FW21** a straight-line call past its bound | a return or trap taken with its stored fuel overdrawn | `bound steps 2 { straight(f) }` and `bound steps 2 { straight_trap(f) }` both return `Error(OutOfTime(steps: 2))`. After the call's own step, the entry starts with 17 fuel, passes its entry check and stores about 4,000 fuel consumed at `return` or `unreachable` (FFI-13 Bounds). `bound steps 300 { straight(f) }` returns `Ok(2000)`: 4,000 operators cost at most 4,016 fuel (the pinned cost table), 251 steps, plus the call's one step. |
| **FW22** a trapping call's charge | a trap charged the store's stale fuel, so a caller that catches it and calls again does quadratic work | Let `L` be the fuel of `loop_div(f, 10⁵)` up to its division, counted from `box_traps.s` with the pinned cost table. The bisected smallest `n` with `bound steps n { (isolate { loop_div(f, 10⁵) }, isolate { loop_div(f, 10⁵) }) }` returning two traps, not `OutOfTime`, lies between 2⌈`L` / 16⌉ and 2⌈`L` / 16⌉ + 1,030 (FFI-14 Fuel at a trap: each call is charged within one granule of its work). Charging the stale value puts it near ⌈`L` / 16⌉. |
| **FW23** a bulk operation under a bound | a dropped per-byte cost, or a bulk operation's check placed after it | `bound steps 100 { fill_mem(f, 65536) }` returns `Error(OutOfTime(steps: 100))`: one `memory.fill` of 65,536 bytes costs 65,536 fuel, 4,096 steps, and the entry starts with 16 × 99 + 1 = 1,585 fuel. `bound steps 10⁴ { fill_mem(f, 65536) }` returns `Ok(0xAB)`. |
| **FW24** `OutOfTime` discards the instance | a call stopped by its bound whose instance is entered again | `p = make(f)`, then `bound steps 1000 { spin(f, 10⁹) }` returns `Error(OutOfTime(steps: 1000))`. Then `count(f)` returns 1, and `isolate { free(f, p) }` returns `Ok(Error(causes))` with `Fail("foreign instance discarded")`. |
| **FW25** the pointer index is per instance | one pointer index shared by every instance, so equal addresses collide | Two tasks each call `make(f)` and get `Some(p)`, then `free(f, p)`, which returns 0. Both cells sit at the same address in their own instance (the pool's first cell), and neither `make` fails with `foreign pointer already held`. |
| **FW26** a handle's generation is checked | a handle of a discarded generation that releases a live cell at the same address | `p = make(f)`, then `isolate { trap(f, 1) }`, then `q = make(f)`, which is `Some` at `p`'s address in the new generation. `isolate { free(f, p) }` returns `Ok(Error(causes))` with `Fail("foreign instance discarded")`, and `live(f)` then returns 1: `q`'s cell is still in use. |
| **FW27** a guest overflow replays | a stack overflow point that depends on the run, not the build | `hirsel test --replay` of a test that runs `isolate { recurse(f, 10⁷) }` under `bound steps 10⁹`, beside a task that prints a line, on a 2-thread scheduler, replays `Sealed`: the steps at the overflow, and so the slice ends around it, are those of the recording. |
| **FW28** Wasmtime's signal handlers beside the runtime's | a guest fault taken as a host crash, or a Hirsel stack overflow taken as a guest trap | On a 2-thread scheduler, task A runs `isolate { trap(f, 2) }` 1,000 times and prints each primary cause, while task B recurses in Hirsel with no bound. Every line A prints is `contained trap: memory out of bounds`, and B's unbounded recursion fails with `Stack` (FAIL-17), ending the run with status 101 and a first stderr line beginning `Stack at`, not a signal. |
| **FW29** fuel exhaustion without a bound | a guest fuel trap that aborts the process | On the slow tier, with no bound current, `isolate { tgrow(f, 2^33) }` exhausts Wasmtime's `2^64 − 1` fuel in about ten minutes (each refused `table.grow` is charged `2^32 − 1`). Its isolate returns the primary cause `Fail("contained trap: out of fuel")` and the program prints a line after it. |
| **FW30** host memory refusal in contained code | an instantiation `mmap`/`mprotect` `ENOMEM` or `OutOfMemory` error that aborts in `trap_text` instead of stopping as `HostMemory` | Under `RLIMIT_AS` = 2 GiB (below Wasmtime's 4 GiB + 64 MiB linear memory reservation), a program prints a line and calls `count(f)`. Under `RLIMIT_DATA` = 96 MiB, it runs one task that calls `count(f)`, so its slot is cached, and then a task under a 192 MiB grant whose instance takes that slot and calls `grow(f, 2048)`. Each stops with status 102 and `run stop: HostMemory` (FFI-13 Bytes, FAIL-15), not `SIGABRT`. |
| **FW31** a reused linear-memory slot | a cached slot that keeps the previous instance's bytes, its size or its open pages, or that charges a grow against the previous grant | A task under 4 MiB calls `count(f)`, `grow(f, 16)` and `fill_mem(f, 1114112)`, which reads back `0xAB` at page 1. The next task, under `bound bytes 196640` (its initial 131,104 bytes plus one page), takes that slot and reads `count(f)` = 1, `fill_mem(f, 0)` = 0 and `grow(f, 0)` = 2 pages, then `grow(f, 1)` = 2 and `grow(f, 1)` = −1. |

### Contained build slices

- **W1.** FFI-10 to FFI-16 on `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, with
  laws FW1 to FW18 and FW20 to FW28. W1 is done only when every one passes on both native
  backends.
- **W2.** `aarch64-apple-darwin` and FW19.

A red law whose cause is containment (FW3, FW5, FW6, FW12, FW14, FW16, FW25, FW26, FW28),
determinism or the step bound (FW7, FW8, FW11, FW17, FW21, FW22, FW23, FW27) or a release
(FW13, FW15, FW24) blocks its slice.

### Prior art

| system | what it does | verdict | where it lands |
|---|---|---|---|
| Goose (`/tmp/ref-goose` at `d5ae5bb`, `docs/implementation.md:5595` <!-- doc-ref: external -->) | `CheckExternSpec` admits scalars and flat fixed-size values. It lends references, slices and builders in place, and states that C must not retain or invalidate them: "These are obligations on C, not proofs the Goose compiler performs". It has no contained boundary. `docs/design/wasm.md` <!-- doc-ref: external --> is about running Goose itself on Wasm. | **NOT TAKEN** for contained code, which proves its loans by copying them rather than trusting C. **ADOPTED** earlier for FFI-5's in-place loans to audited code. | FFI-12 copies |
| Wasmtime's deterministic-execution guide (`docs.wasmtime.dev/examples-deterministic-wasm-execution.html`) and its `Config`/`Store` docs at 48.0.10 | NaN canonicalization, relaxed SIMD made deterministic or off, fuel rather than epochs, `memory_init_cow` off for fuel, growth limited by a store limiter, no nondeterministic imports | **ADOPTED**: each item. **ADAPTED**: growth is decided by Hirsel limits through `ResourceLimiter` rather than by rejecting grow, so C allocators work. **REPLACED**: `relaxed_simd_deterministic` by refusing relaxed SIMD outright. | FFI-13, FFI-14, FFI-15 |
| Shopify Functions (`shopify.dev`, WebAssembly for Functions) | untrusted merchant code as Wasm under Wasmtime, one export per target, input through imported read functions, stderr ignored | **NOT TAKEN**: its host-read import API. Contained calls take typed arguments and copied loans, so no import exists to audit. The published page states no fuel or memory limits, and none is quoted here. | none |

### Rulings

The items left open in the first text are ruled here, after the soundness review
contained-foreign-review-2903. Where this text differs from the review, it says so.

- **O1. A modified artifact is code execution.** The trust is accepted as written. A contained
  artifact comes only from the build user's own part cache or is compiled in place. Kiln RE and
  the code store carry module bytes, never artifacts. The cache key holds the toolchain image
  identity (FFI-15 Caching). No signature binds an artifact.
- **O2. Signal handlers.** `signals_based_traps(true)` stays. The runtime installs its handlers
  before Wasmtime's, and their coexistence on Linux is law FW28, a W1 law, not an open
  question. Mach exception ports on macOS, and their loss across `fork()`, belong to W2.
- **O3. Executable memory on macOS arm64.** Closed by the loader (FFI-15): the artifact's code
  sits in the image's signed `__TEXT`, and no executable memory is mapped or written at run
  time. W2 confirms it on a Mac.
- **O4. Spectre and side channels.** In-process containment is enough for v1. The guest has no
  clock, no threads and no shared memory, so it has no timer to read a cache channel with. The
  heap mitigations stay on, and FFI-10 says timing channels are not a promise.
- **O5. Steps across a trap.** The reported value is deterministic but was wrong by an unbounded
  amount: a loop with no call never wrote the counter. A trapping entry is now charged its
  stored fuel plus one granule, capped at the bound (FFI-14 Fuel at a trap), and the granule
  makes the stored value lag by less than one granule plus one straight-line run. Laws FW8 and
  FW22.
- **O6. Does a contained interface need a grant?** Yes (FFI-14 Grants).
- **O7. Address space per instance.** Per-task instances and the 4 GiB reservation stay for
  W1. The ceiling, about 30,000 live instances and then `HostMemory`, is stated in FFI-13
  Bytes. Discarded instances' slots are reused (FFI-13 Slot reuse). A per-unit lease would
  add CONC-10 events.
- **O8. Per-task instances cost the shared-handle model.** Accepted. The target libraries
  hold no state across tasks, and the other design adds to the total-order promise, which is
  not allowed.
- **O9. Two Cranelifts.** Both are built, in the driver only (FFI-15 The pin). The backend is
  not downgraded and Wasmtime stays on the LTS line.
- **O10. `loan_area` is guest code.** The guest's own business, as FFI-12 says. A run-owned
  region needs multi-memory, which widens the feature set for no safety gain.
- **O11. Unbounded calls.** Kept: no slice end without a bound, exactly as for Hirsel code. A
  caller that runs an untrusted module wraps the call in `bound steps`.
- **O12. Trusting Wasmtime and Cranelift.** Accepted as the price of in-process containment
  (FFI-15 The pin).
- **O13. Loan copies cost no steps.** They now do: one step for each 16 KiB, or part, copied
  in and again copied back, all taken at the call before anything is copied (FFI-13 Steps,
  MEM-30).
- **O14. `OutOfTime` takes the instance.** The discard stays. The stopped call's state is
  partway through an update, and resuming it after its scope has exited would be a
  continuation. A wrapper puts its bound outside the handle's lifetime. Law FW24.
- **O15. The `Provider` stop.** Neither a clause nor a new name. An unlisted trap under a closed
  feature set is a failed runtime invariant and aborts as one. `OutOfFuel` is listed because
  both bounded and unbounded calls can reach it (FFI-14 Traps).
- **O16. Pass keys and the toolchain.** Already met: the pass key holds the toolchain image. The
  real defect was that FFI-14 promised reuse for tests that take `Foreign[I]`, which PROG-4
  never reuses. This text differs from the review here. The review recommended amending PROG-4
  so that a test whose only parameters are contained `Foreign[I]` could be reused. This lane
  was directed to keep PROG-4 and the driver (`crates/hirsel-core-driver/src/test_run.rs`
  lines 342-349) as they are, so FFI-14 and FW17 now say a contained test is run every time.
  Extending reuse to contained tests stays possible later, as a PROG-4 change.
- **O17. Mixed programs.** Agreed. Nothing changes here.

The review's fuel mechanism is adopted with one change. The review gave the store the bound's
fuel plus `2^40`, counted the bound at granule ends, and never used `Trap::OutOfFuel`. A granule
end counts one granule however much a bulk operation charged, though. Under `bound steps 10⁹`,
about 65,000 fills of 16 MiB pass the `2^40` margin while the granule ends count only about
6.7 × 10⁷ steps, so the bound would have run far over before anything stopped it. Here the
store holds the bound's fuel plus one. Wasmtime's fuel checks stop the entry with
`Trap::OutOfFuel` if it runs out. The run also checks the stored fuel at return and at
any other trap (FFI-13 Bounds). Granule counting decides only slice ends.
