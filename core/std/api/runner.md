# Runner

Package `store`, module `runner`, implements RUNNER-SPEC v2.2 R0: the entries of a
place, as a library over v1 (reserved R13). R0 has `Registry[G, H]`, `compute`,
`granted`, `describe`, `call`, `start` and `finish`. `transact`, `handles`, the
head and sessions belong to R1.

The wire types are `Kind`, `Refusal`, `Fault`, `Outcome` and `Described`, all
`codec + send`. `Room` and `Bad` carry Capacity and DecodeError, because those types
are not codec. `Bad.kind` numbers DecodeError's variants in declaration order from 0.
`Raised(u32)` numbers the first cause's CauseKind in declaration order from 1, so
Bounds is 2.

An entry's key is its `code.identity`, followed by the u64 little-endian
fingerprints of the argument schema and then the result schema. Registering a key that
is already present returns `AlreadyRegistered(key)` and leaves the registry
unchanged. Allocation failure while registering returns `Full`. Each kind has one
erased adapter. `describe(r, mut out)` appends the codec of `List[Described]`, in
registration order. Each element holds the kind, the identity, the argument schema
bytes, the result schema bytes and an empty error schema. Registration order, an
added argument field and a changed result type each change these bytes, and two
`place` runs in two tasks give equal bytes.

`call(r, key, arg, n)` runs a compute entry in the caller's task. It decodes the
argument, runs the body and encodes the result, all inside `bound bytes n` and
`isolate`. The outcomes are:

- `Refused(Budget)`: the place cannot grant n;
- `Refused(Malformed)`: the argument does not decode;
- `Failed(OutOfBytes)`: the body exhausts n after entry;
- `Failed(Raised)`: the body faults.

The decoder's peak memory therefore counts against n. `start(r, tasks, g, key,
arg, n)` spawns a task under `bytes n` that holds the grant g and decodes and runs
the granted entry. A spawn the place cannot grant returns `Budget`. `finish` joins
that task, and a task that fails or is cancelled gives `Failed(Raised)`. A key that
names no entry of the kind being asked for gives `NoEntry`.

`place(mut r)` is the user's function that registers every entry. An owning
closure is never shareable, so every task that runs entries runs `place` itself.
Laws and cases: `core/programs/store/runner_laws.hirsel`, run by
`core/tools/runner_tests.py` (target `//:core-runner-r0`).
