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
  release executable with no digest reader may omit capture on an untaped run.
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

A Run tape captures these metadata fields in its argument snapshot. Replay pins
the recorded value before main instead of hashing the replay executable.

`Welcome.executable` carries this digest, including an empty digest for
unavailable identity. Generated bindings record digest, target and `n_place`.
`Welcome` carries `(principal, q, budget, executable, target, n_place)`, retaining
tag 4; target and allowance are independent of the executable digest.
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
type Release: codec + send { describe: Describe, release: Bytes, grants: List[String], root_limit: u64, recipe: Bytes }
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
after `--` with `hirsel run`. Both options remain in the argument snapshot for
Run tape replay.

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
ordinary value codec. Its type, `Release`, is declared with `Describe` above.

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
  `Net=` followed by the rule text `--grant` takes, and `Dir=` followed by the
  path `--dir` took. A directory path stays as given. It is resolved against the
  working directory at startup, which the manifest does not know.
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

- the entry, the program's `main`, as its identity bytes and its address;
- every group hash in the entry's closure, in ascending order;
- every body hash in that closure with its annex hash, in ascending order of body hash;
- the launch inputs: the backend (Cranelift or LLVM release), the target, the grants in the
  manifest's canonical form, the root limit and `n_place`.

Its root is `R = H("hirsel.release-recipe.v1", recipe)`. A place's `place`, `migrate`, handle
and launch code is reached from `main`, so the closure covers it. The annexes hold the site
maps failures report, so a comment that moves lines changes R but no address. R does not
cover the host's toolchain: one R can assemble to different bytes under another compiler.

**Admission.** A host takes a recipe only when an authorised deploy request names its R (the
host's control boundary). A hash proves only that bytes are intact. It never proves that they
were checked. Code from outside that boundary arrives as source, and the host publishes it
itself with the normal checker. Before the host uses anything a request sends, it checks, in
order, and refuses with the first failure:

1. sizes, counts, formats and versions are within the host's bounds and are ones its
   toolchain supports (`Unsupported`);
2. the recipe hashes to R, and every object hashes to the hash the recipe lists for it
   (`Hash`);
3. the closure walked from the entry through the groups' `External` bindings equals the
   listed objects exactly (`Missing`, `Extra`);
4. every slot's identity equals the bound member's or builtin's identity, every `Internal`
   index is in range, every builtin's semantics number is known, each group's members are in
   ascending identity order and form one strongly connected component, and the entry's
   identity matches (`Binding`);
5. every body and annex decodes within bounds and re-encodes to its own bytes (`Malformed`).

Admission does not check types or ownership again. Admitted objects stay under the attempt's
pending pin. The client sends R and the recipe. The host answers with the hashes it lacks, in
one round, and the client sends exactly those.

**Hot deploy.** `hirsel deploy R --code-store DIR` admits R and fetches what is missing. The
host then assembles the executable with its installed toolchain for the recipe's backend and
target, records its digest beside R, and copies it into the image store. All of this happens
before the preview and before the old place exports, outside the write pause. A refusal or
assembly failure stops the deploy before anything is touched. A retry after drift reuses the
assembled executable. The deploy reports the fetch time, the assembly time and the write pause
as three separate figures. An executable that is retained (current, previous, pending, or named
by a live release interval or a tape) keeps its own bytes, its recipe and its recipe's closure.
A recorded digest is recovered from those bytes and never by assembling again.

Laws: `assemble_is_build`, where `hirsel build` and `publish` followed by `build --recipe` give
byte-identical executables on one toolchain; `admit_refuses` covers each refusal above, and
nothing is assembled until all checks pass; `site_only_change`, where a moved comment keeps
every address, changes R, and the failure sites reported move with it.

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
