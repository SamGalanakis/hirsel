# C21-CLIENT-CORE audit report

Snapshot audited: `3ee0621a603659ab0168f565b99012b642415419`
Expected tree: `a4aac830c45398a66591f2c44b707aaf3cef281b`
Scope: the exact 13 owned files in the dispatch, with adjacent protocol, host,
FFI, Android, and web files read only as consumers. No tests, builds, or
application code were run.

## Findings

### F1 — Generic `thread_action` has no history-bound identity

**Verdict: recommend fix. Confidence: high.** A generic thread action can be
queued from a stale UI/native callback after the client has reset from history
A to history B. If B reuses the numeric thread ID, the host applies the action
to B's thread. This is a reachable write path, not merely a type-theoretic
state: Android, web, FFI, and the native test API all call the history-less
operation.

The owned client API accepts only a numeric ID and immediately emits the
history-less wire command:

> `crates/hirsel-client-core/src/client.rs:317-329`
> ```rust
> pub fn thread_action(
>     &self,
>     thread_id: u64,
>     action: String,
>     data: serde_json::Value,
>     expected_revision: Option<u64>,
> ) {
>     self.queue_frame(ClientToHost::ThreadAction {
>         thread_id,
>         action,
>         data,
>         expected_revision,
>     });
> }
> ```

The client already has the required identity and validation pattern for the
specialized presentation helpers. They require the expected history, current
online state, and matching thread revision before queuing:

> `crates/hirsel-client-core/src/client.rs:406-429`
> ```rust
> let store = self.inner.read_store();
> if store.history_id.as_deref() != Some(expected_history.as_str())
>     || store.connection != crate::ConnectionState::Online
>     || !store
>         .threads
>         .iter()
>         .any(|thread| thread.id == thread_id && thread.revision == expected_revision)
> {
>     return false;
> }
> ...
> self.queue_frame(ClientToHost::ThreadAction {
>     thread_id,
>     action: action.into(),
>     data,
>     expected_revision: Some(expected_revision),
> });
> ```

The reset itself is correct but only protects frames already queued before the
reset. `LocalStore::apply_hello_ok` replaces the store on a history change:

> `crates/hirsel-client-core/src/store.rs:237-265`
> ```rust
> let changed = self
>     .history_id
>     .as_ref()
>     .is_some_and(|old| old != &history_id);
> if changed {
>     ...
>     *self = Self::default();
>     ...
> }
> ```

The transport then clears pending raw frames:

> `crates/hirsel-client-core/src/transport.rs:316-323`
> ```rust
> let history_changed =
>     store.apply_hello_ok(history_id, threads, processes, host_version);
> if history_changed {
>     client
>         .pending_frames
>         .lock()
>         .unwrap_or_else(|e| e.into_inner())
>         .clear();
> }
> ```

But the protocol carries no history at all:

> `crates/hirsel-proto/src/client.rs:95-101`
> ```rust
> ThreadAction {
>     thread_id: u64,
>     action: String,
>     #[serde(default)]
>     data: serde_json::Value,
>     #[serde(default)]
>     expected_revision: Option<u64>,
> },
> ```

The host forwards that incomplete address and resolves the current thread by
numeric ID:

> `crates/hirsel-host/src/protocol.rs:456-464`
> ```rust
> ClientToHost::ThreadAction {
>     thread_id,
>     action,
>     data,
>     expected_revision,
> } => {
>     state
>         .handle_thread_action(thread_id, action, data, expected_revision)
>         .await?;
> }
> ```

> `crates/hirsel-host/src/thread_commands.rs:115-127`
> ```rust
> pub async fn handle_thread_action(
>     &self,
>     id: u64,
>     action: String,
>     data: serde_json::Value,
>     expected_revision: Option<u64>,
> ) -> anyhow::Result<Thread> {
>     let expected_history = self.storage.history_id().await?;
>     let current = self
>         .storage
>         .thread(id)
>         .await?
>         .ok_or_else(|| anyhow::anyhow!("unknown thread: {id}"))?;
> ```

Lifecycle actions then mutate without comparing the caller's history:

> `crates/hirsel-host/src/thread_commands.rs:168-199`
> ```rust
> "archive" => {
>     validate_empty_lifecycle_data(&action, &data)?;
>     self.storage.archive_thread(id, true).await?
> }
> ...
> "unsnooze" => {
>     validate_empty_lifecycle_data(&action, &data)?;
>     self.storage.snooze_thread(id, None).await?
> }
> ```

Generated instrument actions check only the current revision and use the
current host history for the eventual turn, so the numeric ID is still the
only caller address:

> `crates/hirsel-host/src/thread_commands.rs:200-217,224-239`
> ```rust
> anyhow::ensure!(
>     expected_revision == Some(current.revision),
>     "instrument changed; reload the thread before submitting this action"
> );
> ...
> self.submit_addressed_turn(
>     &expected_history,
>     format!("thread-action-{id}-{}-{generated}", current.revision),
>     id,
>     ...
> )
> ```

The omission crosses every active consumer layer:

> `crates/hirsel-client-ffi/src/lib.rs:395-408`
> ```rust
> pub fn thread_action(
>     &self,
>     thread_id: u64,
>     action: String,
>     data_json: String,
>     expected_revision: Option<u64>,
> ) -> Result<(), ClientError> {
>     ...
>     self.core
>         .thread_action(thread_id, action, data, expected_revision);
> }
> ```

> `android/app/src/main/kotlin/dev/hirsel/android/pairing/Connection.kt:85-87`
> ```kotlin
> fun action(threadId: ULong, action: String, data: String = "{}", revision: ULong? = null) {
>     runCatching { client?.threadAction(threadId, action, data, revision) }
> }
> ```

> `android/app/src/main/kotlin/dev/hirsel/android/chat/ThreadInstrument.kt:23-28`
> ```kotlin
> val submit: (String, JSONObject) -> Unit =
>     { action, data -> connection.action(thread.id, action, data.toString(), thread.revision) }
> ```

> `android/app/src/main/kotlin/dev/hirsel/android/chat/ChatScreen.kt:175-181`
> ```kotlin
> ReplyChip(if (thread.settledAt == null) "Settle" else "Reopen") { connection.action(focused, if (thread.settledAt == null) "settle" else "reopen") }
> ```

> `app/src/threads/types.ts:68-74`
> ```ts
> | { type: "thread_action"; thread_id: number; action: string; data: unknown; expected_revision?: number };
> ```

> `app/src/threads/store.ts:118-120`
> ```ts
> export function threadAction(id: number, action: string, data: unknown = {}, expectedRevision?: number): void {
>   ...
>   sendFrame({ type: "thread_action", thread_id: id, action, data, expected_revision: expectedRevision });
> }
> ```

> `app/src/threads/ThreadShell.tsx:102-103`
> ```tsx
> <ThreadInstrument ui={current()?.instrument ?? undefined} onAction={(action, data) => threadAction(props.id, action, data, revision)} />
> ```

For a concrete reachable sequence, a UI displays `(history-a, thread 5)`;
the transport receives `HelloOk(history-b)` with another thread 5; the owned
store resets and clears already-pending frames; then a delayed `archive` or
`settle` callback invokes the existing FFI/Android/web API with only `5`. The
client queues it, the host resolves current history-b/thread-5, and the
lifecycle arms above mutate it. `expected_revision` does not protect the
ordinary lifecycle arms, and equal initial revisions are also possible. The
existing tests deliberately reuse ID 5 across histories, demonstrating this
identity shape: `crates/hirsel-client-core/src/icon_tests.rs:19-29` and
`crates/hirsel-client-core/src/showcase_tests.rs:19-29`.

The bounded consumer search was:

```text
rg -n '\bthread_action\b|\bthreadAction\b|connection\.action\(' crates/hirsel-client-core/src crates/hirsel-client-core/tests/client_flow.rs crates/hirsel-client-ffi/src android/app/src/main/kotlin/dev/hirsel/android app/src/threads app/src/views
```

Result: **39 matches**. This is a reachable cross-layer write path, not a
latent-only API possibility.

**Duplicate-truth check.** No duplicate write path was found: this is a
missing identity component, not two copies of history being updated
inconsistently. The client already keeps one `LocalStore.history_id`; the
target should reuse it rather than add another mutable history field.

**Target representation, per layer.**

- In `hirsel-client-core`, replace the free arguments with an explicit
  `ThreadActionRequest { history_id: String, thread_id: u64, action: String,
  data: serde_json::Value, expected_revision: Option<u64> }`. The public method
  should return `Result<(), ClientError>` (or the existing boolean convention)
  and require `LocalStore.history_id == request.history_id`, `Online`, and a
  matching thread when a revision is supplied before queueing. It should use
  the existing store history, not copy it.
- In `hirsel-proto`, make `ClientToHost::ThreadAction` carry
  `history_id: String` alongside `thread_id`, or use a concrete shared
  `ThreadAddress { history_id: String, thread_id: u64 }` field. The latter is
  preferable if the same address is reused by related thread commands.
- In `hirsel-host`, change `handle_thread_action` to accept the address and
  call the existing history-scope validation before resolving or mutating the
  thread. Every lifecycle and generated-action arm must operate only after
  that validation.
- In FFI, Android, and web, add `history_id` to the action call/frame and
  capture it at action origin. `Connection.action`, the constrained
  `ThreadInstrument`, and web `threadAction` must pass the displayed/current
  tuple. The no-history API must not remain as an accepting overload.

This representation makes `(old history, reused ID)` unrepresentable at the
host mutation boundary and makes stale callbacks fail before a frame is
queued, while retaining one source of truth for the current client history.

**Smallest credible cutover.** Owned files: `crates/hirsel-client-core/src/client.rs`
and `crates/hirsel-client-core/tests/client_flow.rs`; `store.rs` is only
needed if the validation is factored into a store helper. Required adjacent
interfaces are `crates/hirsel-proto/src/client.rs`,
`crates/hirsel-host/src/protocol.rs`, `crates/hirsel-host/src/thread_commands.rs`,
`crates/hirsel-client-ffi/src/lib.rs`,
`android/app/src/main/kotlin/dev/hirsel/android/pairing/Connection.kt`,
`android/app/src/main/kotlin/dev/hirsel/android/chat/ThreadInstrument.kt`,
`android/app/src/main/kotlin/dev/hirsel/android/chat/ChatScreen.kt`,
`app/src/threads/types.ts`, `app/src/threads/store.ts`,
`app/src/threads/ThreadShell.tsx`, and `app/src/threads/actions.ts`.
No database schema or migration is involved.

**Regression and cutover risk.** This is a deliberate wire/API break: all
native, web, FFI, and host peers must move together. Preserve the existing
`expected_revision` semantics for instrument actions, reject stale history
before any host lookup/write, retain the pending-frame clear on reset, and
ensure delayed callbacks report a local rejection rather than silently
mutating the replacement thread.

**Validation required (not run).** Add a client-flow regression that loads
history A/thread 5, resets to history B/thread 5, invokes the stale generic
action, and asserts no outbound frame. Add a protocol/host test that submits
history A/thread 5 against current history B and proves no lifecycle or
generated action mutates B. Update FFI/Android/web compile and wire fixtures,
and retain the existing pre-reset queue test. Existing coverage demonstrates
the adjacent condition but not this one: the queue-reset test at
`crates/hirsel-client-core/tests/client_flow.rs:666-738` only proves already
queued actions are dropped, while the typed navigation test at
`crates/hirsel-client-core/tests/client_flow.rs:1042-1123` proves a history-
bound related target is rejected. No tests were executed for this audit.

### F2 — `ClientConfig` can represent a WebSocket with iroh-only auth

**Verdict: recommend fix. Confidence: high.** The owned configuration exposes
transport selectors, iroh credentials, and auth as independent public fields.
`validate` checks host/ticket presence and the iroh secret but never enforces
the transport/auth relation. A direct Rust caller can therefore construct a
configuration accepted by `Client::new` that always connects over WebSocket
and always sends an auth variant the host rejects.

The independent representation is explicit:

> `crates/hirsel-client-core/src/config.rs:56-67`
> ```rust
> pub struct ClientConfig {
>     pub host: String,
>     pub iroh_ticket: Option<String>,
>     pub iroh_secret_key: Option<String>,
>     pub auth: HelloAuth,
>     pub reconnect: ReconnectPolicy,
> }
> ```

The constructors establish valid combinations, but the public fields allow
callers to bypass them:

> `crates/hirsel-client-core/src/config.rs:69-103`
> ```rust
> pub fn new(host: String, token: String) -> Self { ... HelloAuth::StaticToken(token) ... }
> pub fn new_iroh(ticket: String, device_token: String, iroh_secret_key: String) -> Self { ... }
> pub fn new_iroh_pairing(...) -> Self { ... HelloAuth::PairingCode { code, device_label } ... }
> ```

`validate` does not inspect `auth` in the WebSocket branch:

> `crates/hirsel-client-core/src/config.rs:105-119`
> ```rust
> match self.iroh_ticket.as_deref() {
>     Some(ticket) if ticket.trim().is_empty() => return Err(ConfigError::EmptyIrohTicket),
>     Some(_) => { ... parse_iroh_identity(secret_key)?; }
>     None if self.host.trim().is_empty() => return Err(ConfigError::EmptyHost),
>     None => {}
> }
> self.reconnect.validate()
> ```

Transport selection is made solely from the optional ticket, while auth is
sent independently:

> `crates/hirsel-client-core/src/config.rs:139-157`
> ```rust
> pub(crate) fn transport_target(&self) -> TransportTarget {
>     self.iroh_ticket.as_ref().map_or_else(
>         || TransportTarget::WebSocket(self.websocket_url()),
>         |ticket| TransportTarget::Iroh(ticket.trim().to_owned()),
>     )
> }
>
> pub(crate) enum TransportTarget {
>     WebSocket(String),
>     Iroh(String),
> }
> ```

> `crates/hirsel-client-core/src/client.rs:133-143`
> ```rust
> config.validate()?;
> let auth = config.auth.clone();
> let iroh_secret_key = config.parsed_iroh_secret_key()?;
> ...
> config,
> ...
> auth: RwLock::new(auth),
> iroh_secret_key,
> ```

> `crates/hirsel-client-core/src/transport.rs:40-46,155-163`
> ```rust
> let target = client.config.transport_target();
> let reconnect = client.config.reconnect.clone();
> let iroh_secret_key = client.iroh_secret_key.clone();
> ...
> let auth = client.current_auth();
> let hello = ClientToHost::Hello { auth };
> if let Err(error) = channel.send(&hello).await { ... }
> ```

The protocol intentionally has auth variants with different peer
requirements:

> `crates/hirsel-proto/src/client.rs:35-41`
> ```rust
> pub enum HelloAuth {
>     StaticToken(String),
>     DeviceToken(String),
>     PairingCode { code: String, device_label: String },
> }
> ```

The host accepts device and pairing auth only for iroh and explicitly rejects
them over WebSocket:

> `crates/hirsel-host/src/protocol.rs:282-308`
> ```rust
> (HelloAuth::DeviceToken(token), Peer::Iroh { node_id }) => { ... }
> (HelloAuth::PairingCode { code, device_label }, Peer::Iroh { node_id }) => { ... }
> (HelloAuth::DeviceToken(_), Peer::WebSocket { .. }) => {
>     Err("device-token auth requires iroh".to_string())
> }
> (HelloAuth::PairingCode { .. }, Peer::WebSocket { .. }) => {
>     Err("pairing-code auth requires iroh".to_string())
> }
> ```

Concrete invalid combination:

```rust
ClientConfig {
    host: "localhost:3089".into(),
    iroh_ticket: None,
    iroh_secret_key: None,
    auth: HelloAuth::DeviceToken("issued-device-token".into()),
    reconnect: ReconnectPolicy::default(),
}
```

`validate` accepts it because `host` is non-empty and the `None` ticket branch
does not constrain `auth`; `Client::new` therefore returns `Ok` at
`crates/hirsel-client-core/src/client.rs:133-149`. `transport_target` selects
WebSocket and `run_session` sends `DeviceToken`; the host returns
`"device-token auth requires iroh"`. The same reachable invalid state exists
for `HelloAuth::PairingCode`. This is reachable for any direct Rust caller of
the public struct fields and `Client::new`; the current FFI constructors happen
to select valid combinations:

> `crates/hirsel-client-ffi/src/lib.rs:314-346`
> ```rust
> Self::from_config(core::ClientConfig::new(host, token), observer)
> ...
> core::ClientConfig::new_iroh(ticket, device_token, iroh_secret_key)
> ...
> core::ClientConfig::new_iroh_pairing(ticket, code, device_label, iroh_secret_key)
> ```

There is a second symptom of the same representation: any non-empty malformed
ticket passes `ClientConfig::validate` and is parsed only during connection:

> `crates/hirsel-client-core/src/transport.rs:124-131`
> ```rust
> let ticket = ticket
>     .parse::<EndpointTicket>()
>     .map_err(|error| format!("invalid iroh ticket: {error}"))?;
> ...
> let secret_key = iroh_secret_key
>     .ok_or_else(|| "iroh transport is missing a client secret key".to_string())?;
> ```

The bounded configuration/auth search was:

```text
rg -n 'ClientConfig|new_iroh|new_iroh_pairing|transport_target|HelloAuth::(DeviceToken|PairingCode)|requires iroh' crates/hirsel-client-core/src crates/hirsel-client-ffi/src crates/hirsel-host/src/protocol.rs crates/hirsel-proto/src/client.rs android/app/src/main/kotlin/dev/hirsel/android
```

Result: **40 matches**.

**Duplicate-truth check.** No write path was found that updates one transport
or auth copy without another. `capture_paired_device_token` updates the
current auth and paired-token cache together (`crates/hirsel-client-core/src/client.rs:116-123`).
The defect is that incompatible values are independently representable; the
target should make one validated transport/auth state own them together.

**Target representation, per layer.**

- In `hirsel-client-core`, make `ClientConfig` fields private and represent
  transport as a tagged value, for example
  `TransportConfig::WebSocket { host: String }` or
  `TransportConfig::Iroh { ticket: ValidEndpointTicket, secret_key: SecretKey }`,
  with `AuthConfig::StaticToken(String)` allowed on either transport and
  `AuthConfig::DeviceToken(String)`/`PairingCode { ... }` allowed only inside
  the iroh constructor. Keep reconnect policy in the config, but construct a
  validated policy rather than exposing an unrelated invalid primitive tuple.
- Make the internal transport target carry the complete validated transport
  value, not `TransportTarget::Iroh(String)` plus a separately optional secret.
  `ClientInner`/`run` should capture that one value; ticket parsing should occur
  in the constructor so an invalid ticket cannot produce a reconnect loop.
- Keep `hirsel-proto::HelloAuth` as the wire auth enum. Its variants are the
  correct wire values; the owned configuration boundary is where the transport
  relation must be enforced. No host wire change is required for this finding.
- FFI constructors should convert directly into the tagged core config. They
  can retain their current public signatures while making the core invalid
  combinations impossible; direct Rust struct literals should no longer be
  available.

This deletes the invalid WebSocket/device-token and WebSocket/pairing-code
states, the independent ticket/secret presence state, and late ticket parsing.
It preserves the valid static-token WebSocket path and both existing iroh
pairing/device-token paths.

**Smallest credible cutover.** Owned files: `crates/hirsel-client-core/src/config.rs`
and its config tests; `crates/hirsel-client-core/src/client.rs` and
`src/transport.rs` need only adapt to the tagged validated value. The FFI
constructors at `crates/hirsel-client-ffi/src/lib.rs:314-346` are the adjacent
conversion boundary. `hirsel-proto::HelloAuth` and host authentication need no
schema change. No database schema or migration is involved.

**Regression and cutover risk.** Making fields private and moving ticket
parsing to construction is a Rust API/timing break for direct callers. Preserve
the WebSocket static-token URL behavior, iroh secret-key persistence and pairing
token rollover, and the existing reconnect policy semantics. Add explicit
valid cases for WebSocket/static, iroh/device, and iroh/pairing, plus rejected
WebSocket/device, WebSocket/pairing, empty/malformed ticket, and invalid
secret-key combinations.

**Validation required (not run).** Add config unit tests for every valid tagged
combination and each invalid cross-product; assert invalid configs fail before
`Client::new` returns. Add a constructor/transport test proving the parsed
ticket and secret are carried together. Exercise the existing FFI constructors
and the host auth matrix. No tests were executed for this audit.

## Coverage contract and explicit skips

Every assigned file was read in full. Coverage and disposition:

| Owned area | Exact files | Disposition |
| --- | --- | --- |
| Config, identity, reconnect, exports | `crates/hirsel-client-core/Cargo.toml`, `src/config.rs`, `src/identity.rs`, `src/lib.rs` | F2 in config; identity parsing/round-trip, reconnect validation/delay, dependency/export surface had no separate material finding. |
| Client lifecycle and command API | `src/client.rs`, `src/observer.rs`, `src/transport.rs` | F1 in generic action addressing; connect/disconnect, command ownership, observer lifecycle, and reconnect control had no separate finding. |
| Local store, optimistic send/echo, reset | `src/store.rs` | No separate finding. `ChatEntry` is a deliberate confirmed/pending sum; echo reconciliation uses client ID plus thread ID; reset clears identity-bound state and preserves only recovered draft text. |
| Thread detail, open requests, related snapshots, briefs | `src/store.rs`, `src/client.rs`, `src/transport.rs` | No separate finding. Related history/revision/link guards, request ownership, and stale snapshot rejection are present. The lifecycle observer path at `transport.rs:381-399` can emit after `apply_thread_related` returns false, but no normal host publication path was found that creates a same-history stale event; it is not promoted without a reachable write consequence. |
| Turns, streams, deltas, activities | `src/store.rs`, `src/transport.rs` | No separate finding. Turn/stream ownership, terminal-state rejection, sequence monotonicity, and activity upsert behavior are distinct projections and are covered by existing fixtures. |
| Test fixtures and public documentation | `src/icon_tests.rs`, `src/showcase_tests.rs`, `src/thread_tests.rs`, `tests/client_flow.rs`, `README.md` | Existing tests demonstrate history-bound icon/showcase rejection, reset queue clearing, related identity checks, message echo/retry, and stream guards. The generic post-reset action gap is F1; valid config-only coverage gap is F2. README documents history-bound related operations and observer semantics; no documentation-only finding. |

The larger `LocalStore` vectors (`briefs`, `related_items`, `streams`,
`activities`, `turns`, and snapshots) are intentional read projections with
distinct product dimensions, not duplicate mutable facts shown to have
diverged. The multiple outbound queues were inspected but no explicit ordering
contract was found whose violation is demonstrably reachable; existing message
order and reset tests are not enough to justify a finding. No DDL, database
table, migration, or shared owned definition was assigned to this cluster.

Additional bounded searches used to test these skips:

```text
rg -n 'pending_frames|pending_creates|pending_sends|flush_pending|sent_this_connection|Command::(SendPending|Retry)' crates/hirsel-client-core/src/client.rs crates/hirsel-client-core/src/store.rs crates/hirsel-client-core/src/transport.rs
```

Result: **37 matches**.

```text
rg -n 'opened_threads|history_has_more|briefs|related_revisions|related_items|streams|requests|processes|created_threads' crates/hirsel-client-core/src/store.rs crates/hirsel-client-core/src/transport.rs crates/hirsel-client-ffi/src/lib.rs android/app/src/main/kotlin/dev/hirsel/android/chat/ChatScreen.kt
```

Result: **85 matches**. These counts are inventory evidence, not claims of
duplicate ownership.

The exclusions file was applied: already tracked outcomes and explicitly
planned work were not restated as findings.

## Verification and handoff

Before inspection, the required checks returned:

```text
git rev-parse HEAD HEAD^{tree}
3ee0621a603659ab0168f565b99012b642415419
a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain
(empty)
```

The report was written outside the repository. After writing it, the same
checks returned:

```text
git rev-parse HEAD HEAD^{tree}
3ee0621a603659ab0168f565b99012b642415419
a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain
(empty)
```

No source files were changed; no tests, builds, application execution, or
live-data/config reads were run.

**Handoff:** Fix F1 first: carry and validate `history_id` through generic
thread actions at the client, wire, host, FFI, Android, and web boundaries.
Then make F2's config a private tagged transport/auth representation with
validated ticket material. Evidence is in this report; expected HEAD is
`3ee0621a603659ab0168f565b99012b642415419`, and source must remain clean.
