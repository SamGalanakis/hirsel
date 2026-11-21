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

1. the encoding version, the number 1;
2. the package's name, a text, and its major, a number;
3. its charge policy (PROG-1), a text, which in v1 is `charge-v1`;
4. its **bindings**: their count, then each binding, sorted by name bytewise and then by
   major numerically. A binding is the dependency's name (a text) and major (a number), then
   a tag: `0` for a toolchain package, or `1` followed by the dependency's 32-byte library
   root;
5. its **files**: their count, then each file, sorted by path bytewise. A file is its path (a
   text), its module path (the number of components, then each component as a text), and its
   bytes (their length as a number, then the raw bytes);
6. its **foreign modules** ([FFI-2](foreign.md#ffi-2-foreign-modules)): their count, then each,
   sorted by module path bytewise. One is its module path, its symbol prefix (a text) and its
   archive rows: their count, then each row, sorted by target bytewise, as the target (a text),
   the file's path (a text), the 32-byte pin, and the archive's bytes (their length as a
   number, then the raw bytes).

`core@1` is bound in every package and is never written as a binding. `std@1` is a toolchain
package (section 2) and is written with tag `0`, so a library root says which std major it
uses, and the toolchain that checks it supplies that std. A toolchain package has no root.
Its content is the toolchain's and enters every key through the compiler and the toolchain
identity, as it does today.

**Inventory.** The files of `C` are exactly the package's `[sources]` (section 2), with their
modules, and its archives are exactly the rows of its `[foreign]` tables. Nothing else in the package directory enters: not `[tests]` sources, `[program]`,
`[tool]`, `hirsel.lock`, any other manifest field, a build or cache directory, or a file the
manifest does not list. A test-only file that joins a library module, as `assembly_laws.hirsel`
joins module `store`, is left out, and the module keeps exactly its `[sources]` files. The manifest enters only through items 2
to 4, after the workspace's inherited values are expanded (section 4) and each member binding
is replaced by that member's root (section 6). File bytes are kept exactly, line endings
included. Permissions, timestamps, ownership and the order of the directory listing never
enter.

**Path rules.** A listed path is relative to the package directory. It is one or more
components joined by `/`. Each component matches `[A-Za-z0-9_][A-Za-z0-9_.-]*` and is at
most 255 bytes, so it is never empty, `.` or `..`, and never starts with a dot. The whole
path is at most 1024 bytes and ends in `.hirsel`. No two listed paths of one package are
equal after ASCII lower-casing, so a package that checks on a case-sensitive file system
unpacks the same way on a case-insensitive one. A module path is one or more identifiers
([syntax](syntax.md)) and is written in the manifest with `.` between components.

**Symlinks.** A listed path may pass through symbolic links, in any component. The tool
resolves the path completely, and the result must be a regular file whose resolved path lies
inside the resolved package directory. Then `C` holds that file's bytes under the listed path,
and records nothing about the link. A path that resolves to anything else is refused (section
9): one that resolves outside the package, to a directory, or to nothing, or one with a link
loop. The rule holds for every member of a workspace whenever its files are read, not only at
publication. So the bytes a member is checked with are the bytes it would publish. The
directory convention below keeps its own walk, which follows a source symlink wherever it
points, because a directory-convention package is never a dependency and never published.

**Limits.** One `C` is at most 64 MiB and holds at most 65 536 files.

**Decoding.** A reader accepts `C` only in this exact encoding. It refuses an unknown
version, a redundant LEB128 byte, a text that is not UTF-8, a path or module path that breaks
the rules above, bindings or files out of order or repeated, a tag other than 0 or 1, a tag
`0` on a name other than `std`, a foreign module that is not a module of its files, an archive
whose bytes do not match its pin, a binding of `core`, bytes left over at the end, and an
encoding over the limits. So decoding and re-encoding give the same bytes, and two packages
with one root have one meaning.

**What the compiler reads.** A published package's sources are the file bytes of the `C`
whose root was checked against its pin (section 7), decoded in memory. The compiler is handed
those bytes, never a path to re-read. A member's sources are read once, through the symlink
rule, and the bytes read are the bytes parsed. The compiler names a published package's file
`name@major/path` in diagnostics, sites and annexes (`json@1/json.hirsel`). It names a
member's file by its path relative to the directory of the member the command was given, as
today's manifest-relative names do. So no name depends on where the package store or the
workspace sits on disk.

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
library they are its whole inventory.

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
  package archive's path relative to this manifest's directory. It is never absolute, and
  resolved against that directory it must lie inside the workspace root, so the lock can name
  it relative to the root. With neither, the root must
  already be in the package store. The source says where content may come from, and the root
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
refusals.

**`[tool]`.** This table is for other tools: hirsel ignores its contents, and they never
enter `C`, the lock or any key.

**The directory convention.** A directory without `hirsel.toml` is package `app@1`. Each
`.hirsel` file under it is one module, named by its relative path components. It depends on
`core@1` and `std@1`, and its entry is `main` in module `main`, as today. It has no lock, binds
nothing else, and cannot be a member or a dependency. `--inputs FILE` stays the explicit
compiler input for measurements: it names packages, files and dependency identities directly,
binds nothing, and reads no manifest or lock. Every check of its packages reads their files as
member files.

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
   needs one package (check, build, publish, run, test, inspect, pack) is refused there.
   `lock` and `fetch` act on the whole workspace.

The lock is `hirsel.lock` in the workspace root's directory.

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
because a member names it by path.

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

1. **Manifests.** Read and validate every member's manifest and the root's (sections 2 to 4).
2. **Member files.** Read every member's `[sources]` and `[tests.sources]` once, under the
   path and symlink rules of section 1. These bytes are the ones the compiler is handed and
   the ones a member's root is computed from.
3. **Content.** For each published root reached so far, open its `C` from the package store,
   or fetch it from its source (section 7). Decode it, and add its bindings to the walk. This
   repeats until no new root appears.
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
X's `[tests.sources]` join in X's test runs only. A request never holds two packages with one
`name@major`, so name resolution reads each identity once.

## 6. Publishing a library

`hirsel pack [DIR] --out FILE` publishes the member at DIR. It resolves the workspace as any
command does, then computes the member's root:

- A binding to another member is encoded as that member's own root, computed the same way,
  recursively. So a member's root is defined whenever its bindings resolve, and it changes
  when any member it reaches changes.
- A binding to a published library is encoded as its root.
- A binding to `std` is encoded as a toolchain binding.

The member's `C` holds its `[sources]` files read through the symlink rule, and nothing of
`[tests]`, `[program]` or `[tool]`. `pack` then writes a package archive (section 7) holding
that `C` and the `C` of every package its bindings reach, other than toolchain packages:
members encoded the same way, and published libraries copied from the store. Before writing,
`pack` decodes the archive it produced and verifies every root again as a fetch would. It puts
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
member edited since that publication has another root, and the pair is a diamond refusal.

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

**Store.** The **package store** is the directory `--package-store DIR` names. By default it is
`hirsel/packages` under the user cache (`$XDG_CACHE_HOME`, else `$HOME/.cache`), beside
today's `hirsel/core-packages`. It holds each verified package as the file
`<64 hex digits of P>`, whose bytes are `C`. A write goes to a temporary file in the store
directory and is renamed into place, so a reader sees a whole entry or none. Every open
re-hashes the entry before decoding it, so the tool never trusts a store entry by its name. An
entry whose bytes do not hash to its name is **damaged**. It is treated as missing: a fetch may
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
- `root`, for a published library only: its root;
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
`store`. A member
row has no root, because a member's bytes change with every edit. The member's files enter
every key through their content (section 10), and a member's root is computed when it is
packed. All paths use `/` and are normalised, with no `.` or `..` components and no trailing
`/`.

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
feed. TOML strings use basic-string escapes (`\"`, `\\`, `\u` for control characters) and
nothing else. Lines end with a line feed alone.

**Staleness.** A lock is **stale** when its bytes differ from the derived lock. The tool then
parses the old lock and reports what changed, one line per difference, rows in order and, within
a row, `root`, then `source`, then `edges`:

```text
  json@1: root sha256:<old> -> sha256:<new>
  json@1: source archive:vendor/jx.hpkg -> archive:vendor/jy.hpkg
  json@1: edges [] -> ["util@1"]
  util@1: added
  yaml@2: removed
```

An old lock that does not parse as a version 1 lock, or whose bytes differ while its parsed
rows are equal, is reported as one line, `  hirsel.lock is not in the form this hirsel
writes`.

**Who writes it.** Every command that resolves a workspace (check, build, publish, run, test,
inspect, pack, fetch and lock) derives the lock. Without `--locked` or `--frozen`, a command
whose derived lock differs from the file writes the derived lock, through a temporary file and
a rename. It writes `hirsel: created hirsel.lock` to stderr when there was none, or
`hirsel: updated hirsel.lock:` followed by the change lines, and then goes on. `hirsel lock`
does only this, and `hirsel new` writes the first lock with the package.

**`--locked`.** Any of those commands refuses a missing or stale lock and writes nothing. It
may still fetch pinned content that the store lacks, because a fetch only fills the store with
verified bytes the manifests already pin.

**`--frozen`.** This is `--locked`, and it also refuses to fetch. Every published root of the
closure must already be in the store, undamaged. The refusal comes in resolution step 3,
before any source is touched, so a `--frozen` command never opens a URL or an archive path.

## 9. Diagnostics

A tool refusal is one block on stderr with exit status 2, like every other `hirsel:` error.
The command writes no lock and no build output. The only thing a refused command can leave
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
| bad path | `hirsel: <manifest>: sources: "<path>" breaks the path rules: <which rule>` |
| case collision | `hirsel: <manifest>: sources: "<a>" and "<b>" differ only in case` |
| source missing | `hirsel: <manifest>: sources: "<path>" does not exist` |
| symlink escapes | `hirsel: <manifest>: sources: "<path>" resolves outside the package` |
| not a regular file | `hirsel: <manifest>: sources: "<path>" does not resolve to a regular file` |
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
| fetch mismatch | `hirsel: <id>: <source> holds sha256:<Q>, but the binding pins sha256:<P>` |
| fetched identity | `hirsel: <id>: sha256:<P> is <other>@<m>` |
| malformed | `hirsel: <id>: <source> is not a package archive: <first decoding failure>` |
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
identities. It gains each published package's library root. So a record checked against one
pin of `json@1` is never found under another, even for a body whose own bytes and named
headers are equal. That holds whatever reads a future key might fail to list, at no cost to
reuse within one pin, because a root never changes. A member contributes no root: its files
reach every key through their bytes, exactly as a package's do today, so editing a member does
not invalidate every record of the workspace.

**Test results.** A reused pass is keyed by the test's code address
([PROG-10](programs.md#prog-10-code-addresses)) and its run configuration (PROG-4). The
address covers the checked IR of every instance the test reaches, in any package, so a
dependency's content reaches the key through every body the test can run. A changed body in a
reached dependency changes the address, and the test runs again. A changed file in a
dependency that the test never reaches leaves the pass reused, which is correct, because the
outcome depends only on code that runs. Nothing new enters the test key. A test whose reach
holds a foreign function has no key ([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)).

**`hirsel inspect --affected-tests`.** The argument is a definition path in any package of the
member's closure (`json@1/json/version`), or a package identity alone (`json@1`). For a
package, the answer is every parameterless test of the member whose coverage includes any
definition of that package. So after a lock change from `json@1 sha256:A` to `sha256:B`,
`--affected-tests json@1` lists exactly the tests whose reuse the change can end.

**The recipe.** R already covers everything a dependency contributes to an executable: every
reached body and its annex is in the recipe, and a body's hash covers its checked IR. The
recipe's foreign-archives part ([FFI-3](foreign.md#ffi-3-archives-are-admitted-by-their-pin-before-linking))
holds each linked archive by its digest. A dependency's file names in annexes are `name@major/path`
(section 1), so R never depends on where the package store is. Two builds of one program
over one closure give one R, on any machine and with any store path.

**Images.** A package image of the closure keys its sources by their content, as today. A
published package's sources are its verified bytes, so its image is keyed by them.

## 11. Commands

| command | does |
|---|---|
| `hirsel lock [DIR]` | resolves the workspace and writes the lock |
| `hirsel fetch [DIR]` | resolves the workspace and fills the store with every published root of the closure, building nothing |
| `hirsel pack [DIR] --out FILE` | publishes the member at DIR (section 6) |
| `hirsel new NAME` | writes `NAME/hirsel.toml`, its sources, its tests and its first lock |

`--locked`, `--frozen` and `--package-store DIR` apply to `check`, `build`, `publish`, `run`,
`test`, `inspect`, `lock`, `fetch` and `pack`. `hirsel lock --locked` checks the lock without
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
store.

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
passes, because `--locked` fetches.

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

**L7. A dependency change reaches each key.** `app`'s test calls `json.version()`, and the lock
binds X.

- After the test passes once, rebinding to Y makes `hirsel test` run the test again (its
  address changed) and `hirsel inspect app --affected-tests json@1` list it.
- Rebinding instead to the root of a `jx` copy whose only difference is a second listed file
  that `app` never reaches makes `hirsel test` report the pass as reused.
- No body record checked under X is reused under Y. The environment holds the root, so the
  check of `app` under Y reuses no record, `json@1`'s included, even though `json.hirsel`'s
  bytes differ in one body only.
- `hirsel publish` of a program over X prints the same R from a store at two different paths.
