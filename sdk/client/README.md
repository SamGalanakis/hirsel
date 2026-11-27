# Shared client runtime

Build with `npm install && npm run build` in this directory. The output is
`dist/client.js` and its declarations, with `browser.js`, `memory.js` and
`node.js` adapters. Build outputs stay untracked. There are no runtime npm
dependencies.

```ts
import {Client} from '@hirsel/client';
import {BrowserStorage} from '@hirsel/client/browser';
import {Frame} from './generated/frame.js';
import {want, kind, identity, bind} from './generated/entries.js';

const client = await Client.open({
  url: 'wss://example.test/runner', frame: Frame, wants: want, kinds: kind, identity,
  storage: new BrowserStorage(),
  onDiagnostic: console.warn,
  onRecovered: outcome => console.log('Recovered request', outcome),
});
await client.whenReady();
const api = bind(client);
```

For reconnect recovery, supply an application `credential` source,
`async () => bytes`, that the server's binding maps to the same Caller on each
open. The client calls it for the first Hello and once more when the place
refuses or expires the credential (Close 4401, Close 1008 before Welcome, or
HTTP 401); meanwhile the state is `CredentialNeeded`.
Storage keeps the Caller returned by the server with each stream. When an
anonymous reconnect receives a fresh Caller, any pending call becomes
`LocallyUnknown` and the new stream starts at position zero.

Node 22 or later uses the same `Client`, global `WebSocket` and generated
bindings. Pass `new NodeStorage(stateDirectory)` from `@hirsel/client/node`.
Use a private local state directory. Clients that share it must run in the
same PID namespace. The Node adapter takes installation and stream leases
through exclusive filesystem claims. It syncs each batch of outbox changes
to `state.wal`. The WAL starts with a checkpoint and stores changes in
checksummed frames. Recovery replays complete frames and discards an
incomplete tail. After 128 changes or 1 MiB of WAL changes, the next write
atomically replaces the WAL with a synced checkpoint before appending.
The state directory holds pending arguments and replies.

For stateless Node, server and ephemeral browser callers, pass
`new MemoryStorage()` from `@hirsel/client/memory`. It keeps in-session
reconnect idempotency with zero disk syncs: a dropped connection replays each
pending call under its key. Its state lasts only as long as the owning process
or tab (and the adapter object within it); a restart or reload starts empty.

When upgrading from the JSON snapshot format, let the old clients finish
their pending calls, close them and use a new state directory.

Each client keeps up to 1+q calls in flight, where q is the extra units the
place's Welcome allows (the runner's default q is 4). Each in-flight call holds
its own leased stream, because a stream carries one stored call at a time.
Calls past that width wait in memory in submission order and become durable
when a lane frees up; completions may arrive in any order. Pass
`{deadlineMs, signal}` as call options to bound or cancel one call alone: a
call not yet sent is refused with `Deadline` (or rejected with the signal's
reason), and a call already sent resolves `LocallyUnknown` while its lane keeps
recovering and reports the real outcome to `onRecovered`. Closing the client
rejects calls still in the queue. `onRecovered`
receives the outcome of a recovered request whose original promise no longer
exists. Call arguments and Want bytes are copied before asynchronous work.
`LocallyUnknown` means the original effect cannot be determined locally.
`RequestMismatch` fences the installation generation. Neither result causes
replacement work. A stream whose history is gone retires alone: its call
resolves `LocallyUnknown`, and the client and its other calls go on. Retired
streams retain their historical record and are never leased again. A later
explicit client can allocate another stream.

`Incompatible` is terminal until `reconnect()` or a bindings reload. `Unbound`
means the place closed the Hello with 1008 before Welcome: its `PlaceConfig.wire`
binding refused the credential and one refresh did not recover, or none was
given (pass `credential`). It is terminal until `reconnect()`. The original
pending request survives both. `StorageUnavailable` permits no sends; close the
client and reopen after storage is available. `close()` releases the stream lease
and preserves pending durable work. Each send checks the installation generation
under its lock, so a live client with an older generation cannot send work.

Subscription callbacks receive `onChange(sub, change)` after the entire Batch
applies. `change.rows` carries the old and new rows for changed keys.
`sub.rows(slot)`, `sub.map(slot)` and `sub.get(slot, key)` share frozen rows.
The array and map keep their identity until the slot changes. Mutating shared
`Uint8Array` or `Map` leaves, or a map returned by `sub.map`, is undefined
behavior. Use `api.subscribeDecoded.<module>.<name>` for typed keys and values.
Its map and delta keys are hex of the ordered slot key. Raw subscriptions
use full wire keys.

Reset, connection loss and terminal client states clear the public images and
retire their old ids. Each cleared subscription receives a data-free
invalidation after the affected handles become `Pending`. Reset and reconnect
resubscribe with `resume: None`. The fresh completion publishes only rows
in the new image, including an empty initial result.

Pass `onEnd` after `onChange` to receive the decoded `SubscriptionEnd` reason.
The client removes the subscription before calling `onEnd` once for its
`Ended` frame. [SDK subscription contract](../../docs/spec/sdk.md#subscriptions)

Pass `{optimistic(draft) { … }}` as the second argument to a generated call,
or the fourth argument to `client.call`. The draft's `get`, `put`, `delete`
and `value` methods operate on your subscription handles. Generated handles
use the slot's typed key. The client replays these synchronous functions in
queue order against the latest server image. A successful outcome commits
its rows and removes its optimistic writes together. Equal bytes keep the
current row and emit no change. A failure removes its writes immediately.
After reconnect, a successful outcome can arrive before the subscription is
current. Its writes remain queued until the affected subscriptions complete or
end. Public reads still return `Pending` before each subscription completes.
Reloading loses the functions and recovers only the durable call.
[SDK optimistic write contract](../../docs/spec/sdk.md#optimistic-writes)

Rooms (`api.room.<module>.<name>`) add the membership to a subscription:
`self`, `peers()`, `set`, `send`, and the roster's `incarnation` and
`revision`. A new incarnation replaces the roster; a reconnect on the same
incarnation keeps the peers its new image shows. `onSeat(self, previous)`
reports every membership-seat change, with `self` undefined when a session end
or room fullness clears the seat. `RoomPeersChange.selfChanged` marks a Part
that changed the peer id, even when `renewed` is false. `onLost` is only for
dropped outgoing ephemeral messages, not seat loss.
[SDK room contract](../../docs/spec/sdk.md#rooms)

The scripted laws run as part of `runner.codec_frames:ts_frame_vectors` in
`//:core-evaluator`. They use the generated Frame codec and independent
STORE P1-P3 fixtures. Real `serve` crash/reconnect qualification is deferred to
W3e; these laws make no claim about that integration.

## Run the subscription example

The [subscription example](tests/subscription-cache.ts) reads a row committed
by the [store and runner program](tests/packed_consumer.hirsel). It displays
the row from `get` and the count from `map` on every `onChange`. The call
temporarily reopens the item through `optimistic`. The runner reply carries
no row update, so the item returns to its committed state.

From the repository root, run the packed-package check in `//:core-evaluator`.
It installs the tarball offline, builds and runs the Hirsel program, then
type-checks and runs the TypeScript example with the generated bindings.

```sh
.buck2/bin/buck2 test //:core-evaluator -- --test-arg=--filter --test-arg=sdk.client:packed_consumer
```
