# Runner launch modes and identity

The runner has ordinary and describe launch modes. The launcher selects the mode
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

- Native Linux launch opens `/proc/self/exe` once and hashes that opened image,
  rather than resolving `argv[0]` or reopening its pathname. Replacement,
  rename or deletion of the file after launch does not change the metadata.
  Both native backends use the same runtime launch boundary.
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
type Exported: codec + send { described: Described, args_fp: u64, result_fp: u64 }
type Describe: codec + send { digest: Bytes, target: String, n_place: u64, exported: List[Exported] }
```

Entries are in registration order. `Described` has the fields `kind`, `identity`,
`args` and `result`, in that order, with no separate error field. Its `result`
is the schema of the value carried by `Returned`: `Result[R, E]` for transact,
and `R` for compute and granted. The two fingerprints come from
`std.codec.fingerprint_of` applied to `args` and `result`, respectively.
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
