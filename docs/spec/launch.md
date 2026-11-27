# Runner launch modes and identity

The runner has ordinary, describe and preview launch modes (preview: below, v2.15). The launcher selects the mode
before program code runs. Both modes enter through the program's `main`, which
calls `runner.main`; describe is the first branch of `runner.main`, before store,
listener or other capability use. Selecting describe grants no capabilities.
Arguments and the describe response use private launcher transport, not grants
to the program. A program must reach this branch without using a capability.

## Immutable launch identity

The runtime supplies one private launch metadata value with `digest`, `target`,
`n_place` and `revision`. `digest` is the 32-byte SHA-256 digest of the executable image's
bytes, as specified by FIPS 180-4. `target` is that image's target triple.
`n_place` is the byte allowance used for this launch's place registration. The
launcher fixes these values before program code runs. The runner reads this
value; it never opens an executable file or hashes it for a session. There is
no public `code` function for launch metadata.

`n_place` is a launcher-supplied run option, carried in launch configuration
alongside digest and target. `runner.main` has no `n_place` argument and reads
its allowance only from metadata. One executable can launch with different
allowances, so the full identity includes `n_place` independently of the digest.
Both native and evaluator launchers supply it, and describe reports it. Bindings
record the allowance of the described launch.

`revision` is the 32-byte `code.revision` of the
program's entry ([PROG-9](programs.md#prog-9-code-descriptors)), the value a recording's admission
already compares ([recording](../recording.md#what-a-recording-holds)), or unavailable for a
program assembled without revisions. It names the code, not the bytes: the same program built by
Cranelift or LLVM, at another optimisation level, or as a recording build has one revision and
different digests. The **release identity** is `revision`, `n_place`, the exported list, the HTTP route
bindings and the per-export slot schemas (RUNNER-SPEC §6 "One identity"), and it, not the digest, binds a store, matches a host record and
stamps a granted trigger. `digest` and `target` are diagnostics: Welcome, `Status`, refusals and
the release rows report them, and no rule compares them. An assembly record's name is the host's
and is not launch metadata: v2.51's `assembly` field and its launch option are deleted.

`n_place` comes from the private option `--hirsel-n-place`<!-- doc-ref: external --> when it is
given, and the runtime records that it was given; otherwise the place runs at its own
`PlaceConfig.n_place` (RUNNER-SPEC §6 "One n_place"). The platform host and launcher always
give it. A const-parameter mechanism was
consideredA const-parameter mechanism was
considered and left out: it is a separate language feature and is unnecessary
for launch configuration. There is no compiler rule naming `runner@1/runner` to
require or extract a constant argument.

- To capture the image on Linux, the native launcher opens `/proc/self/exe`
  once and maps it read-only. The first digest reader hashes that mapping and
  releases it. If mapping fails, the launcher hashes the opened file directly.
  Replacement, rename or deletion of the file after launch leaves the digest
  stable. Both native backends use the same runtime launch boundary. A closed
  executable with no digest reader may omit capture on a run that is not recorded.
- Darwin has no name for the running image itself, only the path it was started
  from. A program started directly opens that path once (`_NSGetExecutablePath`)
  and hashes the file at once, before its first statement, and keeps neither the
  descriptor nor a mapping. Replacement, rename or deletion of the file after
  that leaves the digest stable. The hash is not deferred: macOS lets the file
  of a running image be written and truncated, so a later read could hash other
  bytes and a mapping could fault. Before it hashes, the program checks that the
  opened file's device and inode are those of the vnode its running code was
  mapped from (`proc_pidinfo` with `PROC_PIDREGIONPATHINFO`, one call). A file
  renamed or linked over the path between the exec and the open is another
  vnode: the digest is then unavailable, never that file's. The check cannot
  see bytes written into the running image's own file in that window, since the
  vnode stays the same. A child of the platform launcher takes the digest the
  launcher verified (below, "macOS") and makes neither check.
- An evaluator launching a built artifact hashes the artifact it loaded, and
  retains that artifact's target. It never hashes the evaluator executable.
  Source-only evaluation has no executable image and is unavailable.
- An embedded runtime receives the digest and `n_place` explicitly from its embedder
  before entry ([EMB-3](embed.md#emb-3-embedded-launch)), and the runtime never reads its
  own image. Its target is the immutable record of the slice the library was compiled
  for, never an embedder value. It copies or pins the supplied values for the whole
  launch; later mutation of the embedder's input cannot change them.
- For a platform launch, the launcher supplies the digest through
  the embedded identity path. It checks the SHA-256 of the opened, read-only
  content-addressed image and executes that same file by descriptor. Replacing
  the store pathname cannot change the executed image or its digest.
- An image that cannot be read at launch produces `Unavailable(reason)`.
  Missing source-only or embedded identity has the same representation. It
  does not fail launch. Its digest is empty; the known target, `n_place` and
  `revision` remain available, so the release identity does not depend on the
  digest.

The value is anchored in the launch owner, before user entry, and stays constant
through every task, describe response and session in that process. An embedded
or evaluator launch owns its own value for the entire run. Views of it cannot
outlive that owner. No filesystem lookup occurs when a view is read.

Launch metadata is an entry argument and passes the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)). A build without recording and a recording build
of one program are different executables, so their digests differ
([recording](../recording.md#what-a-recording-holds)).

`Welcome.executable` carries this digest, including an empty digest for
unavailable identity. Generated bindings record digest, target and `n_place`.
`Welcome` carries `(caller, q, budget, executable, target, n_place, now_ms, retention_ms)`
under tag 4 of `hirsel.4`. Target and allowance are independent of the executable digest.
When either identity is unavailable the diagnostic is "identity unknown", with
no mismatch report. A differing available digest, target or `n_place` produces
a developer diagnostic only. Compatibility is decided by entry identity and
schema fingerprints; a mismatch never rejects a compatible call.

## Launcher-resolved directories

A `Dir` grant is a path, or one of four forms the launcher resolves at startup:
`xdg-config:<app>`, `xdg-cache:<app>`, `xdg-data:<app>` and `xdg-state:<app>`. A CLI
keeps its configuration, cache, data and state there without taking `Env` or knowing
the invoker's home directory, as the XDG Base Directory specification lays them out.
Each is an end-state grant source, as `env:` is for a `Secret`.

- **Name.** `<app>` matches `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, so it is never `.`,
  `..`, a hidden name, a path or a flag. `hirsel build`, `run` and `test` refuse
  another name in `--dir` or in a manifest's `dirs`, and the launcher checks it again.
- **Encoding.** The baked configuration tags a launcher-resolved directory as its own
  kind of slot (NUL, the form, `:`, the name; no path contains NUL), so the launcher
  never recognises one by reading a path. A relative directory whose name starts
  like a form is written with `./`: `--dir ./xdg-config:foo` is that path. Displays
  (`Dir=` in the release manifest, `--dir` in a refusal) write the form as `--dir`
  takes it.
- **Base.** For `xdg-config` it is `$XDG_CONFIG_HOME` when that is set and absolute,
  else `$HOME/.config`; for `xdg-cache`, `$XDG_CACHE_HOME` or `$HOME/.cache`; for
  `xdg-data`, `$XDG_DATA_HOME` or `$HOME/.local/share`; for `xdg-state`,
  `$XDG_STATE_HOME` or `$HOME/.local/state`. A relative value is ignored, as the XDG
  specification says, and `$HOME` too must be absolute. The directory is `<base>/<app>`.
  When neither variable gives a base the run does not start (status 103), with the
  line `not started: directory grant `<form>:<app>` needs $<VAR> or $HOME set to an
  absolute path`. The program sees neither value unless it also holds an `Env` that
  covers it. Darwin uses the same rule; there is no `~/Library` form. An embedded
  launch reads no environment: each form's base comes from the embedder's `dir_bases`
  ([EMB-3](embed.md#emb-3-embedded-launch)), and a form whose base is not given refuses
  `start`.
- **Symlinks.** Each component is followed, as a path grant's open is. Every existing
  component must be a directory once followed: a symlink to a directory is that
  directory, and a dangling symlink or a file is refused (status 103, the `directory
  grant` line with the resolved path), with nothing created through it.
- **Creation.** The missing components are created with `mkdirat`, mode 0700, and an
  `EEXIST` counts as success. They are created last: after every other grant is
  given and every refusal [CAP-4](capabilities.md#cap-4-entry-grants) names has
  passed, just before `main`, so a run that does not start leaves no directory behind.
  A replay, a describe launch and a launch whose directories are rebound (the dry
  run and the prepare launch) create nothing.
- **Recording.** The directory is the `Dir` grant's root, as a path grant's is; a
  recording holds the grant, not where it was resolved.

`tmp:` is not a form: a shared temporary directory needs a private per-run directory
and its cleanup, which are their own design.

## Describe launch

`runner.main` runs the supplied `place(mut Registry[G, H])` under
`bound bytes n_place`, using the launch's fixed allowance, without opening a
store or listener. It encodes this record with the ordinary value codec:

```hirsel
type Kind: codec + send = Compute | Transact | Query | Granted
type Described: codec + send { kind: Kind, identity: Bytes, args: Bytes, result: Bytes }
type Exported: codec + send { described: Described, args_fp: u64, result_fp: u64, revision: Bytes }
type Describe: codec + send { digest: Bytes, target: String, n_place: u64, revision: Bytes, exported: List[Exported] }
type ReleaseIdentity: codec + send { revision: Bytes, n_place: u64, exported: List[Exported] }
type ReleaseManifest: codec + send { describe: Describe, release: Bytes, grants: List[String], root_limit: u64, recipe: Bytes }
```

`revision` is the launch's entry revision, empty when it is unavailable. A `Describe`'s
**identity digest** is SHA-256 over `u8 18 ‖ "hirsel.identity.v1" ‖` the value codec's bytes of
`ReleaseIdentity { revision, n_place, exported, bindings, slots }` taken from it (RUNNER-SPEC §6 "One identity").
Entries are in registration order. `Described` has the fields `kind`, `identity`,
`args` and `result`, in that order, with no separate error field. Its `result`
is the schema of the value carried by `Returned`: `Result[R, E]` for transact,
and `R` for compute and granted. The two fingerprints come from
`std.codec.fingerprint_of` applied to `args` and `result`, respectively.
`revision` is the entry's 32-byte `code.revision` (PROG-9). It is empty in
programs assembled without revisions. It contributes to `release` and does
not enter the entry key or the client's `Want`.
Registration and encoding are bounded. A successful child writes exactly one
Describe and exits 0. A place capacity error is `PlaceCapacity(Room)` and a
duplicate registration is `AlreadyRegistered(identity)`. The child encodes
either error on stdout and exits 2. The error sum has that variant order.

Place registration and assembly and encoding of the success response share
one `bound bytes n_place`. After that bound ends, the parent may encode the
status-2 `PlaceCapacity` error. The helper's capture allowance is
`max(n_place, PLACE_CAPACITY_ERR_LEN)`, where `PLACE_CAPACITY_ERR_LEN = 20`
comes from the codec's u32 error tag plus Room's two u64 fields: 4 + 8 + 8.
Allowances of 0 and 1 return that capacity error intact. Capturing only
`n_place` is a mutant of this law, not an allowed truncation.

The CLI runs the built server as a child with an absolute wall-clock deadline,
10 seconds by default, configurable with `--place-timeout`. It drains the
bounded response while the child runs. At expiry it kills and reaps that child
and reports `PlaceTimedOut`; no generated output is written. The deadline
covers entry through `main`, registration, encoding and child exit. Capacity,
duplicate registration and timeout errors leave the output destination alone.

The native launcher consumes the private prefix before constructing user `Args`. Every native
executable recognises the same private options, place or not: `--hirsel-describe`<!-- doc-ref: external -->,
`--hirsel-preview`<!-- doc-ref: external -->, `--hirsel-n-place=<bytes>`<!-- doc-ref: external -->, `--hirsel-wake`<!-- doc-ref: external -->,
`--hirsel-idle=<ms>`<!-- doc-ref: external -->, `--hirsel-witness=<version>`<!-- doc-ref: external -->, `--hirsel-store-id=<hex>`<!-- doc-ref: external -->,
`--hirsel-install-predecessor=<hex>`<!-- doc-ref: external -->, `--hirsel-install-child=<activation>`<!-- doc-ref: external --> and
`--hirsel-dev-store`<!-- doc-ref: external --> (each of the last two ends the prefix), and `--hirsel-exec=<approval>`<!-- doc-ref: external -->
in an executable with Exec bindings only. The prefix is
the leading run of arguments that are recognised private launch options: the first argument
that is not one ends it, and that argument and every one after it become `Args` verbatim, so a
private option written after a place argument is never read as one. `--hirsel-install`<!-- doc-ref: external -->
and `--hirsel-activation=<version>`<!-- doc-ref: external --> are place arguments, read from `Args`
by `runner.launch_selection`, so they come after every private option. Five words are read
from the first argument after the prefix, and only in a place: `--hirsel-dry-run <dir>`<!-- doc-ref: external -->,
`--hirsel-store-verify <dir>`<!-- doc-ref: external -->, `--hirsel-store-repair <dir>`<!-- doc-ref: external -->,
`--hirsel-tapes-check=<writer-digest> <dir>`<!-- doc-ref: external --> (launch word 20, below) and
`--hirsel-prepare`<!-- doc-ref: external --> (below). The linker bakes a `place` bit into the executable's launch
configuration when the program reaches the toolchain's `runner` package, which holds every
reader of these words. The link decides by where the package came from, never by its name: the
runner is the toolchain's when the installed toolchain supplied it (its `core-packages` archive),
when it is a workspace member whose directory is `programs/<name>` under the library root the
invoker names (`--libraries ROOT`, or a checkout's `core` when the compiler is not installed), or
when the invoker's `--inputs` manifest marks it `"toolchain": true`. The library root and the
`"toolchain"` mark are the invoker's trust, exactly as supplying `core@1` is: whoever chooses them
already chooses the core the program is built on. Any other `hirsel.toml` member or published
archive named `runner@1` is a plain package, its program gets no bit, and its `launch_*`
functions are not bound to the launch words. The native link reads the provenance from the
check, the recipe link from the recipe's toolchain part (below). `hirsel dry-run`, `hirsel store verify --release` and `hirsel store repair --release` read
the bit from the executable before touching the store or starting the program. If the bit is
clear or absent, the command names the executable on stderr and exits 104. In a directly launched
executable without the bit these words are ordinary `Args`: the program keeps its own
directories and its standard output, and receives the words as written. A malformed private
option in the prefix is refused before user code: the process writes `not started: ` and the
reason to standard error and exits 103. A launch that gives `--hirsel-n-place`<!-- doc-ref: external -->
runs at that allowance in every mode; one that does not runs at its own `PlaceConfig.n_place`
(a describe at the 16 MiB default), whose wire configuration must repeat that value or the
start is refused `Disagrees` naming both, and `Status` and Welcome report it (RUNNER-SPEC §6
"One n_place"). There is no private option that names an assembly record (RUNNER-SPEC v2.42 deleted
the planned one, never built). Evaluator and embedded launch configurations
carry the same mode, the allowance and whether it is explicit.
SDK generation uses the driver's `describe_child` helper;
the CLI and generator share its bounded read, absolute deadline, kill and reap.

Laws: `place_run_bounded` kills a looping place at its deadline and observes no
store or listener; `identity_stable` keeps the launch digest after executable
replacement; `identity_unavailable` launches with an empty digest and produces
no mismatch diagnostic when the image is unreadable or evaluation is source-only.

## Place activation

A place passes the result of `runner.launch_selection(Args)` to its runner. The
ordinary place option `--hirsel-activation=<version>`<!-- doc-ref: external --> selects the activation
version that must match the current image under STORE's LOCK; version zero
expects an empty store. Omitting the option selects `Reopen`: genesis on an
empty store, and otherwise the store's current activation, so a plain restart
of the same executable reopens its store. `--hirsel-install`<!-- doc-ref: external --> requests a
standalone install and is mutually exclusive with an activation version. Use it
after `--` with `hirsel run`. The install runs `place` and `migrate`, validates
both handle sets, commits the activation under the store lock, closes the store
and exits. Start the executable again to serve.

An install exits 0 whatever host the store belongs to: it skips the current-identity guard
and writes its own release identity. That is right for a standalone place, and for a store a
platform host serves it is right only when the identity is one the host keeps a record for.
A hand install meant for a hosted group must have the release identity of one of the records
the host can adopt, the marked deploy's OLD or NEW that the group still keeps: run an
executable of that record's code (any backend or optimisation level, since the identity is the
code's) with the record's allowance first and the install last, for example

```sh
./place --hirsel-n-place=1048576 --hirsel-install
```

The install then registers and writes its `Describe` at that allowance, not at `PlaceConfig.n_place`.
Written in another order (`./place --hirsel-install --hirsel-n-place=1048576`), the first
argument ends the private prefix, so the allowance is a user `Arg` that `launch_selection`
skips: the install runs at `PlaceConfig.n_place`, it exits 0, and its identity matches no
record when the two allowances differ.
The group's `Status.failing[].last` names those candidates with their identity digests and
allowances when the host finds no match, and keeps that cause ahead of each retry's own refusal or exit status
until a read matches. The group's
next retry reads the store and serves that record at the installed activation
(RUNNER-SPEC §14.3 "Leaving no record"); a group left failing by a decided deploy recovers
only this way, because a hot deploy needs a live OLD. Both options are run arguments,
which a recording holds like every entry argument. The launcher passes
`--hirsel-activation`<!-- doc-ref: external --> to serving children.

A store is bound to the release identity that activated it (RUNNER-SPEC §6, "Restart and
rebuild", "One identity"). The same code built by another backend, at another optimisation
level or as a recording build opens it. Other code, another allowance, another exported
list, other HTTP route bindings or other slot schemas are refused `NotCurrent` with the store's version and identity digest and this launch's,
and both executable digests as a diagnostic. To move a store to a new build, stop the old process and start
the new build once with `--hirsel-install`<!-- doc-ref: external -->, or deploy it with `hirsel deploy`.

A refused selection returns `NotCurrent(found)` before handles run. A disk
genesis or standalone install requires a known release identity (an available `revision`)
and returns `IdentityUnknown` if it is unavailable.

**Held runs (RUNNER-SPEC v2.66 §14.6 "The activation fate").** An install whose store holds
a run that this activation holds (one parked, or one started under another release identity)
settles it in the install's own commit. `--hirsel-held=release|cancel`<!-- doc-ref: external --> gives every held run a
fate, and each `--hirsel-held-entry=PATH=release|cancel`<!-- doc-ref: external --> gives one for that entry path. Both
are run arguments, like `--hirsel-install`<!-- doc-ref: external -->. An install that finds a held run with no fate, or
an entry option whose path is the entry path of no pending run and of no entry the old or new
release registers, exits 1 with `Unsettled` and the trigger report, and publishes no cut. A hand install that recovers a hosted group therefore names a fate when the
store holds held runs:

```sh
./place --hirsel-n-place=1048576 --hirsel-install --hirsel-held=release
```

A prepare launch takes `--hirsel-prepare <absolute-export-dir> <attempt> <image-digest> <fates>`<!-- doc-ref: external -->
as its place arguments. The attempt and image digest use lowercase hex for 16 and
32 bytes. `<fates>` is the lowercase hex of the deploy's `Fates` in the value codec, which is
the request's `held` field. The native launcher binds directory designations to that export
directory. The child receives no listener or private endpoint. The place
passes the scratch directory to `runner.prepare_deploy`. It verifies the image
key, stages the migration, validates both handle sets, and
syncs `PREPARED` before exiting.

## Release manifest

`hirsel build --manifest PATH` writes the release manifest after it links the
executable. It runs the built executable's preview launch (below) with an
empty old list through `describe_child`, with the same absolute deadline and
allowance. So the entries and `release` come from running `place` inside the
executable, and from no second list or second hash (RUNNER-SPEC D1, extended in
v2.15 §12.2). A preview error or timeout writes no manifest and fails the build
with that error. The manifest is one value of this type, encoded with the
ordinary value codec. Its type, `ReleaseManifest`, is declared with `Describe` above.

- `describe` is the preview child's `Describe`: digest, target, `n_place` and
  the exported entries in registration order. `n_place` is the allowance the
  child ran with. A hosted launch that selects another `--hirsel-n-place`<!-- doc-ref: external -->
  is a different release identity, and Welcome and the status route report it.
  `revision` is the entry's.
- `release` is the preview child's `release`, which it computes with
  `runner.release_digest`: SHA-256 over `u8 17 ‖ "hirsel.release.v1" ‖` the
  value codec's bytes of the registry's exported list. The driver never hashes
  the list itself. The running place computes the same function from its
  registry and reports it on the status route.
- `grants` is every grant baked into the executable, one canonical string each,
  sorted bytewise: a kind name as `--grant` takes it (`Clock`, `Random`, …),
  `Net=` or `Env=` followed by the rule or pattern text `--grant` takes, `Foreign=`
  followed by the interface's definition path `D` (`Foreign=sqlite@1/sqlite/Sqlite`), `Exec=`
  followed by the profile file path and the profile names as `--grant` took them
  ([EXEC-1](exec.md#exec-1-an-exec-grant-confers-what-its-programs-can-do-and-hirsel-does-not-confine-them)), and
  `Dir=` followed by the path or launcher-resolved form `--dir` took
  ([launcher-resolved directories](#launcher-resolved-directories)). The strings are a display of the recipe's grant bindings
  (below), which say which parameter receives each one. A directory path stays as
  given. It is resolved against the launch's working directory at startup, which
  the manifest does not know.
- `root_limit` is the executable's root limit.
- `recipe` is the 32-byte root R of the release recipe the executable was assembled
  from (below). `release` says what the public interface runs, R says what code was
  assembled, and the digest says which bytes run. R does not enter `release`, the
  release identity or the store's `releases` rows: two recipes of the same code under two
  toolchains have one identity.

Grants and the root limit are part of the executable's build key, so a manifest
describes exactly one executable. Limits the place sets in its own code (`q`,
`n_session`, `budget` and the others) are covered by the digest.
`hirsel manifest show PATH` prints a manifest as text. The manifest is the file `--manifest`
wrote. An executable carries none, and `show` refuses an ELF or Mach-O file as an executable.

## Release recipe and host assembly

`hirsel build` works in two steps. It checks the program and writes its code objects and its
release recipe into a code store. Then it assembles the executable from that recipe. Both
steps are the only build path. `hirsel publish` runs the first step alone and prints R.
`hirsel build --recipe R --code-store DIR` runs the second step alone, from objects already in
DIR.

**Objects.** A code store holds immutable objects, each named by the hash of its own bytes:
bodies, groups, annexes and values (PROG-10), and recipes. A **recipe** has a version, the normal-form
version and these parts:

- the entry, the program's `main`, as its identity encoding (PROG-10) and its address;
- every group in the entry's closure, as its hash and byte length, in strictly ascending order
  of hash;
- every body in that closure, as its hash and byte length with its annex's hash and byte
  length, in strictly ascending order of body hash;
- every **value** a body of that closure binds to a computed constant (PROG-10), as its value
  hash and byte length, in strictly ascending order of hash. A value object holds the value
  hash and the value itself, so a host binds the constant without running the program;
- the **foreign modules** ([FFI-3](foreign.md#ffi-3-archives-are-admitted-by-their-pin-before-linking)):
  one foreign leaf per reached foreign module, with its identity, symbol prefix, whole archive
  table and reached functions, in strictly ascending order of module identity;
- the **foreign archives** (FFI-3): the distinct archives the leaves and Foreign bindings
  select for the recipe's target, each its SHA-256 and byte length, in strictly ascending
  order of hash. An archive's bytes are a code-store object named by that SHA-256;
- the **toolchain packages**: each package of the closure the installed toolchain supplied
  (above), as its name and version, in strictly ascending order; the recipe link sets the
  `place` bit from this part alone;
- the launch inputs: the backend (Cranelift or LLVM release), the target, the root limit,
  `n_place`, and the **grant bindings**.

The grant bindings follow `main`'s capability parameters in declaration order. Each binding
gives the parameter's ordinal, its kind, and the values that parameter receives, in order and
with their count: each directory path a `Dir` parameter takes, each rule text a `Net`
parameter takes, or the one interface definition identity a `Foreign` parameter takes.
A `Foreign` binding then holds the SHA-256 and byte length of the selected archive. An
`Exec` parameter takes the profile file path as given and the selected profile
names, and its binding then holds the grant's policy digest
([EXEC-2](exec.md#exec-2-launch-profiles-and-narrowing)). That digest covers each image pin
and every launch constraint, so replacing the profile file changes what the binding
approves. Every launch with an `Exec` grant, `hirsel run` included, loads the policy once,
compares its digest with this binding and builds the grant from that same loaded policy
(EXEC-1).
So exchanging the paths given to two `Dir` parameters changes R. The
manifest's sorted `grants` strings are derived from the bindings for display and comparison;
nothing is assembled or launched from them.

Its root is `R = H("hirsel.release-recipe.v4", recipe)`; the version is 4, and versions 1 (no
foreign parts), 2 (no toolchain part) and 3 (no values) are never read or written. A place's `place`, `migrate`, handle
and launch code is reached from `main`, so the closure covers it. The annexes hold the site
maps failures report, so a comment that moves lines changes R but no address. The optimized
native link stores coordinates for the site words its retained sections reference. Each
surviving site word keeps its original value. R does not cover the host's toolchain: one R
can assemble to different bytes under another toolchain, and the assembly record (below)
says which.

**Working directory.** A directory path in a recipe stays as given. A relative path is resolved
when the executable starts, against the launch's working directory, which is an input of the
launch and never of the recipe. `hirsel run` and `hirsel build --manifest` launch in the
caller's working directory. The launcher starts every child of a group (describe, preview,
prepare and serving) in that group's working directory, set by `hirsel register --workdir DIR`
when the group is added to the host. A recipe that moves to another host therefore resolves its relative paths
against that host's place directory.

**Authority.** A host takes a recipe only when an authorised deploy request names its R. That
is a request on the host's operator socket ("The operator socket" below), whose directory
permission and peer-UID check are the boundary. A request that names a raw R is the caller's explicit statement that a checker it
trusts produced this whole R, its bindings and annexes included; a hash proves only that bytes
are intact, and never that they were checked. A caller with narrower authority, such as a
deployment service that only schedules deploys for others, never forwards a raw R. It either
selects a publication the host has recorded, by its R, or submits source, which the host
publishes itself with the normal checker, using the host's own compiler caches and package
images and the bounds below. Its caches may be a resident compiler's memory
([cli](cli.md#resident-compiler)); a publication is the same R whether a resident or a
one-shot compiler produced it. The operator approves the grant bindings for each deploy.
A recipe that grants `Foreign=D` is deployed only when the request passes `--grant Foreign=D`
itself, and the deploy's output shows, before any child runs,
the interface, target, selected archive digest and recipe of each such grant
([FFI-4](foreign.md#ffi-4-the-foreign-capability)). A recipe that grants `Exec=FILE NAME`
is deployed only when the request passes that grant itself. The host then loads the
policy from `FILE`, resolved against the place's working directory, and compares its digest
with the binding's before any child of the attempt runs. Preview, prepare and serving each
check the digest again, and refuse to start on a different one
([EXEC-1](exec.md#exec-1-an-exec-grant-confers-what-its-programs-can-do-and-hirsel-does-not-confine-them)).
One local principal that both publishes and deploys needs no
signature. A request from another machine would add publisher authentication at this point: a
signature over R by a key the host is configured to trust establishes who vouched for R, and
the operator still decides whether to deploy it.

**Admission.** Before the host keeps or uses anything a request sends, it checks the request in
this order and refuses with the first failure. Steps 1-3 come before the host answers with the
objects it lacks.

1. The request comes through the control socket and names R. The recipe fits the launcher's
   recipe byte limit, hashes to R, and decodes completely and canonically (PROG-10's
   rule for one encoding per value): every list in strictly ascending order with no repeat, no
   bytes left over, and a version and normal-form version the host supports. The closure must
   fit the launcher's object count limit. A failure is `Unsupported`, `Hash`, `Malformed`, or `Limit(which)`.
2. The host takes the collection lock and reserves every object the recipe lists that the code
   store already holds, so collection cannot remove it, and only then computes what it lacks.
3. It reserves staging space for the lacking objects' listed lengths against its host-wide
   staging limit, within the attempt's limits on total bytes, on memory for decoded
   objects, on members, slots and nesting, on outstanding objects and on elapsed time.
   A request over a limit, or one the host cannot reserve now, is
   `Limit(which)`.
4. The host answers with the hashes it lacks, in one round. Each received object is charged
   against its listed length and the attempt's limits while its bytes stream in, partial frames
   included. An object the host did not ask for, or one already received, is
   `Unsolicited(h)` and is not kept. An object whose bytes do not hash to its listed hash is
   `Hash(h)`.
5. Every group, body, annex and value of the closure decodes within the limits under PROG-10's
   encoding rules, each annex against its own body, before any of it is put into a map.
   A body in a different normal-form version is `Unsupported`. Republish from source.
   Invalid frames are `Malformed`.
6. The closure walked from the entry through the groups' `External` bindings, compared as typed
   sets of groups, bodies, the annex paired with each body, foreign leaves and foreign
   archives, equals the listed objects exactly (`Missing`, `Extra`). An `External` address
   that is no listed group's member is resolved to a foreign function by recomputing FFI-9's
   address from each foreign leaf: its archive-set digest from the leaf's table, and each
   function's address from its identity, header text and C symbol, the leaf's prefix followed
   by the function's name. An address that neither a group nor a leaf gives is `Missing`. A
   leaf, or a function of one, that no slot reaches is `Extra`. Each leaf's table has a row
   for the recipe's target (`Missing` otherwise), and the foreign archives part is exactly the
   set of the pins and lengths selected by those rows and by the `Foreign` bindings
   (`Missing`, `Extra`). Each body's computed constant slots name, through its member's
   `Value(h)` bindings, exactly the listed values: a value no slot binds is `Extra(h)`, a slot
   whose value is not listed is `Missing(h)`. Each value fits its constant's type and hashes,
   with that type's closure hash, to `h` (`Binding`). Admission binds each constant to its value
   here, so every body that leaves admission is lowered with its constants bound.
7. Every slot's identity equals the bound member's or builtin's identity, every `Internal`
   index is in range, each group's members form one strongly connected component, the entry's
   identity matches, and the closure has one address per identity with every slot's signature
   equal to its target's (PROG-10), a foreign function's signature being the one its header
   text states (`Binding`, `Conflict(identity)`). The entry annex carries the checked
   parameter types in declaration order. Each entry parameter is a 64-bit word in
   CoreIR and has exactly one grant binding at its ordinal. The binding's kind must
   match the parameter type. For `Foreign[D]` the binding must also name the same
   interface definition identity D. A mismatch is `Binding`. No two leaves have one prefix
   (`Conflict(identity)`, the second leaf's). Before anything is linked, each foreign archive
   passes FFI-3's format and symbol checks for the leaves that select it.
8. Every `Builtin(n)` and every engine-operation tag is one the recipe's backend implements at
   exactly that number (`Unsupported`).

Admission does not check types or ownership again. A refusal, a disconnect or a host restart
releases the attempt's reservations and discards its staging. Publication of outside source and
assembly run under the same kind of limits on time, memory and output. How an optimised build keeps its peak memory
bounded by its largest release partition is an implementation matter
([release compile memory](../internal/release-memory.md)).

**Assembly records.** An **assembly record** is an immutable host record naming one realised
combination: R, the toolchain fingerprint, and the image digest that assembly produced. Its name
is the hash of those three. It is the host's name and is in no launch metadata or `Describe`
(RUNNER-SPEC v2.42): an activation names a release identity, which several records can share
(one R under two toolchains, or two recipes that differ only in grants), and the host picks
among them by RUNNER-SPEC §14.3 "What serves". The **toolchain fingerprint** is a hash of the effective environment
of the assembly: the resolved target features, the compiler and backend, the runtime ABI and
runtime archive, the linker, every link input and the effective settings. A host's toolchain is
an immutable installation, so every file it loads is covered. Image bytes are kept once per
digest, and any number of records may name one digest: two recipes that differ only in
`n_place` give two records and one image, and one recipe assembled under two toolchains gives
two records and two images. Native objects keep their own key (body, annex and emission
environment) and the final link keeps its own (R and the toolchain fingerprint). The host
reuses a cached mapping from such a key to bytes only when it produced that mapping itself under
that exact key. Objects from any code store are verified content, never a cached mapping.

**Durable publication.** The host's code store, its image store and its records are written
with the barriers the store uses (`host.sync` and `host.sync_dir`), in this order, so that after
any crash every record the host has synced names only complete, synced objects. The driver's
build cache, where a power cut may turn a write into a miss, is never used for them.

1. Received objects go into the attempt's private staging directory, which no reader and no
   collection sees.
2. After admission, each new object is synced, renamed into the code store and the code store's
   directories are synced. An object already present is kept, never rewritten.
3. Assembly writes the executable in staging. The host syncs it, renames it into the image store
   by digest and syncs that directory, then writes, syncs and renames the assembly record and
   syncs its directory.
4. The host writes, syncs and renames the attempt's **pending record**, naming the attempt and
   its assembly record, and syncs its directory. The reservations become the pending record's
   roots. Nothing after this point, preview included, runs before this sync.

Pin acquisition, reservation and deletion take one collection lock, so no object or image is
removed between a presence check and the pin that keeps it. Collection removes only what no
root reaches. The roots are the union of: the assembly record of each retained activation
(current, previous, and each live `releases` interval), each pending record, each recording pin, and
each live reservation. An assembly record keeps its image, its recipe and the recipe's closure.
The host keeps a per-store index from activation version to the assembly record that serves it,
so the store's `releases` rows keep their format. A decided selection, a commit, an Installed
outcome (at the attempt's `v`), a genesis adoption, a no-record group's adoption (at the read's
activation) and a host-run install write it; a probe's
intent never does (RUNNER-SPEC §14.3 "What serves"), and a recording pin names the assembly record of
the run it records and is synced before `Pinned` is answered. A retained image carries its runtime. The host reads its recipe envelope to recover
launch settings and keep referenced objects. A retained digest whose image is missing gives `Unknown(ImageGone)`.

`hirsel register ... --record DIRECTORY` registers a recording group ("Recording a group"
below): every admission for it assembles with the recording runtime and refuses any recipe
with a foreign leaf
([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)),
and the CLI refuses such a recipe itself before it creates DIRECTORY or sends anything. The host
assigns each serving process a recording ID and syncs its pin before launching
the process. A pin names the assembly record and carries its expiry. The
default retention period is 30 days, set by `register --tape-retention DAYS`.
`hirsel replay --state HOST_STATE --tape TAPE` resolves the recording ID through
the pin. It asks a running host with `ResolvePin` on `HOST_STATE/operator.sock`. When no host
listens there, it takes `HOST_STATE/launcher.lock` and then the catalog's `LOCK`, both without
waiting, and reads the pin with `libexec/hirsel/pin_reader` (`platform_main/read_pin`), which
replays the catalog journal read-only and prints `found <assembly hex>` or `gone`. A held lock or a
host that does not answer exits 75 "temporarily unavailable"; a reader error is an error, never
`Gone`. A `Gone` pin, or a pinned image no longer in the image store, exits 3
(`Unknown(ImageGone)`). Replay then refuses a recipe with a foreign leaf before reading any record of the tape
(FFI-8), and runs the verified image from the host image store. Replay takes the
arguments and launch metadata from the recording.

When the host starts, before any collection, it reconciles every attempt. Staging with no
pending record is removed. A pending record that no surviving child or held answer can decide
is **Undecided**: a durable state that keeps both OLD's and NEW's roots, under which no install
command, ordinary wake, second deploy or retirement may change the group's roots. The host
decides it only by what the store reports under its `LOCK`, through a recovery probe. The probe is a
serving launch with no predecessor at a fresh generation. Its pair is saved in the
pending record as probe intent, never as the group's current pair, and a refused probe's
cause compares the store's activation with that intent, never with the group's pair: a probe of
NEW at 10 refused at 3 says it requested 10 and found 3 (RUNNER-SPEC §14.5 "Genesis"). Every
probe also names the attempt's activation `v` as its witness version. The first probe opens NEW's
assembly at `v`. An open of NEW at `v` that reports `releases[v].prepared`
equal to the attempt's prepared digest proves the install (RUNNER-SPEC §14.3; an open at `v`
with another `prepared` proves this attempt did not install): the host
makes NEW the current assembly and activation, keeps OLD's as retained, and records the
deploy `Installed`, in one durable transaction, and the probe goes on serving. A probe
refused `NotCurrent` reports one capture of the store, read before it exits and without
application handles: its store ID, activation, identity digest, on objects@1 its holder pair, and
`releases[v]` (RUNNER-SPEC §14.3 "The probe's read"). On objects@1 the capture is a follower
read; on log@1 the probe keeps the store's `LOCK` after the refused comparison and reads its
own recovered state. Row `v` decides the attempt even when another install has since moved the
store past `v`: the attempt's digest is `Installed`; another `prepared` is failed (`not
installed after recovery`), and so is no row on log@1, or on objects@1 no row under another
pair than the one OLD recorded with its import. On objects@1 no row under OLD's pair proves
nothing yet: OLD's delayed write can still land, so the next probe opens OLD's assembly at
OLD's activation, and that open, or its read, decides. The outcome never chooses what serves.
An Installed outcome also records NEW's assembly for `v` in the
activation index, whatever activation the read reports. In the same transaction the host
selects the store's current activation with a record whose identity digest is the read's
(RUNNER-SPEC v2.42), so a group never returns to `v` from a later activation. Two records can
share an identity (the same code under two backends, or a recipe that changed only grants), so
when both OLD and NEW match, the attempt's decided outcome picks: Installed serves NEW, failed
serves OLD. A probe that opens always serves the record it opened. When neither matches,
nothing is selected, the group's row keeps a durable no-record mark (the attempt, the read's
activation, its identity digest and allowance), and
the group is failing with the cause `No recovery candidate matches the store at activation a`,
followed by that identity digest (or "does not decode") and allowance, and the names, identity
digests and allowances of the candidates below. The mark, not the start that last failed, carries the
cause: `Status` formats it from the mark and appends the last start's own refusal or exit
status, so retries refused at the stale activation, retries that end before they report, and
a host restart keep it (RUNNER-SPEC §14.5 "Failing and backoff"). Each retry of a marked group, once refused, reads the
store as the probe does, without the witness row; when the read's identity digest is that of
the attempt's OLD or NEW that the group still keeps (both: by the outcome, as above), one
transaction makes that record current at the read's activation and writes it into the activation index there, and the
group relaunches at once (RUNNER-SPEC §14.3 "Leaving no record"). Adoption turns the mark
`Adopted`, which clears the previous cause. If the relaunch, or any later start before an open,
is refused and its read matches no candidate (another install moved the store again), the mark
is `Unmatched` again with a new cause at the activation that read found, and `Status` shows it.
A relaunch that dies before it reports leaves the mark `Adopted` and contributes only its own
exit status. The first accepted `PlaceOpened` clears the mark in either state. The deploy's outcome and its
index entry are not changed, and while nothing matches the selection is left alone. Roots follow RUNNER-SPEC §14.3 "Roots". Any other
answer (`Held`, a missing image, a failed read, an I/O failure) decides nothing: both roots stay, and the probe is retried under the
ordinary backoff. Neither a timer nor a retry count decides a root.
The platform host records the attempt as `Preparing` before its describe, preview and
prepare children and OLD's export. A `Preparing` row proves no cutover was sent, so after a host
loss it is aborted: OLD, if it still runs, is sent `HostAbort`, and the failure is recorded on
OLD's answer or end, never before (RUNNER-SPEC §14.3 "Preparing"). A drift retry records
the original attempt as well as the retry. Its abort releases either pause owner, including
when the host restarted before OLD took the retry export. An open that proves NEW's
install must also report the release that the preview reported for NEW, the release row's
`prepared` digest, and the store ID
recorded for the group. The first accepted `PlaceOpened` binds the group incarnation to
its store ID in the catalog before the host lets the runner serve. A first open at genesis
also records the activation the runner installed, replacing the empty-store selection 0.
Later serving starts, including probes and candidates, reopen
the selected store under its lock and check that ID before activating or serving. If the
registered directory is empty because its mount is missing, the fresh store has a different
ID. The launcher logs both IDs and the place refuses to serve. Restoring the original store
allows the group to start again.
The platform host's catalog keeps the attempt's pending row from before the candidate's start
to the cutover's decision, with the exact cutover command it sent; a new session sends that
command again and the launcher answers it from what it kept, so a host lost at any point in
the cutover installs NEW at most once (RUNNER-SPEC §14.3 "Across sessions").

**Hot deploy.** `hirsel deploy R --code-store DIR` admits R, fetches what is missing, assembles
with the host's installed toolchain for the recipe's backend and target, and publishes the
assembly record and the pending record, in that order. All of this happens before the preview
and before the old place exports, outside the write pause. A refusal or assembly failure stops
the deploy before anything is touched. The preview, the prepare child and the cutover all use
the attempt's one assembly record: the preview runs that record's image with the recipe's
`n_place` and grant bindings; the prepare child is given that image's digest; and before the
host accepts the import it checks that the prepared target's `Describe` and release record are
the ones the preview reported for that record. OLD's first drift is retried once and keeps the
record: the deploy row returns to `Preparing` at a fresh attempt, OLD exports again under its
original write pause, the prepare child and import run again with the preview's `Describe` and
release, and once the discarded candidate has ended the new one starts at a new generation; that cutover keeps the original pause
deadline. A drift of the retried attempt fails the deploy (`drift`). The row records that the
retry is spent, so a host restarted after it re-sends the retry's cutover unchanged. The write pause keeps hot deploy's own checks (attempt and prepared
digest, store identity, version and activation, capacity, and the deadline), and no fetch,
assembly or closure walk runs inside it. The deploy reports the fetch time, the assembly time
and the write pause as three separate figures. The write pause starts with the first cutover
request. If OLD held a write during the pause, the host finishes the report when NEW's wire
writer confirms that it flushed the first durable write outcome. Otherwise the pause ends at
NEW's Ready. If NEW exits or retires without flushing a durable write outcome, its Ready ends
the pause. The host can serve another deploy while a report waits for this notice, and the
report wait has no deadline. A recorded digest is recovered from its retained bytes and never
by assembling again.

Laws: `grant_binding_order`, where a launch that reports which directory each `Dir` parameter
received gives the given paths, and exchanging them exchanges the reports; `assemble_is_build`,
where `hirsel build` and `publish` followed by `build --recipe` give byte-identical executables
on one toolchain, as agreement only; `assembly_environment`, where a change of runtime archive
or of a builtin's semantics number under one R gives another fingerprint and never a cached
image; `admit_refuses` covers each refusal above, and nothing is assembled until all checks
pass; `shared_digest`, where two recipes sharing one digest and one recipe with two digests
each keep their own record, bytes and allowance through a host restart and a collection;
`site_only_change`, where a moved comment keeps every address, changes R, and the failure sites
reported move with it.

Version note (2026-11-21, deploy trust): the recipe lists grant bindings by parameter instead of
sorted grant strings, and object lengths; admission is bounded before the host asks for
objects; a raw R is the caller's attestation; assembly records replace one record per digest;
and host stores follow the publication order above. Pre-1.0, no recipe had been published, so
nothing migrates.

## Preview launch

*(Note: `hirsel build --manifest` and `hirsel manifest show` are built; the standalone `hirsel preview`<!-- doc-ref: planned --> and `hirsel confirm`<!-- doc-ref: planned --> CLI verbs below remain unbuilt.)*

`hirsel preview --exe NEW NEW.manifest OLD.manifest`<!-- doc-ref: planned --> compares two releases
without a store, a listener or a write grant. It refuses with exit 2 when NEW's
launch digest differs from NEW.manifest's, or either is unavailable. It then
runs NEW as a child in preview mode, with the same deadline, kill and reap as
describe. The child's arguments are the private prefix `--hirsel-preview`<!-- doc-ref: external --> and
`--hirsel-n-place=<bytes>`<!-- doc-ref: external -->. It reads OLD's `List[Exported]` on stdin, bounded
by the allowance. It runs `place` under `n_place` and answers each old entry,
in OLD's order, with the session's own answer to a Hello `Want`, `answer_want`
(RUNNER-SPEC §12.2). It writes one `Preview{describe: Describe, release: Bytes,
answers: List[Entry]}` and exits 0. Its errors are describe's, with the same
exit 2. With an empty old list it is how `hirsel build --manifest` reads a
release.

The CLI prints, per old entry, `unchanged` (`Ready`), `resolvable` (`Resolve`)
or `gone` (`Gone`); then each NEW entry whose key is in no old entry as `new`;
then the grant strings added and removed, and any change of target, root limit
or `n_place`; and last `schema: unknown`. The preview cannot read the target
store's catalog, so it never reports schema readiness as known. It gates
nothing: it exits 0 whatever the differences are.

`hirsel confirm MANIFEST ADDR`<!-- doc-ref: planned --> reads `GET /hirsel/status` at ADDR under an
absolute deadline, 10 seconds by default (`--confirm-timeout`<!-- doc-ref: planned -->), and prints
`confirmed`, `not ready: <phase>` (a 503), `not listening`, `different:
<fields>` or `unknown` (an empty digest on either side, with the other fields
equal). It exits 0 only for `confirmed` (RUNNER-SPEC §12.2).

Laws: `preview_is_placed` (RUNNER-SPEC L31) checks the preview's answers against
a live session's `Placed` answers for the same list; `manifest_exact` (L35)
checks the grants, the root limit and `release`; `confirm_identity` (L32);
`place_run_bounded` also covers the preview child, which observes no store or
listener.

## Dry run and dev loop

*(See RUNNER-SPEC v2.45 and STORE-SPEC v2.57.)*

`hirsel dry-run --exe NEW --store DIR` opens a private copy-on-write image of the place's offline store at DIR, runs NEW's `on_migrate` and then `schema` in mode `Migrate` against it, and prints what an activation would do: each table and index the declaration adds, drops or renames, each backfill with its row count, each unique index whose backfill would fail with the first conflicting key pair, and the sizes `store.sized` derives for the place's root limit with any `Shortage`. It commits nothing to DIR and exits nonzero when the activation would fail. `hirsel dev` (below) activates each edit through the ordinary install and reopen. Neither verb has a mode the serving path lacks: both run the ordinary place open (RUNNER-SPEC §6). Each backfill line names why the index rebuilds (`new index`, or the inputs whose revision changed). A store another process holds exits 75. `hirsel store verify DIR --release EXE` and `hirsel store repair DIR --release EXE` start the release the same way, verify on a copy and repair on DIR itself; launch word 20 says which (0 serves, 1 dry-run, 2 store verify, 3 store repair, 4 tapes check), and [STORE-SPEC §15.6](../../core/programs/store/STORE-SPEC.md) has what verify and repair do. `hirsel tapes check DIR --recorded WRITER --release WITNESS` starts WITNESS on a copy and carries WRITER's 32-byte executable digest in launch word 27. [RUNNER-SPEC §7](../../core/programs/runner/RUNNER-SPEC.md#7-recording-and-re-execution-r5-at-store-spec-v24) defines the check.

Backfill counts give the source rows read and the rows emitted into the
preview image. A unique backfill reports the first duplicate by encoded
index key, then the first two encoded primary keys for that key. The preview
keeps the first row in its scratch index so it can check later declarations.
Any such conflict makes the migration fail.

Dry run takes the Store's lock while it captures the private image. It
refuses a store held by another process. The candidate uses the root limit
embedded in the executable and the same Store sizing overrides it would use
when serving. Arguments after `--` go to the candidate.

### The dev loop

`hirsel dev` keeps one place serving the bytes on disk while the developer edits them. Each
edit that changes what the place runs is an ordinary activation: the same stop, install and
start a standalone upgrade uses ("Place activation"), against a store that persists across
sessions. Dev speaks no wire protocol. The listener belongs to dev and each serving generation
inherits it, and every client recovers across a swap by itself, as it does across a production
restart (the SDK client restores its subscriptions).

**Built today and planned.** Main runs the older loop: a temporary store, a stat poll, a child
`hirsel build`, and a session proxy that holds the public socket and replays each client's
Hello and Subscribe on the new backend. B24's law (`cli_dev_restores_subscription` in
`tools/tests/wire_admission.py`) checks that proxy and stays green until the replacement laws
below land with the proxy's deletion. Everything else in this section is specified, not built;
the forms marked planned are the ones that do not exist yet.

**Commands.**

| Form | Does |
|---|---|
| `hirsel dev [SERVER] [--listen ADDR] [--store DIR] [--fresh] [--sdk DIR] [--n-place BYTES] [--json]`<!-- doc-ref: planned --> | runs the loop |
| `hirsel dev [SERVER] --wait [--timeout SECONDS] [--json]`<!-- doc-ref: planned --> | blocks until the bytes on disk serve, or have failed |
| `hirsel dev [SERVER] --status [--json]`<!-- doc-ref: planned --> | prints the loop's latest event |
| `hirsel dev [SERVER] --stop`<!-- doc-ref: planned --> | stops the loop |

`SERVER` is a package directory with a server entry and defaults to the current directory.
The loop also takes the build options `--limit`, `--grant`, `--dir`, `--backend`,
`--libraries`, `--runtime`, `--source-core` and `--source-std`, and passes arguments after `--`
to the place. Without a `[program]` run configuration dev builds with the store's `--dir`
first, then the user's `--dir`, `--limit` and `--grant`, then the root limit `1610612736` unless
`--limit` names one, `Clock`, `Random`, `Tasks`, `Process` and `Stderr`, and `Net=Listen
127.0.0.1/32 0-65535` unless a `Net` grant is given. A package whose `[program]` declares
`limit`, `grants` or `dirs` is the build's only source of them ([packages.md](packages.md)):
dev adds none, the three flags are refused as for `hirsel build`, and the first `dirs` path is
the store slot.

| Option | Meaning | Default |
|---|---|---|
| `--listen ADDR` | the address clients connect to; loopback only | `127.0.0.1:7700` |
| `--store DIR` | use this existing or new store directory; dev never deletes it | the current dev-owned generation |
| `--fresh`<!-- doc-ref: planned --> | start a new dev-owned store generation; refused with `--store` | off |
| `--sdk DIR`<!-- doc-ref: planned --> | publish the TypeScript SDK to DIR for every activated generation | none |
| `--n-place BYTES` | the one allowance for describe, install, serve and SDK generation | 16 MiB (`16777216`) |
| `--json` | one JSON object per line on stdout and nothing else there | off |
| `--timeout SECONDS` | with `--wait`<!-- doc-ref: planned -->, give up after this long | none |

The control forms talk to the loop holding `SERVER`'s dev directory, as `hirsel serve --stop`
does to a resident compiler. Builds go through the resident compiler (`--resident`), which
stays running when dev exits.

**One allowance.** A describe has no `PlaceConfig`, so dev cannot learn `PlaceConfig.n_place`
without opening the store. Dev therefore passes the allowance explicitly on every launch it
makes, from `--n-place`, defaulting to the describe default of 16 MiB, and prints it in the
`serving` line. An app's `PlaceConfig.n_place` does not apply under dev unless `--n-place`
names it. So a supplied store that the app activated at its own allowance has another identity,
and dev's first start migrates it unless `--n-place` names that allowance. Every launch uses
this argv order, so the allowance is inside the private prefix:

```text
<exe> --hirsel-n-place=N [--hirsel-install <install words>] <dev's store grant> <user --dir grants…> <user arguments…>
```

The store's `Dir` grant takes the first grant position whatever `--dir` values the user
supplies. The compiler's store is a separate directory and is never the place's store. Dev's
install releases every held run, so a run in flight across a rebuild continues on the new code
at its next attempt (RUNNER-SPEC §14.6 "The activation fate"). The fate rides on the install
launch itself: the runner maps dev's predecessor install to the fates `all: Release`. So dev
passes no `--hirsel-held`<!-- doc-ref: external --> word and refuses one among the program arguments, as it refuses a
launch selection.

**Identity.** Dev compares identity digests (RUNNER-SPEC §6 "One identity") computed from
`Describe` by a driver port of the runner's `identity_digest`. The port's oracle is a vector set
the runner package emits, `(describe bytes, identity digest)`, for a default allowance, a
nondefault allowance, an HTTP-binding change, a slot-schema change and a backend-only change.
Dev never treats `Describe.digest` (the executable digest) or `Status.release` (the exported
list's digest) as the identity.

**The dev directory** is `.hirsel-dev/` in `SERVER`:

| Path | Content |
|---|---|
| `dev.lock` | `flock` held by the running loop for this package |
| `dev.sock` | control socket for `--wait`<!-- doc-ref: planned -->, `--status` and `--stop` |
| `stores/<G>/` | dev-owned store generations. `<G>` is a decimal dev creates with `mkdir` and never reuses; each holds a `.hirsel-dev-owned` marker written at creation |
| `stores/current` | a regular file naming the current `<G>`, written by temp file, rename and parent sync |
| `gen/<N>/place` | immutable executables. Dev keeps the serving one, the candidate, and both OLD and NEW of any unresolved install |
| `state.json` | a locator only: the store path, the last known OLD and NEW executable paths and identity digests, and an interrupted attempt. It never authorizes a migration or a deletion |

**Per-store supervisor lock.** `dev.lock` excludes only this package's loops, and another
package's dev can name the same store with `--store`. Dev records its PID in
`hirsel-dev/<st_dev>-<st_ino>.lock` and locks that file in the runtime root selected
as for [the resident compiler](cli.md#resident-compiler), with `/tmp/hirsel-dev-<uid>/`
as its last fallback. Dev also locks a private descriptor of the store directory for its
whole life so a second dev on the same store refuses even with a different runtime root.
Other writers are fenced by the store's own `LOCK` and by the install's predecessor check
(rule 6).

**States.** A **receipt** is the SHA-256 over the canonical list of `(path, content digest)` of
every input a build read, plus the digest of the compiler executable that served it. The build
returns it, on failure too. Content digests come from the bytes the build captured, never from
file metadata, and the build compiles the same capture it hashes, so a tree that goes A, B, A
during a build yields the receipt of the bytes compiled.

| State | Serving | `--wait`<!-- doc-ref: planned --> |
|---|---|---|
| `Building` | the previous generation, if any | blocks |
| `Swapping` | nothing; connections wait (rule 10) | blocks |
| `Serving` | generation `g` | 0 at the success observation |
| `Broken(build)`, `Broken(discover)` | the previous generation, if any | 1 at the failure observation |
| `Broken(migration)` | OLD, reopened | 1 at the failure observation |
| `Broken(start)`, `Broken(crashed)` | nothing | 1 at the failure observation |
| `RecoveryPending` | nothing; dev makes no serving claim | 1, with both identities |

Rules:

1. **Start.** Take `dev.lock`. The store is `--store DIR`, else the generation `stores/current`
   names, else a new generation; with `--fresh`<!-- doc-ref: planned -->, always a new one (rule 11). Take the per-store
   supervisor lock, bind the listener, build and describe. Then start the candidate with a
   plain reopen (no selection) through the launcher's child path with the host channel. On an
   empty store that is genesis. A `PlaceOpened` reply proves the store holds this identity and
   writes nothing; dev records the store ID from it. Only a `PlaceCurrent(version, read)` reply
   leads to an install: the store holds another identity, `read.binding`, and dev installs with
   it as the expected predecessor and `read`'s store ID as the expected store (rule 6), whether
   or not `state.json` locates an executable for it. A held store,
   corruption, `IdentityUnknown`, a failed readiness probe and a missing proof are each their
   own `broken` cause and never lead to an install. `state.json` is updated after each typed
   outcome; a crash before the update leaves a stale locator, and the next start's plain reopen
   decides.
2. **Change detection.** The watch set is the build's input set: every file workspace discovery
   reads (manifests, `[sources]`, `tests.sources`, foreign archives) and the directories that
   hold them. inotify watches directories, never files. Watches survive a failed discovery and
   are rebuilt on `IN_Q_OVERFLOW`, `IN_IGNORED` or replacement of a watched directory, and a
   2 s rescan is the floor. A new file reaches the build only when a manifest names it. Events
   only wake the loop; the receipt decides.
3. **Debounce.** A cycle starts 50 ms after the last wake and at most 500 ms after the first.
4. **Busy.** A change during `Building` or `Swapping` never cancels the cycle. When it settles,
   dev captures again and runs another cycle if the capture differs from the receipt.
   Settlement is serialized with child exits and SDK publication.
5. **Three-way gate.** Against the serving generation:
   - the same executable digest: nothing the place runs changed. Dev discards the candidate,
     records its receipt as serving and reports `unchanged`. The place keeps running;
   - the same identity, another executable (a foreign archive, a backend option, a lowering
     change): stop and reap OLD, start NEW with a plain reopen and require `PlaceOpened`
     (rule 6a). No install runs. A refusal is `Broken(start)`, and dev reopens OLD as in rule 7;
   - another identity: rule 6. An install always migrates and writes a new activation, so dev
     never installs an identity the store already holds.
6. **Swap.** Record OLD's and the candidate's immutable paths in `gen/` and `state.json` before
   the cut. Send OLD SIGTERM, wait up to 5 s, send SIGKILL, and reap it. Then install NEW
   through the launcher's child path with two install words: the **expected predecessor
   identity digest**, which the runner compares with the store's current activation binding
   under `LOCK` before `on_migrate` and refuses `InstallPredecessor(found)` on drift; and the
   expected store ID (launch word 14), which the install checks. That install answers
   `PlaceInstalled(v, _)`, `PlaceInstallRefused(cause)`, or nothing when uncertain, and the
   launcher accepts `PlaceInstalled` without a target version. Dev never uses `InstallAt`,
   whose target is the store's commit head, which advances with every commit. The install
   settles every held run `Release` in its activation commit. Then:
   - `PlaceInstalled(v)`: start NEW with a plain reopen; it must reply `PlaceOpened` at `v`;
   - `PlaceInstallRefused(cause)`: rule 7;
   - no reply, a nonzero exit without a reply, or a dead child: rule 8.

   6a. **Ready.** A generation serves only when its own child has sent `PlaceOpened` on the host
   channel and `GET /hirsel/status` answers `phase: "ready"` (not `paused`) with the expected
   executable digest and allowance, within 10 s; otherwise `Broken(start)`. Status and stop
   answer throughout build, install and this wait.
7. **Refused install.** The refusal is typed and came before the cut, so OLD's activation
   stands. Dev reopens OLD with a plain reopen, requires `PlaceOpened`, and enters
   `Broken(migration)` with the refusal and the install's stderr. If OLD's reopen is refused,
   dev enters `RecoveryPending` instead of claiming OLD serves.
8. **Uncertain install.** A failed install does not imply OLD reopens: after the durable cut a
   lost proof can leave NEW current. Dev probes without installing: a plain reopen of NEW, and
   if NEW is refused, a plain reopen of OLD. Whichever replies `PlaceOpened` is current and
   serves (`Serving` for NEW, `Broken(migration)` for OLD). If neither does, dev keeps both
   executables, reports `RecoveryPending` with both identity digests and the refusals, and makes
   no serving claim. The next cycle probes again before any install. Dev never runs a second
   install to resolve an uncertain one. The persistent-store guarantee covers a local directory
   backing; on an objects@1 backing dev reports the backing at start and the probe rules of
   "Hot deploy" apply.
9. **A place that exits by itself** puts dev in `Broken(crashed)`. The next cycle still needs a
   live child and rule 6a before dev reports `Serving`.
10. **Listener.** Exactly one acceptor per phase. Dev's parent keeps the public listener open
    continuously. Only a serving child inherits it, as FD 3 with `LISTEN_FDS`, and a child is
    spawned only after its predecessor is reaped. A connection that arrives between OLD's reap
    and NEW's start waits in the listener's backlog. When nothing will serve soon
    (`Broken(start)`, `Broken(crashed)`, `RecoveryPending`, or `Broken(build)` or
    `Broken(migration)` with no place), a dev worker accepts and closes each connection so that
    clients back off instead of hanging; dev stops and joins that worker before it hands the
    listener to a child.
11. **Fresh.** `--fresh`<!-- doc-ref: planned --> creates `stores/<G+1>/` with its marker, writes `stores/current` and
    starts there. It deletes nothing at start. A later start removes an old generation only if
    all of these hold: it is under `.hirsel-dev/stores/`; it was opened with `O_NOFOLLOW` and is
    not a symlink; it carries the marker; it is not `stores/current`; its `LOCK` can be taken
    without blocking, so no writer holds it; and it overlaps no source, SDK or compiler
    directory. Removal never deletes a held `LOCK` inode, which would let a second opener lock a
    new inode beside a live writer. A `--store` directory is never removed.
12. **Stop.** SIGINT, SIGTERM or `--stop` stops the place by rule 6's signal sequence, reaps it,
    removes `dev.sock` and exits 0.

**Inputs fixed for a session.** The trees named by `--libraries`, `--runtime`, `--source-core`
and `--source-std`, and the build options, are not watched. Changing them needs a dev restart.
A replaced compiler executable is in the receipt, so it does trigger a cycle.

**Clients across a swap.** Every open connection belongs to the child that accepted it and
ends when that child stops.

| Client | Unchanged surface | Changed surface |
|---|---|---|
| SDK client, bound Caller | closes, backs off, reconnects with the same Caller, resubscribes with new IDs and replays its pending request; `admit(p)` returns the stored reply or runs a request that never committed | one `Placed(Gone)` for any want makes the client `Incompatible` before it sends its pending call; nothing reloads the page unless the app's handler or the bundler does, in no guaranteed order |
| SDK client, anonymous | a new Caller on reconnect, so a pending call settles `LocallyUnknown` and its effect may exist | as above |
| Page reload | promises and room handles are gone; a new client with the durable stream reports recovery through its callback | as left |
| Raw WebSocket, HTTP | the socket closes during the swap; a raw client needs its own reconnect, Hello, placement and subscriptions, and an HTTP request can lose its reply | as left |

Dev prints no warning about anonymous Callers; the client explains `LocallyUnknown` when it
happens. There is no `Upgrade` frame: a changed surface reaches the page as `Placed(Gone)` and
then `Incompatible` (RUNNER-SPEC §6 "Matching an old client").

**Browser origin.** The runner's WebSocket ingress refuses an upgrade whose `Origin` header is
present and is not `http://` + `Host`, under a launch option dev passes; a request with no
`Origin` (a native client) passes. It defends a loopback place from pages of other origins in
the developer's browser. The supported bundler topology is the bundler proxying the place's
path: the page loads from the bundler (say `http://127.0.0.1:5173`) and the bundler forwards
the WebSocket upgrade to `--listen` keeping its own `Host` (Vite's `server.proxy` with
`ws: true` and `changeOrigin: false`), so `Origin == http://` + `Host` holds. A page that
connects from the bundler's port directly to `--listen` is refused. There is no allow-list.

**SDK publication.** `--sdk DIR`<!-- doc-ref: planned --> and `hirsel sdk ts` publish through one procedure, which
defends DIR's parent against an externally supplied directory holding user files:

1. **Lock.** Take a publisher `flock` on `<parent>/.<name>.hirsel-sdk.lock`, held to the end.
2. **Stage.** Render into `<parent>/.<name>.next-<pid>` on the same filesystem and fsync every
   file, the stage and the parent. A stage left by a dead publisher is removed under the lock.
3. **Validate.** DIR must be absent, empty, or owned: a real directory (not a symlink) whose
   inventory is exactly what the generator emits for the release its own `manifest.json`
   names: the fixed files plus each entry's stub path and the directories those paths imply.
   Any other file, directory or symlink is unknown. DIR must not contain or be contained by
   `SERVER`'s sources, the dev directory, the store or the compiler store. Anything else
   refuses with exit 2 and touches nothing.
4. **Identical.** If DIR already holds byte-identical content, delete the stage and stop.
5. **Publish.** Under the same lock validate again; a destination that changed since step 3
   refuses. Rename the stage to DIR if DIR is absent, else exchange them with
   `renameat2(RENAME_EXCHANGE)`. Fsync the parent, then remove the old tree, now at the stage
   name. `EINVAL`, `ENOSYS` or `EXDEV` from the exchange is a refusal naming the filesystem;
   there is no per-file fallback.

The exchange publishes a pathname atomically; it is not a snapshot across opens. A reader that
resolves files through DIR by path sees one generation per resolution. A reader that needs
several files from one generation compares `entries.js`'s `identity.digest` with
`manifest.json`'s `launch.digest` and retries the pass on a mismatch or `ENOENT`. In dev, steps
1-3 run while OLD serves, and steps 4-5 after a successful install and before NEW starts, so the
SDK never names a release that is not activated. Dev also publishes at genesis, on a plain
reopen, and whenever DIR is missing or not current for the activated release. If publication
fails after an install, NEW stays activated, dev reports `serving` with `sdk: "failed"` and the
cause, and retries from NEW's describe on the next wake without reinstalling.

**Output.** The first stdout line is `hirsel dev listening on <addr>`, printed once the first
generation serves, even if that is after a failed first build. Status lines go to stderr, and
the compiler's and the place's own output passes through. Nothing clears the screen.

```text
hirsel dev listening on 127.0.0.1:7700
hirsel dev: serving generation 1, store .hirsel-dev/stores/1, allowance 16777216
hirsel dev: generation 2 serving (built 212 ms, swapped 140 ms, sdk published)
hirsel dev: unchanged
hirsel dev: build failed; generation 2 still serving
hirsel dev: migration refused; generation 2 reopened
hirsel dev: recovery pending: neither <new> nor <old> opens the store; waiting for a change
```

With `--json`, stdout carries one object per line and nothing else: the place's stdout goes to
dev's stderr, and dev drains both child pipes. Every object has `event`, `generation` (or
`null`), `attempted` (the receipt of the latest attempted capture), `active` (the receipt
serving, or `null`) and `activated` (the identity digest the store holds as far as dev has
proof, or `null`).

| `event` | Extra fields |
|---|---|
| `serving` | `listen`, `store`, `identity`, `n_place`, `build_ms`, `swap_ms`, `sdk` (`"published"`, `"identical"`, `"failed"` or `null`), `sdk_error` |
| `unchanged` | `build_ms` |
| `broken` | `cause` (`"build"`, `"discover"`, `"migration"`, `"start"`, `"crashed"`, `"held"`, `"identity"`, `"store"`), `detail` (at most 64 KiB), `status` |
| `recovery_pending` | `old`, `new` (identity digests), `detail` |
| `building`, `stopped` | none |

`--status --json` prints the latest object, and `--wait --json`<!-- doc-ref: planned --> the observation it settled on.

**`--wait`<!-- doc-ref: planned -->.** A waiter's request makes the loop take a fresh capture C after the request
arrives. The **success observation** is the first point where the loop is `Serving` a
generation `g` whose receipt equals C's digest, whose child passed rule 6a, and whose SDK
publication is `published` or `identical` (or `--sdk`<!-- doc-ref: planned --> is off); `--wait`<!-- doc-ref: planned --> prints `g` and the
receipt and exits 0. The **failure observation** is a settled `Broken(*)` or `RecoveryPending`
with a receipt equal to C's digest, or C itself failing discovery (a malformed manifest, a
missing named file) with a second capture after the debounce giving the same cause and the same
partial digest; `--wait`<!-- doc-ref: planned --> prints the cause and exits 1. A waiter never waits for an edit later
than its own capture, and later edits can supersede what it reported.

| Command | 0 | 1 | 2 | 75 |
|---|---|---|---|---|
| `hirsel dev` | stopped by a signal or `--stop` | an I/O failure the loop cannot continue past | refused before serving | — |
| `--wait`<!-- doc-ref: planned --> | success observation | failure observation | usage | no loop, or `--timeout` passed |
| `--status` | a loop answered | — | usage | no loop |
| `--stop` | the loop stopped | — | usage | no loop |

**Refusals** are one block on stderr with exit 2:

| Case | Text |
|---|---|
| `--listen` is not loopback | `dev listens on a loopback address` |
| another loop holds `dev.lock` | `hirsel dev is already running for <dir> (pid N)` |
| another dev holds the store's supervisor lock | `hirsel dev is already supervising store <dir> (pid N)` |
| another process holds the store | the store's own `Held` refusal |
| `--fresh`<!-- doc-ref: planned --> with `--store` | `--fresh starts a dev-owned store; it cannot be combined with --store`<!-- doc-ref: planned --> |
| `--store` or `--sdk`<!-- doc-ref: planned --> overlaps sources, the dev directory, the compiler store or each other | `<dir> overlaps <other>` |
| `--sdk DIR`<!-- doc-ref: planned --> is not absent, empty or owned | `<dir> is not an SDK directory hirsel generated; it holds <first unknown entry>` |
| the exchange is unsupported | `<dir>: directory exchange is not supported on this filesystem` |
| `SERVER` has no server entry | the build's refusal |
| run inside the resident compiler | `dev must run in the terminal process` |
| an option dev does not take | `unsupported dev option` |

**Laws** (the dev/add design's slices S1-S5; cases in the package-directory harness of
`crates/hirsel-core-driver/src/test_run_native.rs` unless named):

- data written in one session is read in the next (the store is not recreated);
- changed code installs and unchanged code reopens, by the typed replies (`PlaceInstalled(v)`
  then `PlaceOpened` at `v`; a third session `PlaceOpened` at the same `v` with no install);
- a refused migration keeps OLD serving on OLD's data;
- the three-way gate: an unchanged executable keeps the child PID, and a backend-only change
  gets a new PID at the same activation version with no install;
- destructive ownership (release gate): `--fresh --store X`<!-- doc-ref: planned --> refuses; a supplied store, an old
  generation that is a symlink, lacks the marker, holds a live `LOCK` or overlaps sources is
  never removed; a held `LOCK` inode is never unlinked;
- the allowance and identity match the runner-emitted vectors, and a nondefault `--n-place`
  session reports it in `Status.n_place`;
- fencing: a different writer's install between OLD's exit and NEW's install is refused
  `InstallPredecessor`; a lost install reply after the cut probes NEW current and before it
  probes OLD current, judged by a separate plain reopen afterwards;
- watches see a rename-over, see a new file once a manifest names it, and an A, B, A edit during
  a build serves B's receipt and then a further cycle serves A; a warm resident build equals a
  fresh one-shot build of the same capture;
- an SDK reader following the reader contract never pairs two generations; after a refused
  migration `manifest.json` names OLD; publication never deletes user files (release gate);
  publication after an install failure does not reinstall;
- `--wait`<!-- doc-ref: planned --> right after an edit returns only once the edit serves; a broken edit exits 1 and its
  fix exits 0; no loop exits 75; concurrent waiters each settle on their own capture;
- swap recovery, replacing B24's same-socket assertion in `tools/tests/wire_admission.py`: a raw
  socket closes across a swap with a retryable code; a fresh raw Hello with the old want is
  answered `Placed(Gone)`; a regenerated client's want subscribes and reads the new value; an
  unchanged want resubscribes on a new socket with new IDs and reads the current value. A bound
  SDK tab's lost reply on both sides of the commit executes once (the stored reply returns after
  it); an unapproved origin is refused and a native client without `Origin` admitted; a
  connection queued between OLD's reap and NEW's start completes its Hello on NEW.

## Platform host and launcher

This section specifies how many programs from many authors are run on one machine. It was
ratified as Agent Platform 2027, Topics 2 and 3 (2026-11-22). The launcher is built as
`hirsel launcher` ("The launcher" below says which of its parts), and the platform host
and its catalog as the store package's `platform` and `platform_main` modules. `hirsel host`
starts the pair, and `hirsel register`, `hirsel deploy` and `hirsel replay --state` are its
operator clients ("The operator CLI" below). The cold-place wake contract they rely on is
RUNNER-SPEC §14.

### Authority groups

An **authority group** is the unit a platform runs. One group is one program, one release
recipe R, one set of grant bindings, one store, and while it is live, one process that holds
that store. The group's name is its key in the platform catalog. It never changes, and no
two groups share a store directory. A deploy replaces a group's recipe and keeps its name and
store. Grants are approved per group at each deploy.

- **One process while live.** At most one process holds the group's store (STORE's `LOCK`).
  A hot deploy's prepare child opens only the export and the staged migration, never the
  store. Preview and describe children open no store. So a cutover runs OLD's head, then
  NEW's head, and never both. A second head is refused `Held` by the store itself.
- **Tenants.** A **tenant** is a billing and quota owner. A tenant owns any number of groups,
  and a group belongs to exactly one tenant. Groups of one tenant are separate by default. Two
  agents that act for separately authorised principals are two groups, even when one tenant
  pays for both, because one store and one process cannot keep their authority apart. Groups
  share nothing: no store, no process, no grant and no listener. Where one group serves
  another, it is a wire call between two places, under the callee's `Caller` policy
  (RUNNER-SPEC rule 13).
- **What an untrusted author submits.** An author whom the operator has not named a trusted
  publisher submits source only. The platform publishes that source itself with the normal
  checker, under its own compiler caches and package images and the limits of "Authority"
  above (the launcher's `Source` input, "Publish and assemble" below). It never accepts a raw R, a code object or an image from such an author, because
  admission does not check types or ownership again (step 8 above). Foreign code, an
  `Exec` grant and a recording build are refused for a group whose author is untrusted
  (FFI-1, EXEC-1).

### The platform host

The **platform host** is a Hirsel program. It is itself a place with its own store, and it
runs under the launcher like a group, as the operator's group. Its catalog is ordinary
tables of that store:

- `groups`: per group name, its tenant, its working directory, its author class (trusted or
  untrusted), its store directory and release memory allowance, its current assembly record
  and activation version, its retained activations' assembly records, and its listener address;
- `wake`: per group name, the one wake row of RUNNER-SPEC §14 (`Dirty(gen)` or
  `Clean(Option[next_due])`) and its generation counter;
- `deploys`: each pending deploy attempt and its assembly record (the "pending record"
  above);
- `pins`: per 16-byte recording ID, the 32-byte name of the assembly record the recorded
  process runs and its expiry in Unix seconds. The host makes the row durable before it
  starts the recorded process. A lookup at time `now` finds the assembly only while
  `now < expiry`; an absent row, an expired one and one an operator removed are all `Gone`.
  A store error is an error, never `Gone`;
- `quotas`: per tenant, its limits on groups, live groups, bytes and deploy rate;
- `config`: the host's `max_places` and its other bounds.

The host does routing, quotas, wake scheduling and deploy orchestration. It decides which
group runs, when it starts and stops, which recipe a deploy installs, and which objects are
still roots. It does not compile, assemble or spawn. It asks the launcher for those
over the launcher's control socket.

**Bounded workers.** "10,000 listeners" means 10,000 groups that are durably registered and
wakeable. It does not mean 10,000 processes. At most `max_places` groups are live at once
(RUNNER-SPEC §14.2). A group that is not live costs a catalog row, a wake row and one
listening socket that the launcher holds.

**Routing.** Each group has one TCP listener, created by the launcher when the group is
added and held by it for the group's whole life. A serving child receives it as its
inherited listener ([net](net.md), `LISTEN_FDS`). Dropping it in the child closes only the
child's descriptor, so the accept queue outlives the child. Cutover and a restart therefore
lose no connection that is in the backlog. A connection to a group that is not live waits in
that backlog. This holds across a child's end and a host restart; a launcher loss loses
the backlog with its only holder (RUNNER-SPEC §14.5). The launcher reports the first readable listener of a cold group to the host
as `Arrived(group)`, and the host wakes the group (RUNNER-SPEC §14.3). Clients find a group's
address from the host's routing table. The host never proxies bytes.

**Running the host.** The launcher starts the host as its first group. Its granted `Dir`
is its store. Its arguments are the Unix root label, the path to the launcher's control
socket, `max_places`, `idle_ms`, and the operator socket's path. The host opens its catalog,
connects, sends `Hello`, and then runs one loop over one queue. A reader task posts each
launcher event into the queue in stream order, beside the host's two alarms: one absolute
wall-time alarm on the earliest due time (RUNNER-SPEC §14.3) or source upload deadline,
and one monotonic alarm on the earliest backoff retry (§14.5). After every input the host applies it, sends the requests
its commit allows in order, and re-arms each alarm on its current minimum, so neither wait
is armed only at start and nothing polls. A lost session, or a frame that does not decode,
ends the host with status 75. The launcher starts it again, and the new session's inventory
reconciles it.

**Quotas.** A start, a deploy or an added group that would exceed its tenant's quota is
refused with the quota it exceeds. The host never evicts a live group to admit another
(RUNNER-SPEC §14.2, "No eviction").

### Starting the host

`hirsel launcher --state S --control S/launcher.sock --lifecycle managed|leader
--operator-listen S/operator.sock --admission FILE --max-places N [--idle-ms M]`
starts the platform host as its first and only
bootstrap child. `--admission` and `--max-places` are required whenever the launcher starts
the platform host. Passing either option or `--idle-ms` without `--operator-listen` is a
usage error. Linux requires `--lifecycle` ("Child lifecycle" below).
N and M are positive decimals. `--max-places` has no default and limits the live groups.
`--idle-ms` defaults to 30000. The launcher binds both sockets
mode 0600 and creates `S/catalog`. The released `hirsel` executable embeds the native
`platform_main` image in an ELF section on Linux and a signed Mach-O section on macOS.
The launcher stages that image in a private temporary directory for its lifetime and
starts it with working directory S and the program-visible argv
`platform_main ctl launcher.sock N M operator.sock`. Direct launcher invocations can
select an external host with `--platform-host FILE`. The host's grants are compiled into the image.
Its `Dir` is `/proc/self/cwd/catalog` on Linux and `./catalog` on macOS. Its Net root `ctl`
is `/proc/self/cwd` on Linux and `.` on macOS, with `UnixConnect ctl launcher.sock` and
`UnixListen ctl operator.sock`, beside Clock, Random and Tasks. The offline pin reader
uses the same `Dir` path. The store backup and restore helper's two `Dir` grants are
`/proc/self/fd/3` and `/proc/self/fd/4` on Linux and `/dev/fd/3` and `/dev/fd/4` on macOS.
Nothing in the host changes its working directory. A missing host image ends the launcher
with status 74.

**The admission envelope.** FILE declares the published-release admission bounds as
`recipe_bytes`, `closure_objects`, `object_bytes`, `decoded_bytes`, `members` and `edges`,
one positive decimal `key=value` per line. `closure_objects` also fixes the redundant
object-count reservation, while the decoder keeps its own hard nesting ceiling. The
launcher refuses a missing, repeated or unknown key. Before it opens the operator socket
it writes the effective envelope to `S/admission`, so an attested publisher reads the
host's bounds before upload; the operator CLI uses that file for its local recipe read and
object exchange. The envelope governs new admissions. Previously admitted releases keep
their assembly records and can still start, replay, or be collected after a grant changes.

**The operator listener.** The launcher passes the operator listener as descriptor 3 and
names it in `HIRSEL_OPERATOR_LISTEN=PPID:3:UID`: the launcher's PID, the descriptor and the
operator's UID (the launcher's effective UID). The host's runtime takes it at startup only
when PPID is its parent and the descriptor is a listening `AF_UNIX` stream socket, marks it
close-on-exec and removes the variable. The host's first `listen_unix` adopts it, and only
when the grant's root and path name the very socket it is bound at (same inode); any other
first listen is Denied. Accept on the adopted listener reads the peer's `SO_PEERCRED` and
closes a connection from any other UID before the program sees it. No other listener is
affected, and a tenant never receives descriptor 3 or the variable.

**The control peer.** While the launcher runs a bootstrap host, a connection to launcher.sock
is accepted as the session only when its `SO_PEERCRED` PID is the live host's; any other is
closed before it can replace the session or end its fence.

**Restart.** The launcher watches the host's pidfd in its one epoll set. When the host ends
with 75, by a signal or with any status other than 64 and 74, the launcher reaps it and
starts it again with the same argv: at once the first time, then after 1, 2, 4 ... s up to
5 min. A run of 5 min or more resets the delay to immediate. Groups, listeners, retained
cutover replies and cgroups stay live across the restart, and the new host's `Hello`
reconciles them. A host that ends 64 (usage) or 74 (catalog or installation) is not started
again: the launcher ends with that status.

**Operator loop.** Beside the launcher reader and both alarms, an accept task posts each
operator connection into the host's one queue, a reader task per connection posts its
frames, and a writer task per connection sends its answers in order. A logical reply is
bounded at 64 MiB. One larger than the 16 MiB frame limit is sent as ordered `Part` chunks
whose frames fit that limit. The host charges queued frames, including the frame being
written, for their wire bytes. The connection's byte budget fits one maximum-size reply
with all its frame headers. Each completed write releases its charge. The host detaches a
connection on a reply over the logical bound, a queue over its byte budget, or a write
failure. A `Begin` before the session's inventory is
reconciled is answered `Error(Busy)`.

### The operator socket

The operator talks to the platform host over `STATE/operator.sock`, a filesystem Unix
stream in the operator-owned state directory (mode 0700; the socket node 0600). It is
separate from `STATE/launcher.sock`, which stays private to the host: a deploy client never
opens a launcher session, because a new connection there replaces the session and drops its
fence. The launcher binds the passive operator listener under its lifetime lock and passes
it only to the host. The host alone accepts on it, and before it decodes a byte it checks the
peer's effective UID against the configured operator UID (Linux `SO_PEERCRED`). A
peer-supplied name, PID, tenant or request ID conveys no authority. No tenant receives either
endpoint or its directory. (The host's handling is the `operator` module and
`platform.asked`/`platform.detach`; `platform_main` runs the accept task over the inherited
listener.)

One connection carries one exchange. Each frame is a little-endian u32 byte length followed
by exactly one value-codec sum; the length excludes itself and is at most 16 MiB. Tags are
u32 in each direction's own namespace, in the declared order of `operator.Ask` and
`operator.Answer`. D is a client-generated random 16-byte request ID (Bytes), A a
host-generated `Attempt(hi, lo)`, G a `Group(name, incarnation)`, H 32 bytes and I a 16-byte
recording ID. A wrong length, an unknown tag, trailing bytes or an out-of-phase frame is
answered `Error(Protocol)` and closes only that exchange.

| Operator → host | Payload and rule |
|---|---|
| 0 Begin(D, G, input, foreign, exec, held) | `input` is 0 Attested(bytes), 1 Publication(H) or 2 Source(manifest, gate), with `manifest` a `launcher.Manifest`'s codec bytes: a source named by content, whose blobs follow as Data ("Source by content" below). A manifest over 1 MiB, or one that breaks its rules, is `Invalid`. `gate`, a bool, asks for the law gate ("Self-editing"); only a source carries one. `foreign` and `exec` are the sorted, duplicate-free Foreign definitions and Exec requests the operator approves. `held` is the deploy's `Fates { all: Option[HeldFate], entries: List[EntryFate] }`, with `HeldFate` tags 0 Release and 1 Cancel and `EntryFate(path, fate)`. It is the fate of each run the deploy holds (RUNNER-SPEC §14.6 "The activation fate"). `entries` that are not sorted by path or that repeat a path are `Invalid`. A source manifest whose files exceed 4 MiB in all is `Invalid`. For an untrusted author the host refuses, as `Denied` and before anything compiles, any input but a source, a source whose tree grants `Foreign` or `Exec`, any approved Foreign or Exec, and a recording group. |
| 1 Data(D, A, bytes) | One nonempty chunk of the admission byte stream, only on the connection that owns D's upload under A. For a source it is the needed blobs' bytes ("Self-editing"). |
| 2 EndInput(D, A) | Ends that stream. A publication needs neither Data nor EndInput. |
| 3 Confirm(D, A, displayHash) | Confirms the exact display the client checked, printed and flushed: displayHash is SHA-256 over the display's UTF-8 bytes. Any other hash is `Error(Denied)`. |
| 4 Status(D) | Returns State, or `Error(UnknownRequest)`. Does no work. |
| 5 ResolvePin(I) | Returns Pin, looked up at the host's wall clock in Unix seconds, or `Error(Store)`. |
| 6 LookupGroup(name) | Returns GroupInfo(G) for the registered incarnation, or `Error(UnknownGroup)`, also while the group is still registering. |
| 7 Register(D, registration, input, foreign, exec) | `registration` is (name, tenant, workdir, trusted, listen, store, memory, args, record, front, packages), with `memory` an `Option[u64]` in bytes, `record` an `Option` of (dir, retentionSeconds) and `packages` the group's package roots, each 32 bytes ("Package roots per group" below). Registers a new group and deploys its first release, below. `foreign` and `exec` are Begin's. `input` is 0 Attested(bytes), 1 Publication(H) or 2 Source(manifest), Begin's input without the gate: a Register is never gated. |
| 8 Decide(D, decision, verdict) | The operator's verdict on D, which must be in AwaitDecision on exactly this `decision` ("Self-editing"); anything else is `Error(Denied)`. Only this socket accepts Decide. `verdict` is 0 Approve(fates) or 1 Refuse. An approval's `fates` settle the held runs the decision shows and are `Fates(all: None, entries: [])` when it shows none; it repeats D's admission under a fresh A, this connection owning it. A refusal fails D `Denied`. |
| 9 SetEditor(G, editor) | Planned ("Self-editing"). `editor` is `Option[EditorGrant]`, with `EditorGrant { uid: u32, named: List[String] }`: the editor's UID and the sorted, duplicate-free `Secret`, `Cred` and `Env` binding lines of G's approved recipe that the operator lets an editor's code use. It sets or clears G's editor in one transaction and answers `GroupInfo(G)`. It is refused `Denied` unless G passes "What editing delegates"; a UID equal to the operator's or the launcher's is `Invalid`; the platform host's group is `Denied`. Only this socket accepts it. |

| Host → operator | Payload and rule |
|---|---|
| 0 State(D, G, input, phase) | A committed snapshot. `input` is Register's input type: a Begin's source appears as Source(manifest), without its gate. Phase tags 0 AwaitInput(A), 1 Admitting(A), 2 AwaitConfirm(A), 3 Assembling(A), 4 Preparing(A), 5 Pending(A, v), 6 Undecided(A, v, reason), 7 Installed(v, assembly, report), 8 Failed(class, detail, report), 9 AwaitDecision(decision), with `decision` the settler's `Decision` ("Self-editing"). Installed and Failed are terminal and end the exchange. AwaitDecision holds no attempt. A known D keeps the same G and input for its whole life. |
| 1 Need(D, A, keys, last) | One of the launcher's ordered parts of the missing set (`List[Wanted]`, bool), or, for a source, of the blobs the group's blob set lacks (`Wanted` gains 5 BlobKey(H), after the four object kinds' tags 0 to 4). |
| 2 Display(D, A, R, text) | The exact Foreign and Exec text to confirm, bound to the verified R. No child runs before it is confirmed. |
| 3 Pin(resolved) | 0 Found(assembly) or 1 Gone. |
| 4 Error(class, detail) | Class tags 0 Protocol, 1 Denied, 2 UnknownRequest, 3 UnknownGroup, 4 Conflict, 5 Busy, 6 Full, 7 Invalid, 8 Unassembled, 9 Store, 10 Uncompiled, 11 NotInstalledAfterRecovery, 12 Exhausted, 13 Red. Uncompiled carries a source's checker diagnostics; Exhausted is a source whose compile ran out of memory. Red is a gated source whose tests were not all green; its detail names each test that failed, stopped, did not start or had not ended by the admission deadline, with its outcome ("Self-editing"). A trigger report with a held run that has no fate, or with an `entries` path that is the entry path of no pending run and of no entry OLD or NEW registers, holds the request for a decision and fails nothing; the former class 13 `Unsettled` is deleted. Failed uses the same classes. An exchange error never changes another request's outcome. |
| 5 GroupInfo(G) | A group lookup's result. |
| 6 Part(total, offset, bytes) | One in-order chunk of an encoded reply larger than one frame, from offset zero, contiguous, ending exactly at total. |

A report is (fetchNs, assembleNs, pause, preview): two `Option[u64]`, a pause (0 NotStarted,
1 AwaitWrite, 2 Measured(ns), 3 MeasurementLost, 4 StartFailed(detail)) and the deploy's
preview ("Deploy preview"), whose parts not yet computed when the deploy ended are empty.

**Durable identity.** The catalog keeps one `requests` row per D: its canonical Begin bytes,
G, input, current attempt, admission deadline and status. The status starts `Open`, becomes
`Held(Decision)` while awaiting a decision, and returns to `Open` when approval repeats
admission. Finishing the request replaces its status with `Done(Phase)`, which keeps only
the terminal phase. The catalog also keeps one `active` row per group naming its active
request. The first transaction for a new D binds it to its Begin bytes and allocates A
from a durable counter before any launcher request. A Begin whose D is known is looked up
before any wake or admission. Different bytes
for that D are `Error(Conflict)`. Identical bytes return the request's current state and may
attach to its work: while another connection owns it, this one gets the snapshot and closes.
Each terminal deploy row and its request's terminal phase are written in one transaction, so a
retried D after any loss of the host reads the recorded outcome and admits nothing. Terminal
records outlive later deploys and the group's removal (removal fails the group's open
request); they are history, never collection roots.

**The disconnect boundary.** Before the input is complete or a required confirmation
arrives, losing the owning connection makes progress impossible. The host sends
`Release(A)`, drops the admission, and keeps D waiting for its input. An identical Begin
takes the upload again under a fresh A and repeats admission and Need from byte zero, under
the request's original deadline (600 s after its first Begin); a request that reaches its
deadline without input fails durably `Invalid`. A complete input goes on without its
connection: EOF only detaches the observer, and a later Begin or Status reads the outcome. A
new launcher session (`Hello`) cancels every running admission; its owner is told
`Error(Busy)` and resends the same Begin. An admission refusal (overlap memory) and `AdmissionFailed`
are D's durable `Failed`. A request held in AwaitDecision has no attempt and no deadline: it
outlives its connection and the host's restart, and ends at a Decide or when a later Begin of
its group supersedes it (`Failed(Denied)`).

**Registering a group.** A Register is a Begin for a group that does not exist yet, under
the same durable identity: a known D attaches or is a Conflict exactly as a Begin's. For a new
D, one transaction records the group's catalog row under a new incarnation, with no assembly
and **no wake row**, charges the tenant's group quota, and writes D's request and active rows
with a fresh A. The host then sends `Bind` for the group's listener and only after it the
`Admit` of the first release, whose approvals have no OLD (the summary's old root limit is 0
and every grant is new). The phases are a Begin's: Need, Data, EndInput, Display, Confirm and
Assembling. A group without a wake row is *registering*: no arrival, due wake or dirty scan
reaches it, LookupGroup and Begin answer `UnknownGroup`, and collection names no root for
it. When its release is assembled, one transaction makes it the group's assembly at
activation 0, gives the group its wake row and records D `Installed(0, assembly, report)`.
The host then sends `Release(A)` and wakes the group, whose first serving start at
activation 0 on its empty store is its genesis ("Place activation"). A registration that
fails (an admission failure, a refused confirmation, a lost deadline, or a listener the
launcher cannot bind) records D `Failed` and removes the group row and its quota charge in
the same transaction, then sends `Close` for its listener, so the name can be registered
again. A name already in the catalog is `Error(Conflict)`, unless its registration is still
running, which is `Error(Busy)`; a registration whose input outlived its deadline fails there
and frees the name. A second group naming a store directory another group already holds is
`Error(Conflict)`. A Register before the host has reconciled with the launcher is
`Error(Busy)`.

**Recording a group.** A group's `args` are the program's arguments in every serving start.
A group registered with a `record` has every serving start recorded: its 16-byte recording
ID is the group's incarnation and the start's generation, each a little-endian u64, so no
two starts share one and a start sent again after a host restart names the same pin; its
destination is `dir/<ID in lowercase hex>.tape`. The pin (ID → assembly, expiry = the
host's wall clock in Unix seconds + retentionSeconds) is written in the transaction that
allocates the start's generation, so no recording runs before its pin is durable. Every
admission for such a group, its registration and each deploy, asks the launcher for a
recording image (`Approvals.recording`), so each serving start can record; an image the
recording runtime cannot replay (a Foreign call) is refused before assembly.

### The operator CLI

`hirsel host --state S --admission FILE --max-places N --lifecycle managed|leader
[--idle-ms M]` runs the launcher of "Starting the host" with `--control S/launcher.sock`,
`--operator-listen S/operator.sock` and its embedded `platform_main` image.
It requires `--admission` and `--max-places`. Linux requires `--lifecycle`. On macOS it
defaults to `leader` and the launcher refuses `managed`. The state directory must be the
operator's and mode 0700.

The clients below connect to `S/operator.sock` only after checking that S is owned by their
own UID with no group or other permission. Each prints `request <D hex>` on stderr before it
sends anything, generating D unless `--request-id HEX` names it, so a client lost at any point
can be run again with the same D and reads the recorded outcome. A terminal outcome prints
one JSON object on stdout: the request, group, incarnation, phase and, when installed, the
activation, assembly and report.

- `hirsel register RECIPE --state S --group NAME --listen ADDR --store DIR --code-store DIR
  [--tenant T] [--workdir DIR] [--trusted]
  [--memory BYTES] [--grant G]... [--record DIR
  [--tape-retention DAYS]] [-- ARGS...]` sends Register with the attested recipe from the
  code store.
- `hirsel deploy RECIPE --state S --group NAME --code-store DIR [--grant G]...
  [--approve-grant +LINE|-LINE]... [--held release|cancel]
  [--held-entry PATH=release|cancel]... [--request-id HEX]` resolves G with LookupGroup, or with
  `--request-id` from the request's own Status, then sends Begin. When the request awaits a decision on exactly
  the diff its `--approve-grant` lines name (`+` for a line gained, `-` for one lost), it
  sends Decide approving it, with the fates its `--held` and `--held-entry` options name, and goes on; otherwise it prints the state, with the decision and its trigger report, and
  exits 2. `--held` sets `held.all`, and each `--held-entry` adds an element of `held.entries`, where PATH is the entry path as the trigger report prints it. Without either option, `held` is `Fates(all: None, entries: [])`. `hirsel deploy --state S --status D` sends only Status.
- `hirsel decide --state S --request-id D`, with the options
  `--approve-grant +LINE|-LINE`, `--approve-migration DIGEST`<!-- doc-ref: planned --> (planned),
  `--approve-display SHA256`<!-- doc-ref: planned --> (planned), `--held release|cancel`, `--held-entry PATH=release|cancel`
  and `--refuse`, decides a held request of either
  principal. It sends Status and prints the whole `Decision` ("Self-editing"), with its
  tree, H and trigger report. With `--refuse` it sends `Decide(D, decision, Refuse)`. Otherwise
  it sends Decide approving that exact decision only when
  its options name every part the decision holds and nothing else: the grant diff line by
  line, the migration digest, the SHA-256 of the approval text, and fates for the held
  groups (`--held`, `--held-entry`, as for deploy); any other set of options prints the
  decision and exits 2 with nothing sent. It never sends Begin. After an approval it
  follows D to a terminal phase or the next hold, as deploy does. This is the only client
  that decides another client's request; `hirsel deploy`'s own `--approve-grant`, `--held`
  and `--held-entry` decide only the request it began.
- `hirsel editor --state S --group NAME (--uid UID [--named LINE]... | --none)`<!-- doc-ref: planned --> (planned)
  sends SetEditor for the registered incarnation and prints the group's JSON object.

Register and deploy stream the admission input on AwaitInput, answer each Need with the missing objects,
and on Display check that the text is the one their own approvals produce, print it, and
send Confirm with its SHA-256. `Error(Busy)` is retried with the same D for up to 60 s.
Their exit status is 0 for `Installed` (1 when the pause report is `StartFailed`), 2 for a
local refusal, a `Failed(Denied)` that is a policy refusal (FFI or Exec), or an AwaitDecision its options do not decide, which prints the decision and its trigger report so that the operator can choose the grants and fates and decide it with `--request-id`, 1 for any other
`Failed`, and 75 for `Undecided`, for no host listening, or for a connection lost before a
terminal phase, which prints `outcome unknown: request D`.

### Deploy preview

Every deploy carries a preview of what NEW changes against OLD, in three parts. The host
builds it; the agent or person who proposed the release holds no deploy authority over it.

- **The grant diff.** The launcher's `Admitted` summary names every authority NEW's recipe
  gains or loses against OLD's approved recipe, as two sorted lists of lines (`added`,
  `removed`). A line is one value of a `Dir`, `Net`, `Env` or `Foreign` grant (`Dir=/data`,
  `Net=RULE`), one whole `Exec`, `Secret` or `Cred` binding with its policy digest when it
  has one (a `Secret` line names its source and patterns, never a secret's value), or one
  other capability by name (`Clock`). The program's own arguments (`Args`) are not
  authority and make no line. A registration has no OLD, so its diff is empty.
- **The entries.** Every entry OLD or NEW exports, by code identity and sorted by it:
  `added` (OLD lacks it), `removed` (NEW lacks it), and for an entry both export `changed`
  or `unchanged` by `code.revision` (PROG-9). The revision covers everything the entry
  reaches, so a release that swaps one function changes exactly the entries that reach it.
  OLD's side is its describe answer, NEW's the preview child's.
- **The triggers.** The prepare child's pending trigger report ("Preparing the pending
  trigger report"), which the launcher relays as that child's answer when it exits 0. A
  prepare child that exits 0 without one fails the deploy. Each `held` group shows the fate
  the activation gives it. A report with a `held` group that has no fate, or with an
  `unmatched` path, holds the request for a decision ("Self-editing" below): OLD's export
  is aborted once, and OLD keeps serving with its store unchanged (RUNNER-SPEC §14.6 "The
  activation fate").

The report of every terminal phase carries the preview as far as it was computed.

**Activation.** An operator's deploy whose grant diff is empty and whose trigger report is
settled assembles and cuts over with no further step; an editor's deploy cuts over alone
only under the settler's rule ("Self-editing" below). A nonempty diff waits for a person's decision, before `Assemble`, because every
child's authority, the preview child's included, comes from the group's approved bindings
("Child modes"). The host releases the attempt and records the diff in the request's row,
in one transaction, and the request's phase is `AwaitDecision(decision)` ("Self-editing"). The hold has no
attempt and no deadline, survives its connection and the host's restart, and ends at a
`Decide` or when a later Begin of the group supersedes it (`Failed(Denied)`). A Decide must
name exactly the held diff: an approval records it as the request's approved diff and
repeats the admission under a fresh attempt and deadline, owned by the deciding connection,
and that admission passes the gate only when it finds the same diff; a refusal fails the
request `Denied`. Approval comes only from the operator, through the operator client
(`--approve-grant`, `--held`, `--held-entry`) on `operator.sock`. No entry of any group
decides, because NEW's code could rewrite that entry's Caller check. "Self-editing" below
widens the hold and the Decide from the grant diff to every fact of the settler.

Laws: L48 `deploy_admission_policy` (the hold and the decision) and L54 `deploy_preview`
(the entry diff and the trigger report), in RUNNER-SPEC §14.3; `grant_diff_names_each_authority`
in the launcher. The candidate re-run on a store fork that reports prompt-cache diffs
is separate from publishing.

### Self-editing: the editor, the law gate and the settler

*Specified 2026-11-25 (self-edit-gate-spec-2794, with RUNNER-SPEC v2.81), amended 2026-11-26
after review self-edit-gate-review-2801 (self-edit-build-1c-2808, RUNNER-SPEC v2.82). Not built:
every socket, ask field, phase, class and law in this section is planned. Only the building
blocks are to be built now (Sam, 2026-11-26: "not building self editing into lash, just the
building blocks"): the law gate (built: a Begin source's `gate`, self-edit-begin-source-2849), operator-decided holds with `hirsel decide`, the migration
and runs facts from OLD's report of the bound PREPARED with its settlement check, and source
by content with per-group package roots. The editor principal (its UID, socket, SetEditor and
revocation), automatic cutover and the editor allow list stay unbuilt.*

An agent edits a running group's program by submitting source. This section says who may
submit it, what runs before the new revision serves, when a person must decide, and how the
cutover commits exactly what was checked. Everything not named here is the deploy path
above.

**Two principals.** The UID of the connecting peer is the only credential. A name, PID,
tenant, request ID or Caller carried in a frame or by a program conveys no authority.

- The **operator** is the peer whose effective UID is the configured operator UID, on
  `STATE/operator.sock` ("The operator socket"). It is the only principal that approves
  authority, confirms approval text, supplies fates or decides a hold.
- An **editor** is a group's editing principal. Each group's catalog row gains
  `editor: Option[EditorGrant]` (the UID and the named `Secret`, `Cred` and `Env` lines),
  which only the operator sets, with SetEditor on `operator.sock` (`hirsel editor`<!-- doc-ref: planned -->). A
  group with no editor cannot be edited. The operator's own group, the platform host, never
  has one. An editor connects to the **editor socket**.
  `hirsel host --editor-listen PATH`<!-- doc-ref: planned --> makes the launcher bind that
  second Unix stream listener under its lifetime lock and pass it only to the host, in
  `HIRSEL_EDITOR_LISTEN=PPID:FD`<!-- doc-ref: planned --> beside
  `HIRSEL_OPERATOR_LISTEN`. The variable names no UID: the runtime admits every peer on
  it, and its accept gives the program the peer's effective UID (`SO_PEERCRED`), so the
  host checks the UID against its catalog. The host's compiled grant names the editor
  socket beside `operator.sock`. Without the option there is no editor socket. The frames
  are the operator socket's.
- Before it decodes a byte, the host checks the peer's effective UID. A peer whose UID is
  no group's editor is closed. An editor's Begin may name only a group whose
  `editor` is its UID; any other group is `Error(Denied)`, and its LookupGroup answers
  `UnknownGroup`. So an editor edits only the groups the operator gave it, and never another
  tenant's group.
- An `editor` equal to the operator UID, or to the UID that the host, launcher and children
  run as, is `Invalid`. With equal UIDs the editor's own processes could open
  `operator.sock`, read `STATE` or signal the launcher. Who can open the editor socket's
  node is the operator's filesystem choice. The peer-UID check is the authority.
- A self-editing agent, and the harness publisher that relays its proposals ("Publishing a
  harness proposal"), run as the editor UID.
- No entry of any group decides, the edited group's included. NEW's code could rewrite that
  entry's Caller check.

**What editing delegates.** Giving a group an editor delegates the group's approved bindings
to code the editor writes. The settler stops the editor from widening those bindings, and
never stops it from using them. NEW's serving child runs as the launcher's user, under
bindings that OLD's operator approved, so the group's approved recipe must reach nothing
of the operator's. The rule is an allow list. A group may have an editor only when every
binding of its approved recipe is one of:

- `Clock`, `Random`, `Tasks`, `Stdout` or `Stderr`;
- `Net` whose rules are all inet rules (no `UnixRoot`, so no Unix endpoint at all);
- `Secret`, `Cred` or `Env` whose line the operator named in the SetEditor that set the
  editor;
- `Dir` that passes the identity rule below.

Anything else (`Exec`, `Foreign`, `Process`, a Unix `Net` rule, an unnamed `Secret`) is
`Denied`. A `Dir` binding of an editable group is resolved from `/`, one component at a
time, with `O_NOFOLLOW` and no `..`. Every component must be a directory, not a link, owned
by root or by the launcher's user, and not writable by group or other. The catalog records
the final directory's (device, inode) when the SetEditor or a Decide approves the binding.
The resolved directory is refused when its (device, inode) is that of any **protected
directory** or of any component on a protected directory's path from `/`, and when any of
its own components is a protected directory. The protected directories are `STATE`, the
launcher's code, image, compiler and package stores, its pass cache
(`STATE/test-results`), the editor socket's directory, the launcher user's home directory,
and the runtime root selected for [the resident compiler](cli.md#resident-compiler),
or `/tmp/hirsel-<uid>` when the compiler uses its last fallback.
So a binding is never at, above or below one of them, through a link or a bind mount.
(Decision 2026-11-26, reversible: "contains or is contained by any directory the launcher's
user owns" is not checkable exactly, because the rule itself requires the user to own the
components; the closed protected list is.) The child's `Dir` resolves beneath its granted
descriptor (`openat2`), so a link that the child makes inside its directory does not lead
out of it.

The host checks the whole rule, resolving every `Dir` again, when it sets `editor`, at
every admission of the group by either principal while it has an editor, and in the
transaction that clears a request for its cutover. Each serving start of the group opens
each `Dir` the same way and refuses a launch whose (device, inode) differs from the
recorded one, so a directory swapped after admission never reaches a child. A failure is
`Denied` for an admission, and fails the request `Denied` at the cutover. An editor's NEW
can never gain such a binding ("What the editor may ask"). The clean end state is a
per-group child UID, neither the operator's nor the launcher's, so that the kernel
separates every principal and the allow list shrinks to "no `Exec`, no `Foreign`". That
needs a privileged launcher and is its own lane.

**What the editor may ask.** On the editor socket the host accepts Begin, Data, EndInput,
Status and LookupGroup. It answers Confirm, Decide, Register and ResolvePin with
`Error(Denied)`. An editor Begin must carry:

- a `Source(manifest, gate)` input whose `gate` is `true`;
- empty `foreign` and `exec`;
- `held` equal to `Fates(all: None, entries: [])`.

Anything else is `Error(Denied)` before anything compiles. So the editor never approves an
authority, never confirms an approval text, never chooses a run's fate, and never skips the
gate. An editor's tree that grants `Foreign` or `Exec`, or whose recipe declares a Foreign
definition or an Exec request, is `Denied` at admission, for a trusted author too, and is
never held: an approved Foreign or Exec binding would make the group fail the allow list
above. Foreign code that a dependency brings in needs a `Foreign` grant (FFI-1), which is a
line of the grant diff, so it is refused the same way.

**The principal is part of D.** D's `requests` row records the principal that sent its
first Begin, as part of its canonical bytes: `Operator`, or `Editor(uid)` with the peer's
UID. On another principal's socket or UID, a Begin for a known D is `Error(Conflict)`, and
Data and EndInput for it are `Error(Protocol)`. On the editor socket, Status of a D whose
recorded principal is not `Editor` of the peer's UID is `Error(UnknownRequest)`, so one
editor cannot read an operator's request or another editor's. The operator may read any
D's Status, because it must see a decision to make it. So the editor cannot attach to an
operator's request, take its upload or answer its Display. An editor request needs no
connection once its input is complete:

- it never waits for Confirm, because approval text holds it for a decision instead;
- the admission that a Decide repeats is owned by the deciding operator connection;
- like any complete input, it goes on without its connection.

While the group has an operator request that is active or held, an editor Begin for the
group is `Error(Busy)`. An editor Begin supersedes only an editor's hold, so an agent cannot
cancel the operator's pending deploy by sending source.

**Revocation.** The transaction that changes or clears a group's `editor` fails every
non-terminal request of the group recorded as `Editor(uid)` for another UID `Denied`, with
the detail `editor revoked`. The transaction that clears an editor request for its cutover
checks that the group's `editor` is still that UID, and otherwise fails it `Denied`, so a
request already preparing under a revoked editor never cuts over.

**The law gate.** A Begin's source input `Source(manifest, gate)` asks for it when `gate` is
`true`. Only a Begin source carries a gate, and a Register is never gated. The operator may
set it, and the editor must. A gated source is
admitted as `Source(tree, gate: true)`. The launcher's step 1 ("Publish and assemble")
runs these steps after `hirsel publish PACKAGE --frozen` gives H in its scratch, and before
it records H's recipe or admits `Publication(H)`; only the authority check, which reads
nothing but the tree's manifest, runs before the publish, so a denied tree compiles
nothing:

1. **The authority check, before any test runs.** Gate tests run as the launcher's OS user.
   The gate first reads every test's run configuration from the tree's `[tests]`
   (packages.md). A test may be granted only `Clock`, `Random`, `Stdout`, `Stderr`, `Tasks`,
   and `Dir` with a relative path that has no `..` component, which resolves inside the
   test's own fresh working directory (PROG-4). The gate stops with `Denied`, naming the test
   and the grant, and runs no test, when any of these holds:
   - the tree grants any test anything else (`Net`, `Env`, `Exec`, `Process`, `Secret`,
     `Cred`, `Foreign`, an absolute `Dir`);
   - a test's reach holds a foreign function, as `hirsel test` reports it;
   - the configuration names a property corpus or a recording.

   This is the gate's own rule, beside the group's approved bindings. A test's authority is
   never approved by a decision, because a test runs before any decision.
2. **The run.** `hirsel test PACKAGE --frozen` runs as the launcher's child in the same
   scratch, on the same files and `hirsel.lock`. It uses the launcher's compiler store,
   package store and the pass cache at `STATE/test-results` (passed as the run's cache
   root, never the user's default cache), the toolchain's memory limit and no test filter, so it runs
   every test of the package (PROG-4). Its `--json` report goes to a file in the scratch
   and is read whole, never cut at a size. Tests run in the `hirsel test` client, never in the
   compiler (cli.md "Resident compiler"). When the launcher user's resident compiler serves
   (lane cas-s4-shared-daemon-2781: one per user per toolchain), the session goes through
   it.
3. **Reuse is PROG-4's and no wider.** A test whose program and run configuration are
   unchanged since a recorded pass reports that pass (`Source::Cached`) without running. Only
   a pass is reused, only for a test without parameters, and never for one whose reach holds a
   foreign function. The pass cache is `STATE/test-results`, mode 0700 under `STATE`. The
   editor cannot write it, because its UID differs, and an editable group's `Dir` can never
   reach it ("What editing delegates"). Its key covers the test's program, its run
   configuration and the toolchain's identity (PROG-4). The resident compiler only speeds the
   gate up: a one-shot `test` gives the same rows, verdicts and cache writes (cli.md), so the
   verdict never depends on whether a daemon served.
4. **The verdict.**
   - **Green**: every test passes, whether executed or reused. A package with no tests is
     green.
   - **Red**: any test fails, stops or does not start, or the request's admission deadline
     passes while a test runs. The launcher stops the test child and answers
     `AdmissionFailed(Red(rows))`, and the host fails D `Red` (class 13). The detail names
     each such test and its outcome.
   - **Lost**: the resident compiler's session is lost while the gate runs (cli.md: a
     `test` request "reports the lost answer"). This is never green and never a durable
     `Red`: the launcher answers `AdmissionFailed(GateLost(detail))`, the host sends
     `Release(A)` and answers the owner `Busy`, as for a lost launcher session, and the
     owner's retried Begin of the same D admits it again.

   No `--laws` strength row is read: a row is never an input to acceptance (R28,
   [laws](../laws.md)). The gate runs inside the admission deadline and holds one of the
   launcher's four admission slots while it runs. The launcher takes the deadline from
   `Admit`, so the gate has only the time left after upload and publication. A `check` or
   `test` child still running
   at the deadline is killed with its process group, and the verdict is `Red` with the one
   row `the tests had not all ended by the admission deadline`.
5. **The binding.** `Admitted`'s summary gains `gate: Option[Green(tree, H, toolchain)]`.
   Here `tree` is the SHA-256 of the tree's `launcher.Manifest` codec bytes, the bytes the
   operator's Begin carried ("Source by content"), and `toolchain`
   the launcher's toolchain identity, and the host records all three in D's row. H is the
   content of the package's sources and does not move with the toolchain, so the toolchain
   is recorded beside it. The tests and the publication are built from the same scratch files under
   `--frozen`, and one tree always gives one H, so the green verdict is the verdict of the
   code H publishes.

The gate filters correctness. It is not an authority boundary. The tree decides which tests
exist, so each settler claim below holds even when the gate is vacuous.

**The settler.** The host evaluates five facts for each request. A fact is **clear** when
it needs no person. Each fact is read from bytes that the cutover names or is bound to:

| Fact | Known at | Clear when | Read from, and bound at the cutover by |
|---|---|---|---|
| gate | before `Admitted` | `Green(tree, H)` | the launcher's summary. The cutover installs H's recipe R, its assembly record, the Describe and release that the preview reported for that record, and PREPARED for that release |
| grants | `Admitted` | the grant diff is empty | R's grant bindings against OLD's approved recipe. The cutover installs R |
| approvals | `Admitted` | R declares no Foreign definition and no Exec request | R, and the display that is bound to it |
| migration | prepare | the catalog delta (PREPARED kind 0x0082) and replacement inventory (0x0083) are both empty, so PREPARED carries no row (0x0084) | the PREPARED bytes OLD validated and bound (STORE-SPEC §3.5a M1–M3), as OLD reports them on `PlaceImported` (RUNNER-SPEC §14.3 "Preparing"), and the cutover names that digest. When no PREPARED exists, the prepare child's side report beside TRIGGERS; then nothing can cut over |
| runs | prepare | for an editor: TRIGGERS has no `held` group and no `unmatched` path, **and** the bound PREPARED has no activation rewrite (0x0085) and ships no range of `store.triggers.pending`; for an operator: as for an editor, or every held group has an approved fate and OLD's own settlement check passed | TRIGGERS, read only for the display and for a hold; the facts that clear it come from OLD's `PlaceImported` report of the bound PREPARED and from OLD's settlement check ("From check to cutover" item 4) |

- **Editor requests.** The cutover is automatic only when all five facts are clear. Then
  the request assembles, prepares and cuts over with no person. A red gate fails D `Red`. An
  editor tree with Foreign or Exec is `Denied` at admission ("What the editor may ask"),
  so `approvals` is never held for an editor, whatever the next sentence says. Any
  other fact that is not clear holds D for the operator. So any grant diff, any Foreign or
  Exec, any schema change and any held run goes to a person. An editor supplies no fates, so
  for an editor "runs" is clear only when nothing is held.
- **Operator requests.** The operator's Begin is the operator's decision on its approvals
  (through Confirm), its migration and the fates it names. A grant diff holds the request,
  as above. A held group that its fates leave unsettled, or an `unmatched` path, also holds
  the request. A gated operator request with a red gate fails `Red`.
- **"Migration clear" is exact.** Migrate stages nothing (STORE-SPEC §1.2):
  - no table is created, gets a new schema, or is migrated, renamed or dropped;
  - no index is created, rebuilt or dropped.

  An index rebuilt because its extractor's composite revision changed is a migration, and
  so is a `migrate_table` that keeps the name and codecs but rewrites rows: either one
  ships a replaced range. A release whose code changes but whose staged catalog is OLD's
  ships nothing, and its activation writes only the activation pair and release row.

**Holds and decisions.** A hold is the AwaitDecision of "Deploy preview", widened to every
fact. Its phase is `AwaitDecision(decision)`, with

```text
Decision { tree: Option[H], root: Option[H], stage: Stage, grants: Grants, approvals: Option[Approvals], migration: Option[Migration], triggers: TriggerReport }
Stage = Admission | Prepared
Approvals { r: H, foreign: List[String], exec: List[String], text: String }
Migration { digest: H, changes: List[String] }
```

`tree` and `root` are the source tree's digest and its H, so the operator sees which source
they approve (`tree` is `None` for an attested or publication input, and `root` is `None`
when the source is ungated). `stage` says which facts
are computed. At `Admission`, `migration` is `None` and `triggers`
is empty because prepare has not run. At `Prepared`, every fact is computed, and an empty
part means that fact is clear. `approvals` holds R's Foreign and Exec lines and the exact
display text, as "Publish and assemble" step 3 makes them. The migration `digest` is the SHA-256 of the 0x0082 frames' payloads, in
order, followed by the 0x0083 inventory's sorted (id, kind) list. Neither frame carries the
export's head or version, which are in 0x0081, so the digest moves only when OLD's catalog
moves: a deploy, or an index build step on OLD, since a catalog record carries the index's
`state` (STORE-SPEC §3.5a "Catch-up"). The second only holds D again, which is safe.
`Inventory` is `{ id, count, digest }` and has no kind, so kind is derived: **dropped** when
the delta's change for `id` has no `after`, otherwise **empty** when `count` is 0, otherwise
**replaced**. Row counts and row digests are left out, so a change of OLD's data alone never
changes the digest. `changes` names each collection that the delta creates, gives a schema,
rebuilds, migrates, renames or drops, for display.

- **Where a hold happens.** Admission-time facts (grants, approvals) hold before
  `Assemble`, as in "Deploy preview". Every child's authority comes from the group's approved
  bindings ("Child modes"), so no preview or prepare child runs under an unapproved grant.
  Prepare-time facts (migration, runs) hold after prepare. The host aborts OLD's export
  once, as for L54's missing report, then releases the attempt and records the hold in one
  transaction, and OLD keeps serving with its store unchanged. The prepare child reports the
  migration even when an unsettled report stops it from writing PREPARED, so the migration
  and the runs hold together. A request therefore holds at most twice, once at each stage,
  until a fact changes under it. The approval given at the `Admission` stage stays in the
  approved set, so the second hold's decision shows only the prepare facts as undecided. No hold keeps an attempt, an export lease, a write pause or an assembly
  reservation. A hold has no deadline and survives its connection and the host's restart.
- **Decide(D, decision, verdict).** The operator sends it, only on `operator.sock`, with
  `verdict` `Approve(fates)` or `Refuse`. Its `decision` must equal the held one byte for
  byte, or the Decide is `Error(Denied)`. When the decision shows a held group or an
  unmatched path, an approval's `fates` replace D's recorded fates. Otherwise they must be
  `Fates(all: None, entries: [])`. An approval adds the decision
  to D's **approved set**, which holds the approved grant diff, display, migration digest
  and fates, and then repeats the admission under a fresh attempt and deadline, owned by the
  deciding connection. The repeated `Admit` carries the approved `foreign` and `exec` lines
  as its approvals. When the display it produces equals the approved `text` byte for byte,
  the Decide stands for its Confirm, and no Display or Confirm is exchanged. Any other display
  holds D again. A refusal fails D `Denied`.
- **Each repeated admission derives every fact again.** It publishes the tree again and
  continues only when it gets the recorded H. Any other H fails D `Invalid` with
  `republished root differs`. The recorded verdict stands only for the same H and the same
  toolchain identity, so the gate does not run again; under another toolchain it runs again. A fact passes when it is clear, or when
  it equals the approved value (the grant diff, the display text, the migration digest), or,
  for runs, when the approved fates give every held group a fate and leave no unmatched
  path. For fates that came from a Decide, every held group of the new report must also be
  one the approved decision showed, by (kind, entry path, argument schema, `hold`); counts
  and due times may differ. A group the decision did not show holds D again, even when
  `all` would give it a fate. The fates an operator names on its own Begin keep the first
  rule, because there `all` is the operator's stated policy. For approvals the approved value is the whole `Approvals`. A fact that differs, or that newly needs a person, holds D again with the new
  decision. Approval of one value never carries over to another.
- **The verdict is durable.** The deploy row gains `settled: Option[H]`, the digest of the
  PREPARED that the settler cleared, written in the transaction that moves the row to
  `Pending`. The host sends or re-sends a cutover only for a row whose `settled` equals the
  digest the cutover names, so a host restarted between import and settle never sends a
  cutover the settler did not clear.
- **Drift retry.** The settler evaluates the retry's own PREPARED and TRIGGERS before it
  sends the retry's cutover, and records `settled` again for the retry's digest. Any fact
  that fails the previous bullets aborts the retry's export once and holds D.
- **Supersession.** A hold ends at a Decide, or when a later Begin of the group supersedes
  it (`Failed(Denied)`), subject to the editor's limit above.

**From check to cutover.** Once a fact is checked, nothing that commits can differ from it:

1. **Code.** D's canonical Begin bytes fix the manifest, so they fix every file by content.
   One tree gives one H, the gate's verdict is H's, and a repeated admission must give H
   again. R is H's recipe. The preview, the prepare child and the cutover all use the
   attempt's one assembly record of R. Before it accepts the import, the host checks that
   PREPARED's target Describe and release are the ones the preview reported for that record
   ("Hot deploy"). So the revision that cuts over is the revision that was tested.
2. **Grants and approvals.** The cutover installs R, whose bindings are the ones that were
   diffed. OLD's approved recipe changes only through another deploy that installs, which
   needs the group's `active` row, and D holds that row. So OLD is the same at `Admitted`
   and at the cutover.
3. **Schema.** The migration fact is a function of the PREPARED bytes whose digest OLD
   binds and the cutover names. OLD computes it while it validates those bytes and reports
   it on `PlaceImported`; the host never reads a file that the prepare child wrote beside
   PREPARED to clear it, because the prepare child runs submitted code, and
   `store.write_prepared` and its siblings are public, so that code can write any report
   it likes beside an honest PREPARED. The cutover is refused `Drift` unless OLD's head is the
   exported base. A catch-up round patches only the ranges that PREPARED ships, and a clear
   migration ships none.
4. **Runs.** The prepare child writes PREPARED only when every held row of its scan has a
   fate (RUNNER-SPEC §14.6). The rewrites that settle those rows are in PREPARED, under the
   digest that the cutover names. The prepare child's code is submitted, so OLD checks the
   settlement itself (RUNNER-SPEC §14.3 "Preparing"): for an editor request with no
   approved fates, a PREPARED with any activation rewrite (0x0085) or any shipped range of
   `store.triggers.pending` is refused; with approved fates, OLD classifies its own pending
   rows at the exported base under NEW's identity digest and entries and the approved
   fates, derives each settled row's value, and refuses the import unless PREPARED applies
   exactly those values, whether in 0x0085 rewrites or folded into a shipped range of
   `store.triggers.pending`, and no rewrite on any other table. So a PREPARED that cancels a
   run the operator released is refused. At the final catch-up round, inside the pause, OLD
   runs the same scan on that round's base; a held row it finds that the approved fates do
   not settle aborts the export and holds D. The settler never releases or cancels a run without a fate
   the operator gave. A run that the scan did not see stays parked by NEW's head under the
   identity guard (RUNNER-SPEC §14.6 "The identity guard"), and the next report shows it as
   held.

**Source by content.** A source is sent as a manifest, not inline:

```text
Manifest { package, grants, dirs, required_entries, files: List[ManifestFile] }
ManifestFile { path: String, digest: H, len: u64 }
```

`package`, `grants`, `dirs` and `required_entries` are the Tree's fields. `files` is sorted
by path with no repeats, and no path is a directory of another (`a` beside `a/b` is
`Invalid`). Paths follow the Tree's rules, and the lengths add up to at most
4 MiB. This one form replaces the inline `Source(tree)` on the operator and editor sockets.
The launcher still receives a whole `launcher.Tree`, which the host assembles.

- **The group's blob set.** Each group has one set of blobs, kept in a host catalog table
  `blobs` keyed by (group incarnation, digest). It holds the blobs named by the manifest of
  the group's installed source and by every request of the group that is not terminal.
  Presence is per group. A digest that another group holds is still Needed, and its bytes
  must still be sent, so Need tells no group about another's content. Groups share nothing
  ("Authority groups").
- **The exchange.** After Begin, the host answers `Need(D, A, keys, last)` with
  `BlobKey(digest)` for each file whose digest the group's set lacks, in manifest order. A
  Need whose last part names no key means the input is complete, and no Data or EndInput
  follows. Otherwise Data carries the needed blobs' bytes back to back, in Need order, each
  exactly its manifest `len`, and EndInput ends them. The host hashes each blob. A digest
  that does not match, a short blob or an extra byte fails D `Invalid`, naming the path.
- **No path trust.** A blob is stored, found and compared only by its SHA-256. A path is a
  name in the manifest and nothing else. The host never opens a file by a submitted path,
  and the same bytes under two paths are one blob. The launcher writes the assembled tree
  into fresh scratch under `STATE` (mode 0700), never in a shared temporary directory,
  because the editor is another local user.
- **Rooting.** D's row roots the blobs D received until D is terminal. The transaction that
  records D `Installed` makes D's manifest the group's installed manifest. The transaction
  that makes D terminal deletes every blob of the group that no remaining manifest names. A
  held request keeps its blobs, so the admission that a Decide repeats, on the operator's
  connection, reads every file from the host. An identical Begin after a lost upload Needs
  only what the set still lacks. The group's removal deletes its blobs with its rows.
- **Package roots per group.** Each group's catalog row gains `packages: List[H]`, the
  package roots the operator allows it, set by Register and SetEditor. A tree's
  `hirsel.lock` root outside the group's set fails with the diagnostic of an absent root,
  so one group's library is neither linked nor detected by another. The bytes are shared
  in the one package store underneath, as blobs may be.
- **Installs that are not source.** An install from an attested or publication input clears
  the group's installed manifest, and its blobs go when no request names them.
- **The request row** stores the manifest in place of the files. The rule that two trees fit
  one stored value no longer constrains `TREE_BYTES`.

What the host does where the rules above leave a choice, and why:

- **Limits.** `launcher.SOURCE_BYTES` is 4 MiB of file content, `MANIFEST_BYTES` 1 MiB of
  manifest codec bytes and `TREE_BYTES` 5 MiB of assembled tree codec bytes, so a source at
  its content limit still assembles. The CLI refuses an oversized tree before it connects.
- **One key per digest.** Need names each digest once, at the first file that carries it,
  in parts of at most 4096 keys; the same bytes under two paths are sent once. A file of
  length 0 is never Needed, and its digest must be SHA-256 of no bytes, or the manifest is
  `Invalid`. So an all-present or all-empty tree Needs nothing.
- **Per-blob commit.** Each blob is stored in its own transaction as soon as its bytes hash
  to its digest, and only while D is not terminal, so a lost upload resumes from the blobs
  that arrived and a crash leaves no blob that no row could root. Blob keys are the group
  incarnation as 8 big-endian bytes, then the digest.
- **The installed manifest moves with the selected release.** The group row's `source` is
  set in the transaction that selects NEW: at the first install, and in recovery's
  selection of a new release. A non-source NEW sets it to none. Recovery that keeps OLD
  leaves it. The sweep in D's terminal transaction keeps the installed manifest's digests
  and the active unfinished request's, and deletes the rest of the group's blobs.
- **Error texts.** A blob that does not hash to its digest fails `Invalid` with "the bytes
  sent for P do not match its digest", an extra byte with "a byte follows the last needed
  blob, P", and an EndInput inside a blob with "the input ended within P".
- **The lock check comes first.** Publish reads the tree's `hirsel.lock` before it
  compiles. A pinned root outside the group's `packages` fails `Invalid` with "ID ROOT is
  not one of the group's package roots, and --frozen does not fetch (source S)", whether or
  not the shared package store holds that root, so the answer tells no group what another
  linked. A lock that is not a version 1 lock is `Invalid`. Register sets `packages`;
  SetEditor does not exist yet and will carry its own list.
- **Green names the manifest.** `Admitted.gate`'s `tree` is SHA-256 of the manifest's codec
  bytes, the bytes the operator signed off on sending, not of the assembled tree.
- **The CLI.** `hirsel register|deploy --source DIR [PACKAGE]` walks DIR: every regular
  file by its `/`-joined UTF-8 path, skipping any component that starts with `.`, and
  refusing a symbolic link or any other kind of file. PACKAGE is the package directory
  within DIR, `.` by default. Every `--grant` becomes a tree grant, and its Foreign and
  Exec ones are also the request's approvals. The CLI sends the manifest, then exactly the
  blobs the host's Need names, in its order, in 256 KiB Data frames, and EndInput; nothing
  when Need names none. A source's display is shown unchecked, since only the host knows
  its H. `register --package-root sha256:HEX` adds one of the group's package roots.

**Laws** (planned, platform host suite unless noted). Each names the bug it catches:

- `editor_cannot_decide`. On the editor socket, Decide, Confirm and Register are `Denied`.
  So is a Begin with Foreign or Exec lines, with a fate, or with an ungated source. A host
  set to the operator UID is `Invalid`, and an editor's Begin for a group whose `editor` is
  another UID is `Denied`. Bug: an agent approves its own grant diff or fates, or edits a
  group it was not given.
- `principal_bound_to_request`. An editor Begin repeating an operator D's bytes is
  `Conflict`, and the editor's Status of that D is `UnknownRequest`. An editor Begin while
  an operator deploy of the group is held is `Busy`. Bug: the editor takes over an
  approved re-admission, or supersedes the operator's hold.
- `gate_tests_exact_revision`. A green gated tree installs a NEW whose release identity's
  revision is the entry revision of the recorded H. A repeated admission whose publish gives
  another H fails `Invalid`. Bug: the tests checked other code than the code that cut over.
- `gate_red_never_cuts_over`. A tree with one failing test fails `Red` naming it, no
  `Assemble` is sent, and OLD serves unchanged. Bug: a red gate is skipped or ignored.
- `gate_test_authority_closed` (launcher). A tree whose `[tests]` grants a test `Net`,
  `Exec` or an absolute `Dir` fails `Denied`, and no test child starts. Bug: agent-written
  test code runs with the launcher user's ambient reach.
- `settler_holds_each_fact`. Five editor trees each change one fact: a grant, a Foreign
  approval, an added table, a rebuilt index, and a held run. Each ends in AwaitDecision
  showing exactly that fact. A sixth tree, which edits one function body and changes no
  fact, installs without a Decide. Bug: the settler misses a fact, or holds a clean edit.
- `decide_rederives`. After an approved migration, the repeated admission derives another
  migration digest and holds again, and does not install. Bug: an approval carries over to
  a value nobody saw.
- `blob_presence_per_group`. Group B's manifest names a digest that only group A holds, so
  B gets `Need` for it. A blob whose bytes hash to another digest fails `Invalid`. Bug: a
  group probes or borrows another group's source.

- `editor_bindings_allow_list`. SetEditor on a group whose approved recipe holds
  `Net=UnixRoot s STATE;UnixConnect s operator.sock` is `Denied`, so editor code never
  reaches `operator.sock` through a Unix rule; so is a `Dir` whose path runs through a link
  or whose directory is `STATE`. Bug: the delegation check is a deny list.
- `editor_dir_swap` (launcher). A `Dir` approved at one (device, inode) and then swapped
  for a link to `STATE` is refused at the next admission and at the next serving start. Bug:
  a binding is re-resolved by path after its check.
- `settler_reads_bound_prepared`. A prepare child that writes an honest PREPARED with a
  dropped table and, beside it, a migration report and TRIGGERS that show nothing, holds D
  with the drop in its decision. Bug: the settler reads a file the submitted code wrote.
- `editor_prepared_cannot_settle`. An editor's PREPARED whose 0x0085 frame releases a held
  run, beside a TRIGGERS with nothing held, is refused by OLD and cuts nothing over; with
  approved `Release` fates, a PREPARED that applies `Cancel` to that run is refused. Bug:
  the activation commit applies a fate the operator did not give.
- `decide_fates_only_shown_groups`. A decision showing one held group, approved with
  `--held release`, holds again when the repeated admission finds a second held group.
  Bug: an approval's `all` fates a run nobody saw.
- `editor_revoked`. Clearing a group's `editor` fails its preparing editor request
  `Denied`, and it does not cut over. Bug: a revoked editor's request still installs.

L48 `deploy_admission_policy` keeps its hold and decision cases, which now use the
`Decision` value.

**Prior art** (from the projects' documentation as known to this lane; not re-opened here):

- **ADOPTED:** GitHub branch protection's rule that the last pusher's approval does not count.
  Here the editor never decides.
- **ADAPTED:** Bazel's remote-execution `FindMissingBlobs` and upload by digest. Here presence
  is per group, not per instance, so Need cannot be used to probe across groups.
- **NOT TAKEN:** Kubernetes admission webhooks, where a policy service that a workload could
  reach decides. Here no entry of any group decides.

### Publishing a harness proposal

An app that enables `harness.release` exposes WIRE queries for the pending proposal and
its source, and a transact entry to settle it. Each checks the publisher's authenticated
Caller before forwarding to `release.pending_cover`, `release.source_cover` or
`release.settle`.
The publisher is an editor-side application with a connection to those entries and
the editor socket ("Self-editing" below); it runs as the editor UID and never opens
`operator.sock`. The app's tools propose the source rows and their build settings
(`package`, `grants` and `dirs`). Its Begin is an editor's: gated, with no Foreign or Exec
approvals and no fates for held runs (`held` is `Fates(all: None, entries: [])`). A proposal
whose settler finds a grant diff, a migration or a held run waits in
AwaitDecision for the operator, and the publisher keeps it outstanding; one with Foreign or
Exec is `Denied` and the publisher settles it Rejected. The proposing turn finishes its
granted trigger before the publisher submits, so the proposing run is not held by its own
edit. After Installed, the publisher starts a fresh turn on NEW. If the proposing run must
continue across the edit, the proposal stays held for the operator's fate. Whether a run's recorded
steps are still valid on new code is the operator's decision.

Run one `harness.release_publish.Publisher` per group incarnation. Drive `poll` when
work is available and while a request is outstanding. Reconnect the app client when
the cutover closes its connection. A reconnect as the same Caller continues that
Caller's WIRE request positions. A fresh authorized Caller has its own positions.
The publisher reads one file per WIRE query, then verifies the full tree hash. Give
both ends a WIRE frame allowance large enough for the largest source file and its
envelope.

The publisher derives D from the group name, incarnation and proposal ID. It asks
Status before reading the named tree. An unknown D submits the unchanged tree through
a Source Begin, as a manifest, and uploads only the blobs the host's Need names. The workspace must include its `hirsel.lock` and its pinned packages
must be in the host's package store. A source tree that moves or fails the frozen
check is rejected. After a restart, a known D uses its recorded input and phase. A completed request therefore cannot deploy again
even if the source rows have since changed. A Source replacement uses OLD's verified recipe memory and place allowances. Every hold
follows the ordinary AwaitDecision and Decide exchange, which only the operator answers. The publisher settles Installed as Published and a
failed request (a refused decision included) as Rejected, retaining the failure text. The app's settle entry can
receive the same decision again after a lost reply.

The source request names the required entry kinds and keys (identity and both codec
fingerprints). In the ordinary candidate preview, the host refuses a candidate as
Uncompiled if a key or kind is missing or has changed. This precedes OLD's export and
import. NEW's handlers must enforce the app's Caller policy and keep the proposal
tables so the publisher can read and settle after activation.

### The launcher

The **launcher** is a thin Rust program. It holds no record of its own that a decision reads
again: every decision record is in the host's catalog. What it keeps on disk is
content-addressed: the code store, the image store and the assembly records, each named by
the hash of its own content (above). Such a file is either complete or absent after a crash,
so keeping it is caching, not state. Its operations are these:

`hirsel launcher --control SOCKET --state DIR --lifecycle managed|leader` serves the control
session of RUNNER-SPEC §14.5 (`--lifecycle` is required on Linux). It holds the launcher lock and the groups' listeners, starts
children, and collects unused objects. `Bind` names the group and its listener address.
`Start` selects
the child's mode, and a serving `Start` may name a recording (its 16-byte recording ID
and destination), which the child receives as `HIRSEL_RECORD` and `HIRSEL_RECORD_ID`.
The launcher waits on a single epoll set for its control session, cold listeners,
child endpoints, pidfds and cgroup population changes.

**Move envelope.** For install mode (tag 0), the fields are the generation, install
kind and `move_envelope`. For serving mode (tag 1), the fields are the generation and
the serving record, whose last field after `witness` is `move_envelope`. Both encode
the platform-authored envelope as a little-endian u32 length followed by the opaque
bytes, before the launch's recording option. The limit is `MAX_MOVE_ENVELOPE = 4096`
bytes. The decoder refuses a larger length as `MoveEnvelopeTooLarge`. An empty envelope
is a zero length. The launcher carries the bytes to the child in its private
`--hirsel-move-envelope=HEX`<!-- doc-ref: external --> prefix. The runtime offers the length at launch word 25
and the bytes at word 26. The fake launcher retains the same bytes for each start.
Ordinary platform starts send an empty envelope. Fleet's claim protocol and the
platform's handling of Fleet moves are specified in [Fleet](fleet.md).

- **Publish and assemble.** It admits recipes (steps 1-8 above) and assembles images. It
  writes objects with the "Durable publication" order above, steps 1-3. Step 4, the pending
  record, is a host catalog row, which the host commits only after the launcher has
  answered that steps 1-3 are synced. Admission runs on the control session, one attempt
  per deploy attempt, at most four at once:
  1. `Admit(group, attempt, input, approvals, deadlineMs)` opens it. The last field is
     the absolute Unix millisecond deadline the host recorded at Begin. It covers the
     source upload, publication, law gate, verification, object exchange and final input
     EOF. Reattachment keeps the original deadline. The launcher converts it to a
     monotonic deadline on receipt and shares that deadline between its phases. A refusal
     names the phase that ran out. The input is `Attested(root)`,
     whose recipe and objects the host relays from its client, or `Publication(root)`,
     whose objects the launcher reads from its own code store, or `Source(tree)`. A tree
     is the codec bytes of `launcher.Tree`: a package path, the grants and `--dir`
     bindings to publish with, and the files (at most 4 MiB in all, plain relative paths,
     no repeats). The launcher writes the files to fresh scratch and runs its own
     `hirsel publish PACKAGE --frozen` there as a child, under the launcher's compiler
     store, package store and code store and the toolchain's memory limit, which it
     inherits. The tree therefore carries its `hirsel.lock`, and its dependencies come
     only from packages the host already holds. A clean exit is a publication H in the
     launcher's code store, which the attempt then admits as `Publication(H)`: admission
     sees only publications. A refused check is `Uncompiled` with the checker's
     diagnostics (at most 64 KiB); a compiler out of memory is `Exhausted`. Neither
     records a recipe. The same tree always gives the same H as `hirsel publish` of the
     same files. A gated source (`Source(tree, gate: true)`) runs the law gate in the same
     scratch after the publish and before it records H's recipe ("Self-editing" below); a
     red gate is `AdmissionFailed(Red(rows))` and records no recipe, and a lost compiler
     session is `AdmissionFailed(GateLost(detail))`.
  2. For an attested input the host sends the exchange bytes of "Admission" above (the
     request frame, then object frames) in bounded `AdmitData` chunks and closes them
     with `AdmitEnd`. The launcher feeds them to the verifier unchanged and answers each
     Need of the verifier as `Need(attempt, keys, last)` events of at most 65536 keys. A
     recipe whose hash is not the attested root is refused before any Need, and so is any
     input after the verifier's final tag. The launcher reads the session no further
     while a verifier has a frame of input it has not taken, so a slow upload bounds the
     launcher's buffer; children's ends are still reported while it waits, and later
     requests wait only until the verifier takes that frame.
  3. `Admitted(attempt, root, summary, display)` reports the verified root, its root
     limit beside the approved OLD recipe's, the grant diff against OLD's recipe,
     the recipe's target and place allowance, the gate's `Green(tree, H, toolchain)` when
     the admission ran it, and the approval text (foreign archives and `Exec` profiles).
     The host records the green verdict in D's row and decides the approvals from it. OLD and NEW serve side by
     side through the cutover, so the two root limits together must fit the group's
     memory limit, or the host answers `Refused(Memory)`, which releases the attempt. A
     nonempty grant diff the operator has not approved releases the attempt and holds the
     request for a decision ("Deploy preview"). Nonempty
     approval text waits for the operator to confirm exactly that text; an editor's request
     holds for a decision instead ("Self-editing"). An untrusted
     author's deploy is source only, without Foreign or `Exec` grants, and is refused
     before `Admit`. The host checks NEW's preview against the target and allowance, and records its identity digest.
  4. `Assemble(attempt)` builds and syncs the image and its assembly record, and
     `Assembled(attempt, assembly, digest, fetch, assembly_ns)` answers it with the
     assembly name, the image digest and the fetch and assembly times in nanoseconds. The
     launcher then holds a reservation of the assembly for the attempt until the host
     releases it, which it does once the deploy's Preparing row is durable.
  `AdmissionFailed(attempt, failure)` (`Denied`, `Invalid`, `Unassembled`, `Uncompiled`,
  `Exhausted`, `Red(rows)` or `GateLost`) ends an attempt at any step. `Release(attempt)` or a new session cancels a running attempt.
- **Launch.** It starts a group's children: install (genesis and standalone install),
  describe, preview, prepare and serving. Each child runs a retained image by digest, from
  the image store, in the group's working directory, with descriptors for its mode
  ("Child modes" below). Each
  launch is keyed by the child's identity, and the launcher holds at most one serving
  launch per group outside a cutover (RUNNER-SPEC §14.3). It reports each child's end to
  the host, with the Clean report of RUNNER-SPEC §14.4 when the child sends one.

  An install start selects `Genesis` (activation 1 on an empty store) or
  `Standalone(v)` (install exactly at the next store version `v`). The check
  and install take the same store lock. The child writes one bounded value on
  standard output after it closes the store. The launcher reports
  `Placed(child, PlaceInstalled(v, false))` on success or
  `Placed(child, PlaceInstallRefused(reason))` on a typed refusal. It keeps that
  answer until the host acknowledges `Ended`. An end without proof leaves the
  outcome uncertain. A recovery probe can resolve it by opening the store under
  its lock ("When the host starts" above). The hot deploy protocol still uses
  OLD's cutover and a serving candidate.
- **Collect.** It removes objects only when the host names the full root set: the union
  that "Durable publication" lists, which the host reads from `groups`, `deploys` and `pins`,
  together with the launcher's own live reservations. A reservation is an object set that
  the launcher has synced for an attempt whose pending row the host has not yet committed;
  it lives in the launcher's memory and ends when the host commits or abandons the row. The
  host releases a reservation only after its pending row's commit is durable. "Durable
  publication" requires one collection lock; the root read and the unlink are now in two
  processes, so the lock becomes a **collection fence** that spans both, in this order:
  1. The host sends `CollectBegin`. The launcher takes the fence and answers. While it is
     held, the launcher still makes reservations, but it ends none: a release the host
     sends is deferred until the fence ends.
  2. The host reads the roots from one catalog snapshot taken after that answer, and
     sends them in `Collect(roots)`: each group's current and retained assemblies, NEW's
     assembly of each Preparing, Pending or Undecided deploy (OLD's is the group's current
     one, so an unresolved deploy keeps both choices), and each unexpired recording pin's.
     A Failed or Installed deploy row is history and roots nothing. The host starts a
     collection every five minutes on a monotonic clock, after the session's inventory is
     reconciled. One collection runs at a time.
     If it cannot read the snapshot it sends `CollectAbort` instead: the launcher removes
     nothing, ends the fence at once, applies the releases it deferred and answers
     `Collected(0)`, so a later collection can run in the same session.
  3. The launcher removes each object that neither those roots nor any reservation it holds
     reaches, then ends the fence and applies the deferred releases. A kept assembly record
     keeps its image and its recipe's closure in the code store (recipe, groups, bodies,
     annexes, foreign archives). The code store is collected under the admission lock: the
     launcher first applies the running admissions' notes, keeps every object an admission
     still holds, and an admission gives up its hold only after its assembly record is put.
     Nor is any code object removed within 24 hours of its publication: a publish that
     finds an object already stored refreshes its modification time, so every object of a
     publication's closure, shared ones included, outlives a collection that runs between
     the publish and the `Admit` that names it (git's `gc.pruneExpire`, for the same
     reason). Only a publication left unadmitted for a day may be removed, and its admission
     then fails (`AdmissionFailed`) until it is published again.

  An object synced under a reservation is therefore either named by a pending row that was
  durable before the host's snapshot, or still reserved when the launcher unlinks. A new
  control session drops a fence without removing anything (RUNNER-SPEC §14.5), and a
  launcher that has no current root set from the host removes nothing.

**Restarts.** The launcher keeps no decision record, but a live launcher does know its
children. The host learns them over the control session: `Hello` ends the old session, and
its answer is an inventory taken in the launcher's one state loop, after which every event
follows in order (RUNNER-SPEC §14.5). An ended child stays in the inventory until the host
acknowledges its end, with its exit status, its report, the selection it was launched with and
the last answer it gave before ending (a `PlaceCurrent` with any read it carries, a refusal or
`PlaceStoreMismatch`), and the host applies that answer before the end. A **host restart** adopts the live children the
inventory names, reconciles each pending deploy as "When the host starts" says above, and
probes only the `Dirty` rows left without a child. A **launcher loss** is fenced by the
launcher lock. The child identities in the delegated cgroup tree are the process
inventory left by the predecessor. A successor in the same delegation empties and removes
those cgroups under the lock before it can answer `Hello`. It removes staging and
answers with no child. The host binds the groups' addresses again and probes every
`Dirty` row. A launcher loss also loses the listener backlogs.

**One control socket.** The launcher's control socket is the platform's single
authority boundary, as the control socket of "Authority" is. Its filesystem permission
admits only the host's OS user. Operators and tenants reach the platform through the host,
which decides and records each request before it asks the launcher. `hirsel host` is this
pair: the launcher, plus the platform host it starts as its first group. `hirsel register`
and `hirsel deploy` send their requests to the host.

**The launcher is the parent, not an `Exec` child.** The host does not start groups
through an `Exec` grant, and it does not start the launcher as one either. An `Exec`
profile could pin the launcher's own image (EXEC-4), so the pin is not the reason. These
are:

- **Groups outlive the host.** A run's children die with it (EXEC-6). If the launcher were
  the host's `Exec` child, a host crash would end the launcher, every live group and every
  listener, so one fault in the operator's group would stop every tenant. With the launcher
  as the parent, a host crash ends only the host: live groups keep serving, cold groups'
  listeners keep their backlogs, and the restarted host adopts the live children (above).
- **Descriptors.** An `Exec` child receives only descriptors 0, 1 and 2 (EXEC-1, EXEC-3).
  The launcher must hold one listener per group and pass each to its serving child, which
  no `Exec` spawn can carry.

A trusted group may hold an approved `Exec` grant to run tools. The launcher's control
socket belongs to the host. No group child inherits it.

**Child lifecycle.** The launcher starts the image directly in a
new session, enters its working directory by descriptor, and passes only its mode's
descriptors. Standard streams use files or `/dev/null`. The runtime consumes the
verified image descriptor at entry. A serving child receives the socket activation
metadata and descriptors 3 through 5.

On Linux, `--lifecycle managed|leader` is required; the launcher never falls back silently
from cgroup containment to leader-only tracking. Neither mode uses a pidfile.

- **`--lifecycle managed`**: requires a delegated cgroup v2 subtree and fails startup before
  opening stores or binding sockets if one cannot be opened (no cgroup2 mount or cgroup v2
  membership, membership outside every cgroup2 root, or a non-delegated cgroup directory).
  The launcher uses a subtree keyed by its state directory, found through
  `/proc/self/mountinfo`: the cgroup2 mount whose root holds the `0::` path of
  `/proc/self/cgroup`, joined with that path less the root, so a relocated or namespaced
  mount resolves. Each child is born in its own cgroup with a pidfd. After the leader exits,
  the launcher requests `cgroup.kill` and reads `cgroup.events`. If the cgroup is still
  populated, epoll waits for a population change. The launcher reports `Ended` after
  observing `populated 0`, reaping any adopted descendants and removing the cgroup. An
  explicit kill targets the whole cgroup.
- **`--lifecycle leader`**: explicitly chooses direct-child tracking without creating or
  recovering cgroups. The pidfd tracks and reaps the direct child; its descendants can
  outlive it and can keep inherited descriptors open.

**macOS** (macOS slice S12). macOS has no cgroups, so the macOS launcher runs in `leader`
mode (`--lifecycle` may be omitted or set to `leader`; `--lifecycle managed` fails startup).
It starts each child with
`posix_spawn` in a new session (`POSIX_SPAWN_SETSID`), never with `fork`, tracks it with
`EVFILT_PROC` registered in its kqueue before it can be reaped, and kills its process group.
A child that exits before its watch is registered is reported at the next turn.
Darwin executes by path only, so a verified image runs from a private clone of exactly the
file the launcher verified (`fclonefileat`, else a copy, mode 0500 in a 0700 directory),
which is also the child's image descriptor; the clone lives as long as the child. Each
clone is a new file, and macOS charges about 130 ms on the first exec of a file.
`posix_spawn` fixes the environment before the child has a PID, so a serving child's
`LISTEN_PID` names the launcher, its parent (net.md, inherited listener).
A descendant that starts its own session escapes the group kill and can outlive `Ended`.
That is the same limit as Linux `--lifecycle leader`, stated here for operators. The
cgroup inventory a successor recovers, `cgroup.kill` and the `cgroup_lifecycle` law do not
apply. Directory watches use `EVFILT_VNODE` on directory descriptors where Linux uses inotify.

### Child modes

| Mode | Runs | Store | Inherited listener | Private descriptors |
|---|---|---|---|---|
| install (genesis, standalone) | `place`, `migrate`, `handles` | the group's store | none | its answer on stdout |
| describe | `place` | none | none | its answer on stdout |
| preview | `place` | none | none | its answer on stdout |
| prepare | `place`, `migrate`, `handles` | the export directory | none | none |
| serving | the place | the group's store | the group's listener (3) | its private endpoint (4) and wake report (5) |

The image's recipe declares its Hirsel grants. The launcher runs children under its
OS user and uses cgroups for lifecycle management in `--lifecycle managed` mode. The serving child's private endpoint
reaches only the launcher's handler for that child. Serving also gets the group's private
staging directory for the export exchange. Prepare receives the path of the named export
directory and syncs `PREPARED` there before it exits.

### Preparing the pending trigger report

The prepare child scans the triggers in the staged store after `migrate` and after opening
both handle sets. It writes `TRIGGERS` beside `PREPARED` in the export directory before
syncing `PREPARED`, and the launcher relays that file as the child's answer when it exits 0
("Deploy preview").

For each pending trigger row of the staged catalog (`store.triggers.pending`, branch 0), the
child classifies it against NEW exactly as NEW's head will at dispatch (RUNNER-SPEC §14.6).
The classes are tried in this order, which is the order of `advance_trigger`, and a row
takes the first that applies:

1. **`cancelling`**: a row whose cancel was requested, parked or not. NEW's head finishes it
   as cancelled at its due time before it looks at its entry, so NEW's entries do not
   matter.
2. **`held`**: a granted trigger that is parked under a hold, or whose attempt is at least
   1 **and** whose revision stamp differs from NEW's identity digest. The stamp is the
   identity digest of the release that started it (or the one a release selected; RUNNER-SPEC
   v2.42 §14.6). The
   activation commit settles every such row by its fate (RUNNER-SPEC §14.6 "The activation
   fate"): `Release` restamps it with NEW's identity digest so that NEW runs its next attempt,
   and `Cancel` requests its cancel so that NEW's head ends it `Cancelled`. So NEW never parks a
   row that this activation would have held. This class comes before `gone`: `triggers.start` runs before
   the head looks for the entry, so a started granted run under another digest whose entry
   NEW removes is held, not consumed. A parked `Request` hold is held even when the
   digests are equal.
3. **`gone`**: NEW's trigger entry list does not allow its entry, or no registered entry of
   NEW has its kind, path and argument schema. NEW's head will consume it with the terminal
   `Gone` when it comes due. Two records can share an identity (the same code under another
   backend, or a recipe that changed only grants); then a started granted run passes
   `triggers.start` and lands here if its entry is gone. NEW's head consumes these rows even when NEW lists no trigger entry at all
   (RUNNER-SPEC §12.1).
4. **`changed`**: an allowed entry whose `revision` (PROG-9) differs from OLD's. It runs
   NEW's code when it comes due.
5. **`unchanged`**: every other row.

The report gives the exact count and earliest due time for each (class, kind, entry path,
argument schema). Each `held` group also carries `fate: Option[HeldFate]`, resolved from the
deploy's `Fates` (the prepare launch's fourth argument). Each `held` group is also keyed by
`hold: Option[HeldBy]`, `None` on other classes, with `HeldBy = Cut | Revision(same_code: bool) | Request(same_code: bool)`: `Cut` is a started run that is not
parked, `Revision` and `Request` name its hold, and `same_code` is true when the rows' stamp
equals NEW's identity digest. A `Request(same_code: true)` group was held by this code, and a `Revision(same_code: true)` group is a
rollback to the code that started it. The fate is one per entry path; `hold` informs it and never selects it
(RUNNER-SPEC v2.66 §14.6, residual R-2393-1). TRIGGERS is the value-codec
`TriggerReport { rows: List[TriggerScanRow], unmatched: List[Bytes] }`, and `unmatched` lists
each `entries` path that is the entry path of no pending row and of no entry OLD or NEW
registers. When every held group has a fate and `unmatched`
is empty, the child writes PREPARED with one activation rewrite per held row (STORE-SPEC v2.103
§3.5a; when NEW's migration replaces `store.triggers.pending`, the scan reads the staged range and the settled rows travel in that range instead); a rewrite list that does not fit fails prepare with `Limit(triggers)`. Otherwise it writes TRIGGERS and no PREPARED, and exits 0; the host holds the request for
a decision ("Self-editing"). When it writes no PREPARED (planned), the child also reports the
migration it staged, the 0x0082 payloads and the 0x0083 (id, kind) list, beside TRIGGERS,
so a hold shows the migration and the runs together. That side report is only displayed:
with no PREPARED nothing can cut over, and a PREPARED's facts come from OLD ("Self-editing"
"From check to cutover"). A report above the prepare child's bound fails
prepare with `Limit(triggers)`. The guard on started granted runs uses the identity digest,
whose entry revision covers changes in `handles` such as table names, bindings and key codecs,
and which a rebuild of the same code by another backend leaves equal.

Laws. Built for both the launcher (`crates/hirsel-core-driver/src/launcher/laws.rs`) and the
platform host (`core/programs/platform/platform.hirsel`, `core/programs/platform/platform_laws.hirsel`
covering L39, L41–L43, L45, L48–L49, L51–L52, and `runner.hirsel` L40): `group_one_head`, where a cutover's NEW head opens the store only after
OLD's head has closed it, and an extra launch of the group is refused `Held`;
`launcher_restart_stateless`, where a launcher killed at every step of a deploy and
restarted gives the host's reconciliation the same result as the host's records alone,
and a host killed at every step adopts the live children and starts no second serving
child of any group; `child_mode_authority`, for descriptors and the control socket;
`collect_reservation_handoff`; and L46 (RUNNER-SPEC §14.3), where a child's end samples
its listener before the watch resumes. The delegated `cgroup_lifecycle` law checks
that a successor recovers the predecessor's cgroup inventory and that an ordinary child's
grandchild PID is gone by `Ended`, and `law_managed_lifecycle_fails_without_delegation`
checks that `--lifecycle managed` fails startup without a delegatable cgroup v2 subtree
instead of falling back silently. The platform host suite
(`platform_laws.hirsel`) also covers `collect_reservation_handoff`'s host half,
admit-after-collect, store-identity enforcement, and drift-retry pause recovery.

## Stream ingress behind a TLS front

**H1** (dependency H1 of [TURN-2](turn.md#turn-2-the-tcp-fallback)). Port 443
on one address is shared: HTTPS for the places that serve HTTP, and TLS-wrapped byte streams
(TURN over TLS today) for places that serve something other than HTTP. This section says how
a place's non-HTTP stream listener sits behind the shared TLS front, and how the program learns
the client's real address. The design is general: the listener receives a plaintext byte
stream from a named TLS client and its address, and nothing in it is specific to TURN.

### The shape: a separate front that sends a PROXY v2 header

A separate TLS front terminates TLS on 443 and routes each connection by SNI. For a stream
name it opens a TCP connection to the place's stream listener. It writes one PROXY protocol
version 2 header first (HAProxy, `proxy-protocol.txt` §2.2), then relays plaintext both ways.
The place reads and checks the header before any byte reaches the program's protocol code.
*Not reversible without a new section:* this is the ingress contract for every non-HTTP stream.

Why this is the end-state, and an in-process terminator is not:

- **The host terminates nothing.** HTTP ingress already sits behind a front. The front
  owns TLS, and the listener reads `X-Forwarded-For` only from trusted prefixes
  (`deploy/wire/README.md`, RUNNER-SPEC R2-g). A program can terminate TLS itself on a
  listener it owns ([TLS-9](tls.md#tls-9-server-handshake)), and a standalone run, a local
  mock or a proxy does. A hosted place on the shared 443 does not. The address is shared by
  many groups, so something must read SNI before any group sees the connection. Terminating
  in each group would also put server certificates, private keys and renewal in every
  group's process, beside the front that HTTPS keeps. One front for both is like things
  treated alike.
- **The platform host never proxies bytes** ("Routing" above). A terminator in the launcher
  or host would make it a byte relay for every stream. A separate front keeps the launcher
  holding listeners, not traffic.
- **Reputable fronts already send the header, and route by SNI before terminating.** nginx
  stream `ssl_preread` plus `proxy_protocol v2` (v2 since 1.31.4; older nginx sends only
  version 1). HAProxy `req.ssl_sni` plus `send-proxy-v2`. Envoy `tls_inspector` plus the
  upstream `proxy_protocol` transport socket. Caddy L4's `tls sni` matcher plus the proxy
  handler's `proxy_protocol v2`. LiveKit runs TURN over TLS exactly this way
  (`ExternalTLS` and `ProxyProtocol` with trusted CIDRs, `pkg/service/turn.go:279`).
- **Not taken:** version 1 (text) headers. A receiver must not guess the format (§2), every
  front named above sends v2, and one decoder is enough. *Reversible:* v1 can be added as a
  second accepted signature later without changing anything here.

### Declaring a stream listener

A place opens its stream listener as it opens any listener: `host.listen` under its `Net`
`Listen` rule ([NET-1](net.md#net-1-socket-operation-declarations)), or the inherited listener
a hosted group receives from the launcher. What makes it an H1 listener is that the program
accepts on it through `proxy.accept` (below), configured with the trusted front prefixes.
Those prefixes come from the same place configuration as HTTP's `ingress` field (HTTP-1
`Config.ingress`, "configured proxies").

For a hosted group, the catalog's `groups` row gains `front: Option[Front]`, with
`Front { names: List[String], forward: Http | Stream }`. These are the SNI names the front
routes to the group's one listener, and the way it forwards them: as HTTP with
`X-Forwarded-For`, or as a stream with a PROXY v2 header. A group serves either HTTP or one
stream protocol on its listener, never both. The host's routing table carries `front`, and the
front's configuration is written from that table. The host still never proxies bytes, and no
group sees another's names. *Reversible:* the field is catalog data. A generator that writes
the front's configuration from the table is slice H1-c.

### Routing by SNI

- One name maps to one listener. The front reads the ClientHello's `server_name` (RFC 6066 §3)
  and picks the listener from the name alone. No ALPN is required. Browsers send none for TURN,
  so TURN cannot depend on one.
- A ClientHello with no SNI, or with a name not in the table, never reaches a stream listener.
  The front sends it to its default (HTTPS) route or closes it.
- An HTTPS name and a stream name cannot be the same name, because without ALPN nothing tells
  the two apart. A place that serves both uses two names, for example `turn.example.org` and
  `example.org`. *Reversible:* an ALPN split for a protocol whose clients do send ALPN can be
  added as a second key later.

### Certificates

The front selects the certificate by the same SNI name and holds every certificate and
private key, including ACME issuance and renewal. A place behind the front holds no server
key and sees no handshake. The TURN URL's host name (`turns:turn.example.org:443`) is the certificate's name.
*Reversible.*

### What the program sees

A plaintext byte stream on an accepted `Socket`, with the same operations, bounds and deadlines
as any accepted TCP socket (NET-1). The front ends TLS both ways:

- the client's `close_notify`, a TLS failure or a TCP end closes the upstream connection, and
  the program sees end of stream (receive returns 0, or `Reset`), which is its "disconnected";
- the program closing its socket makes the front send `close_notify` to the client;
- the front's idle timeout must be longer than the program's own idle rule, so the front never
  cuts a stream the program still counts as live.

`host.accept`'s `Addr` and `net.peer_addr` stay the kernel's peer, which is the front. The
client's address is a value that `proxy.accept` returns. The std implementation splits
the accepted socket for the bounded read, then rejoins it under NET-7.

### The header: required, checked, consumed exactly

```hirsel signatures std/proxy
// package std, module proxy
pub type Header { source: Addr, destination: Addr }  // the client, and the address it dialed
pub type Refusal = Untrusted | Missing | Malformed | Local | Family | TooLong | Silent | Ended | Failed(NetError)
pub type Step = Need(u64) | Done(Header, u64)  // total bytes needed so far; or the header and its length
pub type Config: shareable + send { trusted: List[ingress_policy.Prefix], header_max: u64, header_ms: u64 }

pub fn config(trusted: List[ingress_policy.Prefix], header_max: u64, header_ms: u64) -> Result[Config, ()]
pub fn step(b: Bytes, header_max: u64) -> Result[Step, Refusal]  // sans-IO; slice U6
pub fn accept(take s: Socket, peer: Addr, c: Config, time: Clock, accepted_ns: u64) -> Result[(Socket, Header), Refusal] waits
```

`config` refuses an empty `trusted` list, since such a listener could accept nothing. It also
refuses a `header_max` below 28 or above 65,551 (16 + 65,535). The defaults are `header_max`
536, the spec's smallest TCP segment payload (§2), and `header_ms` 3,000, the spec's "at least
3 seconds to cover a TCP retransmit" (§2).

On an H1 listener the header is **required**. `accept` takes the socket and returns it only
with a valid header. Every refusal drops the socket inside `accept`, which closes the
connection before any byte reaches the program. The checks run in this order:

1. **`Untrusted`.** `peer` (IPv4-mapped IPv6 normalized as in `wsock`) is outside every
   `trusted` prefix. This is checked before any byte is read. Ignoring the header, as
   `X-Forwarded-For` is ignored from an untrusted peer, is wrong on a byte stream. The 12-byte
   signature would reach the protocol code, and for STUN its first byte 0x0D reads as a
   message start. LiveKit uses the same REQUIRE-or-REJECT policy.
2. **`Missing`.** The first 12 bytes are not the signature
   `0D 0A 0D 0A 00 0D 0A 51 55 49 54 0A`. This includes a version 1 `PROXY ` line and a
   direct client that skipped the front.
3. **`Malformed`.** The version nibble of byte 13 is not 2, or the command nibble is neither
   0 nor 1. Or byte 14 is not a value §2.2 lists. Or the length in bytes 15 and 16 is shorter
   than the family's address block (12 for IPv4, 36 for IPv6). Or the bytes after the address
   block do not tile exactly into TLVs (type, two-byte length, value).
4. **`Local`.** Command 0 (LOCAL). §2.2 says a receiver accepts LOCAL with the real endpoints.
   On an H1 listener those endpoints are the front's, so a LOCAL connection would carry the
   front's address as the client's. *Deliberate deviation.* Fronts health-check an H1 listener
   with a plain TCP connect, which closes `Missing` after the connect has already proven the
   listener up.
5. **`Family`.** A PROXY command with byte 14 not `0x11` (TCP over IPv4) or `0x21` (TCP over
   IPv6): UNSPEC, UDP, or UNIX.
6. **`TooLong`.** 16 + length exceeds `header_max`. This is checked as soon as the first 16
   bytes are in, before the rest is read.
7. **`Silent`** and **`Ended`.** The header is incomplete at `accepted_ns + header_ms` on
   `clock`, or the peer ends first. **`Failed`** carries any other receive error.

`accept` reads exactly the header and nothing more. It receives with `max_receive` set to
the bytes `step` still needs (16 first, then 16 + length), so the program's first receive
returns the first byte after the header. TLVs are skipped, `PP2_TYPE_CRC32C` included: the
checksum is not verified, since the front is trusted and the link is local. *Reversible.*
A new TLV need adds a reader and changes nothing above.

`Header.source` is the client's address: the program's peer for every purpose, including
admission keys, quotas and TURN's XOR-MAPPED-ADDRESS. `Header.destination` is the address the
client dialed: the program's local address for that connection, which TURN's nonce binds.

**Where the deadline runs.** `accept` runs in the connection task the program spawns for
each accepted socket, never in the acceptor. A silent trusted peer therefore holds one
connection slot for `header_ms` and never stalls accepting. A cold hosted group works
unchanged: the header waits in the launcher-held backlog with the connection, and
`accepted_ns` is the accept, not the connect.

### Trust boundary

- The stream listener must be reachable only from the front: bind it to a private address,
  or to loopback with the front on the same host. A trusted prefix trusts every process at
  that address. A loopback prefix trusts every local process, so a loopback listener is safe
  only on a host where every local process is the operator's.
- Net authority is unchanged. The header names addresses and grants none. Accepted peers
  inherit the listener's authority (NET-6), and `source` is data the program keys on.
- The trusted list is the operator's, never the client's, and is separate from Net, as
  HTTP's `ingress` is.

### Build slices

| slice | size | depends on | content | model |
|---|---|---|---|---|
| **U6** (turn.md) | S | — | `proxy.step`, the sans-IO decoder: signature, version and command, family, length, TLV tiling, `TooLong` | Devin SWE-2.1 high, as turn.md assigns |
| **H1-a** stream accept | S | U6 | `proxy.config`, `proxy.accept`: the trust check before reading, exact reads, deadline from `accepted_ns`, drop on refusal; std API page | Sol |
| **H1-b** front deployment | S | H1-a | `deploy/ingress/`: HAProxy and nginx (≥ 1.31.4) configurations for one HTTPS name and one stream name on 443, and a `tools/tests/` script that runs both in containers like `wire_proxy.py` | Sol |
| **H1-c** catalog `front` | M | H1-a | the `groups` row's `front`, its routing-table entry, and the generator that writes H1-b's configuration from the table | Sol |

U6 and H1-a can land together. H1-b is the end-to-end proof, and TURN over TLS on a shared 443
needs only U6, H1-a and H1-b.

### Laws

- **`proxy_v2_header`** (U6 and H1-a, in std's own laws). It catches a decoder that misreads
  the layout, accepts what §2.2 forbids, ignores an untrusted or missing header instead of
  closing, or reads past the header. Cases: TCP4 and TCP6 headers built by hand from §2.2's
  layout, with and without a TLV, give exactly their `source` and `destination`. The same
  header delivered one byte at a time gives the same result. A stream byte written right after
  the header is the program's first received byte. An untrusted peer and a missing header are
  each closed with no byte read past the decision. LOCAL, UNSPEC, UDP, UNIX, version 1,
  a short length, a TLV overrunning the length, and a length over `header_max` are each closed.
  A silent trusted peer is closed at `header_ms`. *Oracle:* `proxy-protocol.txt` §2.2's byte
  layout, by hand.
- **`tls_front_stream`** (H1-b). It catches a front configuration that routes by anything
  but SNI, forwards a stream name without the header, or leaks one name's traffic to the
  other's listener. With HAProxy and nginx in turn: `openssl s_client -connect
  127.0.0.1:443 -servername stream.test` with no `-alpn` reaches the stream listener. The
  listener's `Header.source` equals the `s_client` socket's address, and bytes echo both ways.
  `-servername web.test` reaches the HTTP listener with one `X-Forwarded-For`.
  `-noservername` never reaches the stream listener. A direct connection to the stream
  listener from a non-front address is closed. Closing `s_client` ends the program's stream.
  *Oracle:* OpenSSL's own client and the kernel's view of its socket, not Hirsel's decoder.
