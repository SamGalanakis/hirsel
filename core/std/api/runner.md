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

R5 records each call (RUNNER-SPEC §7). Every reply a run writes carries, in the same Tx
and after the reply, a `store.CallRecord`: the requested key, the exact argument bytes,
the stream, and the `code.revision` of the entry that ran, which `transact` copies when
it registers the entry. That holds for a commit, for an abort's kept reply, and for the
runner's reply-only commit after a body fails or runs out of steps. The one exception
is `Failed(Gone)`, whose entry no longer exists: its reply has no record.
`store.recorded(db, branch, v)` reads a record back from a disk store; a memory store
keeps none.

`place_head(db, place, n_place, handles, inbox, mail, tasks, n_publisher)` runs on
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

R2 adds `query(mut r, f, n, s)`, which registers a subscription entry. Its body reads at a
snapshot inside `look`, under `bound bytes n` and `s` steps, and returns a `Cover`: one
data-tree key range of one table, made with `cover(shape, lo, hi)`, which keeps that
table's published value. A query key's result fingerprint is the unit schema's, because
its rows travel under their tables' codecs. The publisher keeps each subscription's
Cover and read set at its base, never its rows (F4), so subscribed memory does not grow
with the rows a Cover spans (L20). A Subscribe runs the query at the
base, and its whole result goes in one Batch that names the sub in `complete` (G3). A
Subscribe on a live (session, sub) replaces it. Each round, from the base to the newest
released snapshot, re-runs only the subscriptions whose reads a change hit. It sends the
difference between the old and the new Cover's rows, a removal being a row with no value,
and for an unchanged Cover only the changed keys inside it (G2). Every row's value is the
table's published value (F10). A query that faults or runs out ends its subscription
with `Ended(sub, Failed(..))`. A session Reset drops all of that session's
subscriptions.

The publisher has one inbox, a `Mailbox` made by `mailbox(n)` with room for n
messages; `place_head` takes the mailbox and `post(mail)` gives the `Sender[PubIn]`
each session holds. Every message to the publisher goes through it in one order: the
head's Releases, a session's Join, its Subscribes and Unsubscribes, and its leaving.
So a Subscribe sent after a session saw version v runs at a base no older than v (G3)
with no other machinery. A session joins the publisher at its first Subscribe and
leaves when it ends; the publisher drops a session whose client channel is gone. A
client sends `subscribe(sub, key, arg)` and `unsubscribe(sub)` as session events. A
Subscribe whose key is not a query entry ends at once with `Ended(sub,
Refused(NoEntry))`. An owner `try_send`s its Subscribes and Unsubscribes (v2.12): on a full
publisher inbox a Subscribe is answered `Ended(sub, Refused(Overloaded))` and registers nothing,
and an Unsubscribe is applied at the owner, which drops that sub's rows and Endeds until a Batch
completes its resubscription; the publisher keeps the sub until then or until the session ends.
The publisher never blocks on a session (L23): every send to a
session is a `try_send`. A closed inbox drops that session's subscriptions. A full one ends
each subscription whose content did not fit with `Ended(sub, Refused(Overloaded))`, and the
client resubscribes. That Ended, and any answer that did not fit, waits in the publisher until
the inbox takes it, retried after every message the publisher handles and before that
session's next content; a retried answer is told at the publisher's current base. The
publisher keeps no subscription content for a full inbox. A subscribed session's owner
wakes the publisher (v2.13, L24): it counts the events it takes since it last found its
inbox empty, and at the inbox's capacity it offers the publisher a `Drained` by `try_send`
(dropped if the publisher's inbox is full), so a waiting Ended reaches its client in a
quiet place too. An owner that keeps up never offers one. A marked session (v2.14: an HTTP
group or a one-shot read) ends without waiting on the publisher (L25). At its join it sends the
Receiver of a retirement mark, a channel whose only Sender it keeps and never sends on. To end,
it drops its inbox and the mark, then offers one `Drained` by `try_send`; its joiner calls
`join_marked(task, publisher)`, which joins the task and offers one more, since a failed or
cancelled session skipped its own. After every message but `Halt`, before the retry pass, the
publisher sweeps: in join order, a mark that answers `Closed` retires its session, with its
subscriptions, its Joiner (ledger and mark) and, in the retry pass, its waiting answers. An
unmarked (WebSocket) session sends `Left`. L25's law, `retired_without_waiting`, is in
runner.hirsel with R2's other publisher laws. Not built yet: the budget ledger
that bounds these bytes (R2-a).

`connect(tasks, place, n_place, head, publisher, client, clock, q, n_session, marked)` spawns a session
task under `bytes n_session` and returns its inbox sender and its task. The session
runs `place` itself under n_place, and its task ends with that `RegisterError` if it
fails. The session answers by itself what V1 lets it: a passed deadline (`Deadline`),
a key that is not a transact entry (`NoEntry`), an argument that does not decode
under the entry's n (`Malformed`), an n its account cannot grant (`Budget`), and a
full slot table (`Overloaded`). It forwards the rest to the head and holds each
forwarded position in a slot until the head's answer arrives. Every send gets exactly
one Outcome. A send of a position already held joins its slot without being forwarded
again if its request digest equals the held one (32 B per held position); with another
digest the session ends with no row for either send, which is Close (1008) on the wire
(v2.11.3 V2). Each send has its own deadline timer, which answers that send alone with
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

WIRE W3b uses reader-funded decoded frames. With payload cap m and q queued
calls, `reader_bytes` reserves `2(1+q)(4m+128) + round8(3m/2) + 8192 + 4096`.
The first term counts both the owner's inbox and requests retained by a stalled
head. The codec admits exact lists; `4m+128` bounds the inline Want list, rounded
identity buffers and the other client variants. The assembly growth and receive
scratch are separate. At m=262144 and q=4 the child allowance is 10,892,544 bytes.
The shared runner harness's `reader_reservation_holds` holds ten maximum Calls
and one assembly allowance under that reservation. It runs on the evaluator and
native engines in `core-runner-r0` after a push, and in `core-runner-wire`.


`serve(n, mut listener, tasks, clock, to_publisher, config, max_sessions, ingress, make)` is the
dormant W3b accept loop. Each accepted connection gets one owner, reader and
writer. `max_sessions` bounds owner handles and completion records independently
of `socket.pending_max`; ingress and principal admission remain W3c. Upgrades run
in their owners, so a stalled upgrade does not block acceptance. `Service[C]`
provides a context, an attach hook for the owner's inbox and the single
`Sender[PubIn]`, and a non-waiting `dispatch` callback. The owner holds that
sender without joining; the W3c service joins lazily on its first Subscribe. The callback is also callable by another transport through
`runner.dispatch`; it contains no socket operations. W3c supplies Hello, call
admission, principal binding, dedup, publisher integration and outcome accounting.

A read credit is acquired before decoding and returned after dispatch consumes
the input. Queued, currently dispatched and producer-held frames together use at
most `1+q` credits. The reader owns their allocations even after sending. The
writer's capacity-one channel is a handoff: the owner waits for `Sent` before
offering again. Control frames precede queued data after the current frame. Both
children are cancelled before either is joined. On graceful Close, the children
return their socket halves through the inbox; after joining, the owner calls
`wsock.finish` under the original absolute Close deadline.

The non-overlapping owner reservation is `reader_bytes + 4096` for the children,
`576` for their task records, `192 + 426(1+q) + 24` for its three channels,
`(1+q)(8192+36)` for outcomes, `budget` for publication, twice
`round8(max_frame+14)` for encoding and a pending writer frame, `n_service` and
`n_place` for service state and the Registry, `round8(300(1+q))` for queue growth,
and `32768` for upgrade work, control bytes and the Close timer. At defaults this
is 12,547,870 + n_service + n_place bytes. The exact-bound `session_records` law
checks 426-byte inbox items, 288-byte child records and a 320-byte owner record
against charge-v1; `writer_one_frame` checks the handoff's exact 88-byte charge.
Root admission adds the owner record (320), its parent-funded pending lease (56),
the factory's bounded state (at most n_service) and a copied ingress list
`round8(25 * prefix_count)` per live owner. Shared listener bookkeeping includes
the completion channel `64+8*max_sessions`, the pending credit channel (64), and
owner-map growth (at most `96*max(4,max_sessions)`); these shared records are counted
once. Logical reservations, live charge, virtual stacks and RSS are different
quantities.
