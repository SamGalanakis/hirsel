# Generated TypeScript bindings

`hirsel sdk ts <program> --output <dir>` builds the program and runs its describe
launch mode. It emits ES modules and `.d.ts` declarations for Node and
browsers, along with the TypeScript client runtime.
The output directory has `package.json` with `"type": "module"`, so Node loads
the same `.js` files even inside a CommonJS project.
It also contains `manifest.json`, generated from the same Describe response.
[Embedding](embed.md#emb-5-swift-bindings-and-the-xcframework) specifies the Swift bindings
from the same manifest, over the C embed ABI instead of WebSocket.

A server using `runner` declares `runner = { major = 1 }` under
`[dependencies]` in `hirsel.toml`.

## 4. Generated code

`codec.js` follows `std.codec` value schemas and ordered key schemas. Integers
through 32 bits are `number`; u64 and i64 inputs and outputs are `bigint` only.
Bytes are `Uint8Array`, strings are strict UTF-8, records are objects in declared
field order, and tuples are TypeScript tuples. Maps retain insertion order.
Unit is `undefined`. Floats encode canonical NaNs and reject other NaN patterns
when decoding. Decoding rejects malformed tags and trailing bytes.

Unit variants are `{tag}`. One-field variants are `{tag, value: T}`.
Multi-field variants are `{tag, values: [T0, T1, ...]}`, with a TypeScript tuple
for `values`. Thus Option is `{tag: "None"} | {tag: "Some", value: T}` and
Result is `{tag: "Ok", value: T} | {tag: "Error", value: E}`. Nested Options
retain every tag, including Some(None) and Some(unit). Nominal `via` types
expose their representation type.

`entries.js` exports the Want list, launch identity and `bind(client)`, whose
result supplies `call`, `subscribe`, `subscribeDecoded` and `room` entry stubs,
each namespaced by module: `api.call.<module>.<name>(args, options?)`,
`api.subscribe.<module>.<name>`, `api.subscribeDecoded.<module>.<name>` and
`api.room.<module>.<name>(args, handlers?)`. Every module is namespaced the same
way, `main` included, and there is no flat name. Calls (compute, granted and
transact entries) appear under `call`; queries appear under `subscribe` and
`subscribeDecoded`; rooms (`runner.room`) appear under `room`.

An entry's **stub path** comes from its definition identity `D`, which in the
identity bytes ends at the first `[` or `(` ([PROG-9](programs.md#prog-9-code-descriptors)).
The package component is dropped, and every remaining component, each module path
component and then the function's name, is percent-decoded. A nested module path
gives nested objects: an entry `fn` in module `a.b`, with `D` =
`app@1/a/b/fn`, is `api.call.a.b.fn`. Two instances of one generic function have
one stub path.

Stub paths must be distinct, and no stub path may be a proper prefix of another,
so a module `a` with an entry `b` and a module `a.b` cannot both be exported (the
object `api.call.a.b` would be both a function and a namespace). The rule covers
every entry kind together. A clash, such as two instances of one generic function
in one module, causes `DuplicateStubName(a, b)` before any output is written,
where `a` and `b` are the two entries written `module.path::name`, for example
`DuplicateStubName(main::pick, main::pick)`.

The transport implements `call(want, argsBytes, resultDecoder)`, returning
`Promise<Outcome<R>>`.

### Subscriptions

`subscribe(want, argsBytes, onChange, onEnd?)` returns a handle with `value()`,
`rows(slot)`, `map(slot)`, `get(slot, key)` and `unsubscribe()`. The four reads
return `Pending` until the current instance completes, `Ready` with the data
from the server plus pending optimistic writes, or `Ended` after an `Ended`
frame or `unsubscribe()`. `current()` reports whether the instance has a
server image. An absent key in a ready image gives `Ready` with
`undefined`.

`value()` reads the query result. `rows(slot)` reads the rows as an array.
`map(slot)` indexes the same rows by lowercase hex of the slot key.
Raw subscriptions use the full wire key. Generated decoded subscriptions
use the ordered key encoding without the store's four-byte collection id.
`get(slot, key)` reads one row by key. The array and map each keep their
identity until that slot changes. Reads and deltas share row references. The
SDK freezes decoded plain objects, arrays and tuples, and exposes
`ReadonlyMap` and readonly byte types. Mutating a shared map or
`Uint8Array`, or any data within a map leaf, is undefined behavior.

`onChange(sub, change)` runs after the client applies the whole Batch or
recomputes the image for a local change. `change.rows` contains typed `RowChange` records with the slot
and hex key. An insertion has `new`, a deletion has `old`, and an update
has both. `change.value` carries the old and new result when its encoded
bytes change. `change.replaced` marks a completing group. Its rows still
describe the difference from the current image, and a byte-identical
completion of a ready image is silent. The first completion publishes even
when its rows and value encoding are empty. An empty value in a
non-completing group leaves that result as it was.

Reset, transport loss and terminal client states clear the public image and
detach old subscription ids immediately. When an image is cleared, its
handler receives `{invalidated: true, replaced: false, rows: []}` after all
affected handles read `Pending`. The next instance's first completion
describes only the new image, including for the same Caller. The SDK ignores
frames of lost connections and groups for retired ids. Reset and reconnect
resubscribe. The SDK delivers the optional `onEnd` once after removing the
matching subscription for an `Ended` frame.

Use `api.subscribeDecoded.<module>.<name>(args, onChange, onEnd?)` for typed
values, rows and keys. A row holds its slot name, decoded key and present
value as `Some`. The generated stub uses the appropriate slot's
`Key<i>_<s>` encoder for `get`. `codec.js` exports `Value<i>`,
`Key<i>_<s>` and `RowValue<i>_<s>`
for the query at index `i` and slot `s` in that query. Use
`api.subscribe.<module>.<name>` for numeric slots and raw byte keys.

### Rooms

`api.room.<module>.<name>(args, handlers?)` joins a room registered with
`runner.room`. The handle is a decoded subscription to the room's admission
view, so `value()` is its `Grant` (`{tag: 'Denied'}`, `{tag: 'Watch'}` or
`{tag: 'Join'}`) and `rows`, `map` and `get` read its declared slots, with
`handlers.onChange` as a subscription's `onChange`. `codec.js` exports
`State<i>` and `Message<i>` beside `Value<i>` for the room at index `i`, from
the State and Message types the room registered. The room's live part adds:

- `self`: this membership's peer id, set by the Part that carries `you`, and
  undefined with `Watch`, while waiting, when `full` or between sessions.
- `peers()`: the roster, a map from peer id to `{caller, state}`, where `state`
  is the peer's newest State or undefined. It holds one incarnation's peers
  only, so a peer id names one admission, `(incarnation, peer)`.
- `incarnation`: the roster's life, the 32 lowercase hex digits of the relay's
  16 incarnation bytes in order, or undefined before the first Part. The relay
  draws a new one when it creates the roster and when the roster empties, and
  restarts peer ids at 1 there.
- `revision`: the roster's membership revision at which `peers()` is complete
  (the last Part with `snapshot` set). It starts at 1 for each incarnation and
  rises by one at every admission and removal. It is undefined from a Part
  that starts a new incarnation or a new membership's image until that
  image's snapshot.
- `set(state)`: the newest value wins. `undefined` clears. The SDK writes a
  `Set` only while the socket holds less than one Live frame (64 KiB) of
  unsent bytes, and otherwise checks again on the next `set`, the next frame
  and a 16 ms timer that runs only while a value waits. A value set before
  membership is stored and becomes the member's first state when `you`
  arrives; each new membership, a reconnect's included, is sent the newest one.
- `send(message, to?)`: writes one `Send` at once, to the whole room or to
  peer `to`. Messages are never coalesced. Before membership, or with more
  than 64 KiB unsent, the message is dropped and `onLost(1)` fires.
- `leave()`: unsubscribes.

`set` and `send` throw `RangeError` for a value whose encoding exceeds the
room's registered `state` or `message` bound, because the server closes the
session with 1002 for such a frame.

The handlers are `onPeers(peers, change)`, with `change` naming the `joined`,
`left` and `set` peer ids, the room's `incarnation` and `revision` after the
change, and `renewed` when the Part began a new incarnation;
`onMessage(from, message, direct)`; `onGap(n)` for messages to this client the
server dropped; `onLost(n)` for messages from this client that were dropped;
`onFull()` when the grant holds but the room has no place yet; and `onEnd` as
for a subscription, once after the server's `Ended`. The SDK applies each
Part's `joined`, then `states`, then `msgs`, then `left`, so a message's
sender is in `peers()` when `onMessage` runs. There is no order between
`Live` and `Batch`.

A Part whose incarnation differs from the room's replaces the roster, never
merges with it: every earlier peer is in `left`, with `renewed` set, even
without `complete`. A `complete` Part on the same incarnation starts a new
membership's image, which may span several Parts. A peer it shows continues
with its caller and the state the image carries, and is not reported as
joined. A peer it has not shown by the Part with `snapshot` set has left, and
is in that Part's `left`. A `full` Part's zero head changes neither the
incarnation nor the revision. When a session ends, `self` clears, `peers()`,
`incarnation` and `revision` stay as last told, and the room resubscribes
under a fresh sub id with the other subscriptions. The next membership's
first image then continues or replaces the roster. This client's own peer id
from an old membership is dead. Parts for a room this client left are
ignored.

### Optimistic writes

Pass `{optimistic(draft) { … }}` as the call options to update subscription
images while a call is queued or in flight. `draft.get(sub, slot, key)`
reads the current draft. `put(sub, slot, key, row)`, `delete(sub, slot, key)`
and `value(sub, result)` edit it. The draft uses the subscription's key type
and encoder. A row passed to `put` must match its slot and key and carry a
`Some` value. Custom subscription codecs provide `encodeRow` and
`encodeValue` for the corresponding writes.

Write synchronous functions that use the draft for reads and writes. The
client runs them in queue order over the latest server images on every
commit and whenever optimistic work enters or leaves the queue. A call made
while the client is `Ready` runs its function and notifies the changed
subscriptions before `call` returns, even while earlier work is still being
written to storage. If the call is then refused, its writes are removed. A throwing
function loses its changes and reports its error to `onDiagnostic`. The
call continues. The functions live in memory and disappear on reload.

The SDK applies a successful outcome's server rows and removes the matching
optimistic writes in the same commit. Equal encoded rows and values stay
silent. After loss or Reset, reads remain `Pending` until the fresh instance
completes. A successful outcome arriving early keeps its overlay until every
subscription it wrote has completed or ended. Failures remove the overlay
immediately. If an instance is current and the server delivers a successful
outcome before its rows in a later Batch, the visible row can revert briefly.

Server rows and Draft writes use the same encoded key in decoded slots,
so confirmation can keep an optimistically inserted row even when the slot
was empty. The collection id belongs to the live store and can differ from
ids in Describe's empty-store declaration run.

For a query, `Described.result` is R's value schema followed by the slot list,
each slot's name, ordered key schema and value schema, each schema encoded as
`std.codec` Bytes. `result_fp` fingerprints the whole, so changing R, a slot's
name, its table's schemas, or the slot order changes the Want. Describe also
carries the explicit `value` and `slots` schemas for each query. Its
`tables` diagnostics list declarations from a Migrate run over an empty
memory store. The generator exports those diagnostics without using them as
part of the release identity.

Compute and Granted calls return `Promise<Outcome<R>>`. Transact calls return
`Promise<Outcome<Result<R, E>>>`: the Returned payload contains the complete
Result tag and payload, including a typed Error after an aborted transaction.
For calls, Described.result is the exact Returned payload schema. The child
fingerprints the full Result for Transact, so changing E changes result_fp too.

The launch identity contains digest bytes, target and `n_place`. Fingerprints
come from the described program; the generator does not implement fingerprinting.

`frame.js` exports the Frame, Outcome, Fault and Refusal codecs, the fixed Raised
and BadKind number tables, and `subprotocol = "hirsel.4"`. Frame is a protocol
type shared by every executable. The schema `std.codec.schema(of[runner.Frame]())`
is generated from the Runner and std sources by `core/tools/frame_schema.py`
and checked in as `core/programs/runner/frame-schema.bin`. The
`core-runner-frame-schema` CI target regenerates it and compares the bytes. The generator reads that file, and
Describe contains only per-program data. A Frame change requires a subprotocol
change. The client checks the negotiated subprotocol. The server answers HTTP
400 if the offer omits `hirsel.4`. The SDK reports Close 1009 as `ReplyTooLarge`.
Outcome has Returned, Failed, Refused, Abandoned and InDoubt at tags 0 through
4. Fault appends Conflict at tag 4 (RUNNER-SPEC v2.45). Refusal appends RequestMismatch at tag 6, HistoryUnavailable at tag 7 and StreamFuture at tag 8. StreamFuture carries the store's keyed time as a u64 in milliseconds (RUNNER-SPEC v2.88).
Superseded is absent. The generated codecs follow the bundled declarations.

`--place-timeout` is a positive number of seconds, default 10. `--n-place` fixes
the registration allowance, default 16 MiB. Ordinary native build options select
the runtime, engine and target. A built executable can also be supplied as the
program. Describe and generation errors leave the output destination untouched.
The CLI uses the driver's shared bounded describe child. Capacity and duplicate
registration errors retain their typed payloads; an expired child reports
`PlaceTimedOut` after the launcher kills and reaps it.

## Schema manifest

`hirsel sdk json <program>` writes the same `manifest.json` bytes to stdout.
Both commands accept a program directory or a built executable. They share
the Describe allowance, timeout, and native build options. Only `sdk ts`
takes `--output`.

The manifest is a compact UTF-8 JSON object with lexicographically sorted
object keys and a final newline. It has four fields.

| Field | Contents |
| --- | --- |
| `version` | Integer `1` for this manifest format. |
| `launch` | `digest`, `target`, `n_place`, and `revision` from Describe. |
| `tables` | Declared collections in Describe order. Each has `name`, `id`, `owner`, and the store catalog's numeric `kind`. |
| `entries` | Exported entries in registration order. |

Each entry has `module` (an array of decoded module path components) and
`name` (the decoded function name). Together they give the TypeScript stub
path from section 4. `identity` retains the full PROG-9 entry identity,
including the instance and signature. `kind` is `compute`, `transact`,
`query`, `granted`, or `room`. `args_fp`, `result_fp`, and `revision` come from
Describe. The launch's `n_place` and both fingerprints are decimal strings
to preserve all 64 bits. `digest`, `identity`, and `revision` are arrays of
byte integers.

An entry's `args` and `result` are codec ASTs. For a query, `result` is the
query's value codec, and its `slots` array supplies the rest of the query
result schema in declaration order. Each slot has `name`, `kind` (`table`,
`cell`, or `log`), `key` (the ordered key codec), `key_json` (the matching
value codec for an HTTP JSON projection), and `value` (the row codec). A
cell has the ordered unit codec for `key`. Calls have an empty `slots` array.
A room's `result` is its admission view's Grant codec and `slots` its declared
slots, as for a query. Only a room has `room`: its `state` and `message` value
codecs and the `peers`, `state_max`, and `message_max` bounds from
`runner.room`.

A codec AST has `key` (true for ordered keys, false for values), `root` (a
type expression), and `definitions` (an array indexed from zero). A nominal
definition has its full `name`, an ordered `arguments` array of instantiated
type expressions, a numeric `kind`, and a `body`.

| Definition kind | Body |
| --- | --- |
| `0` (record) | Fields in declaration order, each with `name`, `removed`, and `type`. |
| `1` (sum) | Variants in wire tag order, each with `name`, `removed`, and the ordered `fields` array of payload types. |
| `2` (`via`) | The representation type expression. |

`removed` is a boolean. Tombstones retain their positions and types.
A type expression is an array starting with its codec schema opcode.

| Expression | Type |
| --- | --- |
| `[0]`, `[1]`, `[2]` | Unit, bool, char |
| `[3]`, `[4]`, `[5]`, `[6]` | u8, u16, u32, u64 |
| `[7]`, `[8]`, `[9]`, `[10]` | i8, i16, i32, i64 |
| `[11]`, `[12]`, `[13]` | f32, f64, String |
| `[14, T]` | List of `T`, with `[14, [3]]` the codec form of Bytes |
| `[15, K, V]` | Map with key type `K` and value type `V` |
| `[16, T]` | Option of `T` |
| `[17, T, E]` | Result with success type `T` and error type `E` |
| `[18, [T0, T1, ...]]` | Tuple with its element types in order |
| `[19, i]` | Reference to definition `i` in this AST |
| `[20]` | Never |
| `[21, T]` | Descending ordered key of type `T` |

## Shared client runtime

The generated directory includes the shared client runtime in `client.ts`,
`persistence.ts`, `browser.ts` and `node.ts`. Compile these sources with your
TypeScript app or bundle them with your browser app. Include the generated
`codec.js`, `entries.js` and `frame.js` beside the compiled runtime. The SDK
needs no runtime npm dependencies.

```text
import {Client} from './generated/client.js';
import {BrowserStorage} from './generated/browser.js';
import {Frame} from './generated/frame.js';
import {bind, identity, kind, want} from './generated/entries.js';

const client = await Client.open({
  url: 'wss://example.test/hirsel',
  frame: Frame,
  wants: want,
  kinds: kind,
  identity,
  storage: new BrowserStorage(),
});
await client.whenReady();
const api = bind(client);
```

For Node 22 or later, use `NodeStorage` from `./generated/node.js` and pass
a private local state directory to its constructor. Processes sharing a state
directory must share a PID namespace. The adapter needs Node type declarations
when you type-check it.

`Client.open` requires the ingress WebSocket `url`, the generated `frame`
codec, `wants` and their `kinds`, and a `storage` adapter. Pass the generated `identity` to
check the launch digest, target and `n_place`. The optional `credential` is an
async source, `() => Promise<Uint8Array | undefined>`, whose bytes go in Hello.
The client calls it before its first Hello and once per credential recovery
(below), never inside its serialized work or the storage lock. `deadlineMs` sets the server's call deadline and defaults to 30,000. A call's own `deadlineMs` and `signal` options (below) bound it alone.
The optional `socket` factory and `clock` override the platform WebSocket and
time source. `onState`, `onDiagnostic`, `onRecovered` and `onBatch` observe
state changes, diagnostics, recovered call outcomes and applied Batch versions.

Browser storage uses strict IndexedDB transactions and generation-scoped Web
Locks. Node storage uses durable file replacement and exclusive filesystem
claims for its leases. A claim records the owner's PID and a unique token.
After the PID exits, contenders advance to a new claim without deleting the
old one. A PID that has been reused can delay recovery until it exits.
Stop all clients using the old `flock` adapter before opening the same
directory with the new adapter.
Recovery format 4 stores a random installation ID and random fencing generation.
Generation validation and each work send share the installation lock. Missing
stream records fence the generation. Pending requests keep their original Caller.

A stream passes a position only after its reply is stored (STORE-SPEC §1.8
S7), so a stream carries one call at a time and the client multiplexes calls
as lanes: each in-flight call holds its own leased stream of the session's
Caller. At most 1+q lanes hold a call, where q comes from Welcome; this matches
the place's session limit of 1+q owed units, so the client never earns an
Overloaded refusal by itself. Calls past that width wait in memory in
submission order. Opening the client leases every free stream of one Caller
that still holds a call, so each recovered call resumes on its own lane next to
new work. Completions settle their own lane in any order; Returned and Failed
answers that arrive together commit in one storage write, and calls that start
together share a pending write and a subsequent in-flight write under the
same installation lock. Each Call frame carries
`min(deadlineMs, the call's remaining time)`. When a call's own deadline passes
or its `signal` aborts before any delivery, its pending record is deleted, the
position stays free, and the call is refused with Deadline (or rejected with
the signal's reason). Once a delivery may have happened, the caller is answered
LocallyUnknown and the lane keeps recovering under the same idempotency key,
reporting the outcome to `onRecovered`; a deadline does not end a call the
place still owes, since the place answers by the deadline it was sent. A
reconnect aborts the old connection and replays every lane's pending call once
under its unchanged key; answers from the old connection are dropped. Each
lane's first call mints a stream ID from the keyed time in Welcome and a cryptographic
64-bit nonce. The client persists the stream ID, position, argument bytes and
in-flight flag before sending. A stream position belongs to a stored call: a
call to a transact entry. Returned and Failed outcomes of a stored call commit
together with the next position. A compute or granted call is sent at the
stream's next position, which only labels its Outcome row, and its outcome
leaves the position unchanged. Nothing is stored for it, so a re-send runs it
again, and a granted call whose delivery is uncertain is reported
LocallyUnknown and never re-sent. The client reads each call's kind from the
generated `kind`, one per `want`. A lost attempt of a stored or compute call
retries the original identity after backoff. A decisive refusal reuses the position only when every attempt received
a no-commit answer.

On WebSocket, HistoryUnavailable allows one remint when the installation
minted the stream and every attempt received a no-commit answer. If prior
delivery is uncertain, the client retires that stream alone and reports
LocallyUnknown for its call; other lanes and the client continue.
On HTTP a matching 410 marker applies to its own delivery. An earlier request
could have reached the place through a proxy retry, so the client reports
LocallyUnknown and retires the stream. Every POST comes from an explicit
recovery transition. RequestMismatch fences the generation: the call that
sent it learns RequestMismatch and every other in-flight call LocallyUnknown.

Incompatible closes remain terminal until an explicit reconnect or bindings
reload. Reconnection waits for all Placed frames, recovers pending calls, and
restores subscriptions using new IDs. Disconnect invalidates old images.
Each subscription stays pending until its new `complete` (above).

The client sends the bytes of the application's credential source in Hello and stores the returned Caller with the stream. It replays
pending work only when Welcome returns the same Caller. If an anonymous
connection receives a new anonymous Caller, the client reports any pending
outcome as LocallyUnknown and starts at position zero for that Caller.

A place's binding may give the session an expiry (RUNNER-SPEC CHANGES-v2.57).
When it is reached, the place closes an established socket with the private
code 4401 `AuthExpired` and answers a private HTTP call with 401. A client that
was offline past its credential's expiry first sees Close 1008 before Welcome
instead. All three are one credential recovery. The client drops the refused
bytes, so no path sends them again, clears every public image at once, enters
the `CredentialNeeded` state, reports a diagnostic and calls the credential
source once. WS and HTTP refusals share the one refresh in flight. The client
reconnects with the fresh bytes. If the source throws, gives no bytes, gives
the bytes just refused, or the place refuses the refreshed credential too, the
client enters `Unbound` with no further refresh. A recovery attempt ends at
Welcome; the next refusal starts a new one. A refresh that settles after
`close()` or fencing is discarded. Durable pending work is replayed only after
Welcome names the same Caller: the client recovers the exact pending identity
and resubscribes with fresh ids and complete images, as on any reconnect. A
different Caller follows the Caller change rules above, and the anonymous
issuer-1 reset to LocallyUnknown is unchanged. Calls queued in memory wait
through `CredentialNeeded`.

A binding that cannot decide answers Close 1013 before Welcome or HTTP 503 at
bind (RUNNER-SPEC §15.6, J2). It is not a credential failure (J5). The client
keeps its credential, does not call the source, and reconnects with jittered
backoff, as after a transport failure. An SSE consumer that gets a 503 with no
body before the head re-requests the same way with the same credential.

An SSE subscription (http.md B-9) shares the same credential source and the
same refresh in flight. Its final event `expired`, and a 401 on any SSE
request or re-request, are the same credential recovery as Close 4401: the
consumer clears that subscription's image, never re-requests with the refused
bytes, and re-requests with the refreshed ones, under the same one-refresh
rule and the same `Unbound` ends. A stream that ends with no final event
(`lease_end` or a place's stop) is not a credential failure: the consumer
re-requests at once with the credential it holds, without calling the source.
If `expired` was lost, that re-request gets 401, which starts the recovery,
so the consumer never needs the event to stay correct. Every re-request is a
fresh subscription (no `Last-Event-ID`), and the image is replaced from empty
at its completing `batch` event.

Call `bindHttp(client, {url}).<op>(args, {signal, onChange, onEnd})` for a
generated Subscribe binding. It returns the same typed image reads as a
decoded WebSocket subscription and a `done` promise. `done` reports
`Ended` with the decoded Outcome, `Refused` with the pre-head HTTP status
and body (other than 401 and a 503 with no body, which recover as above), or
`Aborted` on cancellation. It rejects with the client's error
if the shared credential recovery ends in `Unbound`. A Reset or a transport
failure clears the image and resubscribes with jittered backoff. Each active
SSE subscription holds one HTTP/1.1 connection for its current request.

## Calling an entry from the terminal

`hirsel call SERVER --url ws://HOST:PORT/PATH [--stream 32-HEX] [-- ENTRY [--FLAG VALUE...]]`
lists a running place's entries, or calls one of them, for debugging. SERVER is
the built executable the place runs. Without ENTRY, or with `-- --help`, the
command prints every entry with its kind, flags and result type. `-- ENTRY
--help` also prints the entry's named types.

ENTRY is `MODULE/NAME`: the entry's module path, its components joined by `.`,
then `/` and the function's name, the stub path of the TypeScript bindings
(section 4) written for the terminal. So `fn` in module `main` is `main/fn`, and
`fn` in module `a.b` is `a.b/fn`. There is no bare-name form, for `main` or any
other module. The listing prints each entry in this form, and the stub path
rules, including `DuplicateStubName`, apply as they do in `hirsel sdk ts`.

**Transport.** The call is an ordinary client session on the server's ingress,
as the TypeScript client's is. It sends Hello without an application credential.
It mints a fresh stream ID from the keyed time in Welcome and a random nonce,
sends one Call at position zero, and reads Batch and Reset frames until the
matching Done arrives. A place applies its credential binding to the CLI's
empty credential as it would to any other client. The upgrade offers `hirsel.4` and checks
`Sec-WebSocket-Accept` and the selected subprotocol. `--timeout` bounds the
connection and is the Call's `deadline_ms`; default 10 seconds. A session that
ends after the Call was sent reports LocallyUnknown. The command sends once
and prints the received outcome, including HistoryUnavailable or Overloaded.
With `--stream`, it uses the supplied ID as-is. The 32 hexadecimal digits
encode the 64-bit birth stamp followed by the 64-bit nonce in most-significant
digit first order. Repeat the command with the same Caller, ID and arguments
to recover a retained outcome. `wss://` is refused as unsupported.

**Schemas.** Entry identities, fingerprints and argument and result schemas come
from SERVER's describe launch mode, as in `hirsel sdk ts`. `--n-place` and
`--place-timeout` mean what they mean there. They must match the running place,
so the types match what is running. The Frame codec is the same one the SDK
uses, from the checked-in Runner frame schema. Before sending a Call, the CLI
checks two things:

- If Welcome names a launch digest, the digest, target and `n_place` must equal
  the Describe's. Otherwise the CLI refuses and sends nothing more.
- The Hello want carries the Describe's identity and fingerprints, and the
  server answers Placed per want. Gone means the server does not serve that
  identity with those fingerprints, and the CLI refuses without sending the
  Call. So does Resolve, which the CLI does not support.

Query entries are listed, but calling one is refused as unsupported, because a
query streams rows rather than returning one outcome.

**Arguments.** Every flag is typed against the argument schema before the Frame
probe or any connection runs:

- If the argument type is a record, pass `-- ENTRY --NAME VALUE ...`, with
  one flag for each non-unit field.
- Otherwise pass the whole argument as `-- ENTRY --arg VALUE`.
- A unit argument takes no flags.

Unknown, repeated and missing flags are refused, and so is a value that does not
fit its type. A refusal names the flag and the expected type.

Primitive values are plain text:

- bool is `true` or `false`.
- Integers are decimal, with `-` only for signed types, and must fit their width
  exactly.
- f32 and f64 are a finite decimal, `NaN`, `inf` or `-inf`.
- char is one Unicode scalar.
- String is the raw text.
- Bytes is hex with an even number of digits.

A `via` type takes its representation's form. Every other value is JSON in the
one mapping below. The same mapping prints outcomes.

**The JSON mapping.** Each `std.codec` value type has exactly one JSON form:

- unit is `null`.
- bool is a JSON boolean.
- Every integer type is an exact JSON integer, including u64 and i64 values
  beyond 2^53. A fraction, an exponent or an out-of-range value is refused.
- f32 and f64 are JSON numbers, or the strings `"NaN"`, `"inf"` and `"-inf"`.
- char and String are JSON strings.
- Bytes (List[u8]) is a hex string.
- Other lists are arrays.
- Map[K, V] is an array of `[key, value]` pairs; duplicate keys are refused.
- Tuples are arrays of their exact arity.
- Records are objects with exactly their live field names. Key order carries no
  meaning, and output keys are sorted.
- Variants, Option and Result use the tagged form of section 4: `{"tag": N}`,
  `{"tag": N, "value": V}` or `{"tag": N, "values": [V0, V1, ...]}`.

**Output.** The CLI prints the Outcome as one JSON line on stdout, in the same
mapping. For Returned, the payload is decoded with the entry's result schema,
for example `{"tag":"Returned","value":{"tag":"Ok","value":4}}`. Other outcomes
print as the Frame codec decodes them, for example
`{"tag":"Failed","value":{"tag":"Raised","value":1}}`. The exit status is 0 for
Returned and 1 for any other outcome. Refusals before sending, and transport
errors, exit 2 with a message on stderr.

Design notes:

- Integers must be exact JSON integers, not strings or a second syntax, so there
  is one form for every type and nothing silently rounds through f64.
- Primitive flags take plain text because it is unambiguous for those types.
- Composite values are JSON rather than Hirsel literal syntax: the driver
  already parses JSON, and section 4 already defines the tagged shapes.
- The command is `call`, beside `sdk`, because both read a server's Describe.
