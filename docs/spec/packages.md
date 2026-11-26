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
same local workspace or a published library fixed by its root, whose content comes from the
package store, a package archive, a URL or a git commit. Git is a transport only: a commit is
packed into canonical content and verified against the root before anything else sees it
(section 7, GIT-1 to GIT-12). The canonical library content
(section 1) is separate from the checked package image and from the executable release root R
([launch](launch.md#release-recipe-and-host-assembly)). A library root names source, and R names
assembled code.

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

`core@1` is bound in every package and is never written as a binding. The toolchain packages
of section 2 can be bound as toolchain packages and are written with tag `0`. A library
root records the major it uses, and the toolchain that checks it supplies that package.
A toolchain binding has no root.
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
the walk's parent directory; an absolute target is refused. A target is split on `/`; empty
and `.` components are skipped, and a target that ends in `/` must reach a directory. At most
40 links are followed on one path, Linux's own bound on a path walk; the 41st makes the path a
link loop. The walk must never leave the
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
| a binding of `core`; a tag other than 0 or 1; a tag 0 on anything but `std` or `store` major 1; a tag 1 on `std` or `store` | `bad binding <name>@<major>` |
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
harness = { major = 1, root = "sha256:…", git = "https://github.com/SamGalanakis/lash-hirsel", rev = "<40 hex>", ref = "refs/heads/main", dir = "harness" }   # a git commit
shared = { workspace = true }                            # see section 4

[program]                 # optional
module = "main"           # default "main"
function = "main"         # default "main"
limit = 67108864          # optional; the executable's root limit
grants = ["Stdout", "Clock"]   # optional; --grant syntax
dirs = ["data"]           # optional; --dir paths, one per Dir parameter in order

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

- `{ major }` alone binds a **toolchain package**. The installed toolchain supplies `std@1`,
  `store@1`, `runner@1`, `webrtc_sfu@1`, `turn@1`, `dtls@1`, `srtp@1`, `rtp@1`, `stun_ice@1`,
  `media_bits@1`, `rtp_jitter@1`, `sframe@1`, `media_container@1`, `media_record@1`,
  `platform@1`, `search@1`, `mcp@1` and `cli@1`. Each binds only toolchain packages: `store@1`,
  `dtls@1`, `srtp@1`, `rtp@1`, `stun_ice@1`, `media_bits@1`, `mcp@1` and `cli@1` bind
  `std@1`; `runner@1` and `search@1` bind `std@1` and `store@1`; `platform@1` binds `std@1`,
  `store@1` and `runner@1`; `turn@1` binds `std@1` and `stun_ice@1`; `sframe@1` and
  `media_container@1` bind `std@1` and `media_bits@1`; `rtp_jitter@1` binds `std@1`,
  `media_bits@1` and `rtp@1`; `webrtc_sfu@1` binds `std@1`, `store@1`, `runner@1` and the
  WebRTC packages other than `rtp_jitter@1`; `media_record@1` binds `std@1`, `media_bits@1`,
  `media_container@1`, `rtp@1`, `rtp_jitter@1` and `webrtc_sfu@1`.
  Binding a package brings in the toolchain packages it binds. A workspace using a toolchain package cannot also bind that
  package as a member or published library. `core` is bound in every package and
  may not be declared.

  [`cli@1`](../../core/programs/cli/README.md) takes explicit `Stdout`, `Stderr` and
  `Stdin` capabilities for terminal output and prompts. Tables use the width of
  stdout on terminals and escaped TSV on pipes. Writers receive `Closed` when
  the stdout consumer goes away. Progress runs in the caller's work loop and
  uses the `Clock` only for terminal updates on stderr.
- `{ major, path }` binds a member of the same workspace (section 4). The path is relative to
  this manifest's directory and must resolve to a member's directory, whose `[package]` has
  this name and major.
- `{ major, root }`, with at most one of `url`, `archive` or `git`, binds a published library.
  `root` pins its content. `url` is an `https://` URL of a package archive (section 7). `git`
  is a repository URL, and it comes with `rev`, the commit, and may come with `dir`, the
  package's directory in that commit, and `ref`, the branch or tag `rev` was resolved from
  (GIT-1). `rev`, `ref` and `dir` appear only with `git`, and `git` never without `rev`. `archive` is a
  package archive's path relative to this manifest's directory, `/`-separated. It is never
  absolute. It is resolved **lexically**: it is appended to the declaring manifest's directory,
  written relative to the workspace root, and each `..` then removes the component before it,
  as text, without looking at the file system. The result must have no `..` left, so it lies
  inside the workspace root, and it is the archive's one spelling: the lock writes it, and a
  fetch opens exactly that path from the root's directory handle under the reading rule of
  section 1, so a link on the way cannot make one spelling name two files. With none of
  `url`, `archive` or `git`, the root must already be in the package store. The source says where content may come from, and the root
  says what it must be: the root enters the package's own `C` and the source never does.
- `{ workspace = true }` alone inherits the workspace's declaration of this name (section 4).

**`[program]`.** The entry is the `pub fn` named `function` in module `module` (PROG-3). With
no `[program]` the entry is `main` in module `main`, as today. A command that needs an entry
refuses a package without that function. A dependency's `[program]` is never read.

`limit`, `grants` and `dirs` are the executable's run configuration. `grants` take the `--grant`
syntax and `dirs` the `--dir` paths, one per Dir parameter in order. A relative `dirs` path is
kept as written and resolves where the executable starts, as a `--dir` path does. `build`,
`publish`, `run` and `dev`'s builds read them; `test`, `replay`, `register` and `deploy` do
not. Under `dev` the first `dirs` path names the Dir slot dev fills with its store at launch,
and dev adds no limit or grant of its own. When any of the
three is present, they are the build's only source of `limit`, `dir` and `grant`: a `--limit`,
`--dir` or `--grant` flag is refused with `this package declares [program] grants in
hirsel.toml. Set limit, dirs and grants there`. `Foreign` and `Exec` approvals are outside the
rule and still come from the command line. Without `limit` the build's default root limit
applies. `build` accepts a recipe that leaves a `main` parameter ungranted and writes one
``hirsel: warning: grant `NAME` is not in the build's configuration; the executable will not
start`` line per such parameter, in parameter order, whichever source configured it.

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
manifest wrote it. A git binding's `dir` is a path inside the commit and is never rebased. An
inherited value is used exactly as written: a member adds nothing to it
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
   2. each pending root left that has a declared source, a `url`, `archive` or `git` in a
      declaration that binds it, is fetched from its lock source (section 8), the first such
      declaration in identity order of the declaring packages. Only that source is tried, and
      its failure is the refusal. A fetched source puts every entry of its bundle in the store
      (section 7), so it can satisfy other pending roots;
   3. only when the first two steps have nothing left to do, each pending root left is
      fetched from the declared source of its nearest **carrier**: follow the bindings back
      from the root, through published packages whose `C` binds it and then their binders,
      to the first published root that has a declared source, taking the least path in the
      order of section 5's refusal paths. That root's bundle holds its whole closure, so it
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
A package also records whether the toolchain supplied it: an installed toolchain archive, a
member under the invoker's library root `programs/`, or an `--inputs` package marked
`"toolchain": true`. Any other member or published package never is, whatever its name. These
are the invoker's trust, as `core@1` is ([launch](launch.md), the `place` bit).
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

`hirsel pack [DIR] --output FILE` publishes the member at DIR. It resolves the workspace as any
command does, then computes the member's root:

- A binding to another member is encoded as that member's own root, computed the same way,
  recursively. So a member's root is defined whenever its bindings resolve, and it changes
  when any member it reaches changes.
- A binding to a published library is encoded as its root.
- A binding supplied by the toolchain is encoded as a toolchain binding.

The member's `C` holds its captured `[sources]` files and `[foreign]` archives, every target's,
and nothing of `[tests]`, `[program]` or `[tool]`. Each archive's captured bytes must match its
pin here (the `member pin` refusal of section 9). `pack` then writes a package archive (section 7) holding
that `C` and the `C` of every package its bindings reach, other than toolchain packages:
members encoded the same way, and published libraries copied from the store. Before writing,
`pack` decodes the archive it produced and verifies every root again as a fetch would, and it
refuses an `--output` that names, by file identity, a manifest, a captured file, the lock or a
store entry. It puts
every package into the package store and prints one line per package of the archive, head
first:

```text
json@1 sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58
```

Publishing never edits a manifest; `hirsel add`<!-- doc-ref: planned --> (section 11) is the one
command that does. Inside the producing workspace the member binding stays a
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
- a `git` repository and `rev`, a commit fetched over Git's smart HTTP protocol and packed
  by the tool (GIT-1 to GIT-12);
- the package store, for a root with no `url`, `archive` or `git`, or one a bundle brought in.

There is no registry, no version range and no search.

**Bundles.** What a fetch yields is a **bundle**: a head `C` and the `C` of every package the
head's bindings reach, other than toolchain packages. An archive's bundle is its entries. A git
source's bundle is the content the tool packs from the commit (GIT-2). Whenever `hirsel pack`
succeeds for that package in a checkout of that commit, on a file system that keeps every name
by its exact bytes, that applied no conversion, the bundle is exactly the archive it writes; a git source may also succeed where that `pack` would not,
because it reads only the members the package reaches (GIT-2 step 4). Every rule below that speaks of an archive's entries holds for every bundle.

**Verifying a fetch.** For a binding of `name@major` to root P, the tool takes the bundle and,
before keeping or using anything:

1. decodes the archive and each entry under the decoding rules of section 1 (a git bundle's
   entries are decoded the same way after packing, so the packer is checked by the decoder);
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

**An I/O failure while keeping entries.** Writing a verified archive into the store is one
rename per entry, so an I/O error part way through (a full disk, a permission change) can leave
some entries written and others not. Each entry kept is whole and was verified, and a store
entry is named by its content, so that subset is valid store state. A command that meets this
reports it as a partial cache write with the I/O error, distinct from a refused archive, and
writes no manifest and no lock. "A refused archive leaves the store exactly as it was" stays
true: verification refusals come before step 4 and keep nothing.

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

### Git sources

A git source is a third way to reach a published root, beside a URL and an archive. The root
stays the only pin: `rev` says where the content is, `root` says what it must be, and the
compiler, the lock's consumers and every key see only the verified `C` (section 10). The tool
speaks the Git protocol itself and never runs a program to fetch, so nothing in a fetched
repository can run code on the host.

**GIT-1. The binding.** A git binding is
`{ major, root, git, rev }`, optionally with `dir` and `ref`:

1. `git` is an `https://` URL with a lower-case host, an optional port and a path, and no
   user information, query or fragment. It is kept exactly as written, and nothing is
   appended to it or removed (no `.git`, no trailing `/`). A host in another case is refused,
   so one repository is not two sources by its spelling.
2. `rev` is the commit's full object id in lower-case hex: 40 digits names a SHA-1
   repository and 64 a SHA-256 one. It is never shortened.
3. `dir` is the package's directory in the commit's tree, under the path component rules of
   section 1 with `/` between components and without the `.hirsel` ending. Absent, the
   package is at the top of the tree. It is never written as `.` or with `..`.
4. `ref` is a full reference name, `refs/heads/<branch>` or `refs/tags/<tag>`, the one
   `hirsel add` resolved to `rev` (GIT-10). Resolution, the lock and every key ignore it; only
   `hirsel add` reads it (GIT-11).

The binding's **source spelling**, used by the lock and in diagnostics, is
`git:<git>#<rev>` followed by `/<dir>` when `dir` is present
(`git:https://github.com/o/r#32e212a4b69a9f3b39893b29f4e4f1e935626a2b/wrap`). The URL has no
`#`, and `rev` has a fixed length, so the spelling parses one way. A **snapshot path** is the
source spelling without `/<dir>`, then `/`, then a path in the commit's tree; it names a
manifest or file of the commit in diagnostics (`git:https://…#32e2…/wrap/hirsel.toml`).

**GIT-2. What a commit yields.** For a git source the tool, in this order:

1. fetches the commit `rev` (GIT-7, GIT-8) and checks the pack (GIT-9);
2. reads the commit's **snapshot**, its tree, as objects in memory under GIT-3. The snapshot
   is never written to a file system;
3. finds the package: its manifest is `<dir>/hirsel.toml` in the snapshot, and its workspace
   is found by section 3 with the snapshot's top as the highest directory searched. So a
   `[workspace]` above `dir` that does not list `dir` refuses, as it would in a checkout, and
   nothing above the top of the tree is ever consulted. A `dir` without a manifest, or whose
   manifest has `[workspace]` and no `[package]`, is refused. The package's `[package]` must be
   the binding's `name@major` (the `member identity` refusal of section 9, with the source
   spelling in place of the path);
4. reads the **reached set**: the workspace root's manifest, the package, and every member
   its `path` bindings reach, transitively, each under sections 2 to 4 with snapshot paths in
   place of host paths. Members that the package does not reach are not read beyond the
   root's `members` list, so an unrelated broken member upstream does not stop the
   dependency. The reached members must each find this root (section 4); a `path` that does
   not name a listed member, or that would leave the top of the tree, is refused as
   `path not a member`. The snapshot's `hirsel.lock` is never read, and no `[tests]`,
   `[program]` or `[tool]` entry of the snapshot is read beyond the manifest's validation;
5. packs: computes the package's root and each reached member's root by section 6, with each
   member binding encoded as that member's root and each published binding by its root, and
   acquires the `C` of every published root those packages reach (GIT-6). The package's `C`
   is the bundle's head;
6. verifies the bundle as section 7 does (head root, identity, closure, no extra), and only
   then puts its entries in the store. Written as an archive, the bundle is at most 512 MiB,
   the archive limit; a larger one refuses as `over the limit of 512 MiB` before anything is
   kept.

Whenever `hirsel pack` succeeds for the same package over a checkout of the same commit, on a
file system that keeps every name by its exact bytes, that applied no conversion (GIT-4), the roots this computes are the roots it prints: the same
files, the same manifests, the same member closure. (`pack` reads every member of the
workspace, so an unreached broken member refuses it and not the git source.) A git source and a release archive of the same content
therefore bind the same root, and two consumers that reach one package by both forms
coalesce.

**GIT-3. Reading the snapshot.** Paths are looked up one component at a time in tree
objects, by exact bytes, never through a file system:

1. A tree entry's mode is an octal number, written in the tree object without leading
   zeros: `100644` or `100755` (a regular file; the executable bit is ignored, as permissions
   are in section 1), `40000` (a directory), `120000` (a symbolic link) or `160000` (a
   submodule commit). Two legacy spellings that old Git versions wrote and Git still reads are
   read as Git reads them: `040000` as a directory and `100664` as a regular file. Neither
   changes a root, which never depends on a mode. A tree that holds any other mode, or two
   entries of one name, is refused when it is read. A tree that is never read is never
   checked.
2. A symbolic link's target is its blob's bytes, at most 4096 bytes. It is followed by
   section 1's reading rule within the walk's containing directory: a relative target from the
   directory that holds the link, split on `/`, with empty and `.` components skipped, `..`
   stepping to the walk's parent, and a trailing `/` requiring a directory; an absolute
   target, an empty target, a target over 4096 bytes, or one that is not UTF-8 is refused as
   `resolves outside the package` (or the workspace, for a member directory). The walk must
   never leave its containing directory, not even on the way to a file inside it. The
   containing directory is the package's directory for its sources, its `[foreign]` archives
   and its manifest; the snapshot's workspace root for a member directory and for an
   `archive` declaration of GIT-6, which section 2 contains in the workspace root; and the
   top of the tree for `dir` and for the search of GIT-2 step 3, which comes before the
   workspace root is known.
3. At most 40 links are followed on one path, section 1's bound; the 41st makes the path a
   link loop, which is refused as `does not resolve to a regular file`.
4. The final object of a file path is a regular file's blob, and the listed path names its
   bytes. A path that ends at a directory, at nothing or at a submodule is refused.
5. A walk that meets a `160000` entry is refused as reaching a submodule (GIT-5).
6. A directory's identity in the snapshot is the sequence of entry names of the walk taken to
   it, after links are followed. It is never a tree object id, because two equal directories
   share one tree id. Section 3's comparison of directories "by the identity of the directory
   opened" uses this identity for a snapshot.

Listed paths already obey section 1's path rules, so a non-UTF-8 name or a case collision can
appear only among entries no manifest lists, and those are never read. The tree rule of
section 1 applies to every packed package as it does to a member.

**GIT-4. Nothing in the repository runs or changes bytes.** The tool never runs `git` or any
other program to fetch or read a commit, and never writes a snapshot file to disk. It reads
only the manifests, listed sources and listed archives of the reached set and the
`archive` declarations of GIT-6. The bytes of a file are its blob's bytes exactly.
`.gitattributes` and its `text`, `eol`, `ident`, `filter`, `export-ignore` and `export-subst`
attributes, `.gitmodules`, `.lfsconfig`, every other dot file, hooks, and the host's Git
configuration and environment have no effect on what is fetched or packed. So the root is
the same on every host and with every installed Git, and no attribute, filter, hook or
helper is ever a way to run code.

**GIT-5. Submodules and large-file pointers.** Submodules are never fetched. A `160000`
entry that no read path meets is ignored; one that a read path meets refuses the path
(`reaches submodule`). A Git LFS pointer is a small text blob and is packed as its bytes, as
every blob is: a listed source that is a pointer fails to check, and a listed archive that is
a pointer fails its pin. The pin refusal says when the found bytes are an LFS pointer (they
begin with `version https://git-lfs.github.com/spec/v1` and a line feed), so the cause is
plain; the hint changes no outcome. A package that needs binary archives commits them as
ordinary blobs or publishes an archive.

**GIT-6. Bindings inside the snapshot.** A reached package's bindings are read from its
effective manifest in the snapshot, the snapshot root's `[workspace.dependencies]`
inherited as section 4 says:

1. a toolchain binding is a toolchain binding;
2. a `path` binding names a reached member, packed by GIT-2 step 5;
3. a published binding `{ major, root, … }` is acquired as the whole of resolution step 3
   acquires one, carriers included, with the snapshot's declarations as the declarations:
   from the store when it holds the root undamaged, then from the source the snapshot
   declares for it, then from the declared source of its nearest carrier in the snapshot, so a
   store entry whose own child was lost is refilled from its binder's source. A declared
   source is an `archive` resolved lexically against the snapshot's workspace root and read
   from the snapshot by GIT-3, a `url` fetched by one GET, or a `git` source fetched by GIT-2
   at its `rev`, never by a ref. With none of these it is refused as `no source`.

A nested fetch is verified against the root that binds it and held in memory. Nothing a git
source acquires is written to the store until the outermost bundle passes section 7; a
refusal anywhere, the outer root included, keeps nothing. One bundle holds at most 4096
packages, as an archive does. The fetches one git source causes, its own and every nested
one, are at most 256, the next refusing (`fetch count`, section 9), and their responses
together are at most 1 GiB as received, the next byte refusing as `over the limit of 1 GiB
received`. One command fetches one (`git`, `rev`) once, whatever the `dir`, and every later
use of that commit reads the snapshot already held. Published content forms no cycle
(section 4), so nesting ends. Under `--frozen` no git source is fetched at all (section 8).

A git dependency chooses hosts its consumers contact: the `url` and `git` declarations in its
snapshot are fetched on a cold store, during `add`, `fetch`, `--locked` and plain builds,
though the lock records them only as `via:<id>`. An archive cannot do this, because it is
complete on its own. The set is fixed by `rev`, which the pack check binds, so only the
commit's author chooses it, never a server; `hirsel add` and `hirsel fetch` print every
source they contact, nested ones included, under `fetched`.

**GIT-7. Transport.** The tool speaks Git's smart HTTP protocol, version 2, over the HTTPS
client a `url` uses (the same roots, proxy settings and redirect rule):

1. `GET <git>/info/refs?service=git-upload-pack` with the header `Git-Protocol: version=2`,
   following redirects only to `https://` URLs, at most 3. A redirect target must itself be
   an info/refs request: its path ends in `/info/refs`, its query is exactly
   `service=git-upload-pack`, and it has no user information; any other target refuses as
   `redirected to <url>, which is not a Git endpoint`. The URL it ends at, less the
   `/info/refs` suffix and query, is the base for every later request of the command, and it
   is never written to a manifest or a lock. The response must be a version 2 capability
   advertisement that offers `fetch`; anything else refuses as `the server does not speak Git
   protocol version 2`. Its `object-format` (absent means `sha1`) must match the length of
   `rev`.
2. A fetch is one `POST <base>/git-upload-pack`, `Content-Type:
   application/x-git-upload-pack-request`, `Git-Protocol: version=2`, that never follows a
   redirect. Its request is `command=fetch`, `object-format`, then `ofs-delta`,
   `no-progress`, `deepen 1` when the advertisement offers `shallow`, exactly one
   `want <rev>`, and `done`. It never sends `have`, `thin-pack`, `filter`, `include-tag`,
   `packfile-uris` or `sideband-all`. So the pack holds every blob of the commit, not only
   the package's: one request and one pack, with no state between requests. A repository
   whose commit is over the response limit cannot be a git source, and the limit's refusal
   says so (section 9).
3. The response is read section by section. Exactly `acknowledgments` and `shallow-info` are
   read and not used; any other section, `packfile-uris` and `wanted-refs` included, refuses
   as `the server sent an unexpected <name> section`. The `packfile` section is read as Git's
   side band: band 1 is pack bytes, band 2 is progress and is discarded, and band 3, like an
   `ERR` packet anywhere, refuses with the server's text. A text that contains `not our ref`
   refuses as `the server would not send commit <rev>` (GIT-8). Band 1 bytes after the pack's
   trailing checksum refuse as `trailing bytes at <offset>`.
4. The tool sends no credentials, reads no `.netrc`, cookie or credential helper, and refuses
   a 401 or 403 as `the repository does not exist or requires authentication; hirsel fetches
   public repositories only`. A host may answer 401 for a repository that does not exist, so
   the text names both causes.

**Limits.** The bytes of each response are counted as received, and a response with any
`Content-Encoding` refuses as `the server answered with Content-Encoding <coding>`. The
advertisement is at most 64 KiB and the `ls-refs` listing at most 16 MiB; the fetch response
is at most 512 MiB, the archive limit. A response that delivers no byte for 60 s refuses as
`the server sent nothing for 60 s`; the same bound applies to a `url` fetch. A pack holds at
most 4 194 304 objects, checked on its header before any object is read. Every object is
inflated to find where the next begins, and its inflated length must equal its header's. An
object's **size** is its length after any delta is applied: a delta's result size, read from
the start of its instructions, is checked before anything is built for it. One object's size
is at most 512 MiB (the largest blob any rule reads is a snapshot `archive`), and the sizes of
all objects together, a delta counted by its result, are at most 4 GiB. A delta chain is at
most 4095 links, Git's own maximum pack depth. Each packed `C` keeps section 1's limits. A
pack limit refuses as a `bad pack` (section 9), `over the limit of <limit>`, with nothing
admitted, as soon as it is passed and never after reading the rest of the response.

**GIT-8. A build asks for the commit, never a ref.** Every fetch outside `hirsel add` sends
`want <rev>` and lists no refs, so a branch that advanced or a tag that moved never changes
what a build gets. Git's own server serves any commit it holds to a protocol version 2
`want` (`upload-pack.c`'s `parse_want`), but a hosted server may serve only commits that a
ref reaches, so a commit that was force-pushed away can become unfetchable. A server that refuses
(`not our ref`) refuses the fetch as `the server would not send commit <rev>`; then the
content is gone from that source, and only the store or a source the manifest is edited to
declare (resolution tries only the first) can supply the root: `hirsel pack`'s archive of
the commit, kept beside the workspace or served at a `url`, binds the same root (GIT-2), so
the swap changes only the lock's source. A served commit need not belong to the named
repository's history: a host that shares objects between a repository and its forks may serve
a commit that exists only in a fork under the upstream URL. The root, not the URL, says what
the content is (GIT-12).

**GIT-9. Checking the pack.** Before reading the snapshot, the tool checks:

1. the pack's header, version 2 or 3, and its trailing checksum;
2. every object's id. An id is computed from the object's type, size and bytes with the
   repository's hash, and is never taken from the server, so a lookup by id finds only bytes
   that hash to it. Every object in the pack is hashed, because the lookup needs every id. A
   SHA-1 id is computed with collision detection, as Git does, through the one hash function
   every object id goes through, and an object flagged as a collision attack refuses the pack
   whether or not the walk reads it;
3. that every `REF_DELTA` base is in the pack (a thin pack is refused), and every
   `OFS_DELTA` base is the start of an earlier entry of the pack;
4. that the object `rev` is in the pack and is a commit, that its tree and every tree and blob
   the walk reads are in the pack with the type the walk expects, and that each commit and
   tree read is well formed: a commit begins with a `tree` line naming an object id, and a
   tree is a sequence of entries, each a mode, a space, a non-empty name without `/` or NUL,
   a NUL and an object id.

`rev` is a locator, and the root is the authority: a server that serves different bytes
under a commit id fails step 2, and a commit id that was made to collide still has to pack to
the pinned root.

**GIT-10. Resolving a reference.** Only `hirsel add` resolves a reference, with one `ls-refs`
request (`symrefs`, `peel`, and a `ref-prefix` for each candidate name) on the base of GIT-7:

1. a `REF` of 40 or 64 lower-case hex digits is the commit itself: nothing is listed and no
   `ref` is written;
2. `refs/heads/<b>` or `refs/tags/<t>` names that reference exactly;
3. any other `REF` is looked up as `refs/tags/REF` and `refs/heads/REF`; when both exist it is
   refused as ambiguous, so no spelling means two things;
4. no `REF` names the branch `HEAD` points at, which must be a `refs/heads/` reference, and
   `hirsel add` prints `HEAD of <git> is <ref> at <rev>` so the choice is seen.

Every match is on the whole name: `ref-prefix` only filters the listing, and a listed
`refs/tags/v10` never answers `v1`.

An annotated tag is peeled to the object it names, and the resolved object must be a commit
(checked on the fetched pack). The `ref` written is the full name.

**GIT-11. Mutable references.** A binding names a commit, so a moved reference never changes
a manifest, a lock or a build. Only `hirsel add` with `--update` takes a new commit. The
**same source** for `add` is the pair (`git`, `dir`). When `add` resolves the bound `ref`
again, the requested `REF` naming the same full name, and it now names another commit, `add`
refuses without `--update` and says only what it saw: a tag `moved`, which a tag should never
do, and a branch `now names another commit`. The fetch has depth 1 and no ancestry, so the
tool cannot tell a branch that went forward from one rolled back to an old commit, and it
never says which. Each prints the bound and the fetched commit and root. A different `REF`,
or a commit id, for the same source is a named update, as a new URL is: it replaces the
binding and prints both commits and roots. A source bound to the same `rev` and root is
`unchanged`.

**GIT-12. Offline and locked.** A root in the store is opened from the store and its source
is never contacted, whatever the source is. Under `--locked`, a root missing from the store
is fetched by GIT-8 from the lock source, and the bundle must reproduce the pinned root or
the command refuses with the `fetch mismatch` refusal and keeps nothing. Under `--frozen`
the tool never fetches, so a git source is never contacted and a missing root refuses as
`frozen, absent`. The store holds verified `C`s only, never Git objects: another command
that needs the same commit for another `dir` fetches it again.

Only `root` identifies content. `git`, `rev` and `ref` are locators that no warm build checks:
a change that edits `root` and keeps `git` and `rev` passes every command over a store that
already holds the new root, however it got there. The review gate for a changed binding is
`hirsel fetch --locked --package-store <empty directory>`, which fetches every root from its
lock source and refuses one that the source does not reproduce.

**The discovery index.** A website may list published libraries as rows
`{id, root, url, first_seen}`, where `first_seen` is when the index first fetched that URL and
verified that root (it cannot be derived from archive bytes); everything else it shows is
derived from verified archives. Submission is a URL, no account owns a name, and two URLs may
serve the same `name@major`. Rows are never edited: changed content at a URL gets a new row and
a mark. **No command contacts it**, and `build` never treats it as an authority. Each row shows
`hirsel add <url> --root sha256:<root>`<!-- doc-ref: planned -->. The index and the origin are
then two observers: if the origin later serves other bytes, `add` refuses. A URL and root copied
together from one compromised page authenticate nothing about the publisher.

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
  - `git:<git>#<rev>` or `git:<git>#<rev>/<dir>`, the source spelling of GIT-1;
  - `store`;
  - `via:<id>`;
- `edges`, the row's bindings as `name@major` texts, sorted as the rows are.

A published row's source comes from the manifests and the `C`s alone, never from what the
store happened to hold or which archive was opened. It is the first `url`, `archive` or `git`
among the declarations that bind that root, taking declaring packages in identity order; a
declaration with `root` alone names no source. With no such declaration, it is `via:<id>`, the first
published package, in identity order, whose `C` binds that root. With neither, it is
`store`. So a package packed from a git commit that no declaration of the workspace binds,
such as a sibling member of the commit (GIT-6), has the source `via:<id>` of the package
whose `C` binds it, like any package an archive brought. A git row's `ref` is not in the
lock: moving `ref` alone changes no content. A member row's source is always `member:<dir>`. A member row that no published binding
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
before any source is touched, so a `--frozen` command never opens a URL, an archive path or a
git repository.

## 9. Diagnostics

A tool refusal is one block on stderr with exit status 2, like every other `hirsel:` error.
A refusal in resolution or in `pack`'s staging comes before the commit point (section 8), so
the command writes no lock and no build output. The only thing a refused command can leave
behind is store entries that were fetched and verified before a later step refused, as under
`--locked` with a stale lock; an entry is named by its content, so keeping it is harmless.
Manifest refusals
name the manifest path as the command reached it, then a colon; a manifest inside a git
commit is named by its snapshot path (GIT-1), and every row below that names a manifest
applies to it unchanged. A refusal that is not about
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
| member pin | `hirsel: <manifest>: foreign.<module>: archive "<file>" for <target> does not match its pin: expected sha256 <E>, found sha256 <O>` (when the member's root is computed), with `; it is a Git LFS pointer, and hirsel does not fetch LFS content` appended when the found bytes are one (GIT-5) |
| no sources | `hirsel: <manifest>: sources: a package lists at least one source` |
| member outside | `hirsel: <root manifest>: workspace.members: "<dir>" resolves outside the workspace` |
| member claimed | `hirsel: <root manifest>: workspace.members: "<dir>" belongs to the workspace at <other root manifest>` |
| output alias | `hirsel: --output <path> is an input of this command` (a manifest, a captured file, the lock or a store entry, compared by file identity) |
| no lock in this mode | `hirsel: --locked needs a workspace lock; <what was given> has none` (or `--frozen`) |
| two dependency forms | `hirsel: <manifest>: dependencies.<name>: give exactly one of path, root, workspace or a toolchain major` |
| two sources | `hirsel: <manifest>: dependencies.<name>: give at most one of url, archive or git` |
| git fields | `hirsel: <manifest>: dependencies.<name>: <field> needs git`, for `rev`, `ref` or `dir` without `git`, and `git needs rev` |
| bad git field | `hirsel: <manifest>: dependencies.<name>.<field>: <what is wrong>`: for `git`, `"<url>" is not an https:// URL`, `has user information`, `has a query` or `has a fragment`; for `rev`, `"<r>" is not a full commit id of 40 or 64 lower-case hex digits`; for `ref`, `"<r>" is not refs/heads/<name> or refs/tags/<name>`; for `dir`, `"<d>" breaks the path rules: <which rule>` |
| git package missing | `hirsel: <manifest>: dependencies.<name>: <source> has no hirsel.toml`, or `<source> is a workspace root, not a package` |
| reaches submodule | `hirsel: <snapshot manifest>: sources: "<path>" reaches submodule "<entry>", and hirsel does not fetch submodules` (and the `archive file` and member forms) |
| bad pack | `hirsel: <id>: <source> sent a bad pack: <first failure>`, the failure one of `bad header`, `checksum mismatch`, `truncated at <offset>`, `trailing bytes at <offset>`, `object <oid> is a SHA-1 collision attack`, `delta base <oid> is not in the pack`, `delta base at <offset> is not an earlier entry`, `commit <rev> is not in the pack`, `object <oid> is not in the pack`, `object <oid> is a <type>, not a <type>`, `object <oid> is not a well-formed <type>`, `tree <oid> has mode <mode>`, `tree <oid> names "<entry>" twice`, `object at <offset> inflates to <n> bytes, not <m>`, `over the limit of <limit>` with `<limit>` one of `512 MiB object`, `4194304 objects`, `4 GiB inflated` or `4095 delta links` |
| git protocol | `hirsel: <id>: fetching <source> failed: <cause>`, with the causes of GIT-7 and GIT-8 in their words: `the server does not speak Git protocol version 2`, `the server's object format is <f>, not <g>`, `the server would not send commit <rev>`, `redirected to <url>, which is not https`, `redirected to <url>, which is not a Git endpoint`, `too many redirects`, `the server answered <status>` (any status other than 200 on a request, 401 and 403 aside), `the repository does not exist or requires authentication; hirsel fetches public repositories only`, `the server answered with Content-Encoding <coding>`, `the server sent an unexpected <name> section`, `the server sent nothing for 60 s`, `the response is over the limit of <limit>` with `<limit>` `64 KiB`, `16 MiB`, or `512 MiB; hirsel fetches the whole commit, so a repository whose commit is larger cannot be a git source` (GIT-7 step 2), `over the limit of 1 GiB received` (GIT-6), `the server said: <text>` |
| toolchain major | `hirsel: <manifest>: dependencies.std: the toolchain has std@1, not std@<n>` |
| declared core | `hirsel: <manifest>: dependencies.core: core@1 is bound in every package and is not declared` |
| path not a member | `hirsel: <manifest>: dependencies.<name>: <path> is not a member of the workspace at <root manifest>` |
| member identity | `hirsel: <manifest>: dependencies.<name>: <path> is <other>@<m>, not <name>@<major>` |
| not listed | `hirsel: <manifest> is inside the workspace at <root manifest> but is not one of its members` |
| root is no package | `hirsel: <root manifest> is a workspace root, not a package; name a member directory` |
| member cycle | `hirsel: the members' path bindings form a cycle: a@1 -> b@1 -> a@1`, starting at the smallest identity on the cycle |
| inherit missing | `hirsel: <manifest>: dependencies.<name>: the workspace declares no <name>` |
| no source | `hirsel: <id> sha256:<P> is not in the package store and its binding names no source` |
| archive outside | `hirsel: <manifest>: dependencies.<name>: archive "<path>" is not inside the workspace root. To share an archive across packages, add a parent hirsel.toml with [workspace] and members listing the package directories, then use a relative path within that workspace` |
| archive incomplete | `hirsel: <id>: <source> lacks sha256:<P>, which <binder> binds as <name>@<major>`, the least such edge |
| archive extra | `hirsel: <id>: <source> holds sha256:<Q>, which nothing in it binds`, the least such root |
| fetch mismatch | `hirsel: <id>: <source> holds sha256:<Q>, but the binding pins sha256:<P>` |
| fetched identity | `hirsel: <id>: sha256:<P> is <other>@<m>`, for every edge whatever the content's origin |
| malformed | `hirsel: <id>: <source> is not a package archive: <first decoding failure>`, the failure in the words of section 1, and `<source>` the archive or url the bytes came from |
| fetch failed | `hirsel: <id>: fetching <url> failed: <cause>` |
| fetch count | `hirsel: <id>: <source> causes more than 256 fetches` (GIT-6) |
| diamond | `hirsel: <id> is bound to more than one root:` then one line per distinct root, each `  <path> = sha256:<root>`, in path order, each with its first path |
| member and root | `hirsel: <id> is a member and is also bound to sha256:<P>, which is not the member's root:` then `  <path> = member` and `  <path> = sha256:<P>`, in path order |
| missing lock | `hirsel: hirsel.lock is missing under --locked; run hirsel lock` (or `--frozen`) |
| stale lock | `hirsel: hirsel.lock is stale under --locked:` (or `--frozen`) then the change lines of section 8 |
| frozen, absent | `hirsel: <id> sha256:<P> is not in the package store, and --frozen does not fetch (source <source>)` |
| frozen, damaged | `hirsel: <id> sha256:<P> is damaged in the package store, and --frozen does not fetch (source <source>)` |

Several manifest refusals are reported together, in member order and then key order, so that
one run shows all of them. The first refusal of a later resolution step is the only one shown.

Text that comes from a server (an `ERR` packet, band 3, a status line) or from a tree (an
entry name, a mode) is written with the lock's string escapes (section 8) and cut at
256 bytes, so no byte a server chooses reaches the terminal as a control sequence.

`hirsel add`<!-- doc-ref: planned --> (section 11) adds these refusals, each exit 2 with nothing written (the store may
hold verified entries):

| case | diagnostic |
|---|---|
| add, locked | `hirsel: <manifest> needs to be updated but --locked was passed` (or `--frozen`: the flag actually given) |
| add, root differs | `hirsel: <id>: fetched root differs from --root`<!-- doc-ref: planned -->, then `  fetched: sha256:<Q>`, `  expected: sha256:<P>` and `  source: <url or archive>` |
| add, source rebound | `hirsel: <source> is already bound to another root`, then `  bound: sha256:<P>`, `  fetched: sha256:<Q>` and `pass --update to accept the fetched root`<!-- doc-ref: planned --> |
| add, manifest changed | `hirsel: <manifest> changed while hirsel add ran; nothing was written`<!-- doc-ref: planned --> |
| add, shared binding | `hirsel: the workspace binds <id> for <members>; pass --update to change it`<!-- doc-ref: planned --> |
| add, local and workspace | `hirsel: <manifest>: dependencies.<name> is bound here and in the workspace; pass --update to replace the local binding with { workspace = true }`<!-- doc-ref: planned -->, then both bindings |
| add, inherited | `hirsel: <manifest>: dependencies.<name> inherits the workspace binding; pass --workspace to change it for the workspace, or --update to bind it here`, for a plain `add` of another source or root |
| add, root absent | `hirsel: sha256:<P> is not in the package store` |
| add, toolchain | `hirsel: the toolchain does not provide <id>` |
| add, not a member | `hirsel: <dir> is not a member of the workspace at <root manifest>` |
| add, bad git source | `hirsel: <SOURCE> is not git+https://HOST/PATH[?dir=DIR][#REF] or github:OWNER/REPO[/DIR][@REF]: <which part>`, for a scheme after `git+` other than `https`, a query other than one `dir=`, a `dir` or `DIR` that breaks the path rules, user information, an `OWNER` that is not `[A-Za-z0-9-]+`, a `REPO` that is not `[A-Za-z0-9._-]+` or is `.` or `..`, a `HOST` that is not lower case, or an empty `REF` |
| add, no such ref | `hirsel: <git> has no branch or tag <REF>` |
| add, ambiguous ref | `hirsel: <git>: <REF> names both refs/tags/<REF> and refs/heads/<REF>; write the full name` |
| add, not a commit | `hirsel: <git>: <REF> names a <type>, not a commit` |
| add, no HEAD branch | `hirsel: <git>: HEAD names no branch; give a branch, a tag or a commit` |
| add, ref moved | `hirsel: <git> <ref> moved:` (a tag) or `hirsel: <git> <ref> now names another commit:` (a branch), then `  bound: <rev> sha256:<P>`, `  fetched: <rev> sha256:<Q>` and `pass --update to accept it`<!-- doc-ref: planned --> (GIT-11) |
| add, virtual root | `hirsel: <root manifest>: no package manifest here; pass --workspace to declare it for the workspace`<!-- doc-ref: planned --> |
| add, no workspace | `hirsel: <manifest>: no [workspace]; hirsel add does not create a workspace boundary`<!-- doc-ref: planned --> |
| add, root on a local form | `hirsel: --root applies to a fetched source`<!-- doc-ref: planned --> |

Resolution refusals (diamond, member and root, cycles) are the rows above, reported against the
edited manifests before anything is written. A failure after `add` has renamed a manifest is
exit status 1, not a refusal: its block names what was committed, what was not, and the command
that completes it (the same `hirsel add`<!-- doc-ref: planned --> again). A partial cache write (section 7) is exit 2
with `cache: partial` and the I/O error.

## 10. Keys, reuse and the recipe

Every key that decides reuse already hashes the content it covers. This section says how a
dependency's content reaches each key, so a stale reuse is impossible. A dependency's source,
a git source's `git`, `rev`, `ref` and `dir` included, enters no key: a package packed from a
commit is a published package with its root, and everything below holds for it unchanged.

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
| `hirsel pack [DIR] --output FILE` | publishes the member at DIR (section 6) |
| `hirsel inspect DIR\|ARCHIVE.hpkg\|DEPENDENCY` | prints the selected package's public interface by default |
| `hirsel new NAME` | writes `NAME/hirsel.toml`, its sources, its tests and its first lock |
| `hirsel add SOURCE` | binds one dependency in a manifest and updates the lock |

Inspect a dependency by its manifest name from the package that declares it.
An archive supplies its head package and verified dependency closure.

`--locked`, `--frozen` and `--package-store DIR` apply to `check`, `build`, `publish`, `run`,
`test`, `inspect`, `replay`, `sdk`, `lock`, `fetch` and `pack`, when the command is given a
package directory; section 8's input modes say what each does without one. `hirsel lock --locked` checks the lock without
writing it, which is the CI freshness check. `hirsel fetch --locked` followed by a later
`--frozen` build over the same store is the offline build. `hirsel publish` keeps its meaning:
it prints an executable's R ([launch](launch.md#release-recipe-and-host-assembly)), and a
library is published by `pack`.

### Adding a dependency

`hirsel add` edits a manifest and updates its lock. It accepts HTTPS URLs, git sources
(`git+https://` and `github:`), `.hpkg` paths, package-store roots, toolchain names and member
directories, in a member's own table or, with `--workspace`, in the workspace's. `github:`
names a repository commit, never a release asset (below).

`hirsel add SOURCE [--root sha256:HEX] [--workspace] [--update] [--dry-run] [--package-store DIR] [--json]`<!-- doc-ref: planned -->

| `SOURCE` | Fetches | Writes |
|---|---|---|
| `https://…` | one GET, `https` redirects only | `name = { major, root, url = "<as typed>" }` |
| `git+https://HOST/PATH[?dir=DIR][#REF]` | one reference listing unless `REF` is a commit id (GIT-10), then one pack fetch (GIT-7) | `name = { major, root, git = "https://HOST/PATH", rev, ref, dir }`, with `ref` absent for a commit id and `dir` absent without `?dir=` |
| `github:OWNER/REPO[/DIR][@REF]` | exactly what `git+https://github.com/OWNER/REPO?dir=DIR#REF` fetches | the same, with `git = "https://github.com/OWNER/REPO"` |
| a path ending `.hpkg` | a read confined to its directory descriptor | `name = { major, root, archive = "<relative to the declaring manifest>" }` |
| `sha256:HEX` | nothing; the root must be in the package store | `name = { major, root }` |
| `NAME@MAJOR` | nothing; the toolchain must provide it | `name = { major }` |
| a directory holding `hirsel.toml` | nothing; it must be a listed member | `name = { major, path = "<relative to the declaring manifest>" }` |

The name and major come from the fetched head (there is no rename). `--root`<!-- doc-ref: planned --> names the head
root the archive must have (`Root::of` its head bytes, section 1), or the root the commit
must pack to, and applies only to the URL, archive, `git+https://` and `github:` forms: it is
a second witness for first use. `--update`<!-- doc-ref: planned --> allows a new root
for a source already bound, or a change to a binding other members consume. `--dry-run`<!-- doc-ref: planned --> does
everything before publication; verified archives may still enter the store. `--json` prints one
object. `--locked` and `--frozen` refuse.

**Which manifest.** The nearest `hirsel.toml` with a `[package]`, walking up from the working
directory, or the workspace root when it has none (then `--workspace`<!-- doc-ref: planned --> is required).

**Bindings already present.** The same URL or archive already bound to another root is treated
as immutable: other bytes there are a moved tag or tampering, so `add` refuses and prints both
roots, and `--update`<!-- doc-ref: planned --> accepts the new one. The same git source, the
pair (`git`, `dir`), resolving to another commit is refused the same way and says whether a
tag moved or a branch now names another commit (GIT-11); `--update` takes the new commit and
its root. Another `REF` or a commit id for the same pair is a named update. A different source for a name already bound
replaces the binding and prints the old and new roots; naming a new source is how a dependency
is updated when there are no version ranges.

**Paths and placement.** Every `path` and `archive` is written relative to the manifest that
declares it (section 2). A member at `apps/ui` adding `vendor/pkg.hpkg` at the workspace root
writes `../../vendor/pkg.hpkg` in its own manifest, or `vendor/pkg.hpkg` in the root's
`[workspace.dependencies]`.

| Context | Without `--workspace`<!-- doc-ref: planned --> | With it |
|---|---|---|
| listed member | the member's `[dependencies]` | root `[workspace.dependencies]`, then the member's `{ workspace = true }`: two operations |
| root that is also a package | its `[dependencies]` | both tables in the one root manifest: one operation |
| virtual root | refused | declaration only: no member gains an edge, so the lock may stay identical (section 4) |
| package with no `[workspace]` above | its `[dependencies]` | refused |
| directory not listed, or nested under another root | refused by discovery | refused |

A member that already binds the name and asks for `--workspace`<!-- doc-ref: planned --> is
refused naming both bindings; `--update`<!-- doc-ref: planned --> replaces the local binding
with `{ workspace = true }`. Changing a `[workspace.dependencies]` entry that other members
consume needs `--update`<!-- doc-ref: planned --> and prints them; the member running `add`
asked for the change. A plain `add` at a member that inherits the name is `unchanged` when the
workspace already binds that root and source, and otherwise refuses without `--update`, so a
member never drifts from the shared binding by accident.

**Transaction.** In this order, and nothing is written before step 8:

1. Refuse `--locked` and `--frozen`. Take the workspace lock (`flock` on the workspace root's
   directory descriptor), which excludes a concurrent `add`.
2. Capture each target manifest and `hirsel.lock`: bytes and file identity.
3. For a git source whose `REF` is not a commit id, resolve it with one reference listing
   (GIT-10).
4. Capture the bundle **once**: one capped HTTPS body, one confined read, or one pack fetch of
   the commit, packed by GIT-2. Decode it once and
   verify it as section 7 does: the head's root, every entry, and the whole reachable closure
   (identities, published edges, missing, extra and duplicate entries), even for children the
   store already holds. A failure refuses with the store untouched. One buffer and one decode
   serve verification, resolution and admission; a body over the limit stops at the cap with
   nothing admitted.
5. With `--root`<!-- doc-ref: planned -->, a head root that differs refuses with the store untouched. Then the
   same-source check, keyed on the URL, the archive, or the pair (`git`, `dir`), not on the
   fetched name: the same source bound to another root or commit refuses without
   `--update`<!-- doc-ref: planned --> (GIT-11).
6. Admit those same bytes to the store (section 7, including its I/O case).
7. Build the edited manifests in memory and resolve the workspace through the real resolver on
   that overlay. Every refusal of section 5 refuses here, with nothing written. Roots the
   resolution needs that the store lacks are fetched under `fetch`'s rules and listed as
   `fetched`, never silently, as is every source a git source's snapshot made the tool
   contact (GIT-6). If every target manifest already has the same name, root and
   source, the result is `unchanged`: derive and commit the lock if it is missing or stale, and
   exit 0. `--dry-run`<!-- doc-ref: planned --> prints and exits 0 here.
8. Capture each target manifest again; bytes or file identity that differ from step 2 refuse
   (`changed while hirsel add ran`<!-- doc-ref: planned -->), never merge.
9. **Publish one manifest:** write it to a temporary file beside it, fsync, rename over it,
   fsync the directory. Then derive `hirsel.lock` from the committed manifests, never from the
   overlay, and commit it as section 8 does.
10. For `--workspace`<!-- doc-ref: planned --> at a listed member, publish the second manifest the same way, then the
    lock again.

**Split operations.** Each operation changes exactly one manifest and leaves a valid workspace
by itself: a `[workspace.dependencies]` entry no member consumes adds no edge and no lock row
(section 4), so "root declared, member not yet consuming" is a workspace every reader (an
editor, `build`, `dev`'s watcher, git) already understands. A crash or failure between the two
operations is reported as `partial` with exit 1, naming the committed manifest and the pending
one; running the same `add` again completes it, and the root step then reports `unchanged`. A
crash between a manifest rename and the lock commit leaves a stale lock, which the next
unlocked command or the next `add` repairs and `--locked` reports. There is no intent journal:
readers that do not take the lock would not see it.

The edit preserves the file's formatting and comments. A new key goes in sorted position when
its table is sorted, and last otherwise; a missing `[dependencies]` is created after
`[package]`. The URL written is the canonical locator (as typed, or the constructed GitHub
repository URL), never a redirect target. A git binding's keys are written in the order
`major`, `root`, `git`, `rev`, `ref`, `dir`.

**Git forms.** Each source text has one meaning, decided by its prefix alone:

1. `https://…` is always one package archive fetched by one GET. A repository page such as
   `https://github.com/o/r` is never sniffed or treated as git; it fails as an archive does.
2. `git+https://HOST/PATH[?dir=DIR][#REF]` is a git source. The `git` written is
   `https://HOST/PATH`, without `git+`, the query or the fragment. The only query accepted is
   one `dir=DIR`, with `DIR` under GIT-1's `dir` rule. `REF` is everything after the first
   `#`, taken as written and resolved by GIT-10. There is no `git+http://`, `git+ssh://` or
   `git+file://` form; each is refused as `add, bad git source`.
3. `github:OWNER/REPO[/DIR][@REF]` is exactly `git+https://github.com/OWNER/REPO?dir=DIR#REF`.
   `OWNER` is `[A-Za-z0-9-]+` and `REPO` is `[A-Za-z0-9._-]+`, and neither is `.` or `..`;
   `DIR` is every component after them up to the first `@`, and `REF` is everything after it.
   Without `@REF` the default branch's tip is taken, and `add` prints the branch and commit
   it chose. `/DIR` always names a directory, and
   never an asset or a package name.

A release asset is still reachable by its plain `https://` URL; the shorthand names the
repository, because a commit carries everything a package needs to be packed and checked,
while an asset carries only what its publisher chose to upload.

**Output.**

```text
hirsel: added json@1 sha256:cf1e…9a02 to hirsel.toml
        from https://example.org/json-1.hpkg (3 packages, 41 KiB, 2 new in the store)
hirsel: updated hirsel.lock:
        + json@1 sha256:cf1e…9a02 url:https://example.org/json-1.hpkg
        + text@2 sha256:77ab…10fe via:json@1
```

An update prints `hirsel: updated json@1 sha256:OLD… -> sha256:NEW…`; `--dry-run`<!-- doc-ref: planned --> prints `would
add` and `would update`; a partial completion prints the committed manifest, the pending one,
and `run the same hirsel add again to finish`<!-- doc-ref: planned -->. With `--json`, one object:

```json
{"result":"added","id":"json@1","root":"sha256:cf1e…","source":"url:https://example.org/json-1.hpkg",
 "committed":["hirsel.toml","hirsel.lock"],"pending":[],"previous_root":null,"dry_run":false,
 "cache":"complete","fetched":[],
 "closure":[{"id":"json@1","root":"sha256:cf1e…","new":true},{"id":"text@2","root":"sha256:77ab…","new":true}]}
```

`result` is `added`, `updated`, `unchanged` or `partial`, and `cache` is `complete` or
`partial`. Exit statuses: 0 for `added`, `updated`, `unchanged` or a dry run; 2 for a refusal
before any manifest rename (section 9); 1 for a failure after one, including `partial`.

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
little-endian 72 and the 72 bytes. `hirsel pack jx --output jx.hpkg` prints
`json@1 <X>` and `hirsel pack jy --output jy.hpkg` prints `json@1 <Y>`. This law catches any
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
and `hirsel pack demo --output demo.hpkg` prints `demo@1 <D>`. Module `aa` precedes `b` by
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
`url = "https://hirsel.invalid/jx.hpkg"`. Fill store S1 with `hirsel pack jx --output jx.hpkg
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
Run from `p`'s parent, `hirsel check p` and `hirsel pack p --output p.hpkg` both exit 2, create no
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
--output wrap.hpkg` prints `wrap@1 <W>` and then `json@1 <X>`. `app` binds
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

**Laws for `hirsel add`.** L15–L22 cover S6 in `s6_add_protocol`, L25–L28 cover S8 in
`s8_add_workspace`, and L29 covers S9 in `s9_add_forms_that_fetch_nothing`. L23 and L24
specify the `github:` form as a git source, and L30 to L37 the git sources of section 7;
their fixtures are under **Git fixtures** after L29. No CLI law claims collision detection:
no public SHA-1 collision is a Git object, because the `blob <size>` header changes the state
the collision blocks need. The check is a unit case beside the hash's own vectors instead: the
one function every object id goes through (GIT-9 step 2) is given the raw bytes of the
published `shattered-1.pdf` (SHA-1 `38762cf7f55934b34d179ae6a4c80cadccbb7f0a`) and must report
a collision.
X and Y are `jx`'s and `jy`'s roots and W is `wrap@1`'s (L6). Each `add` runs in a workspace
whose `hirsel.toml` and `hirsel.lock` bytes the law records before it starts.

**L15 (L6.1). A refused add changes no file.** The workspace resolves `json@1` at X through
member `a`. Package `app` runs `hirsel add`<!-- doc-ref: planned --> on an archive whose closure holds `json@1` at Y
(`wrap@1` rebuilt over `jy`). It exits 2 with the diamond refusal, and every manifest and
`hirsel.lock` is byte-identical afterwards. The law catches Cargo's order: writing the manifest
before resolving it.

**L16 (L6.2). `--root`<!-- doc-ref: planned --> stops a substituted archive.** `vendor/jx.hpkg` holds `jy`'s archive;
`hirsel add vendor/jx.hpkg --root <X>`<!-- doc-ref: planned --> exits 2 with
`fetched root differs from --root`<!-- doc-ref: planned --> naming Y and X, writes nothing, and the store holds no entry
named Y.

**L17 (L6.3). The written binding builds offline, warm and cold.** After `hirsel add`<!-- doc-ref: planned --> of
`vendor/jx.hpkg`, `hirsel build --frozen` passes with no network, and passes again over an
empty store after `hirsel fetch` from the written source alone.

**L18 (L6.4). The same source with other bytes is refused.** After adding `vendor/jx.hpkg`,
replace the file with `jy`'s archive: `hirsel add vendor/jx.hpkg`<!-- doc-ref: planned --> exits 2 naming X and Y, and
with `--update`<!-- doc-ref: planned --> it binds Y.

**L19 (L6.5). Comments and order are kept.** A manifest with comments and a sorted
`[dependencies]` gains the new key in sorted position, every other byte unchanged; the oracle is
a hand-written expected file.

**L20 (L6.6). One capture.** An HTTPS fixture that serves `jx`'s archive and then `jy`'s for the
same URL gets exactly one request, and the admitted entries are X's. A local archive replaced
after its read is read once. A head whose name changed cannot evade the same-source rule.

**L21 (L6.7). Caps and partial admission.** A streamed response over 512 MiB, a bad final
entry, and a missing or extra closure entry each admit nothing. An I/O failure injected during
admission reports `cache: partial` and changes no manifest. One peak-RSS reading on a
near-limit fixture and on many small entries stays under the 2 GiB cap.

**L22 (L6.8). Crash points.** A crash after the manifest rename and before the lock commit
leaves a lock that `hirsel lock` repairs, and the next `add`, even `unchanged`, repairs it too.
Two concurrent adds of different sources both end committed or one refuses; neither loses the
other's binding. The oracle is the lock `hirsel lock` derives from the committed manifests.

**L23 (L7.1). A `github:` reference is a commit.** The git fixture server stands in for
`github.com` and serves repository r1 (below) at `/o/r`, with `refs/tags/v1` and
`refs/heads/main` at C1, a decoy `refs/tags/v10` at C2, and `HEAD` pointing at
`refs/heads/main`. In a single-package
workspace `app`, `hirsel add github:o/r/wrap@v1` writes exactly

```text
wrap = { major = 1, root = "sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689", git = "https://github.com/o/r", rev = "32e212a4b69a9f3b39893b29f4e4f1e935626a2b", ref = "refs/tags/v1", dir = "wrap" }
```

and `hirsel add github:o/r/wrap` (no `REF`) in a fresh copy writes the same with
`ref = "refs/heads/main"`. `github:o/r/wrap@32e212a4b69a9f3b39893b29f4e4f1e935626a2b` writes
it with no `ref`, and the request log shows no `ls-refs`. The expected lines are written by
hand from W (section 6) and C1. The law catches a shorthand that still names a release asset,
a `/DIR` read as a package name, a reference matched by prefix (the decoy `v10` would give
C2 and W2), and a written `ref` that is not the full name.

**L24 (L7.2). Each source text has one meaning.** Each of these exits 2 with the `add, bad git
source` refusal and writes nothing: `github:o/r/../x`, `github:o`, `git+https://alpha.test/r1?x=1`,
`git+https://user@alpha.test/r1`, `git+https://alpha.test/r1#` (empty `REF`), `git+http://alpha.test/r1`,
`git+ssh://alpha.test/r1`. With r1 also holding a branch named `v1`,
`hirsel add git+https://alpha.test/r1?dir=wrap#v1` refuses with `add, ambiguous ref`, and
`#refs/tags/v1` succeeds. `hirsel add https://alpha.test/r1`, where the fixture answers with
an HTML page, fails as an archive (`malformed`) and the request log holds one GET to `/r1`
and nothing under `/r1/info/refs`. The law catches a source that is sniffed and a short ref
that picks one of two meanings.

**L25 (L8.1). Two members share one binding.** Two members adding `vendor/jx.hpkg` with
`--workspace`<!-- doc-ref: planned --> end with one `[workspace.dependencies]` entry and two
`{ workspace = true }` bindings.

**L26 (L8.2). Paths are relative to the declaring manifest.** A member at `apps/ui` adding
`vendor/jx.hpkg` writes `../../vendor/jx.hpkg` locally and `vendor/jx.hpkg` in the root, and a
cold fetch from each succeeds.

**L27 (L8.3). A virtual root only declares.** `--workspace`<!-- doc-ref: planned --> at a
virtual root leaves `hirsel.lock` byte-identical.

**L28 (L8.4). Partial completion.** A crash between the root and member operations leaves a
workspace that `hirsel lock --locked` accepts after one derive, and rerunning the same `add`
completes it. Two members with different local pins of `json@1` (X and Y) refuse through the
resolver's diamond rule.

**L29 (S9). Forms that fetch nothing.** `hirsel add std@1` for a toolchain package and, from
a subdirectory of member `apps/ui`, `hirsel add ../../../libs/a` for a listed member each write
a binding (`a = { major = 1, path = "../../libs/a" }`, spelled from the declaring manifest) that
the next `--frozen` build of an entry using both accepts. `std@2`, an unlisted directory and
`--root` with either form refuse and write nothing.

**Git fixtures.** L23, L24 and L30 to L37 run a local Git server in the package-directory
harness, behind the CONNECT proxy and test CA of the existing HTTPS fixture, as `alpha.test`
(and as `github.com` for L23). No law reaches the internet or runs a host `git`. The server
holds repositories as tables of objects and refs written in the law. It answers
`info/refs` with a protocol version 2 advertisement (`version 2`, `ls-refs`,
`fetch=shallow`, `object-format=sha1`), `ls-refs` from its ref table, and `fetch` with a
version 2 pack of every object reachable from the wanted commit at depth 1, undeltified
unless the law says otherwise, written from Git's pack format documentation
(`gitformat-pack`). It logs every request: its path, its `command`, and its `want` and
`ref-prefix` lines. The commit ids below were computed by Git 2.43.0 over exactly these
bytes, with author and committer `Fixture <fixture@hirsel.invalid>`, so they are independent
of hirsel and of the fixture's own writer; the law fails if the fixture's objects do not
hash to them.

Repository r1 holds five files, each ending in one line feed:

- `hirsel.toml`: `[workspace]` and `members = ["json", "wrap"]`;
- `json/hirsel.toml` and `json/json.hirsel`: `jx` exactly (section 12, **Known roots**);
- `wrap/hirsel.toml`: the lines `[package]`, `name = "wrap"`, `major = 1`,
  `charge = "charge-v1"`, an empty line, `[sources]`, `"wrap.hirsel" = "wrap"`, an empty line,
  `[dependencies]`, `json = { major = 1, path = "../json" }`;
- `wrap/wrap.hirsel`: `pub fn one() -> u64 { 1u64 }`, `wrap@1`'s file of L6.

Its commit C1 = `32e212a4b69a9f3b39893b29f4e4f1e935626a2b` (time `1700000000 +0000`, message
`v1`) has tree `f437e62ea2472c628183ef6ac6aad55bd0a9f7d9`. C2 =
`0b8c48e76c8462464b5ce67fd884449440d57844` (time `1700000060 +0000`, message `v2`, parent C1)
changes only `wrap/wrap.hirsel` to `pub fn one() -> u64 { 2u64 }`. `wrap` at C1 packs to W of
L6 and binds `json@1` at X; at C2 its `C` is 107 bytes and its root is
W2 = `sha256:27b883e20a7c9640cf072d584cd1e8dd198aecd0795e034a7cfba52048f523a0`, computed by
a reference encoder written from section 1, which also reproduces X and W. Unless a law says
otherwise, `refs/tags/v1` and `refs/heads/main` point at C1 and `HEAD` at `refs/heads/main`.

Each variant below is r1 at time `1700000000 +0000` with message `v1`, changed as stated,
and is served with its own commit as `refs/tags/v1`, `refs/heads/main` and `HEAD`'s target:

- r2 = `f9ae88b99451269b8b6b0d7d37bac85c316f44f7`: `wrap/wrap.hirsel` is a symbolic link
  (mode `120000`) to `../json/json.hirsel`;
- r3 = `da14b90edd082f0beed3c0f09fc23aaad0a60849`: `wrap/wrap.hirsel` is a link to
  `real/wrap.hirsel`, and `wrap/real/wrap.hirsel` holds the bytes;
- r4 = `859eddd90bb3d572224eafa424b8fe05ccff88cf`: adds the three files below and the
  submodule entry `vendor/evil` (mode `160000`, commit C2); every other blob is r1's;
- r5 = `1ec6268ebb01745682597da6ad7e90ec97c89a85`: `wrap/hirsel.toml` has the line
  `"ext/x.hirsel" = "x"` right after `"wrap.hirsel" = "wrap"`, and `wrap/ext` is a submodule
  entry (commit C2).

r4's `.gitattributes`, `.gitmodules` (its indents are tab characters) and `.lfsconfig` are,
each line ending in a line feed:

```text
* text eol=crlf filter=sentinel ident export-subst
wrap/wrap.hirsel export-ignore
```

```text
[submodule "evil"]
	path = vendor/evil
	url = https://alpha.test/evil
```

```text
[lfs]
	url = https://alpha.test/lfs
```

r6 = `068a6be0a352dea1d8d7faf1e80eb05040387670` (time `1700000000 +0000`, message `v1`, no
parent; its own commit is `refs/tags/v1`, `refs/heads/main` and `HEAD`'s target) is `demo@1`
of section 12 at the top of the tree. Its `hirsel.toml` is:

```text
[package]
name = "demo"
major = 1
charge = "charge-v1"

[sources]
"api.hirsel" = "api"
"aa.hirsel" = "aa"
"b.hirsel" = "b"

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

`api.hirsel` is `pub opaque type A {}` and `pub opaque type B {}`, `aa.hirsel` is
`pub fn ping(f: Foreign[api.A]) -> u64`, `b.hirsel` is `pub fn pong(f: Foreign[api.B]) -> u64`,
`native/aa-x86.a` is `abc` and `native/aa-arm.a` is empty, each source line ending in a line
feed, as in section 12. `native/b.a` is the 126-byte Git LFS pointer below, whose SHA-256 is
`70edffc238bc4d1e48b82fd69143269662f8daf7556f399d76669f3976dad323`:

```text
version https://git-lfs.github.com/spec/v1
oid sha256:3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d
size 1
```

The ids above were produced by a script running Git 2.43.0 over exactly these bytes; a law
needs only the bytes stated here.

**L30. A package in a subdirectory with a sibling `path` dependency.** In a single-package
workspace `app` binding nothing, `hirsel add git+https://alpha.test/r1?dir=wrap#v1` exits 0
and writes L23's line with `git = "https://alpha.test/r1"`. The lock is exactly:

```text
# hirsel.lock: written by hirsel; do not edit.
version = 1

[[package]]
id = "app@1"
source = "member:."
edges = ["wrap@1"]

[[package]]
id = "json@1"
root = "sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58"
source = "via:wrap@1"
edges = []

[[package]]
id = "wrap@1"
root = "sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689"
source = "git:https://alpha.test/r1#32e212a4b69a9f3b39893b29f4e4f1e935626a2b/wrap"
edges = ["json@1"]
```

The store holds exactly the entries X and W, and the request log is one `info/refs` GET, one
`ls-refs` with `ref-prefix refs/tags/v1` and `ref-prefix refs/heads/v1`, and one `fetch`
whose only `want` is C1. `hirsel pack wrap --output w.hpkg`, run in a checkout of r1 at C1,
prints `wrap@1 <W>` then `json@1 <X>`: the same roots. Three variants keep W:

- the server sends the same objects as a pack with `OFS_DELTA` and `REF_DELTA` entries;
- r1 with a third member `broken` listed, whose `broken/hirsel.toml` is not TOML: `broken`
  is never reached, so it is never read;
- `wrap` binds `json = { major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }` and the
  commit holds `vendor/jx.hpkg`, `jx`'s archive: X is read from the snapshot, and the request
  log gains nothing;
- `wrap` binds `json = { major = 1, root = "<X>", git = "https://alpha.test/rj", rev = "<J>" }`,
  where `rj` is a server-built commit J holding `jx` at the top of its tree: X is fetched from
  `rj`, the request log gains one `info/refs` GET and one `fetch` whose only `want` is J and
  no `ls-refs`, and `fetched` lists `git:https://alpha.test/rj#<J>`.

The law catches a sibling `path` dependency that is not packed to its own root, a member
binding encoded by its path instead of its root, a `dir` that selects the wrong package, a
root over Git object ids instead of over `C`, and a resolver that reads members the package
never reaches.

**L31. A tampered or substituted commit keeps nothing.** In `app`, written by hand:
`wrap = { major = 1, root = "<X>", git = "https://alpha.test/r1", rev = "<C1>", dir = "wrap" }`.
`hirsel check app` exits 2 with exactly
`hirsel: wrap@1: git:https://alpha.test/r1#32e212a4b69a9f3b39893b29f4e4f1e935626a2b/wrap holds sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689, but the binding pins sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58`,
and the store stays empty. With the binding's root W restored, two servers that lie are
refused, each leaving the store empty:

- one that sends `wrap/wrap.hirsel` with the bytes of C2 in place of C1's blob
  `b8b63ee2ea62f1ec73509eedeed52c7206deaffe`: `sent a bad pack: object b8b63ee2ea62f1ec73509eedeed52c7206deaffe is not in the pack`,
  because the bytes sent hash to C2's blob id and nothing names that;
- one that answers `want <C1>` with C2's objects: `sent a bad pack: commit 32e212a4b69a9f3b39893b29f4e4f1e935626a2b is not in the pack`.

Two packs built by the server with bad deltas are refused the same way: a `REF_DELTA` whose
base is absent, and two `REF_DELTA` entries that name each other, each `sent a bad pack: delta
base <oid> is not in the pack` with the base the first delta names. And with L30's nested
variant (`wrap` binding `json` by `git` from `rj`), the binding's root set to X makes the
outer root differ after the nested source fetched and verified X: the command refuses and the
store stays empty, X included.

The law catches a fetch that trusts the server's object ids or the ref, a missing delta base
treated as empty or followed for ever, and an admission, nested or outer, before the root
check.

**L32. A link that leaves the package.** `hirsel add git+https://alpha.test/r2?dir=wrap#v1`
exits 2, writes nothing and leaves the store empty, with exactly
`hirsel: git:https://alpha.test/r2#f9ae88b99451269b8b6b0d7d37bac85c316f44f7/wrap/hirsel.toml: sources: "wrap.hirsel" resolves outside the package`,
although the target is inside the commit. In the control r3, the link stays inside `wrap`, and
the add writes root W: the link's bytes are never hashed and the unlisted `real` directory
never enters. A server-built commit in which `wrap/wrap.hirsel` reaches its file through a
chain of 40 links inside `wrap` gives W; with 41 it is refused as `does not resolve to a
regular file`. Three more cases:

- r1 with an extra top-level tree entry named `json/json.hirsel`, holding `jy`'s source (a
  server-built tree; Git never writes a name with `/`): the add still writes W and the lock
  still pins X, because a path is looked up one component at a time;
- `wrap/up` is a link to `..` and `wrap/wrap.hirsel` a link to `up/wrap/real.hirsel`, with
  `wrap/real.hirsel` holding the bytes: refused as `"wrap.hirsel" resolves outside the
  package`, although the final file is inside, as L12 is for a member;
- the workspace at `ws/` lists `lib`, `ws/lib` is a link to `../elsewhere/lib` holding a
  valid package, and `ws/app` binds it by `path`, with `dir = "ws/app"`: refused as
  `hirsel: git:https://alpha.test/<repo>#<rev>/ws/hirsel.toml: workspace.members: "lib" resolves outside the workspace`.

The law catches a walk that contains links only to the commit instead of the package or the
workspace, containment decided on a normalised path, a tree flattened into a map of paths, a
link loop decided by the host, and an encoder that hashes link text.

**L33. Nothing in the repository runs or changes bytes.** The law's `PATH` begins with a
directory holding an executable `git` that creates the file `SENTINEL` and exits 1, and
`GIT_CONFIG_GLOBAL` names a file setting `filter.sentinel.smudge`, `filter.sentinel.clean`,
`filter.sentinel.process` and `core.hooksPath` (holding every hook) to programs that each
create `SENTINEL`. `hirsel add git+https://alpha.test/r4?dir=wrap#v1` exits 0 and writes root
W with `rev = "859eddd90bb3d572224eafa424b8fe05ccff88cf"`; `SENTINEL` does not exist; the
request log holds nothing under `/evil` or `/lfs`. A fetch through the `git` program fails or
creates `SENTINEL`; a checkout that applies `eol=crlf` makes `wrap.hirsel` 30 bytes and
another root; one that applies `export-ignore` loses `wrap.hirsel`; one that follows
`.gitmodules` requests `/evil`. Each is red here.

**L34. Locked and offline builds use the commit, never the ref.** After L30:

1. With the server stopped, `hirsel build app --frozen` passes.
2. With a new empty store S, `hirsel check app --locked --package-store S` passes, S holds
   exactly X and W, and the request log is one `info/refs` GET and one `fetch` whose only
   `want` is C1, with no `ls-refs`.
3. With `refs/tags/v1` moved to C2 on the server, step 2 gives the same result.
4. With a store holding W and not X, `--locked` fetches C1 again through `wrap@1`'s source
   (resolution step 3.3) and passes.
5. With a server that answers `want <C1>` with `ERR upload-pack: not our ref
   32e212a4b69a9f3b39893b29f4e4f1e935626a2b`, step 2 exits 2 with exactly
   `hirsel: wrap@1: fetching git:https://alpha.test/r1#32e212a4b69a9f3b39893b29f4e4f1e935626a2b/wrap failed: the server would not send commit 32e212a4b69a9f3b39893b29f4e4f1e935626a2b`.
6. `hirsel check app --frozen --package-store S` over an empty S exits 2 with exactly
   `hirsel: wrap@1 sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689 is not in the package store, and --frozen does not fetch (source git:https://alpha.test/r1#32e212a4b69a9f3b39893b29f4e4f1e935626a2b/wrap)`,
   and the request log is empty.

7. With `info/refs` on `alpha.test` redirecting to the same repository on a second fixture
   host `beta.test`, step 2 passes and the request log shows the `fetch` POST once, on
   `beta.test`. When `beta.test` answers that POST with a redirect, step 2 exits 2 with
   `the server answered 302`, and when the first redirect points at `http://beta.test/…`, it
   exits 2 with `redirected to http://beta.test/r1/info/refs?service=git-upload-pack, which is not https`.

The law catches a build that resolves a ref, a `--frozen` command that touches the network,
a store miss that cannot be refilled from the pinned commit, and a POST that follows a
redirect or a base taken from anywhere but the `info/refs` answer.

**L35. A moved tag is reported and never taken silently.** After L30, move `refs/tags/v1` to
C2 on the server. `hirsel add git+https://alpha.test/r1?dir=wrap#v1` exits 2, leaves every
manifest, the lock and the store as they were (no W2 entry), and writes exactly:

```text
hirsel: https://alpha.test/r1 refs/tags/v1 moved:
  bound: 32e212a4b69a9f3b39893b29f4e4f1e935626a2b sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689
  fetched: 0b8c48e76c8462464b5ce67fd884449440d57844 sha256:27b883e20a7c9640cf072d584cd1e8dd198aecd0795e034a7cfba52048f523a0
pass --update to accept it
```

With `--update` it writes `rev = "0b8c48e76c8462464b5ce67fd884449440d57844"` and root W2 and
prints `hirsel: updated wrap@1 sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689 -> sha256:27b883e20a7c9640cf072d584cd1e8dd198aecd0795e034a7cfba52048f523a0`.
Bound instead from `#main` and with `refs/heads/main` moved to C2, the first line reads
`hirsel: https://alpha.test/r1 refs/heads/main now names another commit:`. Bound from `#v1`,
with a tag `v2` at C2 and `v1` unmoved, `hirsel add git+https://alpha.test/r1?dir=wrap#v2`
exits 0 without `--update`, writes `rev` C2, `ref = "refs/tags/v2"` and root W2, and prints the
update line. The law catches an add that follows a moved tag, one that cannot tell a moved tag
from a new source, and one that calls a branch's change progress.

**L36. Submodules and LFS pointers.** `hirsel add git+https://alpha.test/r5?dir=wrap#v1` exits
2 with exactly
`hirsel: git:https://alpha.test/r5#1ec6268ebb01745682597da6ad7e90ec97c89a85/wrap/hirsel.toml: sources: "ext/x.hirsel" reaches submodule "ext", and hirsel does not fetch submodules`,
and the request log holds no second repository. With r6 tagged `v1`,
`hirsel add git+https://alpha.test/r6#v1` exits 2 with exactly
`hirsel: git:https://alpha.test/r6#068a6be0a352dea1d8d7faf1e80eb05040387670/hirsel.toml: foreign.b: archive "native/b.a" for x86_64-unknown-linux-gnu does not match its pin: expected sha256 3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d, found sha256 70edffc238bc4d1e48b82fd69143269662f8daf7556f399d76669f3976dad323; it is a Git LFS pointer, and hirsel does not fetch LFS content`.
r4's unread submodule entry was ignored in L33. The law catches submodule recursion and a
pointer packed as if it were the archive.

**L37. Every bound holds before memory or the store is spent.** Each of these servers makes
`hirsel check` of L31's binding with root W exit 2 with `sent a bad pack: over the limit of
<limit>` and leaves the store empty: a pack header declaring 4 194 305 objects (`4194304
objects`), refused before any object arrives, which the server's still-open response shows;
objects whose inflated bytes pass 4 GiB together (`4 GiB inflated`); a chain of 4096 deltas
(`4095 delta links`); and a delta bomb, a 16 MiB blob of zeros and one `OFS_DELTA` of 4096
copy instructions of 16 777 215 bytes each (the most one copy instruction names), whose
instructions are 16 KiB and whose result would be nearly 64 GiB (`512 MiB object`), refused
before anything is built for it. A response that streams past 512 MiB is the git protocol row
of section 9 instead: `fetching <source> failed: the response is over the limit of 512 MiB;
hirsel fetches the whole commit, so a repository whose commit is larger cannot be a git
source`. An object whose inflated length is not its header's is refused as `object at
<offset> inflates to <n> bytes, not <m>`. A pack near the bounds, C1's objects with a 500 MiB
blob of zeros and seven deltas that each copy it and insert one byte (500 MiB on the wire,
4 194 304 007 bytes inflated), is checked by a child process whose peak RSS stays under L21's
2 GiB cap. A chain of 257 repositories, each commit's package
binding the next by `git` and `rev`, makes `hirsel add` of the first refuse with
`hirsel: <id>: git:https://alpha.test/n0#<rev> causes more than 256 fetches` and leaves the
store empty. The law catches a client that reads a hostile pack to its end before applying a
bound, a delta whose result is built before its size is checked, and nesting with no bound.
