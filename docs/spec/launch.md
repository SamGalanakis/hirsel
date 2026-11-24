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

`revision` (RUNNER-SPEC v2.42, Spec Q5; not yet built) is the 32-byte `code.revision` of the
program's entry ([PROG-9](programs.md#prog-9-code-descriptors)), the value a recording's admission
already compares ([recording](../recording.md#what-a-recording-holds)), or unavailable for a
program assembled without revisions. It names the code, not the bytes: the same program built by
Cranelift or LLVM, at another optimisation level, or as a recording build has one revision and
different digests. The **release identity** is `revision`, `n_place` and the exported list
(RUNNER-SPEC §6 "One identity"), and it, not the digest, binds a store, matches a host record and
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
- An evaluator launching a built artifact hashes the artifact it loaded, and
  retains that artifact's target. It never hashes the evaluator executable.
  Source-only evaluation has no executable image and is unavailable.
- An embedded runtime receives the digest, target and `n_place` explicitly from
  its embedder before entry. It copies or pins the supplied value for the whole
  launch; later mutation of the embedder's input cannot change it.
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
`ReleaseIdentity { revision, n_place, exported }` taken from it (RUNNER-SPEC §6 "One identity").
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

The native launcher consumes the private prefix `--hirsel-describe`<!-- doc-ref: external --> and
`--hirsel-n-place=<bytes>`<!-- doc-ref: external --> before constructing user `Args`. The prefix is
the leading run of arguments that are recognised private launch options: the first argument
that is not one ends it, and that argument and every one after it become `Args` verbatim, so a
private option written after a place argument is never read as one. `--hirsel-install`<!-- doc-ref: external -->
and `--hirsel-activation=<version>`<!-- doc-ref: external --> are place arguments, read from `Args`
by `runner.launch_selection`, so they come after every private option. A malformed private
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
level or as a recording build opens it. Other code, another allowance or another exported
list is refused `NotCurrent` with the store's version and identity digest and this launch's,
and both executable digests as a diagnostic. To move a store to a new build, stop the old process and start
the new build once with `--hirsel-install`<!-- doc-ref: external -->, or deploy it with `hirsel deploy`.

A refused selection returns `NotCurrent(found)` before handles run. A disk
genesis or standalone install requires a known release identity (an available `revision`)
and returns `IdentityUnknown` if it is unavailable.

**Held runs (RUNNER-SPEC v2.64 §14.6 "The activation fate").** An install whose store holds
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
  `Dir=` followed by the path `--dir` took. The strings are a display of the recipe's grant bindings
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
`hirsel manifest show PATH` prints a manifest as text.

## Release recipe and host assembly

`hirsel build` works in two steps. It checks the program and writes its code objects and its
release recipe into a code store. Then it assembles the executable from that recipe. Both
steps are the only build path. `hirsel publish` runs the first step alone and prints R.
`hirsel build --recipe R --code-store DIR` runs the second step alone, from objects already in
DIR.

**Objects.** A code store holds immutable objects, each named by the hash of its own bytes:
bodies, groups and annexes (PROG-10), and recipes. A **recipe** has a version, the normal-form
version and these parts:

- the entry, the program's `main`, as its identity encoding (PROG-10) and its address;
- every group in the entry's closure, as its hash and byte length, in strictly ascending order
  of hash;
- every body in that closure, as its hash and byte length with its annex's hash and byte
  length, in strictly ascending order of body hash;
- the **foreign modules** ([FFI-3](foreign.md#ffi-3-archives-are-admitted-by-their-pin-before-linking)):
  one foreign leaf per reached foreign module, with its identity, symbol prefix, whole archive
  table and reached functions, in strictly ascending order of module identity;
- the **foreign archives** (FFI-3): the distinct archives the leaves and Foreign bindings
  select for the recipe's target, each its SHA-256 and byte length, in strictly ascending
  order of hash. An archive's bytes are a code-store object named by that SHA-256;
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

Its root is `R = H("hirsel.release-recipe.v2", recipe)`; the version is 2, and version 1, which
had no foreign parts, is never read or written. A place's `place`, `migrate`, handle
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
5. Every group, body and annex of the closure decodes within the limits under PROG-10's
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
   (`Missing`, `Extra`).
7. Every slot's identity equals the bound member's or builtin's identity, every `Internal`
   index is in range, each group's members form one strongly connected component, the entry's
   identity matches, and the closure has one address per identity with every slot's signature
   equal to its target's (PROG-10), a foreign function's signature being the one its header
   text states (`Binding`, `Conflict(identity)`). No two leaves have one prefix
   (`Conflict(identity)`, the second leaf's). Before anything is linked, each foreign archive
   passes FFI-3's format and symbol checks for the leaves that select it.
8. Every `Builtin(n)` and every engine-operation tag is one the recipe's backend implements at
   exactly that number (`Unsupported`).

Admission does not check types or ownership again. A refusal, a disconnect or a host restart
releases the attempt's reservations and discards its staging. Publication of outside source and
assembly run under the same kind of limits on time, memory and output.

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

`hirsel dry-run --exe NEW --store DIR` opens a private copy-on-write image of the place's offline store at DIR, runs NEW's `on_migrate` and then `schema` in mode `Migrate` against it, and prints what an activation would do: each table and index the declaration adds, drops or renames, each backfill with its row count, each unique index whose backfill would fail with the first conflicting key pair, and the sizes `store.sized` derives for the place's root limit with any `Shortage`. It commits nothing to DIR and exits nonzero when the activation would fail. `hirsel dev SERVER` runs SERVER against a `Temp` store, rebuilds on a source change, and on each rebuild performs the same migration on the live temporary store and resubscribes every open session, so a query's new slot list or R reaches the client as a `complete` group. Neither verb has a mode the serving path lacks: both run the ordinary place open (RUNNER-SPEC §6).

Backfill counts give the source rows read and the rows emitted into the
preview image. A unique backfill reports the first duplicate by encoded
index key, then the first two encoded primary keys for that key. The preview
keeps the first row in its scratch index so it can check later declarations.
Any such conflict makes the migration fail.

Dry run takes the Store's lock while it captures the private image. It
refuses a store held by another process. The candidate uses the root limit
embedded in the executable and the same Store sizing overrides it would use
when serving. Arguments after `--` go to the candidate.

`SERVER` is the package to rebuild. The supervisor gives its first directory
grant a temporary Store and holds a public loopback listener. The default
address is `127.0.0.1:7700`, and `--listen ADDR` changes it. The server uses
`runner.launch_selection` so the supervisor can run the ordinary install
before restarting it. A build refusal leaves the current executable serving.
A migration refusal before activation reopens the current executable.
Its install passes `--hirsel-held=release`<!-- doc-ref: external -->, so a run in flight across a rebuild continues
on the new code (RUNNER-SPEC §14.6 "The activation fate"). The
supervisor retains each WebSocket, repeats its Hello and live Subscribe
requests on the new connection, and forwards the complete groups with the
original subscription numbers. It uses the new query's result fingerprint
when the named entry still has the same argument fingerprint.

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
is its store, and its one Unix endpoint is the launcher's control socket; its arguments are
that socket's root label and path, `max_places` and `idle_ms`. The host opens its catalog,
connects, sends `Hello`, and then runs one loop over one queue. A reader task posts each
launcher event into the queue in stream order, beside the host's two alarms: one absolute
wall-time alarm on the earliest due time (RUNNER-SPEC §14.3) and one monotonic alarm on the
earliest backoff retry (§14.5). After every input the host applies it, sends the requests
its commit allows in order, and re-arms each alarm on its current minimum, so neither wait
is armed only at start and nothing polls. A lost session, or a frame that does not decode,
ends the host with status 75. The launcher starts it again, and the new session's inventory
reconciles it.

**Quotas.** A start, a deploy or an added group that would exceed its tenant's quota is
refused with the quota it exceeds. The host never evicts a live group to admit another
(RUNNER-SPEC §14.2, "No eviction").

### Starting the host

`hirsel launcher --state S --control S/launcher.sock --operator-listen S/operator.sock
--platform-host PREFIX/libexec/hirsel/platform_main [--max-places N] [--idle-ms M]` starts
the platform host as its first and only bootstrap child. N and M are positive decimals,
1 and 30000 by default. The launcher binds both sockets mode 0600, creates `S/catalog`, and
starts the installed executable with working directory S and the program-visible argv
`platform_main ctl launcher.sock N M`. The host's grants are compiled into that executable:
its `Dir` is `/proc/self/cwd/catalog`, and its Net root `ctl` is `/proc/self/cwd` with
`UnixConnect ctl launcher.sock` and `UnixListen ctl operator.sock`, beside Clock, Random and
Tasks. Nothing in the host changes its working directory. A missing executable is an
incomplete installation, and the launcher ends with status 74.

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
frames, and a writer task per connection sends its answers from a bounded queue. A
connection whose queue is full or whose write failed is dropped and detached, so no
operator stalls the loop. A `Begin` before the session's inventory is reconciled is answered
`Error(Busy)`.

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
| 0 Begin(D, G, input, foreign, exec, held) | `input` is `launcher.Input`: 0 Attested(bytes), 1 Publication(H), 2 Source(tree). `foreign` and `exec` are the sorted, duplicate-free Foreign definitions and Exec requests the operator approves. `held` is the deploy's `Fates { all: Option[HeldFate], entries: List[EntryFate] }`, with `HeldFate` tags 0 Release and 1 Cancel and `EntryFate(path, fate)`. It is the fate of each run the deploy holds (RUNNER-SPEC §14.6 "The activation fate"). `entries` that are not sorted by path or that repeat a path are `Invalid`. A source that is not a tree within 4 MiB is `Invalid`. For an untrusted author the host refuses, as `Denied` and before anything compiles, any input but a source, a source whose tree grants `Foreign` or `Exec`, any approved Foreign or Exec, and a recording group. |
| 1 Data(D, A, bytes) | One nonempty chunk of the admission byte stream, only on the connection that owns D's upload under A. |
| 2 EndInput(D, A) | Ends that stream. A publication needs neither Data nor EndInput. |
| 3 Confirm(D, A, displayHash) | Confirms the exact display the client checked, printed and flushed: displayHash is SHA-256 over the display's UTF-8 bytes. Any other hash is `Error(Denied)`. |
| 4 Status(D) | Returns State, or `Error(UnknownRequest)`. Does no work. |
| 5 ResolvePin(I) | Returns Pin, looked up at the host's wall clock in Unix seconds, or `Error(Store)`. |
| 6 LookupGroup(name) | Returns GroupInfo(G) for the registered incarnation, or `Error(UnknownGroup)`, also while the group is still registering. |
| 7 Register(D, registration, input, foreign, exec) | `registration` is (name, tenant, workdir, trusted, listen, store, memory, args, record), with `memory` an `Option[u64]` in bytes and `record` an `Option` of (dir, retentionSeconds). Registers a new group and deploys its first release, below. `input`, `foreign` and `exec` are Begin's. |
| 8 Decide(D, grants, approve) | A person's decision on D, which must be in AwaitDecision on exactly this grant diff ("Deploy preview"); anything else is `Error(Denied)`. An approval repeats D's admission under a fresh A, this connection owning it; a refusal fails D `Denied`. |

| Host → operator | Payload and rule |
|---|---|
| 0 State(D, G, input, phase) | A committed snapshot. Phase tags 0 AwaitInput(A), 1 Admitting(A), 2 AwaitConfirm(A), 3 Assembling(A), 4 Preparing(A), 5 Pending(A, v), 6 Undecided(A, v, reason), 7 Installed(v, assembly, report), 8 Failed(class, detail, report), 9 AwaitDecision(grants). Installed and Failed are terminal and end the exchange. AwaitDecision holds no attempt. A known D keeps the same G and input for its whole life. |
| 1 Need(D, A, keys, last) | One of the launcher's ordered parts of the missing set (`List[Wanted]`, bool). |
| 2 Display(D, A, R, text) | The exact Foreign and Exec text to confirm, bound to the verified R. No child runs before it is confirmed. |
| 3 Pin(resolved) | 0 Found(assembly) or 1 Gone. |
| 4 Error(class, detail) | Class tags 0 Protocol, 1 Denied, 2 UnknownRequest, 3 UnknownGroup, 4 Conflict, 5 Busy, 6 Full, 7 Invalid, 8 Unassembled, 9 Store, 10 Uncompiled, 11 NotInstalledAfterRecovery, 12 Exhausted, 13 Unsettled. Uncompiled carries a source's checker diagnostics; Exhausted is a source whose compile ran out of memory. Unsettled is a deploy whose trigger report has a held run with no fate, or an `entries` path that is the entry path of no pending run and of no entry OLD or NEW registers; its report names them. Failed uses the same classes. An exchange error never changes another request's outcome. |
| 5 GroupInfo(G) | A group lookup's result. |
| 6 Part(total, offset, bytes) | One in-order chunk of an encoded reply larger than one frame, from offset zero, contiguous, ending exactly at total. |

A report is (fetchNs, assembleNs, pause, preview): two `Option[u64]`, a pause (0 NotStarted,
1 AwaitWrite, 2 Measured(ns), 3 MeasurementLost, 4 StartFailed(detail)) and the deploy's
preview ("Deploy preview"), whose parts not yet computed when the deploy ended are empty.

**Durable identity.** The catalog keeps one `requests` row per D: its canonical Begin bytes,
G, input, current attempt, admission deadline and, once decided, its terminal phase, and one
`active` row per group naming the request that holds it. The first transaction for a new D
binds it to its Begin bytes and allocates A from a durable counter before any launcher
request. A Begin whose D is known is looked up before any wake or admission. Different bytes
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

`hirsel host --state S [--max-places N] [--idle-ms M]` runs the launcher
of "Starting the host" with `--control S/launcher.sock`, `--operator-listen S/operator.sock`
and the installation's `libexec/hirsel/platform_main`, and does not return. The state
directory must be the operator's and mode 0700.

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
  sends Decide approving it and goes on; otherwise it prints the state, with the diff, and
  exits 2. `--held`<!-- doc-ref: planned --> sets `held.all`, and each `--held-entry`<!-- doc-ref: planned --> adds an element of `held.entries`, where PATH is the entry path as the trigger report prints it. Without either option, `held` is `Fates(all: None, entries: [])`. `hirsel deploy --state S --request-id D --refuse-grants` refuses the diff D
  awaits a decision on. `hirsel deploy --state S --status D` sends only Status.

Register and deploy stream the admission input on AwaitInput, answer each Need with the missing objects,
and on Display check that the text is the one their own approvals produce, print it, and
send Confirm with its SHA-256. `Error(Busy)` is retried with the same D for up to 60 s.
Their exit status is 0 for `Installed` (1 when the pause report is `StartFailed`), 2 for a
local refusal, a `Failed(Denied)` that is a policy refusal (FFI or Exec), or a `Failed(Unsettled)`, which prints the trigger report so that the operator can choose the fates for a new request, 1 for any other
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
  `unmatched` path, fails the deploy `Unsettled`: OLD's export is aborted once, and OLD
  keeps serving with its store unchanged (RUNNER-SPEC §14.6 "The activation fate").

The report of every terminal phase carries the preview as far as it was computed.

**Activation.** A deploy whose grant diff is empty assembles and cuts over with no further
step. A nonempty diff waits for a person's decision, before `Assemble`, because every
child's authority, the preview child's included, comes from the group's approved bindings
("Child modes"). The host releases the attempt and records the diff in the request's row,
in one transaction, and the request's phase is `AwaitDecision(diff)`. The hold has no
attempt and no deadline, survives its connection and the host's restart, and ends at a
`Decide` or when a later Begin of the group supersedes it (`Failed(Denied)`). A Decide must
name exactly the held diff: an approval records it as the request's approved diff and
repeats the admission under a fresh attempt and deadline, owned by the deciding connection,
and that admission passes the gate only when it finds the same diff; a refusal fails the
request `Denied`. Approval comes through the operator client (`--approve-grant`) or any
caller the gateway lets decide, under the same Caller rules.

Laws: L48 `deploy_admission_policy` (the hold and the decision) and L54 `deploy_preview`
(the entry diff and the trigger report), in RUNNER-SPEC §14.3; `grant_diff_names_each_authority`
in the launcher. The candidate re-run on a store fork that reports prompt-cache diffs
is separate from publishing.

### Publishing a harness proposal

An app that enables `harness.release` exposes WIRE queries for the pending proposal and
its source, and a transact entry to settle it. Each checks the publisher's authenticated
Caller before forwarding to `release.pending_cover`, `release.source_cover` or
`release.settle`.
The publisher is an operator-side application with a connection to those entries and
the operator socket. The app's tools propose the source rows and their build settings
(`package`, `grants` and `dirs`). The publisher supplies no Foreign or Exec approvals, and no fates for held runs (`held` is
`Fates(all: None, entries: [])`). A proposal deploy that would hold a run therefore fails
`Unsettled`, and the publisher settles it as Rejected. Whether a run's recorded steps are still
valid on new code is the operator's decision.

Run one `harness.release_publish.Publisher` per group incarnation. Drive `poll` when
work is available and while a request is outstanding. Reconnect the app client when
the cutover closes its connection. A reconnect as the same Caller continues that
Caller's WIRE request positions. A fresh authorized Caller has its own positions.
The publisher reads one file per WIRE query, then verifies the full tree hash. Give
both ends a WIRE frame allowance large enough for the largest source file and its
envelope.

The publisher derives D from the group name, incarnation and proposal ID. It asks
Status before reading the named tree. An unknown D submits the unchanged tree through
a Source Begin. The workspace must include its `hirsel.lock` and its pinned packages
must be in the host's package store. A source tree that moves or fails the frozen
check is rejected. After a restart, a known D uses its recorded input and phase. A completed request therefore cannot deploy again
even if the source rows have since changed. A Source replacement uses OLD's verified recipe memory and place allowances. A
grant difference follows the ordinary AwaitDecision and Decide exchange. The publisher settles Installed as Published and a
failed request as Rejected, retaining the failure text. The app's settle entry can
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

`hirsel launcher --control SOCKET --state DIR` serves the control
session of RUNNER-SPEC §14.5. It holds the launcher lock and the groups' listeners, starts
children, and collects unused objects. `Bind` names the group and its listener address.
`Start` selects
the child's mode, and a serving `Start` may name a recording (its 16-byte recording ID
and destination), which the child receives as `HIRSEL_RECORD` and `HIRSEL_RECORD_ID`.
The launcher waits on a single epoll set for its control session, cold listeners,
child endpoints, pidfds and cgroup population changes.

- **Publish and assemble.** It admits recipes (steps 1-8 above) and assembles images. It
  writes objects with the "Durable publication" order above, steps 1-3. Step 4, the pending
  record, is a host catalog row, which the host commits only after the launcher has
  answered that steps 1-3 are synced. Admission runs on the control session, one attempt
  per deploy attempt, at most four at once:
  1. `Admit(group, attempt, input, approvals)` opens it. The input is `Attested(root)`,
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
     same files.
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
     the recipe's target and place allowance, and the approval text (foreign archives and
     `Exec` profiles). The host decides the approvals from it. OLD and NEW serve side by
     side through the cutover, so the two root limits together must fit the group's
     memory limit, or the host answers `Refused(Memory)`, which releases the attempt. A
     nonempty grant diff the operator has not approved releases the attempt and holds the
     request for a decision ("Deploy preview"). Nonempty
     approval text waits for the operator to confirm exactly that text. An untrusted
     author's deploy is source only, without Foreign or `Exec` grants, and is refused
     before `Admit`. The host checks NEW's preview against the target and allowance, and records its identity digest.
  4. `Assemble(attempt)` builds and syncs the image and its assembly record, and
     `Assembled(attempt, assembly, digest, fetch, assembly_ns)` answers it with the
     assembly name, the image digest and the fetch and assembly times in nanoseconds. The
     launcher then holds a reservation of the assembly for the attempt until the host
     releases it, which it does once the deploy's Preparing row is durable.
  `AdmissionFailed(attempt, failure)` (`Denied`, `Invalid` or `Unassembled`) ends an
  attempt at any step. `Release(attempt)` or a new session cancels a running attempt.
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

Under a delegated cgroup v2, the launcher uses a subtree keyed by its state directory.
Each child is born in its
own cgroup with a pidfd. After the leader exits, the launcher requests `cgroup.kill`
and reads `cgroup.events`. If the cgroup is still populated, epoll waits for a population
change. The launcher reports `Ended` after observing `populated 0`, reaping any adopted
descendants and removing the cgroup. An explicit kill targets the whole cgroup.

On a host without a delegated subtree, the pidfd tracks and reaps the direct child.
Its descendants can outlive it and can keep inherited descriptors open. Neither path uses a pidfile.

### Child modes

| Mode | Runs | Store | Inherited listener | Private descriptors |
|---|---|---|---|---|
| install (genesis, standalone) | `place`, `migrate`, `handles` | the group's store | none | its answer on stdout |
| describe | `place` | none | none | its answer on stdout |
| preview | `place` | none | none | its answer on stdout |
| prepare | `place`, `migrate`, `handles` | the export directory | none | none |
| serving | the place | the group's store | the group's listener (3) | its private endpoint (4) and wake report (5) |

The image's recipe declares its Hirsel grants. The launcher runs children under its
OS user and uses cgroups for lifecycle management. The serving child's private endpoint
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
`hold: Option[HeldBy]`, `None` on other classes, with `HeldBy = Cut | Revision | Request(same_code: bool)`: `Cut` is a started run that is not
parked, `Revision` and `Request` name its hold, and `same_code` is true when the rows' stamp
equals NEW's identity digest. A `Request(same_code: true)` group was held by this code
(RUNNER-SPEC v2.64 §14.6, residual R-2393-1). TRIGGERS is the value-codec
`TriggerReport { rows: List[TriggerScanRow], unmatched: List[Bytes] }`, and `unmatched` lists
each `entries` path that is the entry path of no pending row and of no entry OLD or NEW
registers. When every held group has a fate and `unmatched`
is empty, the child writes PREPARED with one activation rewrite per held row (STORE-SPEC v2.102
§3.5a); a rewrite list that does not fit fails prepare with `Limit(triggers)`. Otherwise it writes TRIGGERS and no PREPARED, and exits 0; the host fails the deploy
`Unsettled`. A report above the prepare child's bound fails
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
grandchild PID is gone by `Ended`. The platform host suite
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

- **The host terminates nothing today.** HTTP ingress already sits behind a front. The front
  owns TLS, and the listener reads `X-Forwarded-For` only from trusted prefixes
  (`deploy/wire/README.md`, RUNNER-SPEC R2-g). The runtime has a client handshake only
  ([TLS-1](tls.md#tls-1-client-handshake)): server-side TLS is not a provider operation
  ([TLS-4](tls.md#tls-4-provider-calls)). An in-process terminator would add server
  certificates, private keys and renewal to every group's process. It would also add a second
  TLS stack beside the front that HTTPS keeps. One front for both is like things treated alike.
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
private key, including ACME issuance and renewal. A place holds no server key and sees no
handshake. The TURN URL's host name (`turns:turn.example.org:443`) is the certificate's name.
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
client's address is a value that `proxy.accept` returns. *Reversible:* the runtime does not
change, and moving the decode into the runtime later would only move this code.

### The header: required, checked, consumed exactly

```hirsel unbuilt
// package std, module proxy
pub type Header { source: Addr, destination: Addr }  // the client, and the address it dialed
pub type Refusal = Untrusted | Missing | Malformed | Local | Family | TooLong | Silent | Ended | Failed(NetError)
pub type Step = Need(u64) | Done(Header, u64)  // total bytes needed so far; or the header and its length
pub type Config { trusted: List[wsock.Prefix], header_max: u64, header_ms: u64 }

pub fn config(trusted: List[wsock.Prefix], header_max: u64, header_ms: u64) -> Result[Config, ()]
pub fn step(b: Bytes, header_max: u64) -> Result[Step, Refusal]  // sans-IO; slice U6
pub fn accept(take s: Socket, peer: Addr, c: Config, clock: Clock, accepted_ns: u64) -> Result[(Socket, Header), Refusal] waits
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
