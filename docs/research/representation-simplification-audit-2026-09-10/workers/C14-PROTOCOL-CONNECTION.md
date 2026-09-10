# C14-PROTOCOL-CONNECTION audit

## Verdict

Two materially useful, high-confidence findings are confirmed at the fixed
snapshot:

| ID | Priority | Finding | Reachability |
| --- | --- | --- | --- |
| C14-01 | High | `HelloBroadcastDedupe` destructively removes a view cache entry without replacing it, and does not forget removed views. | Reachable for every connected client after a view upsert; the remove/recreate form leaves a real view missing from the client. |
| C14-02 | High | The browser uses a lifetime `everAuthed` latch to classify a new socket's pre-handshake error, masking credential rejection after reconnect. | Reachable after a host restart/token rotation or any later connection that rejects the old static token. |

No source files were edited. Tests, builds, application code, live services,
and live data were not executed or read.

## Snapshot and method

The required pre-audit checks were run:

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  [empty]
```

This was a read-only pass under the exact C14 ownership boundary, using the
schemasmash questions (invalid representable states, duplicate truth, and
amplification) plus the audit-your-codebase coverage discipline. The supplied
exclusions were read first. Existing #2–14 outcomes and the planned #10 native
artifact viewer are not reported.

## Coverage contract

### Whole-file owners inspected

The complete contents of these owned files were inspected:

```text
app/src/lib/api.ts
app/src/lib/endpoint.ts
app/src/lib/history.ts
app/src/lib/pending.ts
app/src/store/reducer.ts
app/src/store/selectors.ts
app/src/store/store.ts
app/src/store/types.ts
app/src/ws/backoff.ts
app/src/ws/client.ts
crates/hirsel-host/src/iroh.rs
crates/hirsel-host/src/protocol.rs
crates/hirsel-host/src/protocol/hello_dedupe.rs
crates/hirsel-host/src/ws.rs
crates/hirsel-proto/src/lib.rs
```

The exact assigned test files were also inspected in full:

```text
app/src/store/reducer.model.test.ts
app/src/store/reducer.processes.test.ts
app/src/store/reducer.views.test.ts
app/src/store/selectors.test.ts
app/src/ws/backoff.test.ts
app/src/ws/client.test.ts
crates/hirsel-host/src/protocol/tests.rs
crates/hirsel-host/tests/iroh_client_flow.rs
crates/hirsel-proto/src/tests.rs
```

### Shared-file definitions inspected

`app/src/protocol.ts` imports, helpers, enclosing plumbing, and every assigned
definition were inspected at lines
`6, 11, 20, 25, 44, 48, 50, 70, 76, 80, 91, 98, 107, 120, 126, 140, 148,
158, 166, 175, 178, 184, 190, 198, 215, 226, 230, 238, 242, 253, 260, 267,
273, 282, 292, 302, 308, 314, 323, 330, 341, 351, 357, 362, 384, 397, 402,
404, 413, 423, 432, 438, 452, 464, 472, 483, 492, 502, 509, 516, 523, 533,
540`. There are no excluded definitions in this file.

`crates/hirsel-proto/src/client.rs` imports, helpers, and the assigned
`HelloAuth` definition at line 37 and `ClientToHost` definition at line 46 were
inspected. `SendMode` (C02), `AgentSlot` (C13), and `PushPlatform` (C25) were
read only as excluded definitions. `crates/hirsel-proto/src/host.rs` imports,
helpers, and the assigned `HostToClient` definition at line 20 were inspected;
there are no excluded definitions in that file. `crates/hirsel-proto/src/lib.rs`
and its re-exports were inspected.

Read-only consumer/conversion context included `app/src/App.tsx`,
`app/src/main.tsx`, the four `hirsel-client-core` source files named by the
spec, host Lash runtime bridge/timeline files, and the listed proto artifact,
chat, models, process, providers, thread, turn, and view modules.

## Finding C14-01 — destructive and stale view broadcast dedupe cache

### Verdict and concrete state

Confirmed reachable. `HelloBroadcastDedupe.views` is intended to hold the last
view projection sent to one connection. The `ViewUpsert` branch removes the
entry and then returns without inserting the new projection. Therefore, after
one upsert the internal state can be:

```text
host active view = V
client view      = V          (if the changed upsert was sent)
dedupe.views[V]  = absent
```

The next identical upsert is treated as new and sent again. A second reachable
sequence is more serious:

```text
hello snapshot:  dedupe.views[v] = A, client has v=A
ViewRemoved(v):  client removes v, dedupe.views still contains A
recreate v=A:   upsert is suppressed as equal, client remains missing v
```

Both sequences are reachable through the existing `show`/`update`/`clear`
view manager APIs and one protocol connection. This is an invalid sent-state
representation and a duplicate-amplification path, not a hypothetical future
view type.

### Evidence at each affected layer

The host's authoritative active collection is updated before the upsert is
published:

`crates/hirsel-host/src/templates/views.rs:105-123`

```rust
active.insert(
    instance_id,
    ActiveView {
        history_id: expected_history.to_owned(),
        view: view.clone(),
        source,
        params,
        patches: Vec::new(),
    },
);
self.publish_upsert(&view);
```

Updates use the same publication path:

`crates/hirsel-host/src/templates/views.rs:169-179`

```rust
active.insert(instance_id.to_string(), record);
self.publish_upsert(&view);
```

Removal deletes the authoritative entry and emits a distinct wire event:

`crates/hirsel-host/src/templates/views.rs:197-204`

```rust
if active.remove(instance_id).is_none() {
    anyhow::bail!("unknown view instance `{instance_id}`");
}
let event = HostToClient::ViewRemoved {
    instance_id: instance_id.to_string(),
};
let _ = self.broadcaster.send(event);
```

The per-connection cache is a separate derived representation initialized from
the hello snapshot:

`crates/hirsel-host/src/protocol/hello_dedupe.rs:4-17`

```rust
pub(super) struct HelloBroadcastDedupe {
    views: HashMap<String, ViewInstance>,
    threads: HashMap<u64, hirsel_proto::Thread>,
}
Self {
    threads: HashMap::new(),
    views: views
        .into_iter()
        .map(|view| (view.instance_id.clone(), view))
        .collect(),
}
```

The defect is the destructive upsert expression and the catch-all removal
behavior:

`crates/hirsel-host/src/protocol/hello_dedupe.rs:38-60`

```rust
HostToClient::ViewUpsert {
    thread_id,
    instance_id,
    placement,
    spec,
} => self.views.remove(instance_id).is_none_or(|snapshot| {
    snapshot.thread_id != *thread_id
        || snapshot.placement != *placement
        || snapshot.spec != *spec
}),
_ => true,
```

The protocol loop invokes this cache for every broadcast, and resets it only
when constructing a fresh snapshot:

`crates/hirsel-host/src/protocol.rs:185-203`

```rust
if !dedupe.should_send(&event) {
    continue;
}
if channel.send(&event).await.is_err() {
    break;
}
```

`crates/hirsel-host/src/protocol.rs:250-265`

```rust
let views = state.views.snapshot().await;
let mut dedupe = HelloBroadcastDedupe::new(views.clone());
dedupe.include_threads(&snapshot.threads);
let hello = HostToClient::HelloOk {
    history_id: snapshot.history_id,
    threads: snapshot.threads,
    processes: state.process_snapshot().await?,
    host_version: host_version(),
    model: state.model_snapshot(),
    subagent_models: Some(state.subagent_model_snapshot()),
    prompts: Some(state.prompt_snapshot()),
    providers: Some(state.provider_roster().await),
    views,
};
```

The Rust wire envelope has the expected two operations and no version field
that could repair a lost cache entry:

`crates/hirsel-proto/src/host.rs:131-139`

```rust
ViewUpsert {
    thread_id: u64,
    instance_id: String,
    placement: String,
    spec: serde_json::Value,
},
ViewRemoved { instance_id: String },
```

The browser mirror preserves the same identity and operation semantics:

`app/src/protocol.ts:481-495`

```ts
export interface ViewUpsertMsg {
  thread_id: number; type: "view_upsert"; instance_id: string;
  placement: ViewPlacement; spec: ViewSpec;
}
export interface ViewRemovedMsg {
  type: "view_removed"; instance_id: string;
}
```

The client dispatches both frames, and the reducer replaces/removes by the same
`instance_id`; this consumer is consistent and exposes the host cache defect:

`app/src/ws/client.ts:391-397`

```ts
case "view_upsert": {
  dispatch({ type: "view_upsert", payload: message });
  break;
}
case "view_removed": {
  dispatch({ type: "view_removed", payload: message });
  break;
}
```

`app/src/store/reducer.ts:10-14`

```ts
case "view_upsert": return { ...state, views: [...state.views.filter(row => row.instance_id !== instance_id), { instance_id, thread_id, placement, spec }] };
case "view_removed": return { ...state, views: state.views.filter(row => row.instance_id !== action.payload.instance_id) };
```

### Duplicate truth and write paths

There is no second authoritative database/table writer. The canonical owner is
`ViewManager.active`; it writes the active view and broadcasts it. The
`HelloBroadcastDedupe.views` map is deliberately a per-connection sent-state
cache. Its divergent writes are local and explicit: `new` inserts snapshot
values, `ViewUpsert` removes without replacing, and `ViewRemoved` does not
remove. No other writer repairs either omission. The fix is therefore to make
the derived cache a complete last-sent projection, not to add another owner or
field to the wire.

### Consumer query

Reproducible bounded query:

```sh
rg -n -S 'ViewUpsert|view_upsert|view_removed|ViewRemoved|ViewInstance' app/src crates/hirsel-host/src crates/hirsel-proto/src | wc -l
```

Result: **71 matching lines**. The matches cover the host owner/publication,
protocol cache and loop, Rust/TypeScript wire definitions, browser dispatch and
reducer, and tests; there is no alternate dedupe implementation.

### Target representation and smallest cutover

Keep the existing wire and application representations unchanged:

- Rust authoritative state remains `BTreeMap<String, ActiveView>` in
  `ViewManager.active`.
- Rust wire remains `HostToClient::ViewUpsert` plus
  `HostToClient::ViewRemoved`.
- The per-connection state remains
  `HashMap<String, ViewInstance>`, explicitly documented and maintained as the
  last-sent view projection.
- TypeScript remains `ViewInstance` keyed by `instance_id`; its reducer already
  replaces/removes by that key.

The smallest credible implementation is only
`crates/hirsel-host/src/protocol/hello_dedupe.rs`: compare against
`self.views.get(instance_id)`, insert the new `ViewInstance` whenever an
upsert is accepted, and handle `ViewRemoved { instance_id }` by removing that
key before returning `true`. No adapter, revision counter, wire migration, or
new boolean is needed. This deletes the invalid “client and host agree but
sent-cache is absent” state and the “client is missing while cache still says
present” state while preserving the existing `instance_id` ownership.

### Risk and validation

Regression/cutover risk is low: the target changes only duplicate suppression
for views. It must continue to suppress an identical upsert while the view is
present, pass a changed upsert, and pass a re-upsert after removal. The map
remains bounded by the connection's current view identities.

Existing tests/fixtures do **not** demonstrate the condition. The websocket
test checks that a view broadcast exists (`crates/hirsel-host/src/ws.rs:230-253`),
the protocol tests exercise Thread dedupe (`crates/hirsel-host/src/protocol/tests.rs:274-286`),
and proto tests round-trip view frames, but no test calls view dedupe twice or
tests remove-then-recreate with the same identity. The browser reducer tests
cover direct upsert/remove behavior, not host dedupe.

Additional validation required, not executed here:

1. Add a focused Rust unit test in `crates/hirsel-host/src/protocol/tests.rs`
   that seeds `HelloBroadcastDedupe` with `v=A`, asserts equal `ViewUpsert` is
   false twice, changed `ViewUpsert` is true once then false, and asserts
   `ViewRemoved(v)` followed by `ViewUpsert(v=A)` is true.
2. Run the focused host protocol test and the existing host websocket protocol
   tests; exercise a real show → clear → show(same `instance_id`, same spec)
   sequence and assert the second upsert is delivered.
3. Run the existing proto round-trip tests to confirm no wire shape changed.

Confidence: **high**. The outcome follows directly from the current map
mutation and the reachable host write/broadcast paths.

## Finding C14-02 — lifetime authentication latch masks re-authentication

### Verdict and concrete state

Confirmed reachable. The browser has two representations of authentication:
the current socket's `authenticated` bit and a lifetime `everAuthed` bit. The
first is reset for every new socket; the second is never reset. After a prior
successful connection and a later network drop, the state before the next
hello is therefore:

```text
authenticated = false       // current socket is awaiting hello_ok
everAuthed    = true        // an earlier socket succeeded
```

If that new socket receives the host's uncorrelated pre-auth `error` frame,
the client takes the runtime-error branch instead of `handleAuthReject`. The
socket then reconnects with the same rejected token indefinitely. The token is
not cleared and the `onAuthReject` callback never returns the app to its auth
gate.

A concrete reachable trigger is: socket 1 authenticates, the host restarts
with a rotated static token, socket 1 drops, socket 2 sends the old token in
hello, and the host rejects it. The same path applies to a later endpoint that
rejects the stored token.

### Evidence at each affected layer

The host defines the pre-auth failure as an uncorrelated `Error` and returns
before sending `hello_ok`:

`crates/hirsel-host/src/protocol.rs:91-116`

```rust
Ok(Some(IncomingFrame::InvalidJson { detail, .. })) => {
    let _ = channel.send(&HostToClient::Error {
        detail: format!("invalid hello: {detail}"),
        client_id: None,
    }).await;
    return;
}
```

The authentication-failure branch has the same uncorrelated shape:

`crates/hirsel-host/src/protocol.rs:104-116`

```rust
let paired_token = match authenticate(&state, auth, &peer).await {
    Ok(token) => token,
    Err(detail) => {
        if let Some(peer) = peer_key {
            tokio::time::sleep(state.auth_throttle.record_failure(peer)).await;
        }
        let _ = channel
            .send(&HostToClient::Error {
                detail,
                client_id: None,
            })
            .await;
        return;
    }
};
```

The canonical Rust wire shape keeps that distinction in the optional
correlation field:

`crates/hirsel-proto/src/host.rs:123-129`

```rust
Error {
    detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
}
```

The TypeScript mirror and protocol documentation say the same thing:

`app/src/protocol.ts:472-478`

```ts
export interface ErrorMsg {
  type: "error";
  detail: string;
  client_id?: string;
}
```

`app/PROTOCOL.md:46`

```text
`upload_blob {client_id,name,mime,data_b64}` returns `blob_ok {client_id,blob}`. `get_blob_url {client_id,blob_id}` returns `blob_url {client_id,blob_id,url,expires_at}` with a short-lived signed relative URL. Upload and retrieval requests are bounded and fail visibly. Authenticated plugin HTTP and `plugin_push {plugin,topic,data}` remain current operational contracts. `error {detail,client_id?}` correlates failures where applicable; a pre-auth failure returns to authentication.
```

The client stores both state copies, resets only the current one on a new
socket, and sets the lifetime one on a successful hello:

`app/src/ws/client.ts:74-88`

```ts
private authenticated = false;
/** Distinguish initial auth errors from later operational failures. */
private everAuthed = false;
```

`app/src/ws/client.ts:294-303`

```ts
const socket = new WebSocket(this.url);
this.socket = socket;
this.authenticated = false;
```

`app/src/ws/client.ts:310-323`

```ts
this.socket = null;
this.authenticated = false;
```

`app/src/ws/client.ts:366-384`

```ts
if (message.type === "hello_ok") {
  this.authenticated = true;
  this.everAuthed = true;
}
```

The classification uses the lifetime bit rather than the current socket phase:

`app/src/ws/client.ts:442-476`

```ts
if (!this.everAuthed && !message.client_id) {
  this.handleAuthReject(message.detail);
  break;
}
```

The later branch instead executes the runtime-error sink:

`app/src/ws/client.ts:468-473`

```ts
} else {
  setProtocolError(message.detail);
}
```

The intended terminal path is already implemented but becomes unreachable in
the later pre-auth-error sequence:

`app/src/ws/client.ts:330-353`

```ts
this.closedByClient = true;
this.clearRequests("Connection closed.");
this.socket?.close();
clearStoredToken();
this.handlers.onAuthReject?.(
  detail && detail.trim().length > 0
    ? detail
    : "Couldn't authenticate — check your token and try again.",
);
```

The app consumer relies on that callback to return to the gate:

`app/src/App.tsx:54-67`

```tsx
const client = startClient(WS_URL, t, {
  onAuthReject: (detail) => {
    setToken(null);
    setAuthError(detail);
  },
});
```

### Duplicate truth and write paths

No external or wire-level duplicate writer was found. The duplicate local
truth is the pair `authenticated`/`everAuthed`. On every `hello_ok`, the
current bit and lifetime bit are set; on socket open/close only the current bit
is reset. That one-sided reset is the divergent write path that permits the
invalid classification state. No host change is required.

### Consumer query

Reproducible bounded query:

```sh
rg -n -S 'everAuthed|authenticated|handleAuthReject|message\.type === "error"' app/src/ws/client.ts app/src/ws/client.test.ts | wc -l
```

Result: **17 matching lines**. These are the complete browser state/routing
sites and the associated auth/reconnect fixtures; there is no separate current
socket phase representation.

### Target representation and smallest cutover

Keep the wire representation unchanged at every boundary:

- Rust remains tagged `HostToClient::Error { detail, client_id: Option<String> }`.
- TypeScript remains `ErrorMsg { type: "error"; detail; client_id? }`.
- The host's pre-auth rejection remains an uncorrelated error followed by
  connection termination.
- `App.tsx` continues to consume `onAuthReject`.

In `app/src/ws/client.ts`, use one current-socket authentication phase. The
smallest cutover is to remove `everAuthed` and classify
`!this.authenticated && !message.client_id` as authentication rejection. An
explicit enum such as `authPhase: "awaiting_hello" | "authenticated"` is an
equivalent clearer representation if the implementation wants to avoid a
boolean; it must be reset when a new socket is installed and set only after
that socket's `hello_ok`.

The minimum affected files are `app/src/ws/client.ts` and
`app/src/ws/client.test.ts`; no protocol migration, adapter, token-format
change, or host/Iroh change is justified. A single current-socket phase cannot
remain latched to a prior connection, so the invalid state no longer causes a
pre-auth rejection to be treated as a runtime error.

### Risk and validation

Regression/cutover risk is low to medium. A plain uncorrelated error before the
current socket's `hello_ok` is an authentication/handshake failure under the
host contract, so routing it to the gate is correct. Existing behavior for a
post-`hello_ok` global error remains the runtime-error path, and a mid-session
close with no error still reconnects. The test should also ensure a correlated
error with `client_id` continues to reject only its pending request.

Existing tests partially demonstrate the boundary but not the defect sequence:

- `app/src/ws/client.test.ts:228-246` proves a first-socket pre-auth error gates.
- `app/src/ws/client.test.ts:248-262` proves a post-auth global error does not
  gate.
- `app/src/ws/client.test.ts:266-286` proves a drop after authentication keeps
  reconnecting when subsequent sockets only close before hello.

No existing fixture performs successful hello → drop → new socket hello →
uncorrelated pre-auth error, so the reported condition is not currently
demonstrated by a test.

Additional validation required, not executed here:

1. Add a focused `app/src/ws/client.test.ts` case that authenticates socket 1,
   closes it, opens the scheduled socket 2, sends an uncorrelated error before
   socket 2's `hello_ok`, and asserts exactly one `onAuthReject`, cleared token,
   no third socket, and preserved host detail.
2. Run the focused test with the repository's `npm test --
   app/src/ws/client.test.ts` command, then run the existing client test file
   without changing its adequate close-only regression.
3. Run the host protocol tests that cover pre-auth error generation and the
   proto round-trip tests to ensure the unchanged wire contract remains
   compatible.

Confidence: **high**. The host's pre-auth error contract, the per-socket reset,
the lifetime-only write, and the wrong branch are all directly visible in the
fixed source.

## Explicit no-findings and deferred areas

- `app/src/lib/api.ts` and `app/src/lib/endpoint.ts`: authenticated REST and
  blob-origin derivation have one source of truth; no C14 representation or
  transport split was found.
- `app/src/lib/history.ts`: history identity is validated and persisted before
  reset handling; the existing different-history invalidation is intentional.
- `app/src/lib/pending.ts`: pending controls are bounded and settle on signal or
  timeout; no unbounded connection state was found.
- `app/src/store/reducer.ts`, `selectors.ts`, `store.ts`, and `types.ts`: the
  direct connection/process/view reducers and selectors use the wire keys and
  terminal states consistently. View direct-reducer behavior is not the
  C14-01 defect; the host sent-state cache is.
- `app/src/ws/backoff.ts` and `backoff.test.ts`: capped exponential delay and
  injected jitter are bounded and covered.
- `crates/hirsel-host/src/iroh.rs` and `crates/hirsel-host/src/ws.rs`: Iroh
  length-delimited frames and WSS text frames enter the same `run_protocol`
  loop; ALPN and frame bounds do not introduce a second envelope or auth
  routing path. The Iroh flow tests cover pairing, device auth, reconnect, and
  invalid/revoked credentials, but not the browser-only `everAuthed` state.
- `crates/hirsel-host/src/protocol.rs`: the common handshake, error correlation,
  snapshot construction, and broadcast loop were inspected. Its interaction
  with the C14-01 cache is reported above; no independent host envelope defect
  was found.
- `crates/hirsel-proto/src/client.rs`, `host.rs`, `lib.rs`, and
  `app/src/protocol.ts`: tagged envelopes, required nullable fields, and
  current snapshots were mirrored without a second C14 wire defect. The loose
  `ViewSpec` is deliberate plugin/view data, not promoted as a finding.
- A read-only cross-layer drift remains between
  `app/src/protocol.ts:42-61`, where `ProcessKind` includes `"subagent"`, and
  `crates/hirsel-proto/src/process.rs:6-10`, where the Rust kind is currently
  only `Monitor`. The current host producer is monitor-only, and resolving the
  stale UI/product surface belongs to adjacent C08 process ownership; no
  reachable C14 wire failure was promoted.
- Blob upload and signed-URL requests are tracked with timeouts and visibly
  fail (`app/src/ws/client.ts:112-166`), matching `app/PROTOCOL.md:46`. Their
  ordinary reconnect replay semantics are not specified as durable Thread
  replay, so this was not promoted over the two confirmed findings. Thread
  replay and lifecycle semantics remain adjacent C02/C21 concerns.
- The unchecked browser `JSON.parse` cast at `app/src/ws/client.ts:306-307`
  is a robustness concern, but no current host producer creates malformed
  frames and it is not an invalid/duplicate representation finding in this
  bounded pass.
- Existing exclusion-map items #2–14 were treated as already tracked; #10's
  native artifact viewer remains explicitly planned and is not a finding.

## Post-report source invariant

Post-write verification was run and returned the required invariant:

```text
git rev-parse HEAD HEAD^{tree}
3ee0621a603659ab0168f565b99012b642415419
a4aac830c45398a66591f2c44b707aaf3cef281b

git status --porcelain
[empty]
```

The report is outside the repository; source cleanliness and the fixed commit
were unchanged.
