# Runner

Package `runner`, module `runner`, implements RUNNER-SPEC v2.32: the entries
of a place, as a library over v1 (reserved R13), including `Registry[G, H]`, `compute`,
`granted`, `transact`, `query`/`query_as`, `describe`, `operate`, wire serving, triggers,
and cold-wake/deploy lifecycles.

To use `runner`, add `runner = { major = 1 }` under `[dependencies]` in
`hirsel.toml`. The installed Hirsel toolchain supplies the package and the
`store@1` it binds.

## No authentication, no authorization

Hirsel and `runner` contain no authentication and no authorization (RUNNER-SPEC
v2.24 rule 13, ruling R6). The runner checks no token, signature, password or
key, mints no credential, keeps no allow or deny list, and makes no access
decision by Caller. There is no badge maker, no Caller-based policy, no per-caller
grant and no `Arc[G]`. Your application does both:

- **Authentication** is your binding, `runner.wire(seed, bind, ..)`: a
  function from a copy of your plain seed and the client's credential bytes (the
  Hello `credential` or the HTTP `Authorization` value) to
  `Result[BoundCaller, BindRefusal]`. `Rejected` gives Close 1008 or HTTP 401.
  `Unavailable` gives Close 1013 or HTTP 503 with `Retry-After: 5`. The runner
  bounds the application binding and passes the credential bytes through.
  `runner.wire_bind(latest_seed, bind, ..)` reads a `Latest[B]` once per bind,
  where `B: shareable + send`. The binding sees a view of that version. A
  `Wire` carries no grant: granted entries get theirs from `PlaceConfig.grants`.
  **Planned (RUNNER-SPEC v2.70):** the seed goes. `runner.wire(bind, bind_bytes,
  bind_steps)` takes `fn(G, Bytes)`, and each bind gets a fresh `G` from
  `PlaceConfig.grants.bind`, as a keyed claim gets one from `claim`. A key set
  rides in that `G` as a `latest.handle`, and a sealed key as a `secret.share`
  holder, so HMAC-keyed bindings work. A `Wire` with no `bind` mint refuses the
  open (`BindNoGrant`). A failed bind mint gives Close 1013, or HTTP 503 or 500,
  and never 1008 or 401.
- **Authorization** is your entry bodies. Transact, query and granted bodies
  receive the Caller and decide access in their own code, for example by reading
  a membership row in the same transaction.

With no binding, every session gets a fresh, unguessable anonymous Caller
(issuer 1, 16 random bytes drawn at Hello). Saved replies are keyed by Caller,
so they never cross sessions; deduplication across reconnects needs a binding.

## Running a place

`runner.main(args, process, tasks, place, serve)` passes a stop receiver to
`serve` on the normal launch path. Pass that receiver with the place config,
status and optional phase watcher to `platform.serve`.
The first [process stop](process.md) closes the channel and starts the drain
specified in RUNNER-SPEC §12.4. Describe mode runs `place` before the serve
callback or the stop task starts.

**Restart and upgrade (RUNNER-SPEC §6).** Pass `runner.launch_selection(args)` as
`PlaceConfig.selection`. A launch without options selects `Reopen`: the first start
on an empty store runs genesis, and every later start of the same executable reopens
the store at its current activation. A store is bound to the image that activated
it, so another executable, the same source rebuilt in another mode included, is
refused `Open(NotCurrent(Active(version, current)))`. To move a store to a new
build, stop the old process and start the new build once with `--hirsel-install`,
which runs `migrate` over the existing data; later starts are plain again. A hosted
place upgrades with `hirsel deploy`. `--hirsel-activation=v` expects version `v`
exactly; the host passes it and an application does not need it.

**Why a place did not start.** `runner.start_text(problem)` is one line naming a
`StartError` and what to change; `runner.main` prints it on stderr. `Exceeds(bound)`
names a setting, its value and the most this place accepts, and `Missing(name)` a
setting that is zero or too small. (v2.45 deletes `Disagrees`: its only case was a
session `n_place` that differed from the place's, and the session no longer carries one.)

**`PlaceConfig.wire`.** `None` serves anonymous sessions: each Hello gets a fresh
anonymous Caller. `Some(runner.wire(seed, bind, bind_bytes, bind_steps))` maps the
client's credential to a Caller with a copy of a plain seed;
`Some(runner.wire_bind(latest_seed, bind, ..))` does it from the current published
seed.

**`PlaceConfig.grants`.** The place's grant sources: `Grants(attempt, call, claim)`,
and `bind` from v2.70 (planned). `attempt` and `claim` are optional
`Mint(seed, mint, mint_bytes, mint_steps)`s (v2.70: `bytes` is a reserved word, so the
fields are named `mint_bytes` and `mint_steps`), and `call` is an
optional `CallMint(mint, entries)`. Every seed has the same type `S`. Each present mint has its own
task, which owns its seed, and its own allowance `mint_bytes`: every live `G` it made stays
charged there, so one use cannot exhaust another's. A mint is
`fn(S, H, I) -> Result[G, Capacity]`, with `I` by view. It never waits, and it runs as
`isolate` around `bound steps mint_steps`, so its error, fault or step overrun fails only its
own request (`Failed(OutOfBytes)`, `Failed(Raised)`, `Failed(OutOfTime)`) and the
task keeps serving. A granted trigger attempt gets `attempt`'s `G`; the mint sees a view of the
attempt's `TriggerContext` and keeps one with `share_trigger_context`. A wire call gets `call`'s `G` only for the granted entries
`call.entries` lists (by path and argument schema, built with `triggers.granted` and
`triggers.allow`). Any other wire call to a granted entry is `Refused(NotAllowed)`
before any mint. A keyed HTTP claim gets `claim`'s `G`. Give each mint its own seed:
duplicate a `Sender` with `channel.sender` and a `Secret` with `secret.share`. A
listed granted trigger entry with no `attempt` mint refuses the open
(`TriggerNoGrant`), as does a `call` list element that names no registered granted
entry (`CallNotGranted`) and a keyed binding with no `claim` mint (`KeyedNoGrant`).
Transact and query entries never see `G`. Mints are never recorded or replayed.

**Sizing `n_place`.** The serving task builds the place's one Registry under
`n_place` and every wire session plans against it, so `n_place` counts once in the
serving budget, not per session. A standalone place has one `n_place`, `PlaceConfig.n_place`: it
bounds `place` in the head and every session, and `Status` and Welcome report it.
The wire configuration does not carry it: each session takes the head's value
(RUNNER-SPEC v2.45; `WireConfig.n_place` and its `Disagrees` refusal are deleted). A standalone start ignores
`--hirsel-n-place`. Under the platform host the recipe's `n_place` (`hirsel publish
--n-place`) replaces both. Generate the SDK with the same value (`hirsel sdk ts
--n-place`); a client whose bindings carry another value reports both on its
`launch identity mismatch` diagnostic.

**Sizing `--limit`.** `platform.serve` uses `runner.operate` to start and drain the place.
`runner.operate_bytes(config)` is what `operate` spends: the
head (`n_place`, `n_publisher` and its own reserve), every session's
reserve up to `max_sessions`, and operate's own 256 KiB. The current task's root
limit (`hirsel build --limit`) must exceed it plus what `main` keeps; a smaller one is
refused `Full(capacity)` with the requested and available bytes. The store takes the
rest. `operate` derives the Store options and reservation using the sizing rules
of `store.sized_with` (store.md "Sizing a store"). Use `store.derived_sizes()` for
default sizing. `PlaceConfig.sizes` can override individual figures.

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

The decoder's peak memory therefore counts against n. `start(r, tasks, g, caller, key,
arg)` spawns a task under the entry's `bytes n` that holds the grant g and decodes and runs
the granted entry, a body `fn(G, Caller, take A) -> Result[R, Capacity] waits`. A trigger
moves its attempt's `G` into `start`. A wire call reaches a granted entry only when the
place's `PlaceConfig.grants.call` lists it; the call's task first asks the `call` mint
for its g and then runs as `start` does; each wire call then runs with its own
freshly minted `G`, so no call consumes or shares another's. Otherwise a wire call to a
granted entry is `Refused(NotAllowed)`, whatever the place's `Wire`. A spawn the place cannot grant returns `Budget`. `finish` joins
that task, and a task that fails or is cancelled gives `Failed(Raised)`. A key that
names no entry of the kind being asked for gives `NoEntry`.

`transact(mut r, f, n, s)` registers a body `fn(mut store.Tx, H, Caller, take A) -> Result[R, E]`.
Only the head runs it, in a store run under the entry's n: `Ok` commits and its
reply is written with the effects; `Error` aborts the effects and the store keeps the
reply alone in the aborted run's frame (STORE-SPEC F2).

R5 records each call (RUNNER-SPEC §7). Every reply a run writes carries, in the same
commit, a `store.CallRecord`: the requested key, the exact argument bytes,
the stream, and the `code.revision` of the entry that ran, which `transact` copies when
it registers the entry. That holds for a commit, for an abort's kept reply, and for the
runner's `store.conclude` after a body fails or runs out of steps. The one exception
is `Failed(Gone)`, whose entry no longer exists: its reply has no record.
`store.recorded(db, branch, v)` reads a record back from a disk store; a memory store
keeps none.

Call `place_head(db, place, n_place, on_migrate, schema, inbox, mail, tasks, n_publisher)`
on the task that opened the `store.Db`. The head runs `place` under `bytes n_place`
and calls `schema` once (RUNNER-SPEC v2.45 rule 12): in `Migrate` mode at genesis,
install and prepare, after the optional `on_migrate`, and in `Open` mode otherwise.
`schema(mut d: store.Declare) -> Result[H, store.Error]` declares every table and
index and returns H, which is `shareable + send`, so the head and the publisher share
it. `handles`, `query_handles`, `store.Schema` and `store.query_table` are deleted.
The publisher runs `place` itself under its task allowance, `bytes n_publisher`.
Open waits for it and returns `Place` if registration fails. The session
computes each request's digest at ingress
(`store.request_digest` over the key and the exact argument bytes). The head admits
each request with `store.admit` (one position per stream, H1–H4), answers another
digest at a position `Refused(RequestMismatch)` without running it, whether the
position is still running (H1) or the store refuses it (AH1), runs at most one body per stream,
groups commits and releases a group only after it is durable (V4). Answers that need
no commit (a duplicate's stored reply, a refusal, `Refused(HistoryUnavailable)` below latest) go in a Release
without a snapshot and the publisher sends them at its base (V3). The publisher
gives each session its diff or Reset and all pending answers in one update per durable
group (G1). The session hands its client a `Down`: `Batched(KeyedBatch)`, where a
`KeyedBatch` is `{version, complete, groups, outcomes}` with wire `Group`s and each outcome a
`KeyedDone{stream, p, outcome}` (the wire `Done` with the store's stream key in place of a
stream slot, until R3), or `Ended(sub, outcome)`, or `KeyedReset(version, outcomes)`.

`query(mut r, f, slots, n, s)` and `query_as(mut r, f, slots, n, s)` register a live
query (RUNNER-SPEC v2.45). A `query` body is `fn(Tx, H, take A)` and never sees the
Caller; a `query_as` body is `fn(Tx, H, Caller, take A)` and gets the subscribing
session's Caller at every run. Both return `Answer[R] { value: R, covers: CoverSet }`:
a computed value of any codec type R, and the rows the client should hold, as up to
16 spans over named **slots**. Register the slots with `slot(mut s, "name", table)`;
a slot's id is its registration index, and the query's key fingerprints R's schema
and the slot list, so the generated SDK has `sub.value()` typed as R and
`sub.rows("name")` typed by the slot's table. The body adds spans with
`cover(mut c, "name", t, lo, hi)`, which is `Missing(name)` for an unregistered slot.
A body runs at a snapshot inside `look`, under `bound bytes n` and `s` steps; row
conversion shares those limits. A `count` query returns `Answer { value: n, covers:
covers() }`; a list query returns `Answer { value: (), covers }`, whose value costs
0 bytes.

Subscriptions with the same key and argument bytes share one **view**, and for
`query_as` only when the Callers' bytes are equal too. A view runs once per round,
encodes its group once and splices it into every subscriber's frame; each subscriber
is still charged the full read set and cover, so sharing never changes admission. A
Subscribe joins a live view's current image without a run. The publisher keeps each
view's CoverSet, read set and last value bytes, never its rows (F4), so subscribed
memory does not grow with the rows a cover spans (L20). A Subscribe's whole result
goes in one Batch that names the sub in `complete` (G3). Each round re-runs only the
views whose reads a change hit. If the re-run's encoded value equals the last one,
the group's `value` is empty (unchanged); a read-set hit still fences the
subscribers' sessions. The group's rows are each slot's difference between the old
and the new cover, a removal being a row with no value, and for a cover-only hit the
changed keys inside it (G2). Every round publishes all its fences, then advances the
round register once, then offers (one barrier). A query that faults or runs out ends
the view with `Ended(sub, Failed(..))` to every subscriber. A session Reset drops all
of that session's subscriptions.

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
nothing (v2.20): each live subscription whose content did not fit is marked due for repair.
A due subscription keeps its read set, so a commit that hits it still fences the session, but
it is not re-run and sends no diff. Once its owner has made progress (below), the publisher
re-runs it at its base and sends its whole result in a Batch whose `complete` names it, which
the client applies by replacing that subscription's image from empty. A replacing Subscribe, an
Unsubscribe or the session's end clears the mark. Ended entries, a pending Reset and any
answer that did not fit wait in the publisher until the inbox takes them. After each message the publisher retries pending outcomes and the
ledger together at its current base. The publisher keeps no subscription content for a
full inbox. An owner with a head lease wakes the publisher (v2.13, L24): it counts the events it takes since it last found its
inbox empty, and at the inbox's capacity it offers the publisher a `Drained` by `try_send`
(dropped if the publisher's inbox is full). Just before it, the owner `try_send`s `()` on its
progress channel, a capacity-1 channel whose receiver it handed over at its join; the
publisher takes that token before each delivery pass, and a taken token makes the session's
due subscriptions eligible for repair. So a waiting ledger reaches its client, and a bounced
subscription is repaired, in a quiet place too. An owner that keeps up never offers one. A marked session (v2.14: an HTTP
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

`Frame` is the wire envelope of subprotocol `hirsel.4`. Its declared variants fix tags
0 through 11: Hello, Call, Subscribe, Unsubscribe, Welcome, Placed, Batch, Ended,
Reset, Set, Send and Live. Call and Done use `store.StreamId { t: u64, n: u64 }`.
Welcome carries the Caller, q, budget, executable digest, target, n_place,
keyed time and stream retention. Batch carries the
version, completing subscription ids, groups and outcomes, in that order. A Group is
`{sub, value, rows}`: `value` is the query's R bytes, empty when unchanged, and each
Row is `{slot, key, value}`. A Row key
is the logical key, with big-endian table and integer key components;
its optional value contains the current value codec's bytes without a stored
schema prefix. Wire serving runs over `runner.serve_wire` / `runner.operate` (below).
Outcome tags are Returned 0, Failed 1, Refused 2, Abandoned 3 and InDoubt 4.
Refusal appends RequestMismatch 6 and HistoryUnavailable 7 after NotAllowed 5.
Fault tags are Raised 0, OutOfBytes 1, OutOfTime 2, Gone 3 and Conflict 4.
A client that offers only the retired tokens `hirsel.1` or `hirsel.2` is closed 1009
with reason `hirsel.4` right after the upgrade, before any Welcome.

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
Laws and cases: `core/programs/runner/runner_laws.hirsel` (R0),
`core/programs/runner/runner_calls.hirsel` (R1) and
`core/programs/store-crash/runner_crash.hirsel` (R1b, native only), run by
`core/tools/runner_tests.py` on the evaluator: R1 in `//:core-runner-r1`, and R0 and R1b in `//:core-runner-r0`.
Natively they run with the rest of the store package under
`hirsel test core/programs/store` (`//:core-store-s1`); `runner_tests.py`
keeps the native cases that test cannot express.
The hand-derived `frame_vectors` law and its `tag_order`, `complete_omitted` and
`refusal_tag_inserted` mutants run with R0, or alone with
`//:core-runner-wire`. The byte
fixtures in `core/programs/runner/frame-vectors.json` are frozen against
RUNNER-SPEC v2.11.4's R2 contract and Outcome/Refusal tags, including F9-F12.
The `hirsel.4` fixtures include the widened stream and Welcome fields and the
Set, Send and Live frames. Regenerate the fixtures with their owning tool.

WIRE W3b uses reader-funded decoded frames. With payload cap m and q queued
calls, `reader_bytes` reserves `2(1+q)(4m+128) + round8(3m/2) + 8192 + 4096`.
The first term counts both the owner's inbox and requests retained by a stalled
head. The codec admits exact lists; `4m+128` bounds the inline Want list, rounded
identity buffers and the other client variants. The assembly growth and receive
scratch are separate. At m=262144 and q=4 the child allowance is 10,892,544 bytes.
The shared runner harness's `reader_reservation_holds` holds ten maximum Calls
and one assembly allowance under that reservation. It runs on the evaluator and
native engines in `core-runner-r0` after a push, and in `core-runner-wire`.


`runner.serve_wire` owns a bounded accept loop and an owner, reader and writer per connection.
`max_sessions` bounds active owners independently of `socket.pending_max`.
The listener can respond with HTTP 503 while a refusal worker is available.

`runner.admitted` carries the bound `Caller`, attenuated entry keys,
recorded arrival and decision times, and byte allowances. The transport binds the
Caller at Hello or at the HTTP request. The wire owner sends the Caller in the
Welcome frame and retains it; no credential byte is kept. Pure `prepare` resolves
current/gone/unknown keys, applies attenuation, decodes under byte and step
bounds, computes the digest, and forms `caller_key || u32-BE(stream)`.
`caller_key` writes the issuer byte, subject length byte and up to 64 subject bytes.

After an anonymous reconnect, the SDK settles pending work as `LocallyUnknown`
and starts at position zero for the new Caller (v2.24).
The composition root supplies retained activation keys through `retain_gone`.
`Send` is opaque. Other transports use the same preparation and stream-key functions.

`wire_place_service` runs place under `n_place` inside the owner. `Service[C]`
has synchronous step, readiness and finish callbacks. `WireWork` keeps the
binding, granted call and deadline tasks inside the owner. Their results arrive
through its inbox. Welcome and Placed precede calls; a second Hello closes 1008.
Each send keeps its unit through writer handoff. Gone calls reach V5; out-of-range
or unknown slots answer NoEntry. The publisher joins lazily on the first Subscribe.
A close flag plus Drained releases publisher state even if the owner closes before
the join acknowledgement, or PubIn is full. Unsubscribe drops queued rows locally.

DONE_MAX is 8192, leaving 8172 reply bytes. Transact checks before commit and
records an oversized reply as Failed(OutOfBytes); compute checks without recording.
At q=4 the owner retains at most 41140 bytes of outcome rows, including five V1
refusals. The sixth unsent refusal closes 1008. Reset preserves every held Done.
Older stored replies above the bound close 1009, after Welcome, without changing stored bytes.
`max_subs` defaults to 128, with publication rows also bounded by the byte budget.

A read credit is acquired before decoding and returned after dispatch consumes
the input. An admission retaining its input keeps that credit until its decision. Queued, currently dispatched and producer-held frames together use at
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
and `32768` for upgrade work, control bytes and the Close timer, plus `4384` for
the Hello timer and its task record. At defaults this is 12,552,254 + n_service +
n_place bytes. Service working space includes its bounded admission job, per-send
deadline tasks and slot metadata. The exact-bound `session_records` law
checks 426-byte inbox items, 288-byte child records and a 320-byte owner record
against charge-v1; `writer_one_frame` checks the handoff's exact 88-byte charge.
Root admission adds the owner record (320), its parent-funded pending lease (56),
the factory's bounded state (at most n_service) and a copied ingress list
`round8(25 * prefix_count)` per live owner. Shared listener bookkeeping includes
the completion channel `64+8*(max_sessions+16)`, the pending credit channel (64), and
owner-map growth (at most `96*max(4,max_sessions)`); these shared records are counted
once. At most sixteen refusal tasks each reserve 16384 working bytes and a
320-byte record. `WireConfig.n_granted` adds an explicit per-owner reservation
for granted entry tasks, inside the session's byte ceiling. It defaults to zero.
The place chooses it to cover the grant tasks it intends to keep live.

RUNNER-SPEC v2.19 defines the Caller in slices RC1, RS, RR and RC4. `Caller{issuer: u8, subject: Bytes}` names the admitted caller. It is not
secret, and its subject is at most 64 bytes. Issuer 1 is anonymous: its subject is 16
random bytes drawn at Hello (v2.24). A binding uses its own issuer number from the
fixed table; a later issuer takes the next unused number when its spec lands. The transport binds the
Caller once, at Hello or at the HTTP request. `Admitted` and each request carry it, and the
store's stream key becomes `caller_key || u32-BE(stream)`, so no token byte is
stored. `caller_key` is the issuer, the subject's length and the subject. RC1
requires a fresh store and starts a fresh SDK recovery generation. Neither the store nor
the runner does authorization: a body receives the Caller and decides access itself.
Transact bodies take `(mut Tx, H, Caller, A)`, query bodies `(Tx, H, Caller, A)` and
granted bodies `(G, Caller, A)`.
Queued subscription content is re-checked against current state before it is
sent (RUNNER-SPEC §3). Each content unit carries the version it was computed at.
Its writer loads the place's round register and then its session's fence after it
takes the unit and before its first byte, and drops the unit, resetting the
session, when a commit since that version hit one of the session's read sets.
Outcomes are not checked: a call's saved reply is delivered as it was committed,
and a duplicate send returns its saved reply unchanged. The catalog changes only
when `schema` runs in `Migrate` mode, published in one commit; in `Open` mode it only
checks the catalog against the declaration. `store.serve` owns each
admitted position's `Completion`. It runs the body as one isolated execution over
one snapshot and commits the record with the reply, so neither commits alone. It
returns a receipt for the first delivery. Replay reads the Caller back from the
recorded stream key or claim key, and a concluded run's base, time and seed from
its record. A run concluded after a refusal for room replays as
`Unknown(AdmissionRefused)`. A run whose step stop differs from the record's
replays as `Unknown(Residency)`. Its recorded-code mode requires an equal replay
identity (the launch digest with the selected constructors, derived by
`place_head`); otherwise its verdict is `Unknown(RevisionUnknown)`. A chosen
candidate always runs (RUNNER-SPEC v2.19).

**Operating a place (RUNNER-SPEC §12.1).**
`operate(config, stop, status, watch)` owns a place from store open to store close.
It supervises a head task (store, activation, publisher), a serve supervisor
with one acceptor child, and the sessions. It publishes
`Starting(Recovering | Activating | Publishing | Listening)`, then `Ready`, then
`Draining` to one `Latest[Status]` root, which any number of readers read
without waiting. It publishes `Ready` only after the activation commit, the
publisher's first base and the bound listener, and starts accepting only after
`Ready`. A stop request starts the drain with one absolute deadline,
D = stop + `grace_ms`. Readiness is withdrawn at once, and admission closes at
min(stop + `lag_ms`, D): the acceptor is cancelled, so the listener refuses.
After that, a new send is `Refused(Overloaded)`, and so is a head FIFO entry
that was never admitted. Admitted work runs to its reply, and then the head
closes the store. At D, every owed send is `Abandoned`, the head is cancelled,
and the store is left at a crash cut for recovery. The result is
`Stopped(next_due)` after a clean drain, `Unstarted` after a clean stop during
startup, or `Forced(n)` at the deadline. With `PlaceConfig.idle = Some(ms)`,
the runner starts a drain with admission closed after both supervisors
confirm the place has been idle for the interval. The place serves `GET /hirsel/status`, a raw route that answers 200
only when `Ready`, with the launch identity and `release_digest`.
The `fixture` module starts a place through `operate` on a memory or temporary
store, with typed calls whose replies are durable, `seen_at` for the first
batch at or after a version, relative bounds on the place's clock, and release
of every resource on `end` or on drop. It runs natively until the evaluator's model
network provider (W8a) lands.


### Store triggers

A place enables the trigger library by calling `triggers.migrate` from its
schema migration and setting `PlaceConfig.triggers` before calling
`runner.operate`. The `TriggerConfig` supplies the entry list and the
Caller for triggered transactions. A granted entry's `G` for each attempt comes from
`PlaceConfig.grants.attempt`, a `Mint` whose function
`fn(S, H, TriggerContext) -> Result[G, Capacity]` runs in its own task under
the mint's `mint_bytes` and `mint_steps`. Listing a granted entry without an `attempt` mint
refuses the open with `TriggerNoGrant`.
With `None` or an empty entry list a place starts no new trigger, but its head still
settles the rows already in its store: cancels and holds run, and a row whose entry is
gone is consumed `Gone` (RUNNER-SPEC §12.1).

A transaction uses `triggers.write(mut tx, tables, entries, target, arg, when)`
with a typed entry reference from `triggers.transact` or `triggers.granted`.
The configured list and the registry control which paths the head can dispatch.
Use `AfterCommit` or `At(T)`, where T is in the units of `store.CallInfo.at`.
Read the terminal result by trigger identity with `triggers.outcome` on a
snapshot or `triggers.fetch_outcome` in a transaction.

The `attempt` mint receives the attempt’s `TriggerContext`. The run uses `trigger_call` to
invoke a fenced transaction and `trigger_boundary` to check its attempt and
any committed cancel request before an outside step. `trigger_snapshot` returns a
read-only store snapshot for the current attempt, with a `store.Reader` to read it through, so the run can read rows off
the write lock; a stale or cancelled attempt gets an error instead. `trigger_hold` commits a
request fingerprint hold. A program's own entry may use
`triggers.request_cancel` or `triggers.release` in a transaction. Passing a
revision to release changes the stamp and clears the hold together. The next
start keeps the trigger identity and increments the attempt. A deploy or install
settles every run it would hold in its own activation commit, as the operator's
`--held` fate directs: `release` restamps the run so that it continues on the new
code, and `cancel` ends it `Cancelled`. A run therefore stays parked only when a
`Request` hold is made on the code that is already running (RUNNER-SPEC §14.6
"The activation fate").

The store step library gives a granted trigger a confined process body.
Include `steps.declare_tables` in the place migration and `steps.register` in the
place registry. Schedule a run with `steps.schedule`, which writes its step
cap in the same transaction as the trigger. The granted entry calls
`steps.run` with the attempt context, a map of grants keyed by step index,
codec input, and a plain body. The body receives the opaque context and input.
A callback receives the grant at its own index.

Use `steps.step` with a `StaticFn` for an outside operation. Pass the callback's
inputs in the step argument. The wrapper makes the started row
durable before calling and the result row durable before returning a value or
`Callback(E)`. After a crash the run's next attempt runs the body from the top:
each finished step returns its recorded result without calling again, and a step
that started and has no result returns `InDoubt(id)` instead of calling again. When a step returns
`InDoubt(id)`, `steps.resolve` takes the original callback descriptor and a
separate static lookup that can record a result under `steps.outside_key(id)`.
Use `steps.resend` only when the program decides to send again with the original
callback. Both calls check the step identity and original request fingerprint.
The fingerprint covers the step kind, callback identity, argument, result and
error schemas, and encoded argument. A mismatch places a hold on the run.

Use `steps.register_transaction` to register a plain transaction callback as
an attempt entry. `steps.transaction` commits its result, writes and any
triggers together. If the callback returns a typed error, the transaction
keeps the error result and rolls back the callback's writes. A park uses this
form to commit its state and a fresh `At(T)` trigger, then returns from the
process.
