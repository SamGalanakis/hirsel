# Packages, workspaces and the lock

This chapter defines how the `hirsel` tool finds the packages of a request, binds each
dependency to exact content, and hands the result to the compiler. A **package** is the
language's unit (PROG-1, [programs](programs.md#prog-1-packages)): a name, a major version and
modules. Its identity is `name@major`, two majors of one name are two packages, and
everything here happens before name resolution: the compiler receives one resolved set of
packages, each with its sources and its exact dependencies, and never sees a manifest, a lock
or a fetch. Nothing in this chapter changes what an accepted program means
([PROG-6](programs.md#prog-6-one-meaning)): a flag such as `--locked` decides only whether a
command proceeds.

The files are `hirsel.toml`, the **manifest** of a package or a workspace, and `hirsel.lock`,
the workspace's **lock**. There is no registry. A dependency is either another package of the
same local workspace or a published library fixed by its root. The canonical library content
(section 1) is separate from the checked package image and from the executable release root R
([launch](launch.md#release-recipe-and-host-assembly)). A library root names source, and R names
assembled code.

It is unbuilt design: the built `hirsel` reads `manifest.json`, and this chapter replaces it.

## 1. Canonical library content

A **published library** is one package's canonical content, its **encoding** `C`, and its
**library root** `P = H("hirsel.library-root.v1", C)`. `H(d, x)` is PROG-10's: SHA-256 over
`d` and then `x`, each preceded by its byte length as an 8-byte little-endian number. A root
is written `sha256:` followed by its 32 bytes as 64 lower-case hex digits. A root is full
SHA-256 and is never shortened.

**Encoding.** In `C`, a number is unsigned LEB128 with no redundant byte, and a text is its
byte length as such a number followed by its UTF-8 bytes. `C` holds, in order:

1. the encoding version, the number 1. Version 1 has held item 6 from its start: bytes that
   end before the foreign-module count are a truncated encoding and are refused, and since no
   library was published before item 6, nothing needs republishing;
2. the package's name, a text, and its major, a number;
3. its charge policy (PROG-1), a text, which in v1 is `charge-v1`;
4. its **bindings**: their count, then each binding, sorted by name bytewise and then by
   major numerically. A binding is the dependency's name (a text) and major (a number), then
   a tag: `0` for a toolchain package, or `1` followed by the dependency's 32-byte library
   root;
5. its **files**: their count, then each file, sorted by path bytewise. A file is its path (a
   text), its module path (the number of components, then each component as a text), and its
   bytes (their length as a number, then the raw bytes);
6. its **foreign modules** ([FFI-2](foreign.md#ffi-2-foreign-modules)): their count, then each.
   One is its module path, encoded as in item 5, its symbol prefix (a text) and its archive
   rows: their count, then each row, as the target (a text), the file's path (a text), the
   32-byte pin, and the archive's bytes (their length as a number, then the raw bytes).

Foreign modules are in strictly ascending order of module path, compared component by
component, each component by its UTF-8 bytes bytewise, and a path that is a proper prefix of
another first. The framed bytes are never compared: `aa` comes before `b`, although the frame
of `b` is the smaller byte string. A module's rows are in strictly ascending order of target
bytewise. Bindings and files are likewise strictly ascending, so no two are equal.

`core@1` is bound in every package and is never written as a binding. `std@1` is a toolchain
package (section 2) and is written with tag `0`, so a library root says which std major it
uses, and the toolchain that checks it supplies that std. A toolchain package has no root.
Its content is the toolchain's and enters every key through the compiler and the toolchain
identity, as it does today.

**Inventory.** The files of `C` are exactly the package's `[sources]` (section 2), with their
modules, and its archives are exactly the rows of its `[foreign]` tables, for every target.
Nothing else in the package directory enters: not `[tests]` sources, `[program]`, `[tool]`,
`hirsel.lock`, any other manifest field, a build or cache directory, or a file the manifest
does not list. A test-only file that joins a library module, as `assembly_laws.hirsel` joins
module `store`, is left out, and the module keeps exactly its `[sources]` files. The manifest
enters only through items 2 to 6: identity, charge, bindings, the `[sources]` mapping of files
to modules, and the `[foreign]` tables, after the workspace's inherited values are expanded
(section 4) and each member binding is replaced by that member's root (section 6). File bytes
are kept exactly, line endings included. Permissions, timestamps, ownership and the order of
the directory listing never enter.

**Path rules.** A listed path, a source or an archive's `file`, is relative to the package
directory. It is one or more components joined by `/`. Each component matches
`[A-Za-z0-9_][A-Za-z0-9_.-]*` and is at most 255 bytes, so it is never empty, `.` or `..`, and
never starts with a dot. The whole path is at most 1024 bytes. A source path ends in
`.hirsel` and an archive path in `.a`. The listed paths of one package, sources and archives
together, form one **tree**, compared after ASCII lower-casing: no two paths are equal, no
path is a proper component prefix of another (so `a.hirsel` cannot also be the directory of
`a.hirsel/b.hirsel`), and two paths that share a directory component after lower-casing spell
it identically. So `Data.hirsel` beside `data.hirsel/x.hirsel` is refused, and so is
`native/Lib.a` beside `native/lib.a` in another target's row. One file is named by one row: two
rows never name one path. A package that checks on a case-sensitive file system therefore
unpacks to the same tree on one that folds ASCII case, such as macOS's default. v1 promises
nothing for file systems with further name rules: Windows device names such as `CON` and
components with trailing dots are not refused. A module path is one or more identifiers
([syntax](syntax.md)) and is written in the manifest with `.` between components.

**Reading member files.** The tool reads a member's manifest, its listed sources and its listed
archives through a handle on the member's directory, never by re-resolving a pathname. It
opens each path one component at a time relative to the directory handle it holds (as
`openat` does), and checks every opened object on its handle (as `fstat` does). A component
may be a symbolic link. The tool follows it by resolving its target the same way: a relative
target from the directory that holds the link, one component at a time, and `..` steps to
the walk's parent directory; an absolute target is refused. The walk must never leave the
package directory, not even on the way to a file inside it. The final object must
be a regular file. Then the listed path names that file's bytes, and nothing about the link is
recorded. A path is refused (section 9) when it leaves the package, when it ends at a
directory or at nothing, when it meets a link loop, and when the host cannot open relative to
a held directory handle, because then containment cannot be established. A member's directory
is opened the same way from the workspace root's directory handle, so a member directory that
is a link to a place outside the workspace is refused, and the lock's `member:<dir>` names
the directory actually read.

What the tool reads it **captures**: the manifest bytes, each source's bytes and each
archive's bytes, under their listed names, read once in resolution steps 1 and 2 (section 5).
Every later use takes the capture and never the file system: computing a root, checking,
packing, test configuration, staging a native archive for the linker (FFI-3), and rendering a
diagnostic with its line numbers. So the bytes a member is checked with are the bytes it would
publish, and a file replaced while a command runs cannot reach it. The directory convention
below keeps its own walk, which follows a source symlink wherever it points, because a
directory-convention package is never a dependency and never published. It captures what it
reads in the same way.

**Limits.** One `C` is at most 64 MiB and holds at most 65 536 source files and
foreign archive rows together. The producer checks the combined count before
encoding `C` or computing its root. The decoder enforces the same count limit.

**Decoding.** A reader accepts `C` only when it is the encoding of a package that a valid
effective manifest describes, with every binding a tag 0 or a root. It applies the same checks
to every `C`, whether it comes from an archive's head, an archive's other entry or the package
store. It refuses, and its first failure is reported in these words:

| refused | first decoding failure |
|---|---|
| a version other than 1 | `unknown version <n>` |
| a number with a redundant byte, or above its range (a major above 4294967295) | `bad number at byte <offset>` |
| a text that is not UTF-8 | `bad text at byte <offset>` |
| a package or binding name outside `[a-z][a-z0-9_]*` or over 64 bytes, a package named `core` or `std` | `bad package name "<name>"` |
| a charge policy other than `charge-v1` | `unknown charge policy "<text>"` |
| a binding of `core`; a tag other than 0 or 1; a tag 0 on anything but `std` major 1; a tag 1 on `std` | `bad binding <name>@<major>` |
| two bindings with one name, at any majors, since source names a dependency by name alone | `two bindings of <name>` |
| bindings, files, foreign modules or rows out of order or repeated | `<part> out of order at <the later item>`, with part `bindings`, `files`, `foreign modules` or `archive rows` |
| no files | `no files` |
| a path or module path that breaks the rules above, including the tree rule | `bad path "<path>": <which rule>` |
| a foreign module that is not formed by exactly one file of item 5 | `foreign module <m> is formed by <k> files` |
| a symbol prefix outside FFI-2's form, or two foreign modules with one prefix | `bad symbol prefix "<prefix>" of <m>` |
| a target that is not 1 to 64 bytes of `[a-z0-9_.-]` | `bad target "<target>" of <m>` |
| an archive whose bytes do not hash to its pin | `archive "<path>" of <m> for <target> does not match its pin` |
| bytes that end inside an item | `truncated at <offset>` |
| bytes left over at the end | `trailing bytes at <offset>` |
| an encoding over the limits | `over the limit of <limit>` |

A `<name>` is written as in the manifest, a module path `<m>` with `.` between components, and
an offset counts bytes from the start of `C`. So decoding and re-encoding give the same bytes,
two packages with one root have one meaning, and a reader never admits a package that no
manifest could publish. The decoder does not parse an archive's bytes: whether they are an
indexed `ar` archive is FFI-3's check at link time.

**What the compiler reads.** A published package's sources are the file bytes of the `C`
whose root was checked against its pin (section 7), decoded in memory. A member's sources are
its capture. The compiler is handed those bytes, never a path to re-read. The compiler names
the files of the member the command was given by their package-relative paths, as today's
manifest-relative names do (`json.hirsel`). It names every other package's file
`name@major/path` (`json@1/json.hirsel`), in diagnostics, sites and annexes, whether the
package is published or a member. So a file's name is the same whether a dependency is a
member, a coalesced member or a published library, and no name depends on where the package
store or the workspace sits on disk.

## 2. `hirsel.toml`

A manifest is a TOML 1.0 document. Every table and key below is the whole set: an unknown
key is refused, except inside `[tool]`. A package manifest has `[package]` and may have the
other tables.

```text
[package]
name = "store"            # [a-z][a-z0-9_]*, at most 64 bytes; never core or std
major = 1                 # 0 to 4294967295
charge = "charge-v1"      # or: charge.workspace = true

[sources]                 # package-relative file = module path; at least one
"store.hirsel" = "store"
"ingress.hirsel" = "ingress"
"wal/append.hirsel" = "wal.append"

[dependencies]            # key = the dependency's package name
std = { major = 1 }                                      # a toolchain package
util = { major = 1, path = "../util" }                   # a workspace member
json = { major = 1, root = "sha256:cf1e…", url = "https://example.org/json-1.hpkg" }
yaml = { major = 2, root = "sha256:…", archive = "vendor/yaml-2.hpkg" }
toml = { major = 1, root = "sha256:…" }                  # already in the package store
shared = { workspace = true }                            # see section 4

[program]                 # optional
module = "main"           # default "main"
function = "main"         # default "main"

[tests]                   # optional; the test configuration of PROG-4
limit = 67108864          # required when [tests] is present
grants = ["Clock", "Random", "Tasks", "Net=Listen 127.0.0.0/8 0-65535"]
dirs = ["."]

[tests.sources]           # test-only files; may join a library module
"assembly_laws.hirsel" = "store"
"runner_wire.hirsel" = "runner_wire"

[tests.runs."store@1/store/frame_size_boundary"]
limit = 134217728         # replaces limit, grants or dirs for one test

[foreign.native]          # optional; module native is a foreign module (FFI-2)
symbols = "hsq_"
archives = [{ target = "x86_64-unknown-linux-gnu", file = "native/x86_64/libsq.a", sha256 = "…" }]

[tool.workloads]          # anything; hirsel never reads [tool]
s1 = { module = "main", grants = ["Stdout"] }
```

Every package is a library: any package can be bound as a dependency and packed. There is no
`[library]` table. A library's content is its `[sources]`, and a program is a library with an
entry (`[program]`).

**`[package]`.** `name` and `major` give the package its identity `name@major`. `charge`
states its charge policy (PROG-1), and `charge-v1` is the only value v1 accepts.

**`[sources]`.** Each key is a file and its value is the module the file belongs to, so each
file is one module and several files may form one module. The path rules of section 1 apply
to every key. The listed files are the package's sources in every build. In a published
library they are its whole inventory. A package manifest without `[sources]`, or with an empty
one, is refused: the directory walk below is only for a directory with no manifest.

**`[dependencies]`.** Each key is a dependency's package name, so a package names at most one
major of each name. Each value has exactly one of these forms. `major` is required in all but
the last.

- `{ major }` alone binds a **toolchain package**: only `std`, and only the major the
  toolchain has (1). `core` is bound in every package and may not be declared.
- `{ major, path }` binds a member of the same workspace (section 4). The path is relative to
  this manifest's directory and must resolve to a member's directory, whose `[package]` has
  this name and major.
- `{ major, root }`, with at most one of `url` or `archive`, binds a published library. `root`
  pins its content. `url` is an `https://` URL of a package archive (section 7). `archive` is a
  package archive's path relative to this manifest's directory, `/`-separated. It is never
  absolute. It is resolved **lexically**: it is appended to the declaring manifest's directory,
  written relative to the workspace root, and each `..` then removes the component before it,
  as text, without looking at the file system. The result must have no `..` left, so it lies
  inside the workspace root, and it is the archive's one spelling: the lock writes it, and a
  fetch opens exactly that path from the root's directory handle under the reading rule of
  section 1, so a link on the way cannot make one spelling name two files. With neither, the
  root must already be in the package store. The source says where content may come from, and the root
  says what it must be: the root enters the package's own `C` and the source never does.
- `{ workspace = true }` alone inherits the workspace's declaration of this name (section 4).

**`[program]`.** The entry is the `pub fn` named `function` in module `module` (PROG-3). With
no `[program]` the entry is `main` in module `main`, as today. A command that needs an entry
refuses a package without that function. A dependency's `[program]` is never read.

**`[tests]`.** This is today's test configuration (PROG-4, from 7b1fd64c), unchanged in
meaning. `limit` is the root limit of every test run, required when `[tests]` is present.
`grants` take the `--grant` syntax and `dirs` the `--dir` paths. `[tests.sources]` lists files,
under the same path rules, that join the package's modules in its own test runs and in no
other build. A test-only file may join a library module or form a module of its own.
`[tests.runs]` keys are test definition paths (`store@1/store/frame_size_boundary`), and each
run replaces any of `limit`, `grants` and `dirs` for that test alone. A run naming a test the
package does not have is refused. A dependency's `[tests]` is never read and never enters `C`.

**`[foreign]`.** Each key is a module of `[sources]` that is a foreign module, with its symbol
prefix and archive table. [FFI-2](foreign.md#ffi-2-foreign-modules) defines the fields and their
refusals. Every row's file is read and captured in resolution step 2, for every target, not
only the build's: a missing, escaping or non-regular archive file is refused like a source
file. A member's archive whose bytes do not match its pin is refused when the member's root is
computed (section 6), and FFI-3 refuses it when a build links it; a check that links nothing
does not refuse it. Every key that depends on a foreign module holds the SHA-256 of the
captured bytes as well as the pin (section 10).

**`[tool]`.** This table is for other tools: hirsel ignores its contents, and they never
enter `C`, the lock or any key.

**The directory convention.** A directory without `hirsel.toml` is package `app@1`. Each
`.hirsel` file under it is one module, named by its relative path components. It depends on
`core@1` and `std@1`, and its entry is `main` in module `main`, as today. It has no lock, binds
nothing else, and cannot be a member or a dependency. `--inputs FILE` stays the explicit
compiler input for measurements: it names packages, files and dependency identities directly,
binds nothing, and reads no manifest or lock. Its files keep the names the JSON gives them,
which may lie outside the JSON file's directory, and each is read and captured once, as a
member's is, but without the containment rule, because an explicit corpus has no package
directory. Neither the directory convention nor `--inputs` has a lock, so `--locked` and
`--frozen` are refused with either (section 11).

## 3. Finding the workspace

A command that takes a package directory D (the current directory by default) finds its
workspace as follows:

1. The **package manifest** M is the nearest `hirsel.toml` at D or above it. With none, D uses
   the directory convention.
2. If M has `[workspace]`, the workspace root is M's directory. Otherwise the tool looks
   further up for the nearest manifest with `[workspace]`. If that manifest lists M's
   directory among its members, it is the root. If it does not, the command is refused,
   because a package inside a workspace that does not list it would resolve differently
   depending on where a command runs. If there is no such manifest, M's directory is a
   single-package workspace.
3. If M has `[workspace]` and no `[package]`, it is a root and not a package. A command that
   needs one package (check, build, publish, run, test, inspect, replay, sdk, pack) is refused there.
   `lock` and `fetch` act on the whole workspace.

The lock is `hirsel.lock` in the workspace root's directory. Directories are compared by the
identity of the directory opened (device and inode), never by the spelling of a path.

## 4. Workspaces

A workspace root's manifest has:

```text
[workspace]
members = ["store", "libs/json", "tools/gen"]   # directories, relative to the root

[workspace.package]
charge = "charge-v1"

[workspace.dependencies]
json = { major = 1, root = "sha256:…", url = "https://example.org/json-1.hpkg" }
util = { major = 1, path = "libs/util" }
```

**Members.** Each member is a directory relative to the root, under the section 1 path
component rules (a component may also lack the `.hirsel` ending). A member path never climbs
with `..`. Each member directory holds a manifest with `[package]` and without `[workspace]`.
A root manifest that also has `[package]` is itself a member, and `.` is never listed. No
member is listed twice, and no two members share a `name@major`. Members are listed exactly:
there are no globs, and a directory is never a member because it sits under the root or
because a member names it by path. Each listed member's directory must find this root by step 2
of section 3: the nearest manifest with `[workspace]` at or above the member's directory, other
than the member's own, is this root's. A member that a nearer workspace root would claim is
refused before inheritance or lock derivation, so one member directory is never expanded under
two roots.

**Inheritance.** A member inherits only what it asks for, by name:
`charge.workspace = true` takes `[workspace.package].charge`, and `name = { workspace = true }`
takes `[workspace.dependencies].name`. `[workspace.package]` has only `charge`.
`[workspace.dependencies]` takes the forms of section 2 other than `workspace = true`. A path
in it, a `path` or an `archive`, is relative to the root's directory, because the root's
manifest wrote it. An inherited value is used exactly as written: a member adds nothing to it
and overrides nothing in it. The effective manifest of a member, with every inherited value
expanded, is what the lock and `C` see.

**Edges.** Membership adds no edge. One member uses another only through a `path` binding in
its own `[dependencies]`, written there or inherited. A module that names a package its own
package does not bind is refused by name resolution as today
([LANG-7](language.md#lang-7-names)), whatever the workspace holds.

**Cycles.** Path bindings among members form no cycle. A published library cannot bind a
member, and its bindings are roots, so published content forms no cycle either.

## 5. Resolution

Resolution turns the members' effective manifests into the workspace's **closure**: every
`name@major` reached from any member, each bound once. It runs in this order, and the first
refusal stops it:

1. **Manifests.** Read, capture and validate every member's manifest and the root's (sections
   2 to 4).
2. **Member files.** Read and capture every member's `[sources]`, `[tests.sources]` and
   `[foreign]` archive files once, every target's, under the reading rule of section 1. These
   bytes are the ones the compiler is handed, the ones a member's root is computed from and
   the ones a build links.
3. **Content.** Acquire the `C` of every published root reached, in rounds, until no new root
   appears. A **pending** root is one reached and not yet decoded, and pending roots are taken
   in order of the `name@major` their edges expect, and then of root. In each round:
   1. each pending root whose store entry is present and undamaged is opened from the store;
   2. each pending root left that has a declared source, a `url` or `archive` in a
      declaration that binds it, is fetched from its lock source (section 8), the first such
      declaration in identity order of the declaring packages. Only that source is tried, and
      its failure is the refusal. A fetched archive puts all its entries in the store
      (section 7), so it can satisfy other pending roots;
   3. only when the first two steps have nothing left to do, each pending root left is
      fetched from the declared source of its nearest **carrier**: follow the bindings back
      from the root, through published packages whose `C` binds it and then their binders,
      to the first published root that has a declared source, taking the least path in the
      order of section 5's refusal paths. That root's archive holds its whole closure, so it
      holds the pending root, and it is fetched even when that root itself came from the
      store;
   4. a root still pending is refused as `no source`, the least first.

   Each `C` opened is decoded (section 1) once per root, and its bindings join the walk. Then,
   for **every** incoming published edge, a binding of `name@major` to root P by any package,
   the decoded package at P must be `name@major`, whether P came from the store, an archive's
   head or another of its entries, and however many edges reached P first; otherwise the edge
   is refused as `fetched identity`. Under `--frozen` steps 2 and 3 never run: the first
   pending root that the store cannot open is refused.
4. **Binding.** Bind each `name@major` once:
   - toolchain packages bind to the toolchain;
   - a member binds to the member;
   - a published library binds to its root.
   Equal roots of one `name@major` coalesce: two packages that both bind `json@1` to the same
   root share one package. Two different roots for one `name@major` are refused, as is a
   member and a root for one `name@major`, unless the root equals the member's own root
   (section 6). A diamond refusal names both paths and both roots (section 9). Different
   majors are different packages and coexist: `json@1` and `json@2` may both be in a closure,
   from two packages.
5. **Lock.** Derive the lock (section 8) and compare it with `hirsel.lock`.

**Paths in refusals.** A **top member** is a member that no other member binds. The **path**
to a binding is the shortest chain of bindings from a top member to the package that declares
it, followed by the bound package. Ties are broken by comparing the chains package by package,
each by name bytewise and then by major numerically. A path is written with ` -> ` between
`name@major` identities (`app@1 -> a@1 -> json@1`).

**Compiler input.** A command on member X hands the compiler X's closure, the packages
reachable from X and nothing else from the workspace, as today's compiler input (the
`CorpusManifest` of `--inputs`). Each package has its identity, its sources (a member's files,
or a published package's verified bytes) and its dependencies: its bindings and `core@1`.
X's `[tests.sources]` join X's modules in X's test runs and in the queries about X's tests
(`hirsel inspect --affected-tests`), and in no other build. `hirsel test` runs the tests
declared in X's own `[sources]` and `[tests.sources]`, including tests with capability
parameters, as PROG-4 specifies. Only parameterless tests can get a cached pass.
`hirsel inspect --affected-tests` considers the parameterless tests in those same files.
A dependency's tests are compiled as its other declarations are, for ordinary checks, and
are never selected, run or given a per-test configuration. A request never holds two
packages with one `name@major`, so name resolution reads each identity once.

**Toolchain packages and images.** Resolution comes first, before any image is chosen. The
toolchain packages a request holds are exactly those X's closure binds: a member that binds
only the implicit `core@1` gets no `std`, on the source path and the installed path alike,
while a directory-convention package keeps its default `std@1` binding. An image is admitted
to supply a package's code only when it was produced for the same toolchain and holds exactly
the packages it is asked to supply: for each, the same identity, the same root for a
published package, the same image stamp (section 10) for a member, and the same bindings. An
image that holds any other package, such as a std image for a core-only request, is not used,
so no namespace reaches the compiler from outside the closure. An explicit `--image` follows
the same rule and is refused with `hirsel: --image <path> does not hold exactly this request's
packages` when it fails.

## 6. Publishing a library

`hirsel pack [DIR] --out FILE` publishes the member at DIR. It resolves the workspace as any
command does, then computes the member's root:

- A binding to another member is encoded as that member's own root, computed the same way,
  recursively. So a member's root is defined whenever its bindings resolve, and it changes
  when any member it reaches changes.
- A binding to a published library is encoded as its root.
- A binding to `std` is encoded as a toolchain binding.

The member's `C` holds its captured `[sources]` files and `[foreign]` archives, every target's,
and nothing of `[tests]`, `[program]` or `[tool]`. Each archive's captured bytes must match its
pin here (the `member pin` refusal of section 9). `pack` then writes a package archive (section 7) holding
that `C` and the `C` of every package its bindings reach, other than toolchain packages:
members encoded the same way, and published libraries copied from the store. Before writing,
`pack` decodes the archive it produced and verifies every root again as a fetch would, and it
refuses an `--out` that names, by file identity, a manifest, a captured file, the lock or a
store entry. It puts
every package into the package store and prints one line per package of the archive, head
first:

```text
json@1 sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58
```

Publishing never edits a manifest. Inside the producing workspace the member binding stays a
`path`, and the published root is what that path binding means to anyone else. A workspace
that wants to depend on the published library instead writes `{ major, root, archive }` or
`{ major, root, url }` with the printed root. A consumer of the archive gets every package its
head's root reaches, each verified against the root that binds it. So the consumer can check
the library's whole transitive meaning without the producer's workspace.

A member and a published root may meet in one closure under one `name@major`. That happens
when a workspace holds library `b` as a member while a published library it uses binds an
earlier publication of `b`. They coalesce exactly when the member's root, computed as above,
equals the published root: the content is then identical, and the member's files are used. A
member edited since that publication has another root, and the pair is a diamond refusal. The
coalesced package keeps the published binding's obligation: its lock row carries that root
(section 8), and its root is in the check environment as a published package's is (section
10). So a later edit that moves the member and the published pin together, from X to Y,
changes the lock, and so does replacing the root binding with a path.

## 7. Package archives, fetching and the store

**Archive.** A **package archive** is the text `hirsel.package-archive.v1` (framed as a text
of section 1), then the number of packages n, which is at least 1, then n entries, each a
`C` framed as its byte length (a number) followed by its bytes. Entry 0 is the **head**. The
others are in strictly ascending order of root. Together they are exactly the packages that
the head's bindings reach, transitively, other than toolchain packages. An archive has no
identity of its own: it is transport. One library can travel in archives that differ only in
how they reach the consumer, and the head's root is what a binding pins. An archive is at most
512 MiB and holds at most 4096 packages. Its file extension is `.hpkg` by convention, and
nothing depends on it.

**Sources.** A v1 source is one of:

- a workspace member's path;
- an `archive` path to a package archive on the local file system;
- a `url`, an `https://` URL fetched by one GET, following redirects only to other `https://`
  URLs, with the response body capped at the archive limit;
- the package store, for a root with neither `url` nor `archive`, or one an archive brought in.

There is no registry, no version range and no search.

**Verifying a fetch.** For a binding of `name@major` to root P, the tool reads the archive's
bytes and, before keeping or using anything:

1. decodes the archive and each entry under the decoding rules of section 1;
2. checks that the head's root is P and that the head's name and major are the binding's;
3. checks the closure: each entry is reached from the head's bindings, each published
   binding of each entry names an entry of the archive, and each such entry's name and major
   match the binding. An archive is complete on its own, and `pack` always writes it so,
   copying any published package it reaches out of the store;
4. only then writes each entry to the store.

A failure at any step keeps nothing from the archive, not even under the root it actually
has. So a mismatched fetch never fills the store under some other name.

Steps 2 and 3 apply to every entry, the head's children as much as the head, and to the
edges between them. An entry's place is earned only by an edge of this archive: a child that
the store already holds does not make up for its absence from the archive, and an entry no
edge reaches is refused as extra. A refused archive leaves the store exactly as it was.

**Store.** The **package store** is the directory `--package-store DIR` names. By default it is
`hirsel/packages` under the user cache (`$XDG_CACHE_HOME`, else `$HOME/.cache`), beside
today's `hirsel/core-packages`. It holds each verified package as the file
`<64 hex digits of P>`, whose bytes are `C`. A write goes to a temporary file in the store
directory and is renamed into place, so a reader sees a whole entry or none. Every open
re-hashes the entry before decoding it, so the tool never trusts a store entry by its name. An
entry whose bytes do not hash to its name, or that hashes to its name but does not decode under
section 1, is **damaged**. It is treated as missing: a fetch may
replace it, and under `--frozen` its refusal says it is damaged (section 9). A store shared by
several workspaces is safe, because its names are its contents.

## 8. `hirsel.lock`

The lock is the workspace's resolved closure, written down so that a change in it can be
reviewed and so that a locked command can say what changed. It is derived entirely from the
effective manifests and the bindings in the published `C`s. No command ever reads a binding
from it, so it is never a second source of truth.

**Rows.** The lock has one row per `name@major` of the closure, sorted by name bytewise and
then by major numerically. `core@1` has no row and appears in no edge list. A row has:

- `id`, the `name@major`;
- `root`, for a published library, and for a member that coalesced with a published binding
  (section 6): the published root;
- `source`, which is one of:
  - `toolchain`;
  - `member:<dir>`, the member's directory relative to the root, or `.` for the root;
  - `archive:<path>`, relative to the root;
  - `url:<url>`;
  - `store`;
  - `via:<id>`;
- `edges`, the row's bindings as `name@major` texts, sorted as the rows are.

A published row's source comes from the manifests and the `C`s alone, never from what the
store happened to hold or which archive was opened. It is the first `url` or `archive` among the
declarations that bind that root, taking declaring packages in identity order; a declaration
with `root` alone names no source. With no such declaration, it is `via:<id>`, the first
published package, in identity order, whose `C` binds that root. With neither, it is
`store`. A member row's source is always `member:<dir>`. A member row that no published binding
names has no root, because a member's bytes change with every edit. The member's files enter
every key through their content (section 10), and a member's root is computed when it is
packed. A coalesced member row has the root its published binding pins, so the pin
obligation is visible and compared. All paths use `/` and are normalised, with no `.` or `..`
components and no trailing `/`. An `archive:` path is the lexical spelling of section 2,
derived from the declaration and the declaring manifest's place alone: deriving or comparing a
lock never opens, resolves or inspects an archive path or a link on it.

**Bytes.** The lock has exactly one encoding, so comparing locks is comparing bytes:

```text
# hirsel.lock: written by hirsel; do not edit.
version = 1

[[package]]
id = "app@1"
source = "member:."
edges = ["json@1", "std@1"]

[[package]]
id = "json@1"
root = "sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58"
source = "archive:vendor/jx.hpkg"
edges = []

[[package]]
id = "std@1"
source = "toolchain"
edges = []
```

The lock holds the comment line, `version = 1`, then one blank line before each row. A row's
keys are always in the order `id`, `root`, `source`, `edges`, and the file ends with one line
feed. A TOML string is a basic string. In it `"` is written `\"`, `\` is written `\\`, each of
U+0000 to U+001F and U+007F is written `\u` and four upper-case hex digits (`\u001B`), and
every other character is written as itself, with no other escape. Lines end with a line feed
alone.

**Staleness.** A lock is **stale** when its bytes differ from the derived lock. The tool then
parses the old lock and reports what changed, one line per difference, rows in order and, within
a row, `root`, then `source`, then `edges`:

```text
  json@1: root sha256:<old> -> sha256:<new>
  json@1: source archive:vendor/jx.hpkg -> archive:vendor/jy.hpkg
  json@1: edges [] -> ["util@1"]
  lib@1: root none -> sha256:<new>
  util@1: added
  yaml@2: removed
```

A row that gains or loses its `root`, as when a member starts or stops coalescing with a
published binding, writes the missing side as `none`.
An old lock that does not parse as a version 1 lock, or whose bytes differ while its parsed
rows are equal, is reported as one line, `  hirsel.lock is not in the form this hirsel
writes`.

**Who writes it.** Every command that resolves a workspace (check, build, publish, run, test,
inspect, replay, sdk, pack, fetch and lock) derives the lock. Without `--locked` or `--frozen`, a command
whose derived lock differs from the file writes the derived lock, through a temporary file and
a rename, at its **commit point**. For every command but `pack` that is when resolution has
succeeded, before the command's own work: a later compile error or failing test leaves the
written lock, which describes the resolved workspace correctly. `pack` first builds and
verifies its whole archive, its limits included, and then writes the lock, puts the entries in
the store and installs its output, in that order. A refusal before the commit point writes no
lock. Each write is atomic on its own, and a later I/O failure can leave earlier writes, which
are correct by content; nothing is rolled back. It writes `hirsel: created hirsel.lock` to stderr when there was none, or
`hirsel: updated hirsel.lock:` followed by the change lines, and then goes on. `hirsel lock`
does only this, and `hirsel new` writes the first lock with the package.

**Input modes.** Only a command given a package directory inside a workspace has a lock. With
`--locked` or `--frozen`, a directory without a manifest, `--inputs`, a `--recipe` build and an
SDK request for an executable are refused with `hirsel: --locked needs a workspace lock; <what
was given> has none` (or `--frozen`), and never fall back to the directory convention.
`hirsel replay PACKAGE` and `hirsel sdk` given a package directory resolve through this chapter
like `check`, honour the flags and the package store, and `sdk` passes them on to the build it
runs.

**`--locked`.** Any of those commands refuses a missing or stale lock and writes nothing. It
may still fetch pinned content that the store lacks, because a fetch only fills the store with
verified bytes the manifests already pin.

**`--frozen`.** This is `--locked`, and it also refuses to fetch. Every published root of the
closure must already be in the store, undamaged. The refusal comes in resolution step 3,
before any source is touched, so a `--frozen` command never opens a URL or an archive path.

## 9. Diagnostics

A tool refusal is one block on stderr with exit status 2, like every other `hirsel:` error.
A refusal in resolution or in `pack`'s staging comes before the commit point (section 8), so
the command writes no lock and no build output. The only thing a refused command can leave
behind is store entries that were fetched and verified before a later step refused, as under
`--locked` with a stale lock; an entry is named by its content, so keeping it is harmless.
Manifest refusals
name the manifest path as the command reached it, then a colon. A refusal that is not about
one manifest starts with `hirsel:` alone. The `…` below stand for the values of the case.
Roots are always written in full.

| case | diagnostic |
|---|---|
| unknown key | `hirsel: <manifest>: unknown key <dotted.key>` |
| bad field | `hirsel: <manifest>: <dotted.key>: <what is wrong>`, for example `package.name: "Json" is not [a-z][a-z0-9_]*` |
| bad path | `hirsel: <manifest>: sources: "<path>" breaks the path rules: <which rule>`, the rule one of `bad component "<c>"`, `component over 255 bytes`, `path over 1024 bytes`, `does not end in .hirsel` (or `.a` for an archive) |
| case collision | `hirsel: <manifest>: <field>: "<a>" and "<b>" differ only in case`, with `<field>` `sources` or `foreign.<module>` for the later path, in the tree order of section 1 |
| file as directory | `hirsel: <manifest>: <field>: "<a>" is a file and also a directory of "<b>"` |
| directory spelling | `hirsel: <manifest>: <field>: "<a>" and "<b>" spell directory "<d>" differently` |
| one file, two rows | `hirsel: <manifest>: foreign.<module>: "<file>" is named by two archive rows` |
| source missing | `hirsel: <manifest>: sources: "<path>" does not exist` |
| symlink escapes | `hirsel: <manifest>: sources: "<path>" resolves outside the package` |
| not a regular file | `hirsel: <manifest>: sources: "<path>" does not resolve to a regular file` |
| no containment | `hirsel: <manifest>: sources: "<path>" cannot be opened relative to the package directory on this host` |
| archive file | the four rows above with `foreign.<module>: archive "<file>"` in place of `sources: "<path>"` |
| member pin | `hirsel: <manifest>: foreign.<module>: archive "<file>" for <target> does not match its pin: expected sha256 <E>, found sha256 <O>` (when the member's root is computed) |
| no sources | `hirsel: <manifest>: sources: a package lists at least one source` |
| member outside | `hirsel: <root manifest>: workspace.members: "<dir>" resolves outside the workspace` |
| member claimed | `hirsel: <root manifest>: workspace.members: "<dir>" belongs to the workspace at <other root manifest>` |
| output alias | `hirsel: --out <path> is an input of this command` (a manifest, a captured file, the lock or a store entry, compared by file identity) |
| no lock in this mode | `hirsel: --locked needs a workspace lock; <what was given> has none` (or `--frozen`) |
| two dependency forms | `hirsel: <manifest>: dependencies.<name>: give exactly one of path, root, workspace or a toolchain major` |
| toolchain major | `hirsel: <manifest>: dependencies.std: the toolchain has std@1, not std@<n>` |
| declared core | `hirsel: <manifest>: dependencies.core: core@1 is bound in every package and is not declared` |
| path not a member | `hirsel: <manifest>: dependencies.<name>: <path> is not a member of the workspace at <root manifest>` |
| member identity | `hirsel: <manifest>: dependencies.<name>: <path> is <other>@<m>, not <name>@<major>` |
| not listed | `hirsel: <manifest> is inside the workspace at <root manifest> but is not one of its members` |
| root is no package | `hirsel: <root manifest> is a workspace root, not a package; name a member directory` |
| member cycle | `hirsel: the members' path bindings form a cycle: a@1 -> b@1 -> a@1`, starting at the smallest identity on the cycle |
| inherit missing | `hirsel: <manifest>: dependencies.<name>: the workspace declares no <name>` |
| no source | `hirsel: <id> sha256:<P> is not in the package store and its binding names no source` |
| archive outside | `hirsel: <manifest>: dependencies.<name>: archive "<path>" is not inside the workspace root` |
| archive incomplete | `hirsel: <id>: <source> lacks sha256:<P>, which <binder> binds as <name>@<major>`, the least such edge |
| archive extra | `hirsel: <id>: <source> holds sha256:<Q>, which nothing in it binds`, the least such root |
| fetch mismatch | `hirsel: <id>: <source> holds sha256:<Q>, but the binding pins sha256:<P>` |
| fetched identity | `hirsel: <id>: sha256:<P> is <other>@<m>`, for every edge whatever the content's origin |
| malformed | `hirsel: <id>: <source> is not a package archive: <first decoding failure>`, the failure in the words of section 1, and `<source>` the archive or url the bytes came from |
| fetch failed | `hirsel: <id>: fetching <url> failed: <cause>` |
| diamond | `hirsel: <id> is bound to more than one root:` then one line per distinct root, each `  <path> = sha256:<root>`, in path order, each with its first path |
| member and root | `hirsel: <id> is a member and is also bound to sha256:<P>, which is not the member's root:` then `  <path> = member` and `  <path> = sha256:<P>`, in path order |
| missing lock | `hirsel: hirsel.lock is missing under --locked; run hirsel lock` (or `--frozen`) |
| stale lock | `hirsel: hirsel.lock is stale under --locked:` (or `--frozen`) then the change lines of section 8 |
| frozen, absent | `hirsel: <id> sha256:<P> is not in the package store, and --frozen does not fetch (source <source>)` |
| frozen, damaged | `hirsel: <id> sha256:<P> is damaged in the package store, and --frozen does not fetch (source <source>)` |

Several manifest refusals are reported together, in member order and then key order, so that
one run shows all of them. The first refusal of a later resolution step is the only one shown.

## 10. Keys, reuse and the recipe

Every key that decides reuse already hashes the content it covers. This section says how a
dependency's content reaches each key, so a stale reuse is impossible.

**Check reuse.** A body record is reused only under an equal `DefinitionKey`, whose fields
hash the body's bytes, the signature closure of every name it reads, its lookups and the
environment (the driver's reuse keys, which PROG-10 keeps out of every address). The **environment** is
the compiler identity, `core`, and each package's namespace: its identity and its dependency
identities. It gains each published package's library root, a coalesced member's included
(section 6), and each package's foreign stamp (below). So a record checked against one
pin of `json@1` is never found under another, even for a body whose own bytes and named
headers are equal. That holds whatever reads a future key might fail to list, at no cost to
reuse within one pin, because a root never changes. A member that no published binding names
contributes no root: its files
reach every key through their bytes, exactly as a package's do today, so editing a member does
not invalidate every record of the workspace.

**Foreign inputs.** A package's foreign metadata is not in its source files, so it enters the
keys on its own, for members and published packages alike. Each package's namespace in the
environment holds its **foreign stamp**: the number of its foreign modules, then for each, in
the order of item 6, its module path, its symbol prefix and its rows in ascending order of
target, each the target, the pin, the byte length and the SHA-256 of the captured bytes. A
package with no foreign module has the stamp of count zero, so removing a module's `[foreign]`
marking, changing a prefix, adding a row or replacing an archive's bytes changes the
environment even when every `.hirsel` file is unchanged. The stamp is computed from the
capture of resolution step 2, or from the decoded `C`, never from a pathname. Every reuse
point that can answer without checking again holds the environment, including a whole-request
record that answers before any definition key is built, an image key and the reach record. A
reach record also holds its foreign summary (FFI-8), under a record version that every record
without a summary lacks, so a record written before the summary existed is never read as one
with an empty summary. A compiled object whose code calls a foreign function is keyed by that
function's address, which covers the C symbol it relocates against, never by the caller's body
alone. A build that reuses any of these still takes the archive bytes it links from this
command's capture and checks them against the pin (FFI-3), so a replaced archive behind an
unchanged pin is refused on a warm build exactly as on a cold one.

**Test results.** A reused pass is keyed by the test's code address
([PROG-10](programs.md#prog-10-code-addresses)), its run configuration (PROG-4) and its
**site digest**: SHA-256 over the annex hash of every body the test reaches, in ascending
order of body hash. The address covers the checked IR of every instance the test reaches, in
any package, so a dependency's content reaches the key through every body the test can run.
The annexes hold the site maps, so a test that compares `Cause.site` values, or prints a
failure's site, is keyed on them as PROG-10 requires; a test's caller is the test runner,
whose site context is fixed by the toolchain already in the key. A changed body in a reached
dependency changes the address, and an edit that keeps the IR but moves a reached site, such
as regrouping `x / y / z` as `(x / y) / z`, changes the site digest. Either way the test
runs again. A changed file in a dependency that the test never reaches changes neither, and
the pass is reused, which is correct, because the outcome depends only on code that runs. A test whose reach
holds a foreign function has no key ([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)).

**`hirsel inspect --affected-tests`.** The argument is a definition path in any package of the
member's closure (`json@1/json/version`), or a package identity alone (`json@1`). A test's
**coverage** is today's: the instances it reaches, the declarations they read transitively,
the constants folded into them and their implicit reads, in any package. For a definition, the
answer is every parameterless test of the member whose coverage includes it. For a package, it
is the union of those answers over the package's definitions. It is a **candidate** list: it
holds every test whose reuse a change to that package can end, and it can hold more. A rebind
that changes nothing a test reaches, such as adding an unused file, leaves the test listed
and its pass reused. A foreign-reaching test is listed when its coverage meets the package,
though it never has a pass to reuse (FFI-8). Saying which passes a particular rebind ended
needs both revisions and is not this query.

**The recipe.** R already covers everything a dependency contributes to an executable: every
reached body and its annex is in the recipe, and a body's hash covers its checked IR. The
recipe's foreign parts ([FFI-3](foreign.md#ffi-3-archives-are-admitted-by-their-pin-before-linking))
hold a foreign leaf for each reached foreign module, from which a reader recomputes its
addresses, and each linked archive by its digest. A dependency's file names in annexes are `name@major/path`
(section 1), whether the dependency is published, a member or a coalesced member, so R never
depends on where the package store or the workspace is. Two builds of one program over one
closure, with the same target, backend, grants, root limit and place count, give one R, on
any machine and with any store path, and whether a dependency's equal content comes from a
member or from a published root.

**Images.** A package image is keyed by each package's **image stamp**, and its file table
is built from the same capture as the stamp, so the two cannot disagree. The stamp of a
package is its identity, its bindings (a published binding by its root), its foreign stamp,
and each file in the order of item 5: its logical name (section 1), its module path, its
parse mode (source or test) and its bytes. A published package's files are its verified
bytes. So renaming a file, even with its module and bytes unchanged, changes the stamp, and
an image built before the rename is never used after it. A producer that misses the image
takes the names from the capture and never synthesises one, so an image's site names, and the
R built from it, equal those of a build from fresh source.

## 11. Commands

| command | does |
|---|---|
| `hirsel lock [DIR]` | resolves the workspace and writes the lock |
| `hirsel fetch [DIR]` | resolves the workspace and fills the store with every published root of the closure, building nothing |
| `hirsel pack [DIR] --out FILE` | publishes the member at DIR (section 6) |
| `hirsel new NAME` | writes `NAME/hirsel.toml`, its sources, its tests and its first lock |

`--locked`, `--frozen` and `--package-store DIR` apply to `check`, `build`, `publish`, `run`,
`test`, `inspect`, `replay`, `sdk`, `lock`, `fetch` and `pack`, when the command is given a
package directory; section 8's input modes say what each does without one. `hirsel lock --locked` checks the lock without
writing it, which is the CI freshness check. `hirsel fetch --locked` followed by a later
`--frozen` build over the same store is the offline build. `hirsel publish` keeps its meaning:
it prints an executable's R ([launch](launch.md#release-recipe-and-host-assembly)), and a
library is published by `pack`.

## 12. Bug laws

Each law names its fixture in full and its expected value. Each value comes from this text,
worked by hand or computed by a reference encoder written from section 1, never from the
implementation. They are cases in the package-directory harness of
`crates/hirsel-core-driver/src/test_run_native.rs`, which runs the CLI over temporary
directories. Fixture archives are packed with a fixture store F of their own. Every command of
a law runs with `--package-store` naming that law's own store, empty when the law starts unless
the law says otherwise, so no law's outcome depends on what an earlier command left in a
store. Each law also has its own user cache (`XDG_CACHE_HOME`), so its own pass cache and
check records, and its own code store, all empty when it starts and kept across that law's
steps, so a step's `reused` or `executed` can only come from an earlier step of the same law.

**Known roots.** Two one-file packages, `jx` and `jy`:

```text
[package]
name = "json"
major = 1
charge = "charge-v1"

[sources]
"json.hirsel" = "json"
```

In `jx`, `json.hirsel` is the 33 bytes `pub fn version() -> u64 { 1u64 }` (32 characters)
followed by a line feed. In `jy` the `1u64` is `2u64`. `jx`'s `C` is these 72 bytes, grouped
by field:

```text
01                                  version 1
04 6a736f6e                         name "json"
01                                  major 1
09 6368617267652d7631               charge "charge-v1"
00                                  no bindings
01                                  one file
0b 6a736f6e2e68697273656c           path "json.hirsel"
01 04 6a736f6e                      module path: one component, "json"
21 70756220666e2076657273696f6e2829202d3e20753634207b2031753634207d0a
                                    the 33 (0x21) file bytes
00                                  no foreign modules
```

The roots are

- X = `sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58` for `jx`, and
- Y = `sha256:df7ca24aa6689979ef02faa3a35084bf96207edbe52b19d78e6273a98dc2ca99` for `jy`.

Here X is SHA-256 over the 8-byte little-endian 22, `hirsel.library-root.v1`, the 8-byte
little-endian 72 and the 72 bytes. `hirsel pack jx --out jx.hpkg` prints
`json@1 <X>` and `hirsel pack jy --out jy.hpkg` prints `json@1 <Y>`. This law catches any
encoding drift: a wrong order, a missing frame, a hash over the archive instead of over `C`,
or a shortened root.

**Known roots with foreign modules.** Package `demo@1` has no bindings and these files, each
ending in one line feed: `api.hirsel` (module `api`) is `pub opaque type A {}` and
`pub opaque type B {}` on two lines (42 bytes), `aa.hirsel` (module `aa`) is
`pub fn ping(f: Foreign[api.A]) -> u64` (38 bytes) and `b.hirsel` (module `b`) is
`pub fn pong(f: Foreign[api.B]) -> u64` (38 bytes). Its manifest adds

```text
[foreign.aa]
symbols = "ha_"
archives = [
  { target = "x86_64-unknown-linux-gnu", file = "native/aa-x86.a", sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" },
  { target = "aarch64-unknown-linux-gnu", file = "native/aa-arm.a", sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" },
]

[foreign.b]
symbols = "hb_"
archives = [{ target = "x86_64-unknown-linux-gnu", file = "native/b.a", sha256 = "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d" }]
```

where `native/aa-x86.a` is the ASCII bytes `abc`, `native/aa-arm.a` is empty and
`native/b.a` is the one byte `b`, each pinned by its FIPS 180-2 SHA-256. Its `C` is 422 bytes,
and its item 6 is, grouped by field:

```text
02                                  two foreign modules
01 02 6161                          module path aa
03 68615f                           prefix "ha_"
02                                  two rows
19 616172636836342d756e6b6e6f776e2d6c696e75782d676e75    target aarch64-unknown-linux-gnu
0f 6e61746976652f61612d61726d2e61   file "native/aa-arm.a"
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855    pin
00                                  no bytes
18 7838365f36342d756e6b6e6f776e2d6c696e75782d676e75      target x86_64-unknown-linux-gnu
0f 6e61746976652f61612d7838362e61   file "native/aa-x86.a"
ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad    pin
03 616263                           bytes "abc"
01 01 62                            module path b
03 68625f                           prefix "hb_"
01                                  one row
18 7838365f36342d756e6b6e6f776e2d6c696e75782d676e75      target x86_64-unknown-linux-gnu
0a 6e61746976652f622e61             file "native/b.a"
3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d    pin
01 62                               bytes "b"
```

Its root is D = `sha256:e073ce479e0454fe08a3b357087e8de69af0e172a90afa5419faf4307f3b8a75`,
and `hirsel pack demo --out demo.hpkg` prints `demo@1 <D>`. Module `aa` precedes `b` by
component bytes although its frame `01 02 61 61` is bytewise greater than `01 01 62`, and the
manifest's row order does not matter. These are wire vectors: the archive bytes are not `ar`
archives, and only FFI-3 at link time asks that they be. The decoder refuses three variants,
each reported as `hirsel: demo@1: archive:demo.hpkg is not a package archive: <failure>` when
it is the head of an archive `app` binds by `{ major = 1, root = "<D>", archive = "demo.hpkg" }`,
because decoding comes before the root check (section 7):

- the same `C` with the two module entries swapped: `foreign modules out of order at aa`, whose
  root, `sha256:b8b4134afbaded2a97a56700b11e35346406eb32701216283453befa2712b428`, a lenient
  decoder would accept as a second package with D's meaning;
- the same `C` with `aa`'s two rows swapped: `archive rows out of order at aarch64-unknown-linux-gnu`;
- the same `C` with the byte of `native/b.a` changed to `c` and its pin kept:
  `archive "native/b.a" of b for x86_64-unknown-linux-gnu does not match its pin`.

So do two binding refusals on `jx`'s `C` with its no-bindings byte `00` replaced: by
`02 04 7574696c 01 01 <X> 04 7574696c 02 01 <Y>` (`util@1` and `util@2`, each valid alone):
`two bindings of util`; and by `01 03 737464 01 01 <X>` (a published `std@1`): `bad binding
std@1`. The law catches a module order taken over framed bytes, a decoder that checks only
what its own encoder sorts, and one that skips the pin of a row for another target.

**L1. Diamond refusal.** The workspace root manifest has `members = ["app", "a", "b"]` and
`vendor/` holds `jx.hpkg` and `jy.hpkg` from above. `a` binds
`json = { major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }` and `b` binds the same with
`<Y>` and `jy.hpkg`. `app` binds `a = { major = 1, path = "../a" }` and
`b = { major = 1, path = "../b" }`. `hirsel check app` exits 2, writes no lock, and its stderr
is exactly:

```text
hirsel: json@1 is bound to more than one root:
  app@1 -> a@1 -> json@1 = sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58
  app@1 -> b@1 -> json@1 = sha256:df7ca24aa6689979ef02faa3a35084bf96207edbe52b19d78e6273a98dc2ca99
```

`app@1` is the only top member, and `a` sorts before `b`. With `b` changed to bind `<X>` and
`jx.hpkg`, the check passes and the lock has exactly one `json@1` row, with root X. That is the
coalesce. With `b` binding `json` major 2 to the root of a `json@2` fixture, the lock has a
`json@1` row and a `json@2` row, and the check passes: majors stay distinct.

**L2. Stale lock.** This is a single-package workspace: `app` binds
`json = { major = 1, root = "<X>", archive = "vendor/jx.hpkg" }` and `std = { major = 1 }`.
`hirsel lock app` writes exactly the lock bytes shown in section 8. Rewrite the binding to
`<Y>` with `archive = "vendor/jy.hpkg"`, and then:

- `hirsel check app --locked` exits 2, leaves `hirsel.lock` byte-identical, and writes exactly:

  ```text
  hirsel: hirsel.lock is stale under --locked:
    json@1: root sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58 -> sha256:df7ca24aa6689979ef02faa3a35084bf96207edbe52b19d78e6273a98dc2ca99
    json@1: source archive:vendor/jx.hpkg -> archive:vendor/jy.hpkg
  ```

- `hirsel check app` without the flag passes, writes `hirsel: updated hirsel.lock:` and the
  same two lines, and leaves the section 8 lock with the X root and source replaced by Y's.
- A stale lock in a transitive position is reported the same way. Workspace members `app ->
  util` (a path binding), with `util` binding the json root: changing `util`'s root and running
  `--locked` from `app` gives the same two lines. This catches a freshness check that compares
  only the named package's own manifest.

**L3. Frozen with missing content.** Take L2's `app` with root X, but give it the source
`url = "https://hirsel.invalid/jx.hpkg"`. Fill store S1 with `hirsel pack jx --out jx.hpkg
--package-store S1`, and write the fresh lock with `hirsel lock app --package-store S1`. Then
`hirsel check app --frozen --package-store S2`, with S2 a new empty directory, exits 2, S2 stays
empty, and its stderr is exactly:

```text
hirsel: json@1 sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58 is not in the package store, and --frozen does not fetch (source url:https://hirsel.invalid/jx.hpkg)
```

`.invalid` never resolves, so a command that tried to fetch would report `fetching … failed`
instead. With the store holding the X entry with one byte flipped, the line says `is damaged
in` in place of `is not in`. With the source changed to `archive = "vendor/jx.hpkg"` and the
lock rewritten, `hirsel check app --locked` against a new empty store S3 fills S3 with X and
passes, because `--locked` fetches. Then, with that `archive` source and lock and with S2 empty
again, make `vendor/jx.hpkg` a FIFO and have the harness block opening its write end on
another thread: `hirsel check app --frozen --package-store S2` gives the `is not in` line
with `(source archive:vendor/jx.hpkg)`, and the harness's open has not returned when the
command exits, so the command never opened the archive. (The harness then opens the read end
itself to release its thread.)

**L4. A symlink escaping the package.** Package `p` lists `"a.hirsel" = "a"`, where `a.hirsel`
is a symbolic link to `../outside.hirsel`, a regular file beside `p`, and `p` has no lock.
Run from `p`'s parent, `hirsel check p` and `hirsel pack p --out p.hpkg` both exit 2, create no
`hirsel.lock` and no archive, and write exactly this to stderr. The refusal is in resolution step
2, before the lock is derived:

```text
hirsel: p/hirsel.toml: sources: "a.hirsel" resolves outside the package
```

In the control case `a.hirsel` links to `real/a.hirsel` inside `p`: the check passes, and the printed root equals the root of a copy of `p` in which `a.hirsel` is a regular file
holding the same bytes. The law catches a walker that follows links out of the package, and an
encoder that hashes the link target's text instead of the file's bytes.

**L5. A test-only file is excluded from the root.** This is `jx` with
`[tests]`, `limit = 67108864` and `[tests.sources] "json_laws.hirsel" = "json"`, where
`json_laws.hirsel` holds an `@test` function in module `json`, the library's own module.
`hirsel pack` prints `json@1 <X>`, the root of `jx` without any test. `hirsel test` on it runs
the test, so the test file did join module `json` for its own run. The law catches an
inventory that walks the directory, and one that keys exclusion by module instead of by file.

**L6. A mismatched fetch keeps nothing.** `app` binds `<X>` with `archive = "vendor/jy.hpkg"`.
`hirsel check app` exits 2 with
`hirsel: json@1: archive:vendor/jy.hpkg holds sha256:df7ca24aa6689979ef02faa3a35084bf96207edbe52b19d78e6273a98dc2ca99, but the binding pins sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58`,
and the store has no entry named by Y or by X.

A non-leaf bundle is checked edge by edge. Package `wrap@1` is the review fixture of section 1's
vectors: one file `wrap.hirsel`, module `wrap`, holding `pub fn one() -> u64 { 1u64 }` and a
line feed, and one binding, `json = { major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }`.
Its `C` is 107 bytes and its root is W = `sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689`. `hirsel pack wrap
--out wrap.hpkg` prints `wrap@1 <W>` and then `json@1 <X>`. `app` binds
`wrap = { major = 1, root = "<W>", archive = "vendor/wrap.hpkg" }`, and `hirsel check app`
passes with the store holding W and X. Three archives built by hand from the same entries are
refused, each leaving the store as it was:

- head W with the entry Y in place of X: `hirsel: wrap@1: archive:vendor/wrap.hpkg lacks
  sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58, which wrap@1 binds as json@1`;
- head W alone, with the store already holding X: the same line, because the store never
  completes an archive;
- head W with entries X and Y: `hirsel: wrap@1: archive:vendor/wrap.hpkg holds
  sha256:df7ca24aa6689979ef02faa3a35084bf96207edbe52b19d78e6273a98dc2ca99, which nothing in it binds`.

**L7. A dependency change reaches each key.** `app`'s test calls `json.version()`, and the lock
binds X.

- After the test passes once, rebinding to Y makes `hirsel test` run the test again (its
  address changed) and `hirsel inspect app --affected-tests json@1` list it.
- Rebinding instead to the root of a `jx` copy whose only difference is a second listed file
  that `app` never reaches makes `hirsel test` report the pass as reused.
- No body record checked under X is reused under Y. The environment holds the root, so the
  check of `app` under Y reuses no record, `json@1`'s included, even though `json.hirsel`'s
  bytes differ in one body only.
- `hirsel publish` of a program over X prints the same R from a store at two different paths,
  and the same R again when `json` is instead a workspace member holding `jx`'s files, bound by
  path, so equal content gives equal names whatever supplies it.

**L8. Foreign metadata reaches every reuse key.** Workspace members `app` and `math`; `app`
binds `math = { major = 1, path = "../math" }`, and its entry calls `math.ping`, a wrapper in
module `math` over the foreign module `native` (prefix `hm_`, one x86_64 row whose file is a
real indexed archive built from the FFI law adapter, with its pin). Every step below runs
with one private cache and one code store kept across the steps, and is compared with the
same command run cold, with an empty cache and an empty code store, on the same files.

- `hirsel check app` passes. Delete `[foreign.native]` from `math/hirsel.toml` and leave every
  `.hirsel` file unchanged: the warm `hirsel check app` is refused, as the cold one is,
  because `native`'s bodyless headers now stand in an ordinary module.
- Restore the table and change only the prefix to `hn_`: the warm `hirsel publish app` prints
  the cold run's R, which differs from the R printed before the change.
- Restore the prefix and add an `aarch64-unknown-linux-gnu` row: the warm `hirsel publish app`
  for x86_64 prints the cold run's R, which differs from the R before (FFI-9).
- Replace the x86_64 archive's bytes with other bytes and keep the pin: the warm `hirsel build
  app` is refused with exactly the cold run's `FFI-3: archive … does not match its pin: …`
  text, and the linker is never started.

The law catches a whole-request record, image key or definition key that hashes only source
files, and a warm build that links an archive without checking it.


**L9. A moved site ends a cached pass.** Two published pins of `math@1`, one file
`math.hirsel`, module `math`: in A it holds `pub fn quot(x: u64, y: u64, z: u64) -> u64 { x / y / z }`,
and in B the body is `(x / y) / z`. Both lower to the same two divisions with the same slots,
so A and B give `quot` one code address. `app`'s parameterless test calls `quot(8, 0, 1)` and
`quot(8, 1, 0)`, each inside an `isolate`, and returns `Ok(())` exactly when the two failures'
`Cause.site` values are equal. Under A both sites start at `x`, and the test passes. Rebind
`app` to B: the outer division's site now starts at `(`, and `hirsel test app` reports the
test executed and failed, as a cold run under B does, instead of reusing A's pass. The law
catches a pass key that holds the address but not the reached annexes.

**L10. A coalesced member keeps its pin.** Workspace members `app` and `json`, where `json`
is `jx` and `app` binds `json = { major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }`.
The member and the pin coalesce, and `hirsel lock` writes exactly:

```text
# hirsel.lock: written by hirsel; do not edit.
version = 1

[[package]]
id = "app@1"
source = "member:app"
edges = ["json@1"]

[[package]]
id = "json@1"
root = "sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58"
source = "member:json"
edges = []
```

Change the member to `jy` and the binding to `<Y>` with `jy.hpkg` in one edit:
`hirsel check app --locked` exits 2, leaves the lock byte-identical and writes exactly

```text
hirsel: hirsel.lock is stale under --locked:
  json@1: root sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58 -> sha256:df7ca24aa6689979ef02faa3a35084bf96207edbe52b19d78e6273a98dc2ca99
```

Instead replacing the binding by `json = { major = 1, path = "../json" }` gives the one change
line `  json@1: root sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58 -> none`. The law catches a lock that drops the
published obligation when a member satisfies it.

**L11. Every edge checks the identity it binds.** Fill the law's store with `jx` by
`hirsel pack jx`. `app` binds `yaml = { major = 1, root = "<X>" }` and nothing else, so X is
opened from the store and no archive is read. `hirsel lock app` exits 2 with exactly
`hirsel: yaml@1: sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58 is json@1`. With `app` binding
`json = { major = 1, root = "<X>", archive = "vendor/jx.hpkg" }` as well, so X is visited first
under the right identity, the refusal is the same line. The law catches a resolver that checks
identity only on a cold archive head, or once per root instead of once per edge.

**L12. Member files are read inside their directories.** Workspace root `W` lists `app` and
`lib`, where `W/lib` is a symbolic link to `../elsewhere/lib`, a valid package outside `W`.
`hirsel lock W` exits 2 with exactly
`hirsel: W/hirsel.toml: workspace.members: "lib" resolves outside the workspace`. Separately,
package `p` lists `"a.hirsel" = "a"`, where `p/up` is a link to `..` and `a.hirsel` is a
link to `up/p/real.hirsel`, which names `p/real.hirsel` again: `hirsel check p`, run from
`p`'s parent, exits 2 with `hirsel: p/hirsel.toml: sources: "a.hirsel" resolves outside the
package`, because the walk left the package although the final file is inside. The law catches
containment checked on a normalised pathname instead of on the walk actually opened. The
concurrent-replacement case that the capture rule closes has no deterministic CLI oracle, and
no law claims it.

**L13. An image supplies only the closure.** Member `core_only` binds nothing, and its module
`main` holds `pub fn main() -> u64 { std.x }` for any std name `x`. `hirsel check core_only`
is refused by name resolution because `core_only@1` does not bind `std`, with the same
diagnostic from a source checkout and from an installed toolchain whose std image is
registered. Given `--image` naming an image of `core_only@1` and `std@1`, the command exits 2
with exactly `hirsel: --image <path> does not hold exactly this request's packages`. The law
catches a toolchain image chosen before resolution.

**L14. A rename reaches the image and the names.** Member `json@1` has one file, `old.hirsel`
in module `json`, holding `pub fn half(x: u64) -> u64 { 8u64 / x }` and a line feed, and `app`
binds it by path; `app`'s entry calls `json.half(0)`. `hirsel check app` builds the closure
image. Rename the file to `new.hirsel`, module and bytes unchanged. The failure `hirsel run
app` reports now names its site in `json@1/new.hirsel`, and `hirsel publish app`
prints the R of a cold run with no image, on the warm image path as on the cold one. The law
catches an image keyed without file names and a producer that synthesises names.
