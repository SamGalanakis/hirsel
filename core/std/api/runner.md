# Runner

Package `store`, module `runner`, implements RUNNER-SPEC v2.4 R0 and R1: the entries
of a place, as a library over v1 (reserved R13). R0 has `Registry[G, H]`, `compute`,
`granted`, `describe`, `call`, `start` and `finish`. R1 adds `transact`, the head
(`place_head`), its internal publisher, and sessions (`connect`, `ask`, `hang_up`).

The wire types are `Kind`, `Refusal`, `Fault`, `Outcome` and `Described`, all
`codec + send`. `Room` and `Bad` carry Capacity and DecodeError, because those types
are not codec. `Bad.kind` and `Raised(u32)` (the first cause's kind) are fixed wire
tables (RUNNER-SPEC §1.1): Bad.kind is Truncated 0, Invalid 1, Utf8 2, Duplicate 3,
Trailing 4, Schema 5, Value 6; Raised is Overflow 1, Bounds 2, Division 3, Unwrap 4,
Fail 5, Cancelled 6, Capacity 7. A kind added later takes the next unused number.

An entry's key is its `code.identity`, followed by the u64 little-endian
fingerprints of the argument schema and then the result schema. Registering a key that
is already present returns `AlreadyRegistered(key)` and leaves the registry
unchanged. Allocation failure while registering returns `Full`. Each register
function (`compute(mut r, f, n)`, `granted`, `transact`) takes the entry's byte bound n,
which the slot keeps; n is in neither the key nor `Described`. Each kind has one erased
adapter. `describe(r, mut out)` appends the codec of `List[Described]`, in
registration order. Each element holds the kind, the identity, the argument schema
bytes, the result schema bytes and an empty error schema. Registration order, an
added argument field and a changed result type each change these bytes, and two
`place` runs in two tasks give equal bytes.

`call(r, key, arg)` runs a compute entry in the caller's task. It decodes the
argument, runs the body and encodes the result, all inside the entry's `bound bytes n`
and `isolate`, so a result too large to encode fails that call, never the caller. The
outcomes are:

- `Refused(Budget)`: the caller cannot grant the entry's n;
- `Refused(Malformed)`: the argument does not decode;
- `Failed(OutOfBytes)`: the body exhausts n after entry;
- `Failed(Raised)`: the body faults.

The decoder's peak memory therefore counts against n. `start(r, tasks, g, key,
arg)` spawns a task under the entry's `bytes n` that holds the grant g and decodes and runs
the granted entry. A spawn the place cannot grant returns `Budget`. `finish` joins
that task, and a task that fails or is cancelled gives `Failed(Raised)`. A key that
names no entry of the kind being asked for gives `NoEntry`.

`transact(mut r, f)` registers a body `fn(mut store.Tx, H, take A) -> Result[R, E]`.
Only the head runs it, in a store run under the entry's n: `Ok` commits and its
reply is written with the effects; `Error` aborts the effects and the head keeps the
reply alone (see the S5 stand-in below).

`place_head(db, place, n_place, handles, inbox, sessions, tasks, n_publisher)` runs on
the caller's task, because `store.Db` is not send. It runs `place` under `bytes
n_place` (rule 10) and `handles` twice (rule 12). The head keeps the first H. The
publisher, which it spawns under `bytes n_publisher`, runs `place` itself and gets the
second H; open waits until it has, and a `place` that fails there fails the open with
`Place`. A second `handles` call that moves the store's version fails the open with
`HandlesChanged`. The head admits each request against its
stream's row (one position per stream, H1–H4), runs at most one body per stream,
groups commits and releases a group only after it is durable (V4). Answers that need
no commit (a duplicate's stored reply, Superseded, a refusal) go in a Release
without a snapshot and the publisher sends them at its base (V3). The publisher
sends each answer as a `Frame` at the version of the last Release that carried a
snapshot (G1).

`connect(tasks, place, n_place, head, client, clock, q, n_session)` spawns a session
task under `bytes n_session` and returns its inbox sender and its task. The session
runs `place` itself under n_place, and its task ends with that `RegisterError` if it
fails. The session answers by itself what V1 lets it: a passed deadline (`Deadline`),
a key that is not a transact entry (`NoEntry`), an argument that does not decode
under the entry's n (`Malformed`), an n its account cannot grant (`Budget`), and a
full slot table (`Overloaded`). It forwards the rest to the head and holds each
forwarded position in a slot until the head's answer arrives. Every send gets exactly
one Outcome. A send of a position already held joins its slot without being forwarded
again. Each send has its own deadline timer, which answers that send alone with
`Abandoned`. The head's answer goes once to each send still unanswered, so to none
when the position was Abandoned and not sent again. The slot table counts, per held
position, the larger of 1 and its unanswered sends, at most 1 + q in all: a joined
send takes a slot, and a re-send of an Abandoned position whose slot is still held
reuses it. `hang_up()` closes the session.

S5 (admit and reply) is not on main. A table `runner.streams`, which the head creates
at open, stands in for it with STORE-SPEC §1.8's rules. An Error reply is kept in a
reply-only commit, where S5 keeps it in the aborted run's frame. The gate waits on
`store.durable` for the group's last ticket. R1's laws run on memory stores, where
that returns at once; crash, poison and gone cases are not built yet.

`place(mut r)` is the user's function that registers every entry. An owning
closure is never shareable, so every task that runs entries runs `place` itself.
Laws and cases: `core/programs/store/runner_laws.hirsel` (R0) and
`core/programs/store/runner_calls.hirsel` (R1), run by `core/tools/runner_tests.py`
(target `//:core-runner-r0`) on the evaluator, Cranelift and LLVM.
