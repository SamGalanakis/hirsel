# Packages, workspaces and the lock

This chapter defines how the `hirsel` tool finds the packages of a request, binds each
dependency to exact content, and hands the result to the compiler. A **package** is the
language's unit (PROG-1, [programs](programs.md#prog-1-packages)): a name, a version and
modules. Its identity is `name@major`, two majors of one name are two packages, and
everything here happens before name resolution: the compiler receives one resolved set of
packages, each with its sources and its exact dependencies, and never sees a manifest, a lock
or a fetch. A package's name is scoped by the DNS name of its owner (`acme.dev/json`,
`github.com/acme/json`), except for the toolchain's own packages, whose names are bare
(`std`, `store`), and for workspace-only packages under `local/` (section 1). Nothing in this chapter changes what an accepted program means
([PROG-6](programs.md#prog-6-one-meaning)): a flag such as `--locked` decides only whether a
command proceeds.

The files are `hirsel.hcfg`, the **manifest** of a package or a workspace, and `hirsel.lock`,
the workspace's **lock**. A dependency is either another package of the
same local workspace or a published library fixed by its root, whose content comes from the
package store, a package archive, a URL, a git commit or its scope's registry.
[Registry](registry.md) specifies Phase 1 named selection, logged status and publication. Git is a transport only: a commit is
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

**Package names.** A package name has one of three forms:

- a **scoped name**, `HOST/NAME` or `HOST/TENANT/NAME`. `HOST` is a DNS name of two to 127
  labels joined by `.`, at most 253 bytes; each label is 1 to 63 bytes of `[a-z0-9-]`, does
  not start or end with `-`, and the last label is not all digits, so no IP address is a host.
  `TENANT` is 1 to 64 bytes of `[a-z0-9-]` that do not start or end with `-`: one path segment
  that a forge host gives an owner (`github.com/acme`). `NAME` is `[a-z][a-z0-9_]*` and at
  most 64 bytes. Every letter is lower case, so the spelling is the normal form and no two
  spellings name one package. The whole name is at most 255 bytes;
- a **local name**, `local/NAME`, with `NAME` as above. `local` has no `.`, so it is never a
  host. A local package is only ever a workspace member: it is never packed, never published
  and never decoded from `C` (sections 6 and 7);
- a **bare name**, `NAME` alone, which is a toolchain package's (section 2). A package
  outside the toolchain never has a bare name.

A name never contains `@`, so the identity `name@major` and every path that starts with it
split at the first `@`: `acme.dev/json@1/json/parse` is package `acme.dev/json`, major 1,
module `json`, item `parse` (PROG-1).

**Versions.** Every package has a **version** `MAJOR.MINOR.PATCH[-PRE]`, written in its
manifest (section 2). `MAJOR`, `MINOR` and `PATCH` are decimal numbers from 0 to 4294967295
without leading zeros. `PRE`, a prerelease, is one or more identifiers joined by `.`, each
1 to 64 bytes of `[0-9A-Za-z-]`, and an identifier of digits alone has no leading zero; the
whole prerelease is at most 255 bytes. There is no build metadata (`+…`). Versions are
ordered as Semantic Versioning 2.0.0 orders them: by `MAJOR`, `MINOR` and `PATCH`
numerically, then a version with a prerelease before the same version without one, and
prereleases identifier by identifier, numeric identifiers numerically and below
alphanumeric ones, which compare by ASCII bytes, a shorter list first when one is a prefix of
the other. The identity's major is the version's `MAJOR`. A version is part of `C`, so the
same files under another version are another root.

**Encoding.** In `C`, a number is unsigned LEB128 with no redundant byte, and a text is its
byte length as such a number followed by its UTF-8 bytes. `C` holds, in order:

1. the encoding version, the number 3. Readers accept only version 3, with no dual reader: a
   library published under an earlier version is republished. Bytes that end before the
   foreign-module count are a truncated encoding and are refused;
2. the package's name, a text, then its version: `MAJOR`, `MINOR` and `PATCH` as numbers and
   `PRE` as a text, empty for none;
3. its charge policy (PROG-1), a text, which in v1 is `charge-v1`;
4. its **bindings**: their count, then each binding, sorted by alias bytewise. A binding is
   the **alias** its sources name the dependency by (a text), the dependency's package name (a
   text) and major (a number), then a tag:
   - `0`, a toolchain package, with nothing after it;
   - `1`, an **exact** binding, followed by the dependency's 32-byte library root;
   - `2`, a **floor** binding, followed by the dependency's 32-byte **tested root**, then
     the **bound version**'s `MINOR` and `PATCH` as numbers. Its `MAJOR`
     is the binding's major, and a bound version never has a prerelease;
5. its **files**: their count, then each file, sorted by path bytewise. A file is its path (a
   text), its module path (the number of components, then each component as a text), and its
   bytes (their length as a number, then the raw bytes);
6. its **foreign modules** ([FFI-2](foreign.md#ffi-2-foreign-modules)): their count, then each.
   One is its module path, encoded as in item 5, its symbol prefix (a text) and its
   containment tag. Tag `0` (native) is followed by its archive rows: their count, then each
   row, as the target (a text), the file's path (a text), the 32-byte pin, and the archive's
   bytes (their length as a number, then the raw bytes). Tag `1` (contained) is followed by its
   module row: the file's path (a text), the 32-byte pin, and the module's bytes (their length
   as a number, then the raw bytes).

Foreign modules are in strictly ascending order of module path, compared component by
component, each component by its UTF-8 bytes bytewise, and a path that is a proper prefix of
another first. The framed bytes are never compared: `aa` comes before `b`, although the frame
of `b` is the smaller byte string. A module's rows are in strictly ascending order of target
bytewise. Bindings and files are likewise strictly ascending, so no two are equal.

`core@1` is bound in every package and is never written as a binding. The toolchain packages
of section 2 are bound with tag `0`. A library root records the major it uses, and the
toolchain that checks it supplies that package. A toolchain binding has no root. Its content
is the toolchain's and enters every key through the compiler and the toolchain identity, as
it does today.

**Aliases.** An alias is `[a-z][a-z0-9_]*`, at most 64 bytes, and never `core`. Sources name a
dependency only by its alias (`json.parse(…)`), so the alias enters `C` and two packages that
both call their dependency `json` can bind two different packages by it. Two aliases of one
package may bind the same identity, and then they name one package: its types are one set of
types whichever alias a source uses, because a definition's identity is its package's name,
never the alias (PROG-1). Bindings of one identity under two aliases carry the same tag, root
and bound version.

**Exact and floor bindings.** A binding records the root its package was checked against. An
exact binding promises nothing beyond that root: a consumer's closure must bind exactly it.
A floor binding also promises that any root of the same identity whose version is at least
the bound version, within the same major, is a valid substitute, and section 5 lets the root
workspace select one. A floor's tested root has the bound version as its own version. `pack`
writes every binding to a published library as an exact binding, because a Git commit, a URL
or an archive is an unlogged source that can call itself any version: nothing but the bytes
the binding pins stands behind it. A floor binding is written only from a release that a
registry has logged under that version ([registry admission](registry.md#2-compiler-admission)); a decoder accepts both
tags from any source, and resolution treats them as section 5 says.

**Inventory.** The files of `C` are exactly the package's `sources` (section 2), with their
modules, and its archives are exactly the rows of its `foreign` tables, for every target.
Nothing else in the package directory enters: not `tests` sources, `program`, `[tool]`,
`hirsel.lock`, any other manifest field, a build or cache directory, or a file the manifest
does not list. A test-only file that joins a library module, as `assembly_laws.hirsel` joins
module `store`, is left out, and the module keeps exactly its `sources` files. The manifest
enters only through items 2 to 6: name and version, charge, bindings with their aliases, the `sources` mapping of files
to modules, and the `foreign` tables, after the workspace's inherited values are expanded
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
effective manifest describes, with every binding a tag 0, an exact root or a floor. It applies the same checks
to every `C`, whether it comes from an archive's head, an archive's other entry or the package
store. It refuses, and its first failure is reported in these words:

| refused | first decoding failure |
|---|---|
| a version other than 3 | `unknown version <n>` |
| a number with a redundant byte, or above its range (a major, minor or patch above 4294967295) | `bad number at byte <offset>` |
| a text that is not UTF-8 | `bad text at byte <offset>` |
| a package name that is not a scoped name (a local or bare name included); a binding's package name that is neither a scoped name nor a bare name the toolchain supplies | `bad package name "<name>"` |
| a prerelease outside the version grammar | `bad version "<version>"` |
| a charge policy other than `charge-v1` | `unknown charge policy "<text>"` |
| an alias outside `[a-z][a-z0-9_]*`, over 64 bytes, or `core` | `bad alias "<alias>"` |
| a binding of `core`; a tag above 2; a tag 0 on anything but a toolchain package at the major the toolchain supplies; a tag 1 or 2 on a bare name; a binding of the package's own identity | `bad binding <alias> = <name>@<major>` |
| two bindings of one identity that differ in tag, root or bound version | `bindings of <name>@<major> differ` |
| bindings, files, foreign modules or rows out of order or repeated | `<part> out of order at <the later item>`, with part `bindings`, `files`, `foreign modules` or `archive rows` |
| no files | `no files` |
| a path or module path that breaks the rules above, including the tree rule | `bad path "<path>": <which rule>` |
| a foreign module that is not formed by exactly one file of item 5 | `foreign module <m> is formed by <k> files` |
| a symbol prefix outside FFI-2's form, or two foreign modules with one prefix | `bad symbol prefix "<prefix>" of <m>` |
| a target that is not 1 to 64 bytes of `[a-z0-9_.-]` | `bad target "<target>" of <m>` |
| an archive whose bytes do not hash to its pin | `archive "<path>" of <m> for <target> does not match its pin` |
| a containment tag above 1 | `bad number at byte <offset>` |
| a contained module whose bytes do not hash to its pin | `module "<path>" of <m> does not match its pin` |
| bytes that end inside an item | `truncated at <offset>` |
| bytes left over at the end | `trailing bytes at <offset>` |
| an encoding over the limits | `over the limit of <limit>` |

A `<name>` is written as in the manifest, a `<version>` as section 1 writes it, a module path `<m>` with `.` between components, and
an offset counts bytes from the start of `C`. So decoding and re-encoding give the same bytes,
two packages with one root have one meaning, and a reader never admits a package that no
manifest could publish. The decoder does not parse an archive's bytes: whether they are an
indexed `ar` archive is FFI-3's check at link time.

**What the compiler reads.** A published package's sources are the file bytes of the `C`
whose root was checked against its pin (section 7), decoded in memory. A member's sources are
its capture. The compiler is handed those bytes, never a path to re-read. The compiler names
the files of the member the command was given by their package-relative paths, as today's
manifest-relative names do (`json.hirsel`). It names every other package's file
`name@major/path` (`acme.dev/json@1/json.hirsel`), in diagnostics, sites and annexes, whether the
package is published or a member. So a file's name is the same whether a dependency is a
member, a coalesced member or a published library, and no name depends on where the package
store or the workspace sits on disk.

## 2. `hirsel.hcfg`

A manifest contains one `pkg_v1.Package` or `pkg_v1.Workspace` record.
The decoder checks every field against the named schema. Source maps pair
each package-relative filename with its module path.

```hcfg
pkg_v1.Package(
    name: "acme.dev/ingest",
    version: "1.4.2",
    charge: ChargeV1,
    sources: [
        ("store.hirsel", "store"),
        ("ingress.hirsel", "ingress"),
        ("wal/append.hirsel", "wal.append"),
    ],
    dependencies: [
        ("std", Toolchain(package: "std", major: 1, authority: [])),
        ("util", Path(package: "local/util", major: 1, path: "../util", authority: [])),
        ("json", Published(
            package: "acme.dev/json",
            major: 1,
            root: "sha256:cf1e…",
            locator: Url("https://example.org/json-1.hpkg"),
            authority: [],
        )),
        ("bjson", Published(
            package: "b.dev/json",
            major: 1,
            root: "sha256:…",
            locator: Archive("vendor/bjson-1.hpkg"),
            authority: [],
        )),
        ("yaml", Published(package: "acme.dev/yaml", major: 2, root: "sha256:…", locator: Store, authority: [])),
        ("http", Published(
            package: "github.com/acme/http",
            major: 1,
            root: "sha256:…",
            locator: Git(url: "https://github.com/acme/http", rev: "<40 hex>", reference: Some("refs/heads/main")),
            authority: [Net, Clock],
        )),
        ("shared", Inherit),
    ],
    program: Some(Program(
        module: Some("main"),
        function: Some("main"),
        limit: Some(67108864),
        grants: Some([Stdout, Clock, Dir("data")]),
    )),
    tests: Some(Tests(
        sources: [("assembly_laws.hirsel", "store"), ("runner_wire.hirsel", "runner_wire")],
        limit: 67108864,
        grants: [
            Clock,
            Random,
            Tasks,
            Net(Listen(cidr: "127.0.0.0/8", first_port: 0, last_port: 65535)),
            Dir("."),
        ],
        runs: [
            ("acme.dev/ingest@1/store/frame_size_boundary", TestRun(limit: Some(134217728))),
        ],
    )),
    foreign: Some([
        ("native", Native(
            symbols: "hsq_",
            archives: [
                ArchiveFile(target: "x86_64-unknown-linux-gnu", file: "native/x86_64/libsq.a", sha256: "…"),
            ],
        )),
    ]),
)
```

Every package is a library. Any package can be bound as a dependency and packed.
A library declares its files in `sources`. A program also has an entry.

**Package identity.** `name` is a scoped, local or bare name (section 1). A bare name is accepted
only in the toolchain's own manifests (below), so a user's package is scoped or local. `version`
is its version (section 1), required, and its `MAJOR` is the identity's major: `name` and
`version` give the identity `name@MAJOR`. There is no `major` key. `charge` states its charge
policy (PROG-1), and the effective charge policy is `ChargeV1`.

The first-party packages that ship with the toolchain sources but are not toolchain packages
(`core/programs/sqlite` and its siblings that section 2's list does not name) are scoped under
`hirsel.ai/` (`hirsel.ai/sqlite@1`), as any other publisher's are.

**`sources`.** Each key is a file and its value is the module the file belongs to, so each
file is one module and several files may form one module. The path rules of section 1 apply
to every key. The listed files are the package's sources in every build. In a published
library they are its whole inventory. A package manifest without `sources`, or with an empty
one, is refused: the directory walk below is only for a directory with no manifest.

**`dependencies`.** Each key is the dependency's **alias** (section 1), the name the
package's sources use for it. Each value has exactly one of these forms. `major` is required
in all but the last. `package` names the bound package in each explicit form. A toolchain
package's alias must be its bare name. So two keys may
bind two packages that share a `NAME` (`json` and `bjson` above), or two majors of one package,
and one package never needs to know what its dependencies' other dependents call them.

- `Toolchain(package, major, authority: [])`, under a key that is a toolchain package's bare name, binds that **toolchain
  package**. The
  installed toolchain supplies `std@1`, `store@1`, `runner@1`, `media_bits@1`, `sframe@1`,
  `media_container@1`, `rtp@1`, `srtp@1`, `dtls@1`, `stun_ice@1`, `turn@1`, `quic@1`,
  `webtransport@1`, `moq@1`, `rtp_jitter@1`, `webrtc_sfu@1`, `media_record@1`, `platform@1`,
  `search@1`, `mcp@1` and `cli@1`: `std` and the inventory of
  `core/programs/toolchain-packages.hcfg`, which is the one list. Each binds only toolchain
  packages: `store@1`, `dtls@1`, `srtp@1`, `rtp@1`, `stun_ice@1`, `media_bits@1`, `quic@1`,
  `mcp@1` and `cli@1` bind `std@1`; `runner@1` and `search@1` bind `std@1` and `store@1`;
  `platform@1` binds `std@1`, `store@1` and `runner@1`; `turn@1` binds `std@1` and
  `stun_ice@1`; `sframe@1` and `media_container@1` bind `std@1` and `media_bits@1`;
  `webtransport@1` binds `std@1` and `quic@1`; `moq@1` binds `std@1`, `quic@1`,
  `webtransport@1` and `sframe@1`; `rtp_jitter@1` binds `std@1`, `media_bits@1` and `rtp@1`;
  `webrtc_sfu@1` binds `std@1`, `store@1`, `runner@1` and the WebRTC packages other than
  `rtp_jitter@1`; `media_record@1` binds `std@1`, `media_bits@1`, `media_container@1`,
  `rtp@1`, `rtp_jitter@1` and `webrtc_sfu@1`. Binding a package brings in the toolchain
  packages it binds. A bare name is a toolchain package's and nothing else's, so a member or
  a published library never shares an identity with one. `core` is bound in every package and
  may not be declared.

  [`cli@1`](../../core/programs/cli/README.md) takes explicit `Stdout`, `Stderr` and
  `Stdin` capabilities for terminal output and prompts. Tables use the width of
  stdout on terminals and escaped TSV on pipes. Writers receive `Closed` when
  the stdout consumer goes away. Progress runs in the caller's work loop and
  uses the `Clock` only for terminal updates on stderr.
- `Path(package, major, path, authority)` binds a member of the same workspace (section 4). The path is
  relative to this manifest's directory and must resolve to a member's directory, whose
  a package record has this name and a version of this major.
- `Published(package, major, root, locator, authority)` binds a published
  library, whose name is never a local name.
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
  section 1, so a link on the way cannot make one spelling name two files. A Phase 1
  `registry` is the canonical HTTPS origin chosen by the user's scope routing configuration.
  The locator cases are `Registry`, `Archive`, `Url`, `Git` and `Store`. Check the registry route before contact. With `Store`, the root must already be in the package store. The source says where content may come from, and the root
  says what it must be: the root enters the package's own `C` and the source never does.
- `Inherit` alone inherits the workspace's declaration of this alias (section 4).

The path and published forms carry `authority`, a list
of capability kinds that bounds what the bound package may reach (section 11). An empty list requires
the package and everything it reaches to be pure. A toolchain binding requires
an empty `authority` list, because the toolchain is the invoker's trust, as `core@1` is. `authority`
never enters `C`: it is the consumer's policy, read only from the root workspace's own
manifests, and a published library's own rows carry none. A `Inherit` row
inherits the workspace row's `authority` with the rest of it (section 4) and adds none.

**`program`.** The entry is the `pub fn` named `function` in module `module` (PROG-3). With
no `program` the entry is `main` in module `main`, by default. A command that needs an entry
refuses a package without that function. A dependency's `program` is never read.

`Program.limit` and `Program.grants` configure execution. Directory grants appear in
parameter order. Relative paths resolve where the executable starts. Build, publish, run
and dev read these fields. Dev fills the first directory slot with its store at launch.
An explicit limit or grant list gives the program ownership of its run configuration.
The CLI refuses competing limit, directory and grant flags. Foreign and Exec approvals
keep their CLI merge rules. A source build, publish or run with `--release`
requires a root limit. Supply `limit` in `program`, or pass `--limit` when the manifest
omits both run configuration fields. A missing limit refuses the
release and names the field. Development builds default to 67,108,864 bytes (64 MiB).
Assembly from `--recipe` uses the limit recorded in the recipe. `build` accepts a recipe
that leaves a `main` parameter ungranted and writes one
``hirsel: warning: grant `NAME` is not in the build's configuration; the executable will not
start`` line per such parameter, in parameter order, whichever source configured it.

**Test configuration.** `Tests.limit` sets the default root limit. The `grants` list includes
directory grants. The `sources` map lists files that join the package's modules for its own
test runs. A test-only file may join a library module or form its own module.

The keys in `runs` are full test definition paths in the package's identity. Each `TestRun`
may override the limit. Omitting `grant_change` inherits all grants. `ReplaceAll` replaces
the list, `ReplaceDirs` replaces only the directories and `ReplaceNonDir` retains the
directories while replacing the other grants. A run naming an unknown test is refused.
A dependency's test configuration does not enter `C`.

**`foreign`.** Each key is a module of `sources` that is a foreign module, with its symbol
prefix and archive table. [FFI-2](foreign.md#ffi-2-foreign-modules) defines the fields and their
refusals. Every row's file is read and captured in resolution step 2, for every target, not
only the build's: a missing, escaping or non-regular archive file is refused like a source
file. A member's archive whose bytes do not match its pin is refused when the member's root is
computed (section 6), and FFI-3 refuses it when a build links it; a check that links nothing
does not refuse it. Every key that depends on a foreign module holds the SHA-256 of the
captured bytes as well as the pin (section 10).

Store declares named workloads in a separate `workloads.hcfg` with the
`workloads_v1.Config` root. Only Store's workload tools read it.

**The directory convention.** A directory without `hirsel.hcfg` is package `local/app@1`,
version `1.0.0`. Each
`.hirsel` file under it is one module, named by its relative path components. It depends on
`core@1` and `std@1`, and its entry is `main` in module `main`, as today. It has no lock, binds
nothing else, and cannot be a member or a dependency. `--inputs FILE` stays the explicit
compiler input for measurements: it names packages, files and dependency identities directly,
binds nothing, and reads no manifest or lock. Its files keep the names the JSON gives them,
which may lie outside the JSON file's directory, and each is read and captured once, as a
member's is, but without the containment rule, because an explicit corpus has no package
directory. Neither the directory convention nor `--inputs` has a lock, so `--locked` and
`--frozen` are refused with either (section 12).

## 3. Finding the workspace

A command that takes a package directory D (the current directory by default) finds its
workspace as follows:

1. The **package manifest** M is the nearest `hirsel.hcfg` at D or above it. With none, D uses
   the directory convention.
2. If M has a workspace record, the workspace root is M's directory. Otherwise the tool looks
   further up for the nearest manifest with a workspace record. If that manifest lists M's
   directory among its members, it is the root. If it does not, the command is refused,
   because a package inside a workspace that does not list it would resolve differently
   depending on where a command runs. If there is no such manifest, M's directory is a
   single-package workspace.
3. If M has a workspace record without a root package, it is a root and not a package. A command that
   needs one package (check, build, publish, run, test, inspect, replay, sdk, pack) is refused there.
   `lock` and `fetch` act on the whole workspace.

The lock is `hirsel.lock` in the workspace root's directory. Directories are compared by the
identity of the directory opened (device and inode), never by the spelling of a path.

## 4. Workspaces

A workspace root's manifest has:

```hcfg
pkg_v1.Workspace(
    members: ["store", "libs/json", "tools/gen"],
    charge: Some(ChargeV1),
    dependencies: Some([
        ("json", Published(
            package: "acme.dev/json",
            major: 1,
            root: "sha256:…",
            locator: Url("https://example.org/json-1.hpkg"),
            authority: [],
        )),
        ("util", Path(package: "local/util", major: 1, path: "libs/util", authority: [])),
    ]),
    selections: Some([
        ("acme.dev/text@2", Selection(root: "sha256:…", locator: Url("https://example.org/text-2.4.1.hpkg"))),
    ]),
)
```

**Members.** Each member is a directory relative to the root, under the section 1 path
component rules (a component may also lack the `.hirsel` ending). A member path never climbs
with `..`. Each member directory holds a manifest with a package record and without a workspace record.
A root manifest that also has a package record is itself a member, and `.` is never listed. No
member is listed twice, and no two members share a `name@major`. Members are listed exactly:
there are no globs, and a directory is never a member because it sits under the root or
because a member names it by path. Each listed member's directory must find this root by step 2
of section 3: the nearest manifest with a workspace record at or above the member's directory, other
than the member's own, is this root's. A member that a nearer workspace root would claim is
refused before inheritance or lock derivation, so one member directory is never expanded under
two roots.

**Inheritance.** A package using `WorkspaceCharge` inherits the workspace's `charge`.
An `Inherit` dependency takes the workspace's declaration under that alias. Workspace
declarations use explicit dependency variants. A path
in it, a `path` or an `archive`, is relative to the root's directory, because the root's
manifest wrote it. A git binding's `dir` is a path inside the commit and is never rebased. An
inherited value is used exactly as written: a member adds nothing to it
and overrides nothing in it. The effective manifest of a member, with every inherited value
expanded, is what the lock and `C` see.

**Edges.** Membership adds no edge. One member uses another only through a `path` binding in
its own `dependencies`, written there or inherited. A module that names a package its own
package does not bind is refused by name resolution as today
([LANG-7](language.md#lang-7-names)), whatever the workspace holds.

**Cycles.** Path bindings among members form no cycle. A published library cannot bind a
member, and its bindings are roots, so published content forms no cycle either.

**Selections.** The workspace root's manifest, and only it, may have `selections`: the root
workspace's choice of one exact root for an identity that the closure binds to more than one
root. A single-package workspace's root is its one manifest. Each key is an identity
`name@major` with a scoped name, and each value is `{ root }` with at most one of `url`,
`archive`, `registry` or `git` (with `rev` and optionally `dir`), under the rules of the published form of
section 2: the source says where the selected content may come from, and the root says what it
must be. A selection has no alias and no `authority`, because it binds no source name: every
edge to that identity, from any package of the closure, is bound to the selected root instead
of its own (section 5). Only `hirsel update`<!-- doc-ref: planned --> writes `selections` (section 12), and a build never adds, drops or changes a row. A member's manifest with
`selections` is refused. A selection of a member's identity is refused, because a member is
already the workspace's choice for its identity.

## 5. Resolution

Resolution turns the members' effective manifests into the workspace's **closure**: every
`name@major` reached from any member, each bound once. It runs in this order, and the first
refusal stops it:

1. **Manifests.** Read, capture and validate every member's manifest and the root's (sections
   2 to 4).
2. **Member files.** Read and capture every member's `sources`, `tests.sources` and
   `foreign` archive files once, every target's, under the reading rule of section 1. These
   bytes are the ones the compiler is handed, the ones a member's root is computed from and
   the ones a build links.
3. **Content.** Acquire the `C` of every published root reached, in rounds, until no new root
   appears. The walk is over the **selected graph**: an edge to an identity that
   `selections` names reaches the selected root, and an edge to an identity a member has
   reaches the member, whatever root the edge itself records. A root that only displaced edges
   record is never acquired, and the bindings of its `C` are never walked, so obligations that
   only a displaced root carried are dropped. A **pending** root is one reached and not yet decoded, and pending roots are taken
   in order of the `name@major` their edges expect, and then of root. In each round:
   1. each pending root whose store entry is present and undamaged is opened from the store;
   2. each pending root left that has a declared source, a `url`, `archive`, `registry` or `git` in a
      declaration that binds it, is fetched from its lock source (section 8), the first such
      declaration in identity order of the declaring packages. Only that source is tried, and
      its failure is the refusal. A registry supplies one frame per root. A bundled source puts every entry of its bundle in the store
      (section 7), so it can satisfy other pending roots;
   3. only when the first two steps have nothing left to do, each pending root left is
      fetched from the declared source of its nearest **carrier**: follow the bindings back
      from the root, through published packages whose `C` binds it and then their binders,
      to the first published root that has a declared source, taking the least path in the
      order of section 5's refusal paths. For a bundled source, that bundle holds its whole closure, so it
      holds the pending root, and it is fetched even when that root itself came from the
      store;
      For a registry carrier fetch the selected child's frame directly, using its own scope
      route for a logged floor, or the carrier for an exact pin (registry §5).
   4. a root still pending is refused as `no source`, the least first.

   Each `C` opened is decoded (section 1) once per root, and its bindings join the walk. Then,
   for **every** incoming published edge, a binding of `name@major` to root P by any package,
   the decoded package at the root the edge reaches, P or the selected root, must be
   `name@major`, whether it came from the store, an archive's head or another of its entries,
   and however many edges reached it first; otherwise the edge is refused as
   `fetched identity`. Under `--frozen` steps 2 and 3 never run: the first pending root that
   the store cannot open is refused.
4. **Binding.** Bind each `name@major` once. Builds never solve: every identity's root is
   fixed by the manifests alone, and this step only checks it.
   - Toolchain packages bind to the toolchain.
   - A member binds to the member. Each published edge to its identity must accept the
     member's root (section 6) as an edge accepts a selected root, below.
   - An identity in `selections` binds to the selected root, and every edge to it must
     accept that root.
   - Any other identity binds to the one root its edges record. When its edges record two or
     more different roots, the closure is refused as a **diamond** (section 9). The refusal
     names every root with its least path, says for each edge whether it is exact or a floor,
     and prints the one command that can repair it, `hirsel update <name@major>`<!-- doc-ref: planned -->,
     when some root satisfies every edge, as below, and otherwise the exact edges that no
     single root satisfies, whose binders must be rebound or republished to agree.

   An edge **accepts** a root Q of its identity when it is an exact edge whose root is Q, or a
   floor edge whose tested root is Q, or a floor edge whose bound version is at most Q's
   version, Q's version has no prerelease, and, when the major is 0, Q's version has the bound
   version's `MINOR`: within major 0 each minor is its own compatibility line. An edge that
   does not accept its bound root is refused (`selection below a floor` or
   `exact edge displaced`, section 9). Equal roots of one identity coalesce: two packages that
   both bind `acme.dev/json@1` to the same root share one package. Different majors are
   different packages and coexist: `acme.dev/json@1` and `acme.dev/json@2` may both be in a
   closure, and so may `a.dev/json@1` and `b.dev/json@1`, whatever their aliases.

   **One root per version.** This check comes first for each identity, before the diamond and
   accept checks. The roots it meets for one identity, every root of an edge to it that step 3
   decoded and the tested root of every floor edge to it, are named by their versions: a
   floor's tested root by its bound version, any other by the version its decoded `C` declares. Two different roots
   under one identity and one version are refused (`two roots for one version`), even when
   every edge would accept the bound one, because two contents that both claim
   `acme.dev/json 1.4.0` leave the version meaning nothing. A displaced exact edge's root is
   never decoded and is not named.
5. **Lock.** Derive the lock (section 8) and compare it with `hirsel.lock`.

**Effective bindings.** Each package of the closure is handed to the compiler with its
**effective bindings**: for each alias in its `C` or its member manifest, the package the
bound identity resolved to in step 4, never the root the binding itself records. A library
whose effective binding differs from the root its `C` records is checked again against the
bound root, its bodies and every key that depends on them included (section 10), so a selected
root that breaks a library's use of it fails that check exactly as a source edit would. The
lock records every such difference (section 8).

**Paths in refusals.** A **top member** is a member that no other member binds. The **path**
to a binding is the shortest chain of bindings from a top member to the package that declares
it, followed by the bound package. Ties are broken by comparing the chains package by package,
each by name bytewise and then by major numerically. A path is written with ` -> ` between
`name@major` identities (`local/app@1 -> local/a@1 -> acme.dev/json@1`).

**Compiler input.** A command on member X hands the compiler X's closure, the packages
reachable from X and nothing else from the workspace, as today's compiler input (the
`CorpusManifest` of `--inputs`). Each package has its identity, its sources (a member's files,
or a published package's verified bytes) and its dependencies: its effective bindings, each an
alias and the identity it names, and `core@1`. Name resolution looks an alias up in the
package's own bindings, so one alias means one package in one package's sources and nothing in
another's.
A package also records whether the toolchain supplied it: an installed toolchain archive, a
member under the invoker's library root `programs/`, or an `--inputs` package marked
`"toolchain": true`. Any other member or published package never is, whatever its name. These
are the invoker's trust, as `core@1` is ([launch](launch.md), the `place` bit).
X's `tests.sources` join X's modules in X's test runs and in the queries about X's tests
(`hirsel inspect --affected-tests`), and in no other build. `hirsel test` runs the tests
declared in X's own `sources` and `tests.sources`, including tests with capability
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
command does, refuses a member with a local name (`pack, local`, section 9), and then checks
the member exactly as `hirsel check DIR` does, dependency authority (section 11) included. A
check that reports any error refuses the pack with the check's own diagnostics, before
anything is encoded, written or put in the store: a published root always names a package
that this toolchain accepted. Then it computes the member's root:

- A binding to another member is encoded as an exact binding to that member's own root,
  computed the same way, recursively. So a member's root is defined whenever its bindings
  resolve, and it changes when any member it reaches changes.
- A binding to a published library is encoded as an exact binding to the root the identity
  resolved to (section 5), which is the root the check above used.
- A binding supplied by the toolchain is encoded as a toolchain binding.

Each binding keeps its alias. Offline `pack` writes exact pins.
Registry publication may encode an authenticated tested dependency as a floor before
computing the member's root ([registry client](registry.md#5-client-selection-and-transport)). The member's
closure must have no displaced edge: when a selection or a member's root displaces the root
that a published package of the member's closure records, `pack` refuses (`pack, displaced
edge`, section 9), because the archive could not hold both the displaced root its edge pins
and the root that was checked, and section 7 accepts only an archive that is complete under
its own edges.

The member's `C` holds its captured `sources` files and `foreign` archives, every target's,
and nothing of `tests`, `program` or `[tool]`. Each archive's captured bytes must match its
pin here (the `member pin` refusal of section 9). `pack` then writes a package archive (section 7) holding
that `C` and the `C` of every package its bindings reach, other than toolchain packages:
members encoded the same way, and published libraries copied from the store. Before writing,
`pack` decodes the archive it produced and verifies every root again as a fetch would, and it
refuses an `--output` that names, by file identity, a manifest, a captured file, the lock or a
store entry. It puts
every package into the package store and prints one line per package of the archive, head
first, each its identity, its version and its root:

```text
fixture.test/json@1 1.0.0 sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7
```

Publishing never edits a manifest; `hirsel add`<!-- doc-ref: planned --> and `hirsel update`<!-- doc-ref: planned --> (section 12) are the
commands that do. Inside the producing workspace the member binding stays a
`path`, and the published root is what that path binding means to anyone else. A workspace
that wants to depend on the published library instead writes `{ package, major, root, archive }` or
`{ package, major, root, url }` with the printed root. A consumer of the archive gets every package its
head's root reaches, each verified against the root that binds it. So the consumer can check
the library's whole transitive meaning without the producer's workspace.

A member and a published root may meet in one closure under one `name@major`. That happens
when a workspace holds library `b` as a member while a published library it uses binds a
publication of `b`. The member binds the identity (section 5), and the published edge must
accept the member's root, computed as above: an exact edge only when it equals the published
root, so the content is identical, and a floor edge also when the member's version satisfies
the floor. A member edited since an exact publication has another root, and the exact edge is
refused as displaced (section 9). The coalesced package keeps each published binding's
obligation: its lock row carries the published root an exact edge pins (section 8), and its
root is in the check environment as a published package's is (section 10). So a later edit
that moves the member and the published pin together, from X to Y, changes the lock, and so
does replacing the root binding with a path.

## 7. Package archives, fetching and the store

**Archive.** A **package archive** is the text `hirsel.package-archive.v2` (framed as a text
of section 1), then the number of packages n, which is at least 1, then n entries, each a
**compressed entry** framed as its byte length (a number) followed by its bytes. A compressed
entry is exactly one Zstandard frame (RFC 8878) whose decompressed bytes are one package's
`C`. Entry 0 is the **head**. The others are in strictly ascending order of root. Together they
are exactly the packages that the head's bindings reach, transitively, other than toolchain
packages. An archive has no identity of its own: it is transport. One library can travel in
archives that differ only in how they reach the consumer or how an encoder compressed them,
and the head's root, over the decompressed `C`, is what a binding pins. An archive is at most
512 MiB as stored, its decompressed entries together are at most 512 MiB, and it holds at most
4096 packages. Its file extension is `.hpkg` by convention, and nothing depends on it.

**The Zstandard frame.** A compressed entry's frame header declares its content size, which is
at most section 1's 64 MiB limit, and a window of at most 8 MiB. It names no dictionary (its
dictionary id is absent or 0). It may carry Zstandard's content checksum, which is then
checked. A frame that breaks one of these rules, a skippable frame, a second frame in one
entry, bytes after the frame, or a frame whose decompressed length differs from its declared
content size is refused as malformed with `bad compressed entry <i>: <which rule>`, before its
`C` is decoded. Decompression stops at the declared size, so no entry can make the reader
allocate more than the limits above. Compression is the encoder's choice: two archives of one
library may differ byte for byte, and both verify to the same roots. `pack` compresses each
entry as one frame at a fixed level; the level is not part of any root.

**Sources.** A source is one of:

- a workspace member's path;
- an `archive` path to a package archive on the local file system;
- a `url`, an `https://` URL fetched by one GET, following redirects only to other `https://`
  URLs, with the response body capped at the archive limit;
- a `git` repository and `rev`, a commit fetched over Git's smart HTTP protocol and packed
  by the tool (GIT-1 to GIT-12);
- a `registry` origin, for an exact package frame at `c/<64hex>.zst` under the
  [Phase 1 wire and trust rules](registry.md);
- the package store, for a root with no locator, or one a bundle brought in.

**Transport.** One command uses one HTTPS client for its URL, git and registry requests:
one connection pool, shared by up to 8 worker threads, so a fetch of several sources reuses
connections to one host and fetches up to 8 sources at once. A `url` body is decompressed and
hashed as it arrives: each entry's frame is decompressed into its `C` and SHA-256 runs over
that `C` as its bytes appear, so a root is known when its entry ends, never after a second
pass over a held copy. Nothing is kept or used until the whole bundle verifies (below), so
streaming changes no outcome. Concurrency changes no outcome either: the pending roots of one
round are fetched together, and their refusals are reported as if the fetches had run one by
one in section 5's order, the least failing source's refusal being the one shown. A fetch that
another refusal made unnecessary may still have put verified entries in the store, which
section 9 allows.

**Bundles.** What an archive, URL or git fetch yields is a **bundle**: a head `C` and the `C` of every package the
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
today's `hirsel/core-packages`. It holds each verified package as the file `xx/<62 hex>`,
where `xx` is the first two of the 64 lower-case hex digits of P and `<62 hex>` the other 62,
and whose bytes are `C`, decompressed. No other name in the store is ever read, so a file of
another form, such as an entry of an earlier encoding version, is never opened. A write goes
to a temporary file in the entry's `xx` directory and is renamed into place, so a reader sees
a whole entry or none. Putting an entry that is already present and undamaged writes nothing:
the store compares the present entry's hash with P and keeps it. Every open
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
`{ package, major, root, git, rev }`, optionally with `dir`, `ref` and `authority`:

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
   `hirsel add` and `hirsel update`<!-- doc-ref: planned --> read it (GIT-11).

The binding's **source spelling**, used by the lock and in diagnostics, is
`git:<git>#<rev>` followed by `/<dir>` when `dir` is present
(`git:https://github.com/o/r#daf4441842dbd47464a183be86f1128a41b2bf26/wrap`). The URL has no
`#`, and `rev` has a fixed length, so the spelling parses one way. A **snapshot path** is the
source spelling without `/<dir>`, then `/`, then a path in the commit's tree; it names a
manifest or file of the commit in diagnostics (`git:https://…#32e2…/wrap/hirsel.hcfg`).

**GIT-2. What a commit yields.** For a git source the tool, in this order:

1. fetches the commit `rev` (GIT-7, GIT-8) and checks the pack (GIT-9);
2. reads the commit's **snapshot**, its tree, as objects in memory under GIT-3. The snapshot
   is never written to a file system;
3. finds the package: its manifest is `<dir>/hirsel.hcfg` in the snapshot, and its workspace
   is found by section 3 with the snapshot's top as the highest directory searched. So a
   a workspace record above `dir` that does not list `dir` refuses, as it would in a checkout, and
   nothing above the top of the tree is ever consulted. A `dir` without a manifest, or whose
   manifest has a workspace record without a root package, is refused. The package record must be
   the binding's `name@major` (the `member identity` refusal of section 9, with the source
   spelling in place of the path);
4. reads the **reached set**: the workspace root's manifest, the package, and every member
   its `path` bindings reach, transitively, each under sections 2 to 4 with snapshot paths in
   place of host paths. Members that the package does not reach are not read beyond the
   root's `members` list, so an unrelated broken member upstream does not stop the
   dependency. The reached members must each find this root (section 4); a `path` that does
   not name a listed member, or that would leave the top of the tree, is refused as
   `path not a member`. The snapshot's `hirsel.lock` is never read, and no `tests`,
   `program` or `[tool]` entry of the snapshot is read beyond the manifest's validation;
5. packs: computes the package's root and each reached member's root by section 6, with each
   member binding encoded as an exact binding to that member's root and each published binding
   by its root, a reached member with a local name refusing as `pack, local` does, and
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
   containing directory is the package's directory for its sources, its `foreign` archives
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
effective manifest in the snapshot, the snapshot root's dependency map
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
a manifest, a lock or a build. Only `hirsel update`<!-- doc-ref: planned --> takes a new commit for a bound
reference (section 12). The **same source** is the pair (`git`, `dir`). `hirsel update`<!-- doc-ref: planned --> resolves
the bound `ref` again; when it now names another commit, update packs that commit and, if the
result checks (section 12), rebinds the row and says what it saw: a tag `moved`, which a tag
should never do, and a branch `now names another commit`. The fetch has depth 1 and no
ancestry, so the tool cannot tell a branch that went forward from one rolled back to an old
commit, and it never says which. Each prints the bound and the fetched commit and root. A row
with no `ref` names a commit and nothing moves it. `hirsel add` of the same source with the
same `REF` refuses when the reference has moved, prints both commits and roots, and names the
`hirsel update <alias>`<!-- doc-ref: planned --> command that takes it; `add` never rebinds a moved reference. A
different `REF`, or a commit id, for the same source given to `add` is a named update, as a new
URL is: it replaces the binding and prints both commits and roots. A source bound to the same
`rev` and root is `unchanged`.

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
- `version`, the package's version (section 1), for every row but a toolchain row: a published
  package's from its decoded `C`, a member's from its manifest;
- `root`, for a published library, the root its identity resolved to (section 5), and for a
  member that coalesced with an exact published binding (section 6): the published root;
- `source`, which is one of:
  - `toolchain`;
  - `member:<dir>`, the member's directory relative to the root, or `.` for the root;
  - `archive:<path>`, relative to the root;
  - `url:<url>`;
  - `registry:<origin>`, for an explicitly routed registry declaration;
  - `git:<git>#<rev>` or `git:<git>#<rev>/<dir>`, the source spelling of GIT-1;
  - `store`;
  - `via:<id>`;
- `edges`, the row's bindings as `name@major` texts, sorted as the rows are. An alias never
  appears: two aliases of one identity are one edge;
- `tested`, only on a published row whose `C` records, for some edge, a root other than the
  one the edge's identity resolved to: one text per such edge, `<name@major> <tested root>`,
  sorted as the rows are. So the lock shows every library that is built against something other
  than what it was published against, and a change of selection changes the lock.

A published row's source comes from the manifests and the `C`s alone, never from what the
store happened to hold or which archive was opened. It is the first `url`, `archive`, `registry` or `git`
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
version = 2

[[package]]
id = "fixture.test/json@1"
version = "1.0.0"
root = "sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7"
source = "archive:vendor/jx.hpkg"
edges = []

[[package]]
id = "local/app@1"
version = "1.0.0"
source = "member:."
edges = ["fixture.test/json@1", "std@1"]

[[package]]
id = "std@1"
source = "toolchain"
edges = []
```

The lock holds the comment line, `version = 2`, then one blank line before each row. A row's
keys are always in the order `id`, `version`, `root`, `source`, `edges`, `tested`, a row
without `tested` has none (never an empty list), and the file ends with one line feed. A TOML string is a basic string. In it `"` is written `\"`, `\` is written `\\`, each of
U+0000 to U+001F and U+007F is written `\u` and four upper-case hex digits (`\u001B`), and
every other character is written as itself, with no other escape. Lines end with a line feed
alone.

**Staleness.** A lock is **stale** when its bytes differ from the derived lock. The tool then
parses the old lock and reports what changed, one line per difference, rows in order and, within
a row, `version`, `root`, `source`, `edges`, then `tested`:

```text
  acme.dev/json@1: version 1.4.0 -> 1.4.2
  acme.dev/json@1: root sha256:<old> -> sha256:<new>
  acme.dev/json@1: source archive:vendor/jx.hpkg -> archive:vendor/jy.hpkg
  acme.dev/json@1: edges [] -> ["local/util@1"]
  acme.dev/reader@1: tested [] -> ["acme.dev/json@1 sha256:<old>"]
  local/lib@1: root none -> sha256:<new>
  local/util@1: added
  acme.dev/yaml@2: removed
```

A row that gains or loses its `root`, as when a member starts or stops coalescing with a
published binding, writes the missing side as `none`.
An old lock that does not parse as a version 2 lock, or whose bytes differ while its parsed
rows are equal, is reported as one line, `  hirsel.lock is not in the form this hirsel
writes`.

**Who writes it.** Every command that resolves a workspace (check, build, publish, run, test,
inspect, replay, sdk, pack, fetch, lock and audit) derives the lock; `add` and `update` derive it from the manifests they commit (section 12). Without `--locked` or `--frozen`, a command
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
`hirsel tape replay PACKAGE` and `hirsel sdk` given a package directory resolve through this chapter
like `check`, honour the flags and the package store, and `sdk` passes them on to the build it
runs.

**`--locked`.** Any of those commands refuses a missing or stale lock and writes nothing. It
may still fetch pinned content that the store lacks, because a fetch only fills the store with
verified bytes the manifests already pin.

**`--frozen`.** This is `--locked`, and it also refuses to fetch. Every published root of the
closure must already be in the store, undamaged. The refusal comes in resolution step 3,
before any source is touched, so a `--frozen` command never opens a URL, an archive path or a
git repository.

A `--frozen` request also makes no network connection and no name lookup at all, for any
purpose. Each request carries one **transport permission**, decided once when its flags are
parsed and before any other work: a `--frozen` request has none, and every other request has
one. Every path that can reach the network takes the permission from the request it serves
and does nothing without it: package and git fetches, remote cache reads, offers and
background uploads ([remote cache](cli.md#remote-cache)), and a resident compiler's work on
the request's behalf ([`hirsel serve`](cli.md#resident-compiler)). There is no
process-wide switch: a resident compiler serving a `--frozen` request and an ordinary one at
once gives each its own permission, and work it does for itself (no request) never uses the
network on a `--frozen` request's behalf. So a `--frozen` build makes zero `connect` calls and
zero DNS queries, one-shot or resident, with a remote cache configured or not.

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
| unknown field | `hirsel: <manifest>: Invalid("unknown field") at <schema path> (bytes <start>..<end>)` |
| bad field | `hirsel: <manifest>: <dotted.key>: <what is wrong>`, for example `package.name: "Acme.dev/json" is not a scoped or local name`, `package.version: "1.04.0" is not MAJOR.MINOR.PATCH[-PRE]` |
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
| two dependency forms | `hirsel: <manifest>: dependencies.<alias>: give exactly one of path, root, workspace or a toolchain major` |
| no package | `hirsel: <manifest>: dependencies.<alias>: "<alias>" is not a toolchain package; give package = "<scoped name>"`, or `a toolchain binding takes no package` |
| local published | `hirsel: <manifest>: dependencies.<alias>: <name> is a local name, which only a workspace member has` |
| bad authority | `hirsel: <manifest>: dependencies.<alias>.authority: "<kind>" is not a capability kind`, or `names <kind> twice`, or `a toolchain binding takes no authority` |
| bad selection | `hirsel: <manifest>: selections: <what is wrong>`: `only the workspace root's manifest selects`, `"<id>" is not a scoped identity`, `<id> is a member of this workspace`, or a published-form refusal of this table in its words |
| two sources | `hirsel: <manifest>: dependencies.<alias>: give at most one of url, archive, git or registry` |
| git fields | `hirsel: <manifest>: dependencies.<alias>: <field> needs git`, for `rev`, `ref` or `dir` without `git`, and `git needs rev` |
| bad git field | `hirsel: <manifest>: dependencies.<alias>.<field>: <what is wrong>`: for `git`, `"<url>" is not an https:// URL`, `has user information`, `has a query` or `has a fragment`; for `rev`, `"<r>" is not a full commit id of 40 or 64 lower-case hex digits`; for `ref`, `"<r>" is not refs/heads/<name> or refs/tags/<name>`; for `dir`, `"<d>" breaks the path rules: <which rule>` |
| git package missing | `hirsel: <manifest>: dependencies.<alias>: <source> has no hirsel.hcfg`, or `<source> is a workspace root, not a package` |
| reaches submodule | `hirsel: <snapshot manifest>: sources: "<path>" reaches submodule "<entry>", and hirsel does not fetch submodules` (and the `archive file` and member forms) |
| bad pack | `hirsel: <id>: <source> sent a bad pack: <first failure>`, the failure one of `bad header`, `checksum mismatch`, `truncated at <offset>`, `trailing bytes at <offset>`, `object <oid> is a SHA-1 collision attack`, `delta base <oid> is not in the pack`, `delta base at <offset> is not an earlier entry`, `commit <rev> is not in the pack`, `object <oid> is not in the pack`, `object <oid> is a <type>, not a <type>`, `object <oid> is not a well-formed <type>`, `tree <oid> has mode <mode>`, `tree <oid> names "<entry>" twice`, `object at <offset> inflates to <n> bytes, not <m>`, `over the limit of <limit>` with `<limit>` one of `512 MiB object`, `4194304 objects`, `4 GiB inflated` or `4095 delta links` |
| git protocol | `hirsel: <id>: fetching <source> failed: <cause>`, with the causes of GIT-7 and GIT-8 in their words: `the server does not speak Git protocol version 2`, `the server's object format is <f>, not <g>`, `the server would not send commit <rev>`, `redirected to <url>, which is not https`, `redirected to <url>, which is not a Git endpoint`, `too many redirects`, `the server answered <status>` (any status other than 200 on a request, 401 and 403 aside), `the repository does not exist or requires authentication; hirsel fetches public repositories only`, `the server answered with Content-Encoding <coding>`, `the server sent an unexpected <name> section`, `the server sent nothing for 60 s`, `the response is over the limit of <limit>` with `<limit>` `64 KiB`, `16 MiB`, or `512 MiB; hirsel fetches the whole commit, so a repository whose commit is larger cannot be a git source` (GIT-7 step 2), `over the limit of 1 GiB received` (GIT-6), `the server said: <text>` |
| toolchain major | `hirsel: <manifest>: dependencies.std: the toolchain has std@1, not std@<n>` |
| declared core | `hirsel: <manifest>: dependencies.core: core@1 is bound in every package and is not declared` |
| path not a member | `hirsel: <manifest>: dependencies.<alias>: <path> is not a member of the workspace at <root manifest>` |
| member identity | `hirsel: <manifest>: dependencies.<alias>: <path> is <other>@<m>, not <package>@<major>` |
| not listed | `hirsel: <manifest> is inside the workspace at <root manifest> but is not one of its members` |
| root is no package | `hirsel: <root manifest> is a workspace root, not a package; name a member directory` |
| member cycle | `hirsel: the members' path bindings form a cycle: local/a@1 -> local/b@1 -> local/a@1`, starting at the smallest identity on the cycle |
| inherit missing | `hirsel: <manifest>: dependencies.<alias>: the workspace declares no <alias>` |
| no source | `hirsel: <id> sha256:<P> is not in the package store and its binding names no source` |
| archive outside | `hirsel: <manifest>: dependencies.<alias>: archive "<path>" is not inside the workspace root. To share an archive across packages, add a parent hirsel.hcfg with a pkg_v1.Workspace and members listing the package directories, then use a relative path within that workspace` |
| archive incomplete | `hirsel: <id>: <source> lacks sha256:<P>, which <binder> binds as <name>@<major>`, the least such edge |
| archive extra | `hirsel: <id>: <source> holds sha256:<Q>, which nothing in it binds`, the least such root |
| fetch mismatch | `hirsel: <id>: <source> holds sha256:<Q>, but the binding pins sha256:<P>` |
| fetched identity | `hirsel: <id>: sha256:<P> is <other>@<m>`, for every edge whatever the content's origin |
| malformed | `hirsel: <id>: <source> is not a package archive: <first decoding failure>`, the failure in the words of section 1, and `<source>` the archive or url the bytes came from |
| fetch failed | `hirsel: <id>: fetching <url> failed: <cause>` |
| fetch count | `hirsel: <id>: <source> causes more than 256 fetches` (GIT-6) |
| diamond | `hirsel: <id> is bound to more than one root:` then one line per edge, in path order, each `  <path> = sha256:<root> exact` or `  <path> = sha256:<root> floor <bound version>`, then either `to choose one root for every edge, run: hirsel update <id>` when some root of these edges is accepted by every edge (section 5), or `no single root satisfies every edge; the exact edges of <binder>, <binder> must name one root`, naming each binder of an exact edge, in path order |
| exact edge displaced | `hirsel: <id> is bound to sha256:<Q> (<by>), but <path> pins sha256:<P> exactly`, with `<by>` `selected in <root manifest>` or `the member <dir>` |
| selection below a floor | `hirsel: <id> is bound to <version> sha256:<Q> (<by>), but <path> requires at least <bound version>`, and for major 0 `requires <bound version> or a later 0.<minor>.x` |
| two roots for one version | `hirsel: <id> <version> is two roots:` then `  <path> = sha256:<A>` and `  <path> = sha256:<B>`, in path order |
| missing lock | `hirsel: hirsel.lock is missing under --locked; run hirsel lock` (or `--frozen`) |
| stale lock | `hirsel: hirsel.lock is stale under --locked:` (or `--frozen`) then the change lines of section 8 |
| frozen, absent | `hirsel: <id> sha256:<P> is not in the package store, and --frozen does not fetch (source <source>)` |
| frozen, damaged | `hirsel: <id> sha256:<P> is damaged in the package store, and --frozen does not fetch (source <source>)` |
| bad compressed entry | `hirsel: <id>: <source> is not a package archive: bad compressed entry <i>: <rule>`, the rule one of `not one Zstandard frame`, `no content size`, `content size <n> over 64 MiB`, `window <n> over 8 MiB`, `needs a dictionary`, `checksum mismatch`, `inflates to <n> bytes, not <m>`, `entries inflate past 512 MiB` (section 7) |
| pack, local | `hirsel: <manifest>: <name> is a local name and is never packed; give it a scoped name to publish it` |
| pack, displaced edge | `hirsel: pack: <binder> binds <id> sha256:<P> exactly, but this workspace builds it with sha256:<Q>; republish <binder> against sha256:<Q> or select sha256:<P>` |
| pack, check failed | the diagnostics of `hirsel check` for the member, then `hirsel: pack: <manifest> does not check; nothing was written` |
| authority exceeded | `hirsel: <manifest>: dependencies.<alias>: <id> reaches <Kind>[, <Kind>…], which authority = [<kinds>] does not grant`, then one line per extra kind, `  <Kind>: <export> -> … -> <operation>`, the least witness path in definition order (section 11) |
| authority unresolved | `hirsel: <manifest>: dependencies.<alias>: <id>: what <definition> reaches cannot be resolved: <cause>`, and an absent `authority` counts as the empty list (section 11) |

`hirsel: <root manifest>: selections.<id> selects sha256:<P>, which no edge of the closure names; hirsel update removes it` is a warning on stderr, not a refusal; the build goes on without the row.

Manifest refusals follow workspace member order. Unknown labels in the same record are
reported in key order using their byte intervals. Diagnostic paths use schema field names. The first refusal of a later resolution step is the only one shown.

Text that comes from a server (an `ERR` packet, band 3, a status line) or from a tree (an
entry name, a mode) is written with the lock's string escapes (section 8) and cut at
256 bytes, so no byte a server chooses reaches the terminal as a control sequence.

`hirsel add`<!-- doc-ref: planned --> (section 12) adds these refusals, each exit 2 with nothing written (the store may
hold verified entries):

| case | diagnostic |
|---|---|
| add, locked | `hirsel: <manifest> needs to be updated but --locked was passed` (or `--frozen`: the flag actually given) |
| add, root differs | `hirsel: <id>: fetched root differs from --root`<!-- doc-ref: planned -->, then `  fetched: sha256:<Q>`, `  expected: sha256:<P>` and `  source: <url or archive>` |
| add, source rebound | `hirsel: <source> is already bound to another root`, then `  bound: sha256:<P>`, `  fetched: sha256:<Q>` and `to take the fetched root, run: hirsel update <alias>`<!-- doc-ref: planned --> |
| add, manifest changed | `hirsel: <manifest> changed while hirsel add ran; nothing was written`<!-- doc-ref: planned --> |
| add, local and workspace | `hirsel: <manifest>: dependencies.<alias> is bound here to another package, root or source than the workspace binding; remove one of them first`, then both bindings |
| add, inherited | `hirsel: <manifest>: dependencies.<alias> inherits the workspace binding; pass --workspace to change it for every member that inherits it, or remove { workspace = true } here first`, for a plain `add` of another source or root |
| add, alias taken | `hirsel: <manifest>: dependencies.<alias> already binds <id>; pass --as to choose another alias`<!-- doc-ref: planned --> |
| add, root absent | `hirsel: sha256:<P> is not in the package store` |
| add, toolchain | `hirsel: the toolchain does not provide <id>` |
| add, local | `hirsel: <id> has a local name; only a workspace member can have one` |
| add, not a member | `hirsel: <dir> is not a member of the workspace at <root manifest>` |
| add, bad git source | `hirsel: <SOURCE> is not git+https://HOST/PATH[?dir=DIR][#REF] or github:OWNER/REPO[/DIR][@REF]: <which part>`, for a scheme after `git+` other than `https`, a query other than one `dir=`, a `dir` or `DIR` that breaks the path rules, user information, an `OWNER` that is not `[A-Za-z0-9-]+`, a `REPO` that is not `[A-Za-z0-9._-]+` or is `.` or `..`, a `HOST` that is not lower case, or an empty `REF` |
| add, no such ref | `hirsel: <git> has no branch or tag <REF>` |
| add, ambiguous ref | `hirsel: <git>: <REF> names both refs/tags/<REF> and refs/heads/<REF>; write the full name` |
| add, not a commit | `hirsel: <git>: <REF> names a <type>, not a commit` |
| add, no HEAD branch | `hirsel: <git>: HEAD names no branch; give a branch, a tag or a commit` |
| add, ref moved | `hirsel: <git> <ref> moved:` (a tag) or `hirsel: <git> <ref> now names another commit:` (a branch), then `  bound: <rev> sha256:<P>`, `  fetched: <rev> sha256:<Q>` and `to take it, run: hirsel update <alias>`<!-- doc-ref: planned --> (GIT-11) |
| add, virtual root | `hirsel: <root manifest>: no package manifest here; pass --workspace to declare it for the workspace`<!-- doc-ref: planned --> |
| add, no workspace | `hirsel: <manifest>: no workspace record; hirsel add does not create a workspace boundary`<!-- doc-ref: planned --> |
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
the compiler identity, `core`, and each package's namespace: its identity, its version, and
its effective bindings (section 5), each alias with the identity it binds. It gains each
published package's library root, a coalesced member's included (section 6), and each
package's foreign stamp (below). So a record checked against one
pin of `acme.dev/json@1` is never found under another, a record checked under a floor's
tested root is never found under the root a selection raised it to, even for a body whose own bytes and named
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
package with no foreign module has the stamp of count zero, so removing a module's `foreign`
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
member's closure (`acme.dev/json@1/json/version`), or a package identity alone
(`acme.dev/json@1`). The path splits at the first `@`, and the identity's name may hold `/`
and `.` (section 1). A test's
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
addresses, and each linked archive by its digest. A dependency's file names in annexes are
`name@major/path`, such as `acme.dev/json@1/json.hirsel` (section 1), whether the dependency is published, a member or a coalesced member, so R never
depends on where the package store or the workspace is. Two builds of one program over one
closure, with the same target, backend, grants, root limit and place count, give one R, on
any machine and with any store path, and whether a dependency's equal content comes from a
member or from a published root.

**Images.** A package image is keyed by each package's **image stamp**, and its file table
is built from the same capture as the stamp, so the two cannot disagree. The stamp of a
package is its identity, its version, its effective bindings (each alias, and a published
binding by its root), its foreign stamp,
and each file in the order of item 5: its logical name (section 1), its module path, its
parse mode (source or test) and its bytes. A published package's files are its verified
bytes. So renaming a file, even with its module and bytes unchanged, changes the stamp, and
an image built before the rename is never used after it. A producer that misses the image
takes the names from the capture and never synthesises one, so an image's site names, and the
R built from it, equal those of a build from fresh source.

## 11. Dependency authority

A dependency row's `authority = [...]` (section 2) is a ceiling on what the bound package's
code may reach. An absent list is the empty list: the bound package is **pure**, so it reaches
no capability kind. The ceiling is checked by the compiler from the checked program, on every
command that lowers CoreIR, `check` and `pack` included, and on a reused image or record as on
a fresh build. It is not a sandbox and not a grant: a run's capabilities still come only from
its launch grants and its parameters, and a package that passes this check gains nothing. It
answers a reviewer's question before anything runs: which kinds of effect can this
dependency's own code perform, whoever passes it the capability.

**Kinds.** The launch grant kinds, without parameters, are `Dir`, `Net`,
`Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`, `Env`, `Process`, `Tasks`, `Args`, `Secret`,
`Foreign`, `Exec` and `Cred`. Phase 1 adds `Compiler` for the explicit PACK-3 capability; its
check operation charges that kind, including when called through a library. A list names each kind at most once, in any order; any other
string is refused (`bad authority`, section 9). A list never narrows a parameter: `Dir` covers
every directory the code is given, and `Foreign` every foreign interface.

**Operations.** Every provider operation of CoreIR (`HostOp`) has a fixed set of zero or more kinds,
fixed by the toolchain in one total table with no default case, so adding an operation without
deciding its kind does not compile. An operation reaches the grant kinds of the capabilities
its authority operands exercise (for example, signing with a secret and a random provider reaches
both `Secret` and `Random`): an operation on a `Dir`, or on a file, entry list or lock
opened from one, is `Dir`; on a `Net`, or a socket, listener, datagram, UDP or TLS stream or
resolver made from one, `Net`; a clock reading or wait, `Clock`; filling bytes from `Random`,
`Random`; writing standard output or error, `Stdout` or `Stderr`; reading standard input,
`Stdin`; reading or narrowing the environment, `Env`; a process stop or trigger, `Process`; a
secret's sealing, keyed hash, derivation, signature or exchange, `Secret`; a credential fetch,
`Cred`; and spawning, waiting on, signalling or narrowing a child process, `Exec`.
Optional `Until` deadlines on another operation do not add `Clock`; only explicit clock
operations and `ProvideClock` count as `Clock`. A virtual
provider's constructor (`Provide*`) counts as the kind it provides, though it only stands in
for one, so a provider package is listed as reaching what it can impersonate; the card says
which of its kinds are provided (below). An operation on a cell, port or call that the program
made itself with such a constructor has no kind, nor has an operation that takes no
capability, such as sealing a packet with a key the program holds. Reaching a native foreign
function, one with no body in its package (FFI-2), is `Foreign`. A contained module's function
(FFI-10) reaches no kind; it stays in the card as a note.

**Reach.** For a package B, `reached(B)` is the set of kinds of the operations in the instances
that B's instances reach, where reaching follows each direct call and each reference to a
function, from every instance whose defining package is in B's closure, and stops at an
instance defined outside it. An instance's defining package is the package of its definition
path (`Identity.definition`), not the package that asked for it, so:

- a closure, or a trait implementation, that a caller passes into B is defined in the caller's
  package and charges the caller, not B, even when B calls it;
- a generic function of B instantiated by a caller is B's instance, and its operations charge
  B; when the analysis cannot tell which package chose a type argument, it charges the generic
  function's own package, which can only over-count;
- a capability handle that B's code uses through `std` charges B, because `std` is in every
  closure: `std`'s file operations are in `reached(B)` exactly when B's code calls them;
- a call through a function value adds no edge, because the value's definition is charged where
  it was written; a call through the runtime (`Callee::Runtime`) adds the runtime function's
  own operations.

A selected member's declarations in `tests.sources` do not serve as roots for its consumers:
those files are absent from the bound package. Calls from tests still materialize ordinary
exports and their generic instances, whose operations charge their defining packages.

The analysis runs over the whole checked program at once and has no step budget. A definition
whose reach it cannot resolve, for any cause, makes `reached` of every package reaching it
**unresolved**, and unresolved is refused under every list (`authority unresolved`, section 9);
there is no wildcard kind that admits it.

**The check.** For every row R of the workspace that binds a package B, whether a member's,
a published or a git row, `reached(B)` must be a subset of R's list. The row's own list is the
only ceiling, so a member that binds B directly can narrow what a deeper binding of B allows,
and no row widens another. A refusal names the row, B, each extra kind and its least witness
path from an export of B to the operation (`authority exceeded`, section 9). A toolchain
binding has no list and is not checked: the toolchain is trusted as a whole. `core` and `std`
are in every closure and are not rows.

A row's list is not part of `C` (section 1), so a published package's root says nothing about
any consumer's ceiling, and changing a list changes no root and no key of section 10. The
check's result is recomputed from the checked program, which those keys already cover.

**The card.** `hirsel audit`<!-- doc-ref: planned --> prints one card per bound package, and
`hirsel update`<!-- doc-ref: planned --> prints the change of each card it causes (section 12).
A card holds three facts:

1. **Carried authority**: each export of B whose parameters or results hold a capability, a
   handle or a function value, with the kinds of the capabilities it takes. This is authority
   a caller hands B on purpose.
2. **Reached kinds**: `reached(B)` per export, with the provided kinds marked, and the contained
   modules B reaches.
3. **Verdict**: `pure` when `reached(B)` is empty and no export carries authority;
   `conditional` when `reached(B)` is empty but some export takes a capability or a function
   value, so what happens depends on what the caller passes; `unknown` when reach is
   unresolved. Otherwise the verdict is the list of reached kinds.

The **per-export diff** compares two cards export by export: an export that appears or goes, a
kind that appears or goes in its reached kinds or carried authority, and a changed verdict.
`hirsel update`<!-- doc-ref: planned --> prints it for each package whose root it changes, before it writes anything,
and a diff that adds a kind outside the row's list is the `authority exceeded` refusal, so an
update never widens authority silently. `hirsel update`<!-- doc-ref: planned --> never edits a list; widening one is a
manifest edit the user makes.

## 12. Commands

| command | does |
|---|---|
| `hirsel lock [DIR]` | resolves the workspace and writes the lock |
| `hirsel fetch [DIR]` | resolves the workspace and fills the store with every published root of the closure, building nothing |
| `hirsel pack [DIR] --output FILE` | makes the checked offline archive of the member at DIR (section 6) |
| `hirsel registry publish [--workspace]`<!-- doc-ref: planned --> | uploads a checked member, or all members dependency-first, to their routed registries ([registry §5](registry.md#5-client-selection-and-transport)) |
| `hirsel inspect DIR\|ARCHIVE.hpkg\|DEPENDENCY` | prints the selected package's public interface by default |
| `hirsel new NAME` | writes `NAME/hirsel.hcfg`, its sources, its tests and its first lock |
| `hirsel add SOURCE` | binds one dependency in a manifest and updates the lock |
| `hirsel update [ALIAS\|ID[@VERSION]]`<!-- doc-ref: planned --> | re-reads sources and selects one root per identity (below) |
| `hirsel audit [DIR]`<!-- doc-ref: planned --> | prints each bound package's binding, tested root and card (section 11) |

Inspect a dependency by its alias from the package that declares it.
An archive supplies its head package and verified dependency closure.

`--locked`, `--frozen` and `--package-store DIR` apply to `check`, `build`, `publish`, `run`,
`test`, `inspect`, `replay`, `sdk`, `lock`, `fetch`, `pack` and `audit`, when the command is given a
package directory; section 8's input modes say what each does without one. `hirsel lock --locked` checks the lock without
writing it, which is the CI freshness check. `hirsel fetch --locked` followed by a later
`--frozen` build over the same store is the offline build. `hirsel build --emit recipe --code-store DIR` keeps its meaning:
it prints an executable's R ([launch](launch.md#release-recipe-and-host-assembly)), and a
library can be packed offline or published to its scope registry.

### Adding a dependency

`hirsel add` edits a manifest and updates its lock. It accepts HTTPS URLs, git sources
(`git+https://` and `github:`), `.hpkg` paths, package-store roots, toolchain names and member
directories and Phase 1 scoped registry names, in a member's own table or, with `--workspace`, in the workspace's. `github:`
names a repository commit, never a release asset (below).

`hirsel add SOURCE [--as ALIAS] [--root sha256:HEX] [--workspace] [--plan] [--package-store DIR] [--json]`<!-- doc-ref: planned -->

| `SOURCE` | Fetches | Writes |
|---|---|---|
| `https://…` | one GET, `https` redirects only | `alias = { package, major, root, url = "<as typed>" }` |
| `git+https://HOST/PATH[?dir=DIR][#REF]` | one reference listing unless `REF` is a commit id (GIT-10), then one pack fetch (GIT-7) | `alias = { package, major, root, git = "https://HOST/PATH", rev, ref, dir }`, with `ref` absent for a commit id and `dir` absent without `?dir=` |
| `github:OWNER/REPO[/DIR][@REF]` | exactly what `git+https://github.com/OWNER/REPO?dir=DIR#REF` fetches | the same, with `git = "https://github.com/OWNER/REPO"` |
| a path ending `.hpkg` | a read confined to its directory descriptor | `alias = { package, major, root, archive = "<relative to the declaring manifest>" }` |
| `sha256:HEX` | nothing; the root must be in the package store | `alias = { package, major, root }` |
| a scoped name with optional `@MAJOR` or exact `@VERSION` | verifies the routed registry's release inclusion and full status replay, then fetches selected frames | `alias = { package, major, root, registry = "<origin>" }` |
| `NAME@MAJOR`, a bare toolchain name | nothing; the toolchain must provide it | `name = { major }` |
| a directory holding `hirsel.hcfg` | nothing; it must be a listed member | `alias = { package, major, path = "<relative to the declaring manifest>" }` |

The package and major come from the fetched head, or the member's manifest. The alias is the
last component of the package name (`json` for `acme.dev/json`) unless `--as ALIAS`<!-- doc-ref: planned -->
gives another, which must be a module path component; an alias the manifest already uses for
another identity is refused (`add, alias taken`), so two packages named `json` from two hosts
are bound side by side by giving one of them `--as`<!-- doc-ref: planned -->. A head with a local name is refused
(`add, local`). `--root`<!-- doc-ref: planned --> names the head
root the archive must have (`Root::of` its head bytes, section 1), or the root the commit
must pack to, and applies only to the URL, archive, `git+https://` and `github:` forms: it is
a second witness for first use. `--plan`<!-- doc-ref: planned --> does
everything before publication; verified archives may still enter the store. `--json` prints one
object. `--locked` and `--frozen` refuse.

**Which manifest.** The nearest `hirsel.hcfg` with a package record, walking up from the working
directory, or the workspace root when it has none (then `--workspace`<!-- doc-ref: planned --> is required).

For a registry name, the source key is (routed origin, identity). In the transaction below
replace bundle acquisition with the [registry selection](registry.md#5-client-selection-and-transport)
and verified frame walk. Check the edited workspace on the overlay before any manifest
rename. A version inside the default 72-hour wait requires its exact SemVer.

**Bindings already present.** `add` binds what it is given and never moves a binding it was
not given. The same URL or archive already bound to another root is refused, printing both
roots: other bytes behind one locator are a moved tag or tampering, and taking them is
`hirsel update`<!-- doc-ref: planned -->'s job (below), which shows the change before it writes. The same git
source, the pair (`git`, `dir`), resolving to another commit is refused the same way and says
whether a tag moved or a branch now names another commit (GIT-11). Another `REF` or a commit
id for the same pair, or a different source for an alias already bound, replaces the binding
and prints the old and new roots: the user named the new source.

**Paths and placement.** Every `path` and `archive` is written relative to the manifest that
declares it (section 2). A member at `apps/ui` adding `vendor/pkg.hpkg` at the workspace root
writes `../../vendor/pkg.hpkg` in its own manifest, or `vendor/pkg.hpkg` in the root's
dependency map.

| Context | Without `--workspace`<!-- doc-ref: planned --> | With it |
|---|---|---|
| listed member | the member's `dependencies` | the workspace dependency map, then the member's `Inherit`: two operations |
| root that is also a package | its `dependencies` | both tables in the one root manifest: one operation |
| virtual root | refused | declaration only: no member gains an edge, so the lock may stay identical (section 4) |
| package with no workspace record above | its `dependencies` | refused |
| directory not listed, or nested under another root | refused by discovery | refused |

A member that already binds the alias to the same package, root and source and asks for
`--workspace`<!-- doc-ref: planned --> has its local binding replaced with `Inherit`;
one that binds it differently is refused naming both bindings (`add, local and workspace`).
`--workspace` changes a workspace dependency entry for every member that inherits it, and
prints those members. A plain `add` at a member that inherits the alias is `unchanged` when
the workspace already binds that root and source, and otherwise refuses (`add, inherited`), so
a member never drifts from the shared binding by accident.

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
   fetched name: the same source bound to another root or commit refuses and names
   `hirsel update`<!-- doc-ref: planned --> (GIT-11).
6. Admit those same bytes to the store (section 7, including its I/O case).
7. Build the edited manifests in memory and resolve the workspace through the real resolver on
   that overlay. Every refusal of section 5 refuses here, with nothing written. Roots the
   resolution needs that the store lacks are fetched under `fetch`'s rules and listed as
   `fetched`, never silently, as is every source a git source's snapshot made the tool
   contact (GIT-6). A displaced exact edge or a floor the new root does not meet refuses
   here as the build would (section 9); `add` never writes `selections`. If every target
   manifest already has the same alias, package, root and source, the result is `unchanged`: derive and commit the lock if it is missing or stale, and
   exit 0. `--plan`<!-- doc-ref: planned --> prints and exits 0 here.
8. Capture each target manifest again; bytes or file identity that differ from step 2 refuse
   (`changed while hirsel add ran`<!-- doc-ref: planned -->), never merge.
9. **Publish one manifest:** write it to a temporary file beside it, fsync, rename over it,
   fsync the directory. Then derive `hirsel.lock` from the committed manifests, never from the
   overlay, and commit it as section 8 does.
10. For `--workspace`<!-- doc-ref: planned --> at a listed member, publish the second manifest the same way, then the
    lock again.

**Split operations.** Each operation changes exactly one manifest and leaves a valid workspace
by itself: a workspace dependency entry no member consumes adds no edge and no lock row
(section 4), so "root declared, member not yet consuming" is a workspace every reader (an
editor, `build`, `dev`'s watcher, git) already understands. A crash or failure between the two
operations is reported as `partial` with exit 1, naming the committed manifest and the pending
one; running the same `add` again completes it, and the root step then reports `unchanged`. A
crash between a manifest rename and the lock commit leaves a stale lock, which the next
unlocked command or the next `add` repairs and `--locked` reports. There is no intent journal:
readers that do not take the lock would not see it.

The edit preserves the file's formatting and comments. A new key goes in sorted position in a sorted map, and last otherwise. The URL written is the canonical locator (as typed, or the constructed GitHub
repository URL), never a redirect target. A new published binding records its package, major, root, typed locator and
`authority: []`. The new edge permits only pure code
until the user writes its list, so binding a package that reaches a capability refuses with
`authority exceeded` and the list the user would have to write.

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

**Output.**

The v1 operator receipt names the requested contract (`plan` or `submit`) and the outcome's
effect. Its `result.plan` includes the added or replaced bindings, authority ceilings, exact
selected roots and versions, lock rows, export differences, and SHA-256 hashes of the files
the edit would write. The receipt also records any fetched input and applicable registry
evidence. Preview and apply use the same plan. The preview has effect `none`, even if the
package store admitted verified content. A partial publication names committed and pending
files.

Exit 0 means the requested plan or edit succeeded. Exit 1 means a definitive refusal or
publication failure. Exit 2 means invalid or stale input. Exit 75 means a blocked edit.
A proposed authority expansion within the declared ceiling holds an ordinary edit before
publication. Save its JSON receipt and pass it to
`hirsel op decide @RECEIPT.json --decision FILE`<!-- doc-ref: planned -->. The
`host_v1.Decision` supplies the exact `plan_digest` and an `Approve` or `Refuse` verdict
with `entries: []` and no `all` field. The publisher resolves the edit again and checks the digest before
writing. A ceiling violation is a refusal.

### Updating dependencies

`hirsel update [ALIAS|ID[@VERSION]] [--plan] [--package-store DIR] [--json]`<!-- doc-ref: planned -->

`hirsel update`<!-- doc-ref: planned --> is the only command, with `add`, that moves a published root, and the only one
that writes `selections` (section 4). A build never solves: it reads the manifests' roots and
selections and refuses what they do not settle (section 5). `update` settles it, in the
workspace transaction of `add` (the workspace lock, captures before and after, one manifest
rename at a time, the lock derived from committed manifests).

1. **Re-read the named rows.** With no argument, every published row of the workspace; with
   an alias, that row of the nearest member (or of the workspace table); with an identity,
   every row that binds it. A git row re-resolves its `ref` (GIT-11) and a URL or archive row
   reads its bytes again. A registry row obtains witnessed candidates and complete status
   under the [registry selection rules](registry.md#5-client-selection-and-transport). A row whose source now yields another root is rebound to it and its
   `rev` with it. A row with `root` alone has nothing to re-read. Each new bundle is captured
   and verified as `add`'s step 4.
2. **Select.** For every identity whose edges in the new closure do not all accept one root
   (section 5), the candidates are the roots those edges name plus eligible verified releases for registry rows, members aside. The selected root
   is the candidate of highest version that every edge accepts; for major 0 it stays within the
   least minor any floor names, unless `ID@VERSION` names another version exactly. `ID@VERSION`
   selects the candidate of that version, and is refused when no eligible candidate has that version. An exact version bypasses only the registry age wait.
   A retired pin with a sole otherwise eligible young fix refuses and prints its exact upgrade command. No candidate
   accepted by every edge is the `diamond` refusal, naming the exact edges whose binders must
   be republished. An identity a member provides is never selected: the member is its root.
3. **Write selections.** A row of `selections` is written for each identity whose selected
   root differs from what some edge records, with the selected root and a source that holds it;
   rows no edge needs any more are removed. A selection row never names a member.
4. **Check before writing.** The edited manifests are resolved and checked on an overlay
   exactly as `hirsel check` checks the workspace, dependency authority included. A refusal
   leaves every manifest and the lock untouched.
5. **Report, then commit.** For each package whose effective root changed, it prints the
   identity, the old and new version and root, and the per-export authority diff (section 11);
   for each package now built against a dependency other than the one its publisher tested,
   one line such as `acme.dev/reader@1: published against acme.dev/json 1.4.0, built with
   1.4.2`. `--plan` stops here. Otherwise the manifests and then the lock are committed.

`update` never edits an `authority` list, never adds or removes a dependency row, and never
crosses a major: a new major is a new identity, which `add` binds. `--locked` and `--frozen`
refuse. Update uses the same operator receipt and exit codes as add.

### Auditing dependencies

`hirsel audit [DIR] [--json]`<!-- doc-ref: planned --> resolves the workspace as `check` does
and prints, for every package of the closure other than toolchain packages: its identity,
version, root and source; the row that binds it and that row's `authority`; every edge whose
recorded root differs from the effective one, as `tested` in the lock shows it (section 8);
and its card (section 11). For registry sources, verify release inclusion and replay status
through the witnessed head, including transitively bound roots. Report retirements,
reinstatements, takedowns and OSV advisories as [registry §5](registry.md#5-client-selection-and-transport)
specifies. Audit writes no manifest or lock.
A package whose reached kinds exceed a row's list is reported in its card and makes the exit
status 2, so `audit` and `check` refuse the same workspaces.

## 13. Verification

The [package-directory harness](../../crates/hirsel-core-driver/src/test_run_native.rs) and
[package edit laws](../../crates/hirsel-core-driver/src/package_add/laws.rs) exercise the
rules above. [Package verification](../../crates/hirsel-core-driver/src/package_add/VERIFICATION.md)
lists the cases and their independent oracles.
