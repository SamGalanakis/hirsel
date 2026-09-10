# C25-HOST-OPS audit

## Verdict

Two material source findings survive the supplied exclusions. The first is a
high-confidence production bootstrap defect: WSS never supplies peer
identity, so the host's per-peer authentication delay is bypassed. The second
is a medium-confidence lifecycle defect: the history reset deletes the
durable push registry and no client path restores it.

No source file was edited. No tests, builds, application code, live data,
configuration, services, processes, or network calls were executed/read.

## Snapshot and method

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  [empty]
```

The complete exclusions file and the complete C25 specification were read
before promotion. This pass used both the schemasmash lens (invalid
representations, duplicate truth, and amplification) and the
audit-your-codebase lens (coverage, ownership, and simplification). Existing
root issues #16 and #18–29 and original #2–14 were not re-reported. F11/#29
editable-provider required fields and F07/#25 blob relocation were explicitly
skipped as assigned elsewhere.

## Findings

### C25-01 — High: production WSS bypasses the host auth throttle

**Verdict and reachability.** `main.rs` serves the router directly, while the
WSS handler accepts an optional `ConnectInfo<SocketAddr>`:

```text
crates/hirsel-host/src/main.rs:34-39,62
  let app = router_from_state(state.clone());
  axum::serve(listener, app).await?;
crates/hirsel-host/src/ws.rs:20-27
  peer: Option<ConnectInfo<SocketAddr>>,
  ... peer.map(|peer| peer.0.to_string())
```

The required Axum `into_make_service_with_connect_info::<SocketAddr>()`
maker is absent from the production server and every inspected WSS test
server. Consequently the WSS `Peer::WebSocket` address is `None`:
`crates/hirsel-host/src/protocol.rs:51-63`. The authentication error path only
records a delay when the peer key exists (`protocol.rs:104-121`), so every
wrong WSS hello skips `AuthThrottle` (`auth.rs:30-71`). Iroh peers do carry a
key, making this a transport-specific bypass. The query

```text
rg -n 'into_make_service_with_connect_info|axum::serve' \
  crates/hirsel-host/src/main.rs crates/hirsel-host/src/ws.rs crates/hirsel-host/tests
```

returned **4** `axum::serve`/maker matches and zero maker matches. The
`ConnectInfo|Peer::WebSocket|record_failure|record_success` query returned
**15** matches, tracing the missing value to the throttle branch.

**Representation/target.** Make the production `main.rs` serve
`app.into_make_service_with_connect_info::<SocketAddr>()`; make the WSS
handler's peer extractor required (with test servers using the same maker),
and keep `Peer::WebSocket { addr: SocketAddr }` as the concrete source of the
key. The current `HashMap<String, FailureRecord>` also never evicts a failed
peer except on success (`auth.rs:30-38,41-71`); this is a secondary amplifier
of the same throttle state, so the target should use a bounded peer-keyed
cache/expiry policy. No duplicate truth writer was found: the throttle has
one in-memory owner; the defect is absent bootstrap identity. This is
distinct from C14's browser `everAuthed` finding.

**Smallest scope, risk, and validation.** Primary files are `main.rs` and
`auth.rs`; `ws.rs`, `protocol.rs`, and their server fixtures are consumer
coordination. The maker changes test server construction and reverse-proxy
address semantics; bounded eviction changes only abuse-state retention.
Add an HTTP test with repeated bad WSS hellos asserting increasing delay and a
bounded-state test for unique peers, then retain the existing direct throttle
unit test. Inspection found no fixture exercising the real WSS throttle.
Confidence: **high**.

### C25-02 — Medium: history reset deletes durable push registrations

**Verdict and reachability.** `push_tokens` is a machine-level durable table
with no history identity:

```text
crates/hirsel-host/src/storage/current.sql:79-84
  CREATE TABLE push_tokens (token TEXT PRIMARY KEY, platform TEXT NOT NULL, ...);
crates/hirsel-host/src/storage/push_tokens.rs:13-36
  INSERT ... ON CONFLICT(token) DO UPDATE ...;
crates/hirsel-host/src/storage.rs:89-143,120
  ... DELETE FROM push_tokens;
```

Authenticated `RegisterPushToken` reaches that upsert through
`crates/hirsel-host/src/protocol.rs:582-587`. `/debug/reset` calls
`reset_history` (`debug.rs:270-273`), whose runtime lane calls storage reset
before projection reset (`lash_runtime/thread_lanes.rs:229-263`). Push
delivery later enumerates the table (`push.rs:308-347`). Thus a registered
token row is present, owner reset commits, and subsequent needs-owner delivery
sees an empty registry. The client registration was already flushed from its
queue (`crates/hirsel-client-core/src/client.rs:438-455`,
`transport.rs:248-262`); history changes clear pending frames rather than
re-registering (`transport.rs:309-324`). This is a reachable loss after reset,
not merely a restart concern.

The query

```text
rg -n 'push_tokens|register_push_token|RegisterPushToken|DELETE FROM push_tokens' \
  crates/hirsel-host/src/storage.rs crates/hirsel-host/src/storage/push_tokens.rs \
  crates/hirsel-host/src/protocol.rs crates/hirsel-client-core/src/client.rs \
  crates/hirsel-client-core/src/transport.rs crates/hirsel-host/src/storage/current.sql
```

returned **16** matches; the reset-to-sender query over reset/runtime/test
files returned **7**. No duplicate writer divergence was found: the client
queue is a transient transport, while SQLite is the sole durable registry.

**Representation/target.** Leave `push_tokens` out of history reset; retain
the existing global token/platform/timestamp table beside durable
`device_tokens`. If an operator needs a total wipe, expose that as a separate
explicit operation. This requires no schema migration. Existing tests prove
upsert, unregister, and reopen persistence
(`storage/push_tokens/tests.rs:5-68`) but no reset survival or post-reset
delivery fixture. Add a reset test that changes history while retaining a
registered token, plus a sender test after reset. Confidence: **medium**;
the behavior is definite, while product intent for `/debug/reset` is not
documented as wiping push subscriptions. ADR0016 describes history/session/
projection reset and does not name push tokens; ADR0011 describes durable,
revocable device credentials.

## Coverage and explicit skips

The complete owned files were inspected: `auth.rs`, `bin/hirsel-pair.rs`,
`debug.rs`, `health.rs`, `lib.rs`, `main.rs`, `push.rs`, `storage.rs`,
`storage/common.rs`, `storage/devices.rs`, `storage/meta.rs`,
`storage/push_tokens.rs`, `storage/schema.rs`, `deploy/hirsel-host.service`,
`storage/devices/tests.rs`, `storage/push_tokens/tests.rs`,
`storage/schema/tests.rs`, and host `src/tests.rs`. In `storage/current.sql`,
the assigned `push_tokens`, `device_tokens`, and `meta` definitions and
enclosing imports/plumbing were inspected; every table listed as owned by
C01–C08, C13, C23, C04, C06, C05, C02, C03, or C07 was excluded as instructed.
`crates/hirsel-proto/src/client.rs:29` `PushPlatform` was checked with its
conversions.

Read-only consumer context covered attachments, blob routes, boot/config and
provider stores, Iroh, model/prompt selection, protocol/hello dedupe,
provider detection/roster, skills, sub-agent models, and WSS. Pairing code
expiry/single-use, device token pin/revoke/persistence, schema validation and
SQLite pragmas, health thresholds, service hardening, push payload/retry
logic, and broadcast/runtime projection ownership had no independent
material finding. The device-label value returned while pairing is
deliberately ignored in favor of the app-supplied label (existing protocol
fixture proves that intent), so it was not promoted. Inline blob behavior,
provider required fields, malformed monitor regex, shell timeout stderr,
latest-activity display, and all other exclusion items were skipped.

Final source check after writing this report:

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  [empty]
```
