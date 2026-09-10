# C07-RELATED audit

Date: 2026-09-10
Cluster: Explicit saved URL/Thread targets, canonical identity, receipts and navigation
Audit mode: read-only schemasmash plus audit-your-codebase pass

## Verdict

Recommend one material fix: enforce the existing Related snapshot owner
invariant at the web receiver (and keep the same invariant in the native
receiver). ThreadRelatedItem.thread_id is a copy of the enclosing source
Thread identity. Native client core already rejects a foreign item, but the web
store writes one without checking it, so the same malformed protocol snapshot
produces different client state.

No second candidate clears the materiality bar. URL/title normalization,
target history/access fencing, receipts, revision ordering, reset behavior and
textual navigation all have coherent current writers and focused coverage.

## Snapshot and constraints

The fixed source snapshot was verified before and after inspection:

~~~
HEAD  3ee0621a603659ab0168f565b99012b642415419
TREE  a4aac830c45398a66591f2c44b707aaf3cef281b
STATUS  ## HEAD (no branch), no source changes
~~~

The named spec and /tmp/hirsel-combined-audit/exclusions.md were read first,
along with CONTRIBUTING.md, CLAUDE.md, and both audit skill instructions.
All assigned sources and the named consumer/conversion context were inspected
with read-only git, rg, nl, sed, cat, and text inspection. No tests, builds,
migrations, application execution, live data/config reads, network, provider
calls, commits, or delegation were performed. The report is the only file
written, outside the source checkout.

## Coverage inventory

| Surface | Inspected definitions and behavior | Result |
| --- | --- | --- |
| crates/hirsel-host/src/storage/thread_related.rs | Full file: URL/title normalization, row-to-proto conversion, scoped list, snapshot/revision, add/remove, receipt replay/record, publication snapshot and human/agent mutation wrappers | F-01 boundary evidence; storage writer is coherent |
| crates/hirsel-host/src/storage/thread_related_tests.rs | Full file: canonical URLs, dedupe/title preservation, capacity, typed/scoped targets, receipt replay, reset/stale fencing and publication | Positive and native/storage guards covered; no web malformed-snapshot fixture |
| crates/hirsel-host/src/storage/current.sql:158-173 | thread_related_items and thread_related_receipts DDL, FKs, one-of target check, per-source uniqueness | No DDL defect |
| crates/hirsel-proto/src/thread.rs:1-113 | Imports, enclosing ThreadDetail plumbing (excluded definition), ThreadRelatedItem and ThreadRelatedTarget | F-01 source copy; target destination identity remains semantically distinct |
| Host/proto publication | thread_messages.rs, thread_read.rs, thread_scope.rs, thread_mutations.rs, scoped_tools.rs, tools/threads.rs, proto/{host,client}.rs, exports | Current host paths derive item and envelope from one DB snapshot; no normal mismatch writer |
| Web navigation/state | app/src/threads/types.ts, app/src/related/store.ts, RelatedList.tsx, lib/{thread-ref,thread-url}.ts, threads/ThreadRef.tsx, threads/store.ts, ws/client.ts, related/url/ref tests | F-01 web receiver; history and URL navigation otherwise fenced |
| Native/core/FFI | hirsel-client-core/{src/client.rs,src/store.rs,src/transport.rs,src/thread_tests.rs,tests/client_flow.rs}, hirsel-client-ffi/{src/lib.rs,src/threads.rs} | Native source-owner guard exists; flat snapshot explains why the field is currently useful |

The explicitly excluded ThreadAttention, Thread, ThreadActivity, ThreadTurnState,
ThreadTurn, ThreadBrief and ThreadDetail definitions were read only for
enclosing plumbing. Adjacent C15-WEB-THREADS, C17-WEB-RELATED, C21-CLIENT-CORE
and C25-HOST-OPS ownership was not claimed; their tracked action/history,
reconnect, configuration, lifecycle and operational findings were not
repeated.

## Finding F-01 — web Related snapshots accept an item for a different source Thread

Verdict: recommend fix. Confidence: medium. Materiality: medium.

### Exact representations and conversions

The database has one canonical owner for each saved association:

crates/hirsel-host/src/storage/current.sql:158-168

~~~sql
CREATE TABLE thread_related_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    url TEXT,
    target_thread_id INTEGER REFERENCES threads(id) ON DELETE CASCADE,
    title TEXT,
    created_at TEXT NOT NULL,
    CHECK((url IS NOT NULL AND target_thread_id IS NULL) OR
          (url IS NULL AND target_thread_id IS NOT NULL AND title IS NULL)),
    UNIQUE(thread_id,url),
    UNIQUE(thread_id,target_thread_id)
);
~~~

thread_related.rs:49-59 selects a source row by WHERE thread_id=?1, then
copies the row source into the item:

~~~rust
let target = match url {
    Some(url) => ThreadRelatedTarget::Url { url },
    None => ThreadRelatedTarget::Thread {
        history_id: history_id.clone(),
        thread_id: r.get(3)?,
    },
};
Ok(ThreadRelatedItem {
    id: r.get(0)?,
    thread_id: r.get(1)?,
    target,
    title: r.get(4)?,
    created_at: super::common::parse_ts(&r.get::<_, String>(5)?)?,
})
~~~

The shared proto item repeats this source identity:

crates/hirsel-proto/src/thread.rs:98-113

~~~rust
pub struct ThreadRelatedItem {
    pub id: u64,
    pub thread_id: u64,
    pub target: ThreadRelatedTarget,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
}
~~~

The same item is nested under a source Thread in detail (thread.rs:87-96),
and under an outer source identity in the incremental frame:

crates/hirsel-proto/src/host.rs:71-81

~~~rust
ThreadOpened { client_id: String, detail: crate::ThreadDetail },
ThreadRelatedChanged {
    client_id: Option<String>,
    history_id: String,
    thread_id: u64,
    revision: u64,
    items: Vec<crate::ThreadRelatedItem>,
},
~~~

The host publication path takes all outer values from a fresh ThreadRelated
snapshot (crates/hirsel-host/src/tools/threads.rs:14-27); normal storage writes
therefore keep the copies equal. The internal ThreadRelated snapshot itself
also derives thread_id and revision from the fetched Thread
(thread_related.rs:76-86).

The web state is already grouped by the outer source:

app/src/related/store.ts:6-9

~~~ts
export interface RelatedOrigin {
  readonly historyId: string;
  readonly threadId: number;
}
interface RelatedList {
  items: ThreadRelatedItem[];
  revision: number;
  loaded: boolean;
  loading: boolean;
  error: string | null;
}
export const [relatedState, setRelatedState] =
  createStore<{ lists: Record<number, RelatedList> }>({ lists: {} });
~~~

But receive stores any item list under the supplied outer source without
checking the embedded source:

app/src/related/store.ts:71-85

~~~ts
function receive(threadId: number, revision: number, items: ThreadRelatedItem[]): void {
  const knownRevision = relatedState.lists[threadId]?.revision ?? -1;
  if (revision < knownRevision) return;
  setRelatedState(draft => {
    draft.lists[threadId] = {
      items, revision, loaded: true, loading: false, error: null
    };
  });
}
...
receive(message.thread_id, message.revision, message.items);
...
receive(message.detail.thread.id, message.detail.thread.revision,
        message.detail.related_items);
~~~

The TypeScript protocol is only a compile-time assertion at the WebSocket
boundary: app/src/ws/client.ts:307 does JSON.parse(event.data as string).
A frame with thread_id: 1 and an item with thread_id: 2 is therefore
representable and accepted by the web receiver.

The native receiver has the missing check:

crates/hirsel-client-core/src/store.rs:352-372

~~~rust
if self
    .related_revisions
    .get(&thread_id)
    .is_some_and(|old| *old > revision)
    || items.iter().any(|link| link.thread_id != thread_id)
{
    return false;
}
self.related_revisions.insert(thread_id, revision);
self.related_items.retain(|link| link.thread_id != thread_id);
self.related_items.extend(items);
~~~

Its existing regression explicitly sends a foreign item and expects rejection:
crates/hirsel-client-core/src/thread_tests.rs:469-483, especially
assert!(!store.apply_thread_related("A", 5, 3, vec![link(2, 6)]));.
The native FFI still exposes a flat source-bearing projection
(crates/hirsel-client-ffi/src/lib.rs:160-169 and
crates/hirsel-client-ffi/src/threads.rs:326-344), so the item field is
currently useful there even though the wire envelope also names the source.

### Concrete invalid state and reachability

This JSON is structurally accepted by the current web types:

~~~json
{
  "type": "thread_related_changed",
  "history_id": "history-a",
  "thread_id": 1,
  "revision": 3,
  "client_id": null,
  "items": [{
    "id": 7,
    "thread_id": 2,
    "target": {"kind": "url", "url": "https://example.com/doc"},
    "title": null,
    "created_at": "2026-09-10T00:00:00Z"
  }]
}
~~~

The web state becomes relatedState.lists[1].items = [item], even though the
item declares source Thread 2. RelatedList then renders it as a reference in
Thread 1 (app/src/related/RelatedList.tsx:18-45); removal sends thread_id: 1
with item ID 7 (related/store.ts:68-69), so the UI can show a row that its own
source-scoped remove cannot delete. Native core rejects the same list and
retains its previous source-1 projection. A malformed
thread_opened.detail.related_items follows the same web path at
related/store.ts:81-85.

This is latent at the trusted host/protocol boundary, not an observed database
row: current host list and publication paths select one source and emit both
copies from it. It is nevertheless reachable by the web handler from a
structurally decoded frame because no runtime schema validator or invariant
check exists there. Protocol evolution, a future host conversion, or a
serialization bug can make the two clients disagree.

There is no duplicate-truth write path that updates only one copy in the
current normal host path. The durable source is the SQL thread_id; the item
field and frame/detail envelopes are read-only projections. The defect is the
representable mismatch plus asymmetric receiver behavior, rather than a
demonstrated live divergence.

### Consumer inventory query

Reproducible bounded query:

~~~text
rg -n 'ThreadRelatedItem|thread_related_items|ThreadRelatedChanged|related_items' \
  crates/hirsel-host/src/storage/thread_related.rs \
  crates/hirsel-host/src/storage/current.sql \
  crates/hirsel-host/src/storage/thread_messages.rs \
  crates/hirsel-host/src/storage/thread_read.rs \
  crates/hirsel-host/src/storage/thread_scope.rs \
  crates/hirsel-host/src/tools/threads.rs \
  crates/hirsel-host/src/lash_runtime/scoped_tools.rs \
  crates/hirsel-proto/src/thread.rs \
  crates/hirsel-proto/src/host.rs \
  crates/hirsel-client-core/src/store.rs \
  crates/hirsel-client-ffi/src/lib.rs \
  app/src/threads/types.ts \
  app/src/related/store.ts \
  app/src/related/RelatedList.tsx
~~~

Result: 51 matches. The relevant conversions are the SQL row mapper,
ThreadDetail/ThreadRelatedChanged host frames, web source-keyed store, native
replace_related_items, and flat FFI snapshot. Target destination
ThreadRelatedTarget::Thread.thread_id is a different fact and is not part of
this source-owner equality.

### Proposed representation and smallest change

The smallest safe cutover keeps the existing public item field for the flat
native/FFI snapshot, but makes one shared receiver invariant explicit:

~~~text
current SQL owner: thread_related_items.thread_id
detail/event owner: enclosing Thread.id or outer thread_id
item owner: item.thread_id, required equal to that enclosing owner
Thread target: target.history_id must equal the current frame/detail history
~~~

In app/src/related/store.ts, make receive return a boolean and reject the
whole snapshot when any item has a different thread_id (and, for a typed
Thread target, a different history_id); do not update the list or settle a
correlated mutation from a rejected snapshot. Pass the current history to both
thread_related_changed and thread_opened calls. In
hirsel-client-core::replace_related_items, retain the existing source check and
add the target-history check; keep the SQL row and host mapper as the canonical
source. Update app/PROTOCOL.md to state these equality invariants. The FFI
flat item remains unchanged, avoiding an unnecessary native API break.

A deeper future simplification could make the wire item source-free and group
native snapshots by thread_id, but that is a wider API change and is not needed
to remove the invalid state found here.

This target prevents a malformed item from entering either client projection
and keeps source ownership in one enclosing context at the web boundary.
Valid current snapshots are unchanged. The cutover risk is low/medium:
receiver behavior changes only for invalid frames, but a rejected correlated
frame needs a visible protocol-error/timeout policy and all protocol fixtures
must preserve the current owner.

### Existing and additional validation

Existing tests demonstrate storage canonicalization, dedupe, capacity, typed
target history checks, receipt replay/no resurrection, reset fencing and native
foreign-item rejection: crates/hirsel-host/src/storage/thread_related_tests.rs:28-431
and crates/hirsel-client-core/src/thread_tests.rs:469-483. They do not send a
foreign item through app/src/related/store.ts; no live values were read and no
tests were executed for this audit.

Required validation after implementation (not run here):

1. Add web related-store cases for a foreign thread_id in both incremental and
   correlated detail snapshots; assert the source list and revision remain
   unchanged and a correlated mutation does not resolve as success.
2. Add web/native cases for a current-history mismatch in a typed Thread target,
   while retaining the existing valid same-history target case.
3. Run the focused web related-store tests, native core related-store tests,
   proto/FFI compile checks and the repository's prescribed full checks.

## Explicit skips and no-finding areas

- URL canonicalization is centralized in thread_related.rs:16-40; it rejects
  non-HTTP(S), credentials, controls, malformed authority and overlong values.
  Title normalization is bounded and control-free at lines 41-48. The storage
  tests exercise these boundaries and exact URL dedupe, so no second URL
  identity finding is promoted.
- thread_related_items one-of URL/Thread shape, per-source uniqueness and
  target/source FKs are coherent for the current mutation API. Target history
  is validated in normalize and current history is re-derived in list; no
  ordinary Thread delete path exists (the only DELETE FROM threads is
  whole-history reset in storage.rs:89-143), so a hypothetical
  cascade/revision bug is skipped.
- thread_related_receipts is transactionally replayed by exact client_id+payload
  and cleared on reset. Existing tests cover replay, changed payload rejection,
  cross-thread reuse and no resurrection; no second receipt owner or retention
  requirement is established.
- ThreadRelated's top-level thread_id/revision alongside its nested Thread are
  an internal mutation/publication adapter. Publication re-reads the current
  snapshot, and no independent writer was found; this is not promoted
  separately from F-01.
- Native history/revision ordering and typed navigation guards are present in
  store.rs, client.rs, web related/store.ts, thread-url.ts and the listed tests.
  Textual #id parsing and same-origin portable URLs preserve current-history
  semantics; no material navigation representation defect was established.
- Independent Thread attention, lifecycle, read, pin, execution, artifact and
  message dimensions are intentional product boundaries. Existing exclusions
  and adjacent worker outcomes were not duplicated.

## Final source check

After the report write, the source checkout still matched the fixed snapshot:

~~~
HEAD  3ee0621a603659ab0168f565b99012b642415419
TREE  a4aac830c45398a66591f2c44b707aaf3cef281b
STATUS  ## HEAD (no branch), no source changes
~~~
