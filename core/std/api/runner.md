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
fingerprints of the argument schema and then the result schema. The result is the
type the `Returned` bytes carry: `Result[R, E]` for a `transact` entry, R for `compute`
and `granted`, so a change to a transact entry's E alone changes its key. Registering a key that
is already present returns `AlreadyRegistered(key)` and leaves the registry
unchanged. Allocation failure while registering returns `Full`. Each register
function (`compute(mut r, f, n)`, `granted`, `transact`) takes the entry's byte bound n,
which the slot keeps; n is in neither the key nor `Described`. Each kind has one erased
adapter. `describe(r, mut out)` appends the codec of `List[Described]`, in
registration order. Each element holds the kind, the identity, the argument schema
bytes and the result schema bytes. Registration order, an
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

`transact(mut r, f, n)` registers a body `fn(mut store.Tx, H, take A) -> Result[R, E]`.
Only the head runs it, in a store run under the entry's n: `Ok` commits and its
reply is written with the effects; `Error` aborts the effects and the store keeps the
reply alone in the aborted run's frame (STORE-SPEC F2).

`place_head(db, place, n_place, handles, inbox, sessions, tasks, n_publisher)` runs on
the caller's task, because `store.Db` is not send. It runs `place` under `bytes
n_place` (rule 10) and `handles` twice (rule 12). The head keeps the first H. The
publisher, which it spawns under `bytes n_publisher`, runs `place` itself and gets the
second H; open waits until it has, and a `place` that fails there fails the open with
`Place`. A second `handles` call that moves the store's version fails the open with
`HandlesChanged`. The session computes each request's digest at ingress
(`store.request_digest` over the key and the exact argument bytes). The head admits
each request with `store.admit` (one position per stream, H1–H4), answers another
digest at a position `Refused(RequestMismatch)` without running it, whether the
position is still running (H1) or the store refuses it (AH1), runs at most one body per stream,
groups commits and releases a group only after it is durable (V4). Answers that need
no commit (a duplicate's stored reply, a refusal, `Refused(HistoryUnavailable)` below latest) go in a Release
without a snapshot and the publisher sends them at its base (V3). The publisher
tells each answer to its session as a `Told` at the version of the last Release that
carried a snapshot (G1). The session hands its client a `Down`: `Batched(KeyedBatch)`, where a
`KeyedBatch` is `{version, complete, rows, outcomes}` with wire `Row`s and each outcome a
`KeyedDone{stream, p, outcome}` (the wire `Done` with the store's stream key in place of a
stream slot, until R3), or `Ended(sub, outcome)`, or `Reset`.

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

`Frame` is the dormant WIRE-SPEC v2 wire envelope. Its declared variants fix tags
0 through 8: Hello, Call, Subscribe, Unsubscribe, Welcome, Placed, Batch, Ended,
Reset. `Want`, `Entry`, `Done` and `Row` are its codec payload types. Welcome carries
the principal, q, budget, executable digest, target and n_place. Batch carries the
version, completing subscription ids, rows and outcomes, in that order. A Row key
is the physical data-tree key, with big-endian table and integer key components;
its optional value contains the current value codec's bytes without a stored
schema prefix. The session still uses `Told`; wire serving is a later slice.
Outcome tags are Returned 0, Failed 1, Refused 2, Abandoned 3 and InDoubt 4.
Refusal appends RequestMismatch 6 and HistoryUnavailable 7 after NotAllowed 5.

The head's stream state is the store's own (STORE-SPEC §1.8, S5 and v2.24's request
identity): `store.admit` and `store.reply`, with no runner table. The gate waits on
`store.durable` for the group's last ticket. R1's laws run on memory stores, where
that returns at once. L1 (R1b) runs on a disk store under the store's W2 crash
adapter, natively only: at every filesystem cut of two scenarios, the recovered store
holds every version a client saw in a frame reporting a commit, and a re-send of each
position gets its reply byte-equal with the effect once; one failed journal sync
stops the head, and no client saw a version the store lost. The adapter cuts once per
run, so a scenario's second crash is not covered. The gone case is not built yet.

`place(mut r)` is the user's function that registers every entry. An owning
closure is never shareable, so every task that runs entries runs `place` itself.
Laws and cases: `core/programs/store/runner_laws.hirsel` (R0),
`core/programs/store/runner_calls.hirsel` (R1) and
`core/programs/store/runner_crash.hirsel` (R1b, native only), run by
`core/tools/runner_tests.py` on the evaluator, Cranelift and LLVM: R1 in the land gate
(`//:core-runner-r1`), and R0 and R1b after the push (`//:core-runner-r0`).
The hand-derived `frame_vectors` law and its `tag_order`, `complete_omitted` and
`refusal_tag_inserted` mutants run with R0 after the push, or alone with
`//:core-runner-wire`. The byte
fixtures in `core/programs/store/frame-vectors.json` are frozen against
RUNNER-SPEC v2.11.4's R2 contract and Outcome/Refusal tags, including F9-F12.
