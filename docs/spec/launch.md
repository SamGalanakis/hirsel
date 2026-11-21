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
  followed by the interface's definition path `D` (`Foreign=sqlite@1/sqlite/Sqlite`), and
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
A `Foreign` binding then holds the SHA-256 and byte length of the selected archive.
So exchanging the paths given to two `Dir` parameters changes R. The
manifest's sorted `grants` strings are derived from the bindings for display and comparison;
nothing is assembled or launched from them.

Its root is `R = H("hirsel.release-recipe.v2", recipe)`; the version is 2, and version 1, which
had no foreign parts, is never read or written. A place's `place`, `migrate`, handle
and launch code is reached from `main`, so the closure covers it. The annexes hold the site
maps failures report, so a comment that moves lines changes R but no address. R does not
cover the host's toolchain: one R can assemble to different bytes under another toolchain, and
the assembly record (below) says which.

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
([FFI-4](foreign.md#ffi-4-the-foreign-capability)). One local principal that both publishes and deploys needs no
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
