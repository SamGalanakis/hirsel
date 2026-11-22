# Runner launch modes and identity

The runner has ordinary, describe and preview launch modes (preview: below, v2.15). The launcher selects the mode
before program code runs. Both modes enter through the program's `main`, which
calls `runner.main`; describe is the first branch of `runner.main`, before store,
listener or other capability use. Selecting describe grants no capabilities.
Arguments and the describe response use private launcher transport, not grants
to the program. A program must reach this branch without using a capability.

## Immutable launch identity

The runtime supplies one private launch metadata value with `digest`, `target`
and `n_place`. `digest` is the 32-byte SHA-256 digest of the executable image's
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
record the allowance of the described launch. A const-parameter mechanism was
considered and left out: it is a separate language feature and is unnecessary
for launch configuration. There is no compiler rule naming `store@1/runner` to
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
- An image that cannot be read at launch produces `Unavailable(reason)`.
  Missing source-only or embedded identity has the same representation. It
  does not fail launch. Its digest is empty; the known target and `n_place`
  remain available.

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
`Welcome` carries `(caller, q, budget, executable, target, n_place)` under
tag 4 of `hirsel.2`. Target and allowance are independent of the executable digest.
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
type Describe: codec + send { digest: Bytes, target: String, n_place: u64, exported: List[Exported] }
type ReleaseManifest: codec + send { describe: Describe, release: Bytes, grants: List[String], root_limit: u64, recipe: Bytes }
```

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

The native launcher consumes the private prefix `--hirsel-describe` and
`--hirsel-n-place=<bytes>` before constructing user `Args`. The allowance defaults
to 16 MiB. An ordinary launch can also select its allowance with the latter
option. Evaluator and embedded launch configurations carry the same mode and
allowance explicitly. SDK generation uses the driver's `describe_child` helper;
the CLI and generator share its bounded read, absolute deadline, kill and reap.

Laws: `place_run_bounded` kills a looping place at its deadline and observes no
store or listener; `identity_stable` keeps the launch digest after executable
replacement; `identity_unavailable` launches with an empty digest and produces
no mismatch diagnostic when the image is unreadable or evaluation is source-only.

## Place activation

A place passes the result of `runner.launch_selection(Args)` to its runner. The
ordinary place option `--hirsel-activation=<version>` selects the activation
version that must match the current image under STORE's LOCK. Omitting the
option selects version zero for empty activation. `--hirsel-install` requests a
standalone install and is mutually exclusive with an activation version. Use it
after `--` with `hirsel run`. Both options are run arguments, which a recording
holds like every entry argument.

A refused selection returns `NotCurrent(found)` before handles run. A disk
genesis or standalone install requires a known executable identity and returns
`IdentityUnknown` if it is unavailable.

A prepare launch takes `--hirsel-prepare <absolute-export-dir> <attempt> <image-key>`
as its place arguments. The attempt and image key use padded Base64 for 16 and
32 bytes. The native launcher binds directory designations to that export
directory and withholds network rules and the inherited listener. The place
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
  child ran with. An ordinary launch that selects another `--hirsel-n-place`
  is a different release identity, and Welcome and the status route report it.
- `release` is the preview child's `release`, which it computes with
  `runner.release_digest`: SHA-256 over `u8 17 ‖ "hirsel.release.v1" ‖` the
  value codec's bytes of the registry's exported list. The driver never hashes
  the list itself. The running place computes the same function from its
  registry and reports it on the status route.
- `grants` is every grant baked into the executable, one canonical string each,
  sorted bytewise: a kind name as `--grant` takes it (`Clock`, `Random`, …),
  `Net=` or `Env=` followed by the rule or pattern text `--grant` takes, `Foreign=`
  followed by the interface's definition path `D` (`Foreign=sqlite@1/sqlite/Sqlite`), `Exec=`
followed by the profile file path and the profile names as `--grant` took them (unbuilt,
[EXEC-1](exec.md#exec-1-an-exec-grant-confers-what-its-programs-can-do-and-hirsel-does-not-confine-them)), and
  `Dir=` followed by the path `--dir` took. The strings are a display of the recipe's grant bindings
  (below), which say which parameter receives each one. A directory path stays as
  given. It is resolved against the launch's working directory at startup, which
  the manifest does not know.
- `root_limit` is the executable's root limit.
- `recipe` is the 32-byte root R of the release recipe the executable was assembled
  from (below). `release` says what the public interface runs, R says what code was
  assembled, and the digest says which bytes run. R does not enter `release`, and the
  store's `releases` rows do not hold it.

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
`Exec` parameter (unbuilt) takes the profile file path as given and the selected profile
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
caller's working directory. `hirsel host` launches every child of a place (preview, prepare,
serving and install) in that place's working directory, set by `--workdir DIR` when the place
is added to the host. A recipe that moves to another host therefore resolves its relative paths
against that host's place directory, and the deploy preview prints each `Dir` binding with the
absolute path it resolves to there.

**Authority.** A host takes a recipe only when an authorised deploy request names its R. In
`hirsel host` that is a request on the control socket, whose filesystem permission is the
boundary. A request that names a raw R is the caller's explicit statement that a checker it
trusts produced this whole R, its bindings and annexes included; a hash proves only that bytes
are intact, and never that they were checked. A caller with narrower authority, such as a
deployment service that only schedules deploys for others, never forwards a raw R. It either
selects a publication the host has recorded, by its R, or submits source, which the host
publishes itself with the normal checker, using the host's own compiler caches and package
images and the bounds below. The operator still approves the grant bindings each deploy would
give, as the preview shows them. A recipe that grants `Foreign=D` is deployed only when the
request passes `--grant Foreign=D` itself, and the deploy's output shows, before any child runs,
the interface, target, selected archive digest and recipe of each such grant
([FFI-4](foreign.md#ffi-4-the-foreign-capability)). A recipe that grants `Exec=FILE NAME`
(unbuilt) is deployed only when the request passes that grant itself. The host then loads the
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

1. The request comes through the control socket and names R. The recipe's bytes are within
   `--max-recipe-bytes`, they hash to R, and they decode completely and canonically (PROG-10's
   rule for one encoding per value): every list in strictly ascending order with no repeat, no
   bytes left over, and a version and normal-form version the host supports. Counts are within
   `--max-closure` objects. A failure is `Unsupported`, `Hash` or `Malformed`.
2. The host takes the collection lock and reserves every object the recipe lists that the code
   store already holds, so collection cannot remove it, and only then computes what it lacks.
3. It reserves staging space for the lacking objects' listed lengths against its host-wide
   `--staging-bytes`, within the attempt's limits on total bytes, on memory for decoded
   objects, on members, slots and nesting, on outstanding objects and on elapsed time
   (`--admit-timeout`). A request over a limit, or one the host cannot reserve now, is
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
is the hash of those three. The **toolchain fingerprint** is a hash of the effective environment
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
so the store's `releases` rows keep their format, and a recording pin names the assembly record of
the run it records and is synced before `Pinned` is answered. A retained image carries its runtime. The host reads its recipe envelope to recover
launch settings and keep referenced objects. A retained digest whose image is missing gives `Unknown(ImageGone)`.

`hirsel host --record DIRECTORY` assembles with the recording runtime, and refuses at
admission any recipe with a foreign leaf
([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused)). The host
assigns each serving process a recording ID and syncs its pin before launching
the process. A pin names the assembly record and carries its expiry. The
default retention period is 30 days, set by `--tape-retention DAYS`.
`hirsel replay --state HOST_STATE --tape TAPE` resolves the recording ID through
the pin, refuses a recipe with a foreign leaf before reading any record of the tape
(FFI-8), and runs the verified image from the host image store. Replay takes the
arguments and launch metadata from the recording.

When the host starts, before any collection, it reconciles every attempt. Staging with no
pending record is removed. For each pending record it probes the place as hot deploy's recovery
does: if the recovered activation is the attempt's (a cut after install but before its
acknowledgement), it writes and syncs the index entry for that activation, then removes the
pending record; otherwise the attempt was abandoned and its pending record is removed once the
recovered activation's own roots are durable. Old and new roots are both kept until then.

**Hot deploy.** `hirsel deploy R --code-store DIR` admits R, fetches what is missing, assembles
with the host's installed toolchain for the recipe's backend and target, and publishes the
assembly record and the pending record, in that order. All of this happens before the preview
and before the old place exports, outside the write pause. A refusal or assembly failure stops
the deploy before anything is touched. The preview, the prepare child and the cutover all use
the attempt's one assembly record: the preview runs that record's image with the recipe's
`n_place` and grant bindings; the prepare child is given that image's digest; and before the
host accepts the import it checks that the prepared target's `Describe` and release record are
the ones the preview reported for that record. A retry after drift keeps the record and takes a
fresh attempt and export. The write pause keeps hot deploy's own checks (attempt and prepared
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

`hirsel preview --exe NEW NEW.manifest OLD.manifest` compares two releases
without a store, a listener or a write grant. It refuses with exit 2 when NEW's
launch digest differs from NEW.manifest's, or either is unavailable. It then
runs NEW as a child in preview mode, with the same deadline, kill and reap as
describe. The child's arguments are the private prefix `--hirsel-preview` and
`--hirsel-n-place=<bytes>`. It reads OLD's `List[Exported]` on stdin, bounded
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

`hirsel confirm MANIFEST ADDR` reads `GET /hirsel/status` at ADDR under an
absolute deadline, 10 seconds by default (`--confirm-timeout`), and prints
`confirmed`, `not ready: <phase>` (a 503), `not listening`, `different:
<fields>` or `unknown` (an empty digest on either side, with the other fields
equal). It exits 0 only for `confirmed` (RUNNER-SPEC §12.2).

Laws: `preview_is_placed` (RUNNER-SPEC L31) checks the preview's answers against
a live session's `Placed` answers for the same list; `manifest_exact` (L35)
checks the grants, the root limit and `release`; `confirm_identity` (L32);
`place_run_bounded` also covers the preview child, which observes no store or
listener.

## Platform host and launcher (unbuilt)

This section specifies how many programs from many authors are run on one machine. It was
ratified as Agent Platform 2027, Topics 2 and 3 (2026-11-22). Nothing in it is built. The
cold-place wake contract it relies on is RUNNER-SPEC §14.

### Authority groups

An **authority group** is the unit a platform runs. One group is one program, one release
recipe R, one set of grant bindings, one store, and while it is live, one process that holds
that store. The group's name is its key in the platform catalog. It never changes, and no
two groups share a store directory. A deploy replaces a group's recipe and keeps its name and
store. Grants are approved per group, at each deploy, as the deploy preview shows them
(above).

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
  above. It never accepts a raw R, a code object or an image from such an author, because
  admission does not check types or ownership again (step 8 above). Foreign code, an
  `Exec` grant and a recording build are refused for a group whose author is untrusted
  (FFI-1, EXEC-1).

### The platform host

The **platform host** is a Hirsel program. It is itself a place with its own store, and it
runs under the launcher like a group, as the operator's group. Its catalog is ordinary
tables of that store:

- `groups`: per group name, its tenant, its working directory, its author class (trusted or
  untrusted), its containment label (below), its current assembly record and activation
  version, its retained activations' assembly records, and its listener address;
- `wake`: per group name, the one wake row of RUNNER-SPEC §14 (`Dirty(gen)` or
  `Clean(Option[next_due])`) and its generation counter;
- `deploys`: each pending deploy attempt and its assembly record (the "pending record"
  above);
- `pins`: each recording pin and its expiry;
- `quotas`: per tenant, its limits on groups, live groups, bytes and deploy rate;
- `config`: the host's `max_places` and its other bounds.

The host does routing, quotas, wake scheduling and deploy orchestration. It decides which
group runs, when it starts and stops, which recipe a deploy installs, and which objects are
still roots. It does not compile, assemble, confine or spawn. It asks the launcher for those
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
that backlog. The launcher reports the first readable listener of a cold group to the host
as `Arrived(group)`, and the host wakes the group (RUNNER-SPEC §14.3). Clients find a group's
address from the host's routing table. The host never proxies bytes.

**Quotas.** A start, a deploy or an added group that would exceed its tenant's quota is
refused with the quota it exceeds. The host never evicts a live group to admit another
(RUNNER-SPEC §13.4, "No eviction").

### The launcher

The **launcher** is a thin Rust program. It holds no record of its own that a decision reads
again: every decision record is in the host's catalog. What it keeps on disk is
content-addressed: the code store, the image store and the assembly records, each named by
the hash of its own content (above). Such a file is either complete or absent after a crash,
so keeping it is caching, not state. Its operations are these:

- **Publish and assemble.** It admits recipes (steps 1-8 above), publishes source for
  untrusted authors, and assembles images. It writes objects with the "Durable publication"
  order above, steps 1-3. Step 4, the pending record, is a host catalog row, which the host
  commits only after the launcher has answered that steps 1-3 are synced.
- **Launch.** It starts a group's children: describe, preview, prepare and serving (below),
  and the install launch of genesis. Each child runs a retained image by digest, from the
  image store, in the group's working directory, with the group's grant bindings. It
  reports each child's exit to the host, with the Clean report of RUNNER-SPEC §14.4 when the
  child sends one.
- **Collect.** It removes objects only when the host names the full root set: the union
  that "Durable publication" lists, which the host reads from `groups`, `deploys` and `pins`,
  together with the launcher's own live reservations. A reservation is an object set that
  the launcher has synced for an attempt whose pending row the host has not yet committed;
  it lives in the launcher's memory and ends when the host commits or abandons the row. A
  launcher that has no current root set from the host removes nothing.

On a restart the launcher has nothing to recover. It removes staging, and it reports to the
host that every child it had has ended. Under containment it has first emptied each child's
container (below). The host then reconciles from its catalog: each pending deploy as "When
the host starts" says above, and each `Dirty` wake row as RUNNER-SPEC §14.5 says.

**One control socket.** The launcher's control socket is the platform's single
authority boundary, as the control socket of "Authority" is. Its filesystem permission
admits only the host's OS user. Operators and tenants reach the platform through the host,
which decides and records each request before it asks the launcher. `hirsel host` is this
pair: the launcher, plus the platform host it starts as its first group. `hirsel deploy`
sends its request to the host.

**The launcher is the parent, not an `Exec` child.** The host does not start groups
through an `Exec` grant, and it does not start the launcher as one either. An `Exec`
profile could pin the launcher's own image (EXEC-4), so the pin is not the reason. These
are:

- **Groups outlive the host.** A run's children die with it (EXEC-6). If the launcher were
  the host's `Exec` child, a host crash would end the launcher, every live group and every
  listener, so one fault in the operator's group would stop every tenant. With the launcher
  as the parent, a host crash ends only the host: live groups keep serving, cold groups'
  listeners keep their backlogs, and the restarted host reconciles (below).
- **Descriptors.** An `Exec` child receives only descriptors 0, 1 and 2 (EXEC-1, EXEC-3).
  The launcher must hold one listener per group and pass each to its serving child, which
  no `Exec` spawn can carry.
- **Containment needs setup that `Exec` forbids.** EXEC-3 starts each child with
  no-new-privileges and keeps it out of the run's private state (EXEC-7). The launcher must
  create and own each child's container (R27), which the operator's start gives it.

`Exec` is still built under this item as a grant that a group of a trusted author may hold
(to run tools), contained by R27 like the rest of the group's processes. The launcher, which the operator starts, is
the only process that starts a group's children, and it owns their containers.

### Containment (R27)

Every child of a group carries one **containment label**, fixed in the group's catalog row:

- **`contained`.** The launcher runs the child under R27's containment
  ([reserved](reserved.md)). The child can reach only the group's working directory and
  the files its `Dir` bindings name, only the network its `Net` bindings and its inherited
  listener allow, and only its own descendants. Every descendant is in a container that the
  child cannot leave, such as a delegated cgroup or a PID namespace. A stop or kill of the
  child ends every process in it, and the launcher reports the child ended only after it
  has seen that the container is empty. CPU and memory have OS limits from the group's
  quota.
- **`uncontained`.** The launcher runs the child as an ordinary process of its own OS user,
  with OS CPU and memory limits from the group's quota and EXEC-6's best-effort death of
  the direct child. Only a group whose author is the operator, or a first-party author the
  operator names, may carry this label. Its program is compiled from source by the
  platform, reaches no foreign code, and holds credentials only in connector groups.

The label covers **every** child kind of the group: serving, describe, preview and
prepare/migrate. Each of these runs code from the group's program (`place`, `migrate` and
`handles` included), so a confined server with an unconfined preview would confine nothing.
A launcher that cannot enforce `contained` on its host refuses at startup to admit any
contained group, and it never runs such a group uncontained. A group whose author is not
the operator or a named first-party author is `contained`, so no external or multi-customer
tenant runs until R27 is built. Changing a group's label is an operator action, recorded in
the catalog, and it takes effect at the group's next start.

### Deploy preview of pending triggers

The deploy preview (above) adds one more report: what NEW will do with the triggers already
pending in the group's store. Only the prepare child can compute it, because only it has
NEW's registry and the store's rows as NEW's migration leaves them. So the scan runs in the
prepare child, after `migrate` is staged and both handle sets are open, and before
`PREPARED` is synced. It writes the report as its own file, `TRIGGERS`, beside `PREPARED`
in the export directory. The `PREPARED` format does not change.

For each pending trigger row of the staged catalog (`store.triggers.pending`, branch 0), the
child classifies it against NEW exactly as NEW's head will at dispatch (RUNNER-SPEC §14.6).
The classes are tried in this order, which is the order of `advance_trigger`, and a row
takes the first that applies:

1. **`cancelling`**: a row whose cancel was requested, parked or not. NEW's head finishes it
   as cancelled at its due time before it looks at its entry, so NEW's entries do not
   matter.
2. **`held`**: a granted trigger that is parked under a hold, or whose attempt is at least
   1 **and** whose revision stamp differs from NEW's launch image digest. The stamp is the
   launch image digest of the code that started it (or the one a release selected). NEW
   parks the second kind with a `Revision` hold at its next start, or keeps the first
   parked, until a release. This class comes before `gone`: `triggers.start` runs before
   the head looks for the entry, so a started granted run under another digest whose entry
   NEW removes is held, not consumed. A parked `Request` hold stays parked even when the
   digests are equal.
3. **`gone`**: NEW's trigger entry list does not allow its entry, or no registered entry of
   NEW has its kind, path and argument schema. NEW's head will consume it with the terminal
   `Gone` when it comes due. Two recipes can differ in `n_place` alone and share an image
   digest; then a started granted run passes `triggers.start` and lands here if its entry
   is gone. NEW's head consumes these rows even when NEW lists no trigger entry at all
   (RUNNER-SPEC §12.1).
4. **`changed`**: an allowed entry whose `revision` (PROG-9) differs from OLD's. It runs
   NEW's code when it comes due.
5. **`unchanged`**: every other row.

The report is one row per (class, kind, entry path, argument schema): the count of
triggers and the earliest due time. These are exact. A report that does not fit the prepare
child's bound fails the preview with `Limit(triggers)`. It is never truncated. The deploy
output prints the rows, `held` and `gone` first. Like the rest of the preview, it gates
nothing. The image-digest guard on started granted runs stays as it is: `Exported.revision`
does not cover a change of `handles` (a table name, a binding, a key codec), and the image
digest does, so a revision-keyed guard would let a started run continue against changed
handles.

Laws (unbuilt): `group_one_head`, where a cutover's NEW head opens the store only after
OLD's head has closed it, and an extra launch of the group is refused `Held`;
`launcher_restart_stateless`, where a launcher killed at every step of a deploy and
restarted gives the host's reconciliation the same result as the host's records alone;
`contained_all_children`, where a describe, preview, prepare and serving child of a
contained group each fail to read a file outside its bindings and leave no process after
it ends; `contained_refused_uncontainable`, where a launcher on a host without the
mechanism admits no contained group; and `trigger_scan_classes` (RUNNER-SPEC §14.7, L40).
