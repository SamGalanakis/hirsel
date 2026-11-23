# Shared client runtime

Build with `npm install && npm run build` in this directory. The output is
`dist/client.js` and its declarations, with `browser.js` and `node.js` adapters.
Build outputs stay untracked. There are no runtime npm dependencies.

```ts
import {Client} from '@hirsel/client';
import {BrowserStorage} from '@hirsel/client/browser';
import {Frame} from './generated/frame.js';
import {want, identity, bind} from './generated/entries.js';

const client = await Client.open({
  url: 'wss://example.test/runner', frame: Frame, wants: want, identity,
  storage: new BrowserStorage(),
  onDiagnostic: console.warn,
  onRecovered: outcome => console.log('Recovered request', outcome),
});
await client.whenReady();
const api = bind(client);
```

For reconnect recovery, supply an application `credential` that the server's
binding maps to the same Caller on each open.
Storage keeps the Caller returned by the server with each stream. When an
anonymous reconnect receives a fresh Caller, any pending call becomes
`LocallyUnknown` and the new stream starts at position zero.

Node 22 or later uses the same `Client`, global `WebSocket` and generated
bindings. Pass `new NodeStorage(stateDirectory)` from `@hirsel/client/node`.
The Node adapter requires the system `flock` command. It holds the installation
lock and each stream lease with kernel file locks; no stale lockfile cleanup is
needed. State replacement writes a private temporary file, syncs it, renames it
and syncs the directory. The state directory holds pending arguments and replies.

Each client leases one stream and sends one call at a time. Calls made while
ready wait in memory in submission order, including behind a recovered call.
A queued call becomes durable when it reaches the front. Closing the client
rejects calls still in the queue. `onRecovered`
receives the outcome of a recovered request whose original promise no longer
exists. Call arguments and Want bytes are copied before asynchronous work.
`LocallyUnknown` means the original effect cannot be determined locally.
`RequestMismatch` fences the installation generation. Neither result causes
replacement work. Retired streams retain their historical record and are never
leased again. A later explicit client can allocate another stream.

`Incompatible` is terminal until `reconnect()` or a bindings reload. `Unbound`
means the place closed the Hello with 1008 before Welcome: its `PlaceConfig.wire`
binding refused the credential, or none was given (pass `credential`). It is
terminal until `reconnect()`. The original
pending request survives both. `StorageUnavailable` permits no sends; close the
client and reopen after storage is available. `close()` releases the stream lease
and preserves pending durable work. Each send checks the installation generation
under its lock, so a live client with an older generation cannot send work.

Subscription callbacks receive a complete current array of raw rows after an
atomic Batch. Reset and reconnect preserve the old cache until the replacement
id appears in `Batch.complete`, including an empty initial result. Old ids are
ignored. All subscriptions use `resume: None`.

Pass `onEnd` after the row callback on `api.subscribe.<name>`,
`client.subscribe()` or `client.subscribeChanges()` to receive the decoded
`SubscriptionEnd` reason. The client removes the subscription before calling
`onEnd` once for its `Ended` frame. Reset and reconnect resubscribe, and an empty
completed Batch calls the row callback with `[]`.

Optional predictions supplied as the fourth `call()` argument are removed when a matching outcome row arrives;
`onPrediction` runs after the complete Batch has been applied.

The scripted laws run as part of `store.codec_frames:ts_frame_vectors` in
`//:core-evaluator`. They use the generated Frame codec and independent
STORE P1-P3 fixtures. Real `serve` crash/reconnect qualification is deferred to
W3e; these laws make no claim about that integration.

The packed-package check runs in `//:core-evaluator`. To run it alone, use
`.buck2/bin/buck2 test //:core-evaluator -- --test-arg=--filter --test-arg=sdk.client:packed_consumer`.
It installs the tarball offline and type-checks and runs generated bindings in
an isolated Node project.
