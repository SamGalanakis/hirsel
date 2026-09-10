# C03-CONVERSATION audit report

## Handoff verdict

Recommend two fixes, both high confidence:

1. Add reverse uniqueness to client_messages.msg_id (C03-01). The
   application projects one optional client id with scalar queries, but the
   schema permits two client ids for one message. This is a latent
   reader-breaking state.
2. Replace the free-form JSON mention list with a validated ordered-unique
   representation and a normalized association table (C03-02). Duplicate
   mentions are reachable through native, FFI, debug, or raw protocol senders
   and are amplified into execution context.

No other candidate survived the tracked-outcome, intentional-dimension, and
cross-cluster checks. This report is read-only audit evidence; no source,
database, configuration, process, or test state was changed.

## Snapshot and method

The required pre-audit commands were run in /workspace/code/hirsel-audit-c03:

~~~text
$ git rev-parse HEAD HEAD^{tree}
3ee0621a603659ab0168f565b99012b642415419
a4aac830c45398a66591f2c44b707aaf3cef281b

$ git status --porcelain
(empty)
~~~

I read /tmp/hirsel-combined-audit/exclusions.md, the complete four
whole-file owners, all six shared definitions named by the dispatch, and the
listed protocol, web, client-core, runtime, and test consumers. Reads were
bounded to local source and reports. No application code was executed, and no
tests, build, migration, live database, or external issue operation was run.

The audit used both required lenses:

- representation validity: whether the persisted and wire shapes can express
  states their readers assume are impossible;
- ownership/control flow: whether one fact has an avoidable second owner or
  is repeatedly amplified by a downstream consumer.

## Coverage and explicit no-finding areas

| Assigned surface | Inspected scope | Result |
| --- | --- | --- |
| crates/hirsel-host/src/storage/thread_messages.rs | Owner-message idempotency, owner insertion, anchor and mention validation, artifact and attachment links, request/turn persistence, agent chat insertion, thread detail, and materialized replies | C03-01 and C03-02 evidence; no additional finding |
| crates/hirsel-host/src/text.rs | Entire file: short_label and its whitespace/truncation behavior | No conversation, message, ownership, or tool-summary state; skipped |
| crates/hirsel-host/src/storage/chat/tests.rs | All three tests: client-id idempotency, tool-summary persistence, and deletion of client/attachment joins | Demonstrates positive paths; lacks the two negative invariant tests described below |
| crates/hirsel-host/src/storage/chat.rs | All chat readers, client-id lookup, deletion, row conversion, author conversion, attachment/message enrichment, and pagination helpers | C03-01 reader evidence; C03-02 read boundary; no separate finding |
| crates/hirsel-host/src/storage/current.sql:31 | chat_messages table | C03-02 storage evidence; no separate finding about body, ref, or tool-call columns |
| crates/hirsel-host/src/storage/current.sql:41 | client_messages table | C03-01 schema evidence |
| crates/hirsel-host/src/storage/current.sql:125 | chat_messages_thread index | Correctly supports the thread/id access pattern; no finding |
| crates/hirsel-proto/src/chat.rs:8 | ChatAuthor | Two explicit serialized author variants are intentional; no finding |
| crates/hirsel-proto/src/chat.rs:22 | ChatMessage, including client correlation, mentions, ref, attachments, and tool calls | C03-01/C03-02 conversion evidence; no separate protocol-only finding |
| crates/hirsel-proto/src/chat.rs:43 | ToolCallSummary | Name/ok summary is intentionally lossy activity output; no finding |

The consumer pass covered the exact files in the dispatch, including
Composer.tsx, ThreadRefPicker.tsx, Timeline.tsx, chat input/attachment
helpers, ThreadMessages.tsx, conversation.ts, client-core client/store,
host runtime bridges/timeline, the current schema, and thread protocol types.
The additional targeted pass covered the native FFI/debug ingress, host
commands, and mention expansion because they were needed to prove reachability.

### Cross-cluster skips

- C02 turn lifecycle: its accepted finding is the already tracked restart
  FIFO/child-preference issue; it does not alter message ownership or mention
  representation.
- C04 blobs: attachment/blob identity and retention remain in its domain. The
  C03 deletion read only checked that message joins are cleaned; it does not
  claim blob ownership.
- C07 related and C25 host ops: no independent message/mention defect was
  found in their adjacent surfaces.
- C15 web threads: exact-ID conversation joins and the already tracked
  activity/tool-summary outcomes were not reopened.
- C21 client-core: pending/confirmed reconciliation is a consumer of
  client_id; the missing reverse database constraint is owned here, while
  generic action-state concerns remain skipped.
- Exclusions #2–14 and #10 were not reported as new findings. Thread pin,
  settlement, attention, visibility, read, execution, citation, and artifact
  dimensions were treated as intentionally independent where the source says
  so.

## Finding C03-01 — client_messages is not one-to-one with a chat message

### Verdict

Recommend. Severity: high for integrity and reader availability if the state
is ever written; reachability in the current normal host path is latent, not
demonstrated. Confidence: high.

### Exact evidence

The shared schema gives client_id a primary key but does not constrain the
reverse direction:

crates/hirsel-host/src/storage/current.sql:41-44

~~~sql
CREATE TABLE client_messages (
    client_id TEXT PRIMARY KEY,
    msg_id INTEGER NOT NULL REFERENCES chat_messages(id)
);
~~~

The owner writer first creates the chat row and then writes exactly one
correlation row in the same transaction:

crates/hirsel-host/src/storage/thread_messages.rs:178-183

~~~rust
tx.execute("INSERT INTO chat_messages(author,body,ref,ts,thread_id,mentions) VALUES('owner',?1,?2,?3,?4,?5)",params![body,anchor,chrono::Utc::now().to_rfc3339(),thread_id,serde_json::to_string(mentions)?])?;
let id = tx.last_insert_rowid() as u64;
tx.execute(
    "INSERT INTO client_messages(client_id,msg_id) VALUES(?1,?2)",
    params![client_id, id],
)?;
~~~

The retry lookup is keyed only by client_id:

crates/hirsel-host/src/storage/thread_messages.rs:96-104

~~~rust
if let Some(id) = tx
    .query_row(
        "SELECT msg_id FROM client_messages WHERE client_id=?1",
        [client_id],
        |r| r.get::<_, u64>(0),
    )
    .optional()?
{
    let message = get_chat_message(&tx, id)?;
~~~

The reader treats the reverse relationship as scalar, not as a collection:

crates/hirsel-host/src/storage/chat.rs:129-145

~~~rust
let mut message = conn.query_row(
    "
    SELECT id, author, body, ref, ts, tool_calls, thread_id, mentions
    FROM chat_messages
    WHERE id = ?1
    ",
    params![id],
    chat_message_from_row,
)?;
message.client_id = conn
    .query_row(
        "SELECT client_id FROM client_messages WHERE msg_id=?1",
        [id],
        |r| r.get(0),
    )
    .optional()?;
~~~

The same scalar assumption is used for every batch-loaded message:

crates/hirsel-host/src/storage/chat.rs:154-165

~~~rust
for message in messages {
    message.client_id = conn
        .query_row(
            "SELECT client_id FROM client_messages WHERE msg_id=?1",
            [message.id],
            |r| r.get(0),
        )
        .optional()?;
~~~

The wire model also says that a message has at most one optional correlation
id:

crates/hirsel-proto/src/chat.rs:21-30

~~~rust
pub struct ChatMessage {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub thread_id: u64,
    #[serde(default)]
    pub mentions: Vec<u64>,
~~~

Web reconciliation consumes that scalar as an acknowledgement key:

app/src/threads/store.ts:158-165

~~~typescript
if (message.author === "owner" && message.client_id) acknowledgeMessage(message.client_id);
if (threadState.removedMessageIds[message.id]) return;
const threadId = message.thread_id;
setThreadState(draft => { draft["histories"][threadId] = { ...prior, messages: mergeById(prior.messages, [message]) }; });
~~~

### Concrete invalid state and reachability

This state satisfies the current DDL:

~~~text
chat_messages: id = 7, author = 'owner', ...
client_messages: ('client-a', 7)
                 ('client-b', 7)
~~~

Each client_id is unique, and each msg_id references an existing message,
so neither constraint rejects it. A get_chat_message, all_chat,
recent_chat, or thread_detail hydration that reaches message 7 executes a
scalar query_row; SQLite/rusqlite returns a multiple-row error instead of an
optional client id. This makes a representable association corrupt the chat
read path, not merely produce an ambiguous acknowledgement.

No normal production writer was found that can currently create the exact
duplicate mapping: append_thread_owner_record allocates a new chat id and
inserts the mapping atomically, while a reused client id takes the existing
row path. The state is therefore latent through direct SQL, a future writer,
or any non-atomic/manual repair path. That is sufficient to report because the
schema and readers disagree about cardinality.

### Duplicate-truth check

No duplicate truth write path was found. ChatMessage.client_id is not stored
in chat_messages; it is derived from client_messages. The writer and reader
have one side-table owner. The defect is missing reverse cardinality,
not two independently updated copies of the same field.

### Reproducible consumer census

The targeted query used to trace the correlation field was:

~~~sh
rg -n -H 'client_messages|message_id_for_client_id|client_id' \
  crates/hirsel-host/src/storage/current.sql \
  crates/hirsel-host/src/storage/chat.rs \
  crates/hirsel-host/src/storage/thread_messages.rs \
  crates/hirsel-host/src/storage.rs \
  crates/hirsel-host/src/thread_commands.rs \
  crates/hirsel-host/src/lib.rs \
  crates/hirsel-client-core/src/client.rs \
  crates/hirsel-client-core/src/store.rs \
  app/src/protocol.ts app/src/threads/store.ts \
  app/src/threads/ThreadMessages.tsx app/src/threads/conversation.ts
~~~

Observed result: 121 matching lines in 10 files. The file set was:

~~~text
app/src/protocol.ts
app/src/threads/store.ts
crates/hirsel-client-core/src/client.rs
crates/hirsel-client-core/src/store.rs
crates/hirsel-host/src/lib.rs
crates/hirsel-host/src/storage.rs
crates/hirsel-host/src/storage/chat.rs
crates/hirsel-host/src/storage/current.sql
crates/hirsel-host/src/storage/thread_messages.rs
crates/hirsel-host/src/thread_commands.rs
~~~

The file list above is the observed rg -l result. The command also included
app/src/threads/ThreadMessages.tsx and app/src/threads/conversation.ts, which
had no matching token but were read as dispatch-listed consumers.

### Smallest credible target

Change only the shared table cardinality and add a focused storage regression:

crates/hirsel-host/src/storage/current.sql

~~~sql
CREATE TABLE client_messages (
    client_id TEXT PRIMARY KEY,
    msg_id INTEGER NOT NULL UNIQUE REFERENCES chat_messages(id)
);
~~~

At the host/proto/client layers, keep the existing exact state:

- chat_messages remains the sole message row.
- client_messages remains the sole owner of the optional
  client_id -> msg_id correlation.
- ChatMessage.client_id remains Option<String>, because agent messages
  intentionally have no client correlation.
- message_id_for_client_id, retry lookup, and readers remain scalar.

The UNIQUE(msg_id) constraint makes the state space match those interfaces:
one client id can identify one owner message, and one owner message can have
zero or one client id. It does not add a second client_id column or move
ownership into ChatMessage.

### Regression, cutover, and validation

Current current.sql is the direct schema source for this snapshot; no
migration or live database was inspected. A clean cutover must use this
constraint for fresh/current-schema databases. If deployed databases exist,
implementation must separately preflight duplicate msg_id rows and reconcile
them before applying the constraint; this report does not claim that any such
rows exist.

Required validation, not run here:

- a storage test proving two distinct client ids cannot insert the same
  message id, and that the rejected transaction does not leave a partial row;
- the existing idempotent retry test still returns the first message and
  false on the second call;
- all_chat, recent_chat, thread_detail, and chat_message still hydrate owner
  and agent messages, including an owner message with no mapping;
- deletion still removes the mapping and the message while retaining the
  independently owned blob;
- schema validation confirms the current database contains the reverse
  unique constraint.

Existing evidence demonstrates only the positive condition:
crates/hirsel-host/src/storage/chat/tests.rs:7-54 proves one client id
retries to one message; :100-162 proves deletion of that one mapping. No
fixture or test demonstrates two client ids for one message, and no test was
executed in this audit.

## Finding C03-02 — mentions are an unbounded, duplicate-permitting list

### Verdict

Recommend. Severity: high for accepted-message semantics and model-context
quality; duplicate input is reachable from non-web senders. Confidence: high.

### Exact evidence at each affected layer

The shared chat row stores mentions as unconstrained JSON text:

crates/hirsel-host/src/storage/current.sql:31-40

~~~sql
CREATE TABLE chat_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    ref INTEGER NULL,
    ts TEXT NOT NULL,
    tool_calls TEXT NOT NULL DEFAULT '[]',
    thread_id INTEGER NOT NULL REFERENCES threads(id),
    mentions TEXT NOT NULL DEFAULT '[]'
);
~~~

The owner acceptance API receives a raw slice. It verifies only that each
mentioned Thread exists; it does not impose uniqueness or a cardinality bound:

crates/hirsel-host/src/storage/thread_messages.rs:134-143

~~~rust
if let Some(anchor) = anchor {
    anyhow::ensure!(
        get_chat_message(&tx, anchor)?.thread_id == thread_id,
        "reply belongs to another thread"
    );
}
for mention in mentions {
    threads::get(&tx, *mention)
        .map_err(|error| anyhow::anyhow!("unknown mentioned Thread #{mention}: {error}"))?;
}
~~~

The same raw list is serialized into the accepted chat row:

crates/hirsel-host/src/storage/thread_messages.rs:177-178

~~~rust
super::blobs::validate_blob_ids(&tx, attachments)?;
tx.execute("INSERT INTO chat_messages(author,body,ref,ts,thread_id,mentions) VALUES('owner',?1,?2,?3,?4,?5)",params![body,anchor,chrono::Utc::now().to_rfc3339(),thread_id,serde_json::to_string(mentions)?])?;
~~~

The host-to-wire conversion deserializes that text directly into Vec<u64>:

crates/hirsel-host/src/storage/chat.rs:175-194

~~~rust
pub(super) fn chat_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    let author: String = row.get(1)?;
    let ts: String = row.get(4)?;
    let tool_calls: String = row.get(5)?;
    Ok(ChatMessage {
        artifact_ids: Vec::new(),
        client_id: None,
        thread_id: row.get(6)?,
        mentions: serde_json::from_str(&row.get::<_, String>(7)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(7, Type::Text, Box::new(e)))?,
        id: row.get(0)?,
~~~

The shared protocol and native request shapes also accept an arbitrary raw
Vec:

crates/hirsel-proto/src/chat.rs:21-30

~~~rust
pub struct ChatMessage {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub thread_id: u64,
    #[serde(default)]
    pub mentions: Vec<u64>,
~~~

crates/hirsel-proto/src/client.rs:83-94

~~~rust
SendThreadMessage {
    client_id: String,
    thread_id: u64,
    body: String,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default)]
    mentions: Vec<u64>,
    #[serde(default)]
    mode: SendMode,
    artifact_ids: Vec<u64>,
},
~~~

The browser picker happens to canonicalize its own derived list:

app/src/lib/thread-ref.ts:122-135

~~~typescript
/** Resolve every ref in a composed body to a live Thread id — the outgoing
 * send_thread_message.mentions. Deduped, order-preserving, and silent about refs that
 * name nothing in the field: an unresolvable #99 stays plain text and is never
 * sent as a mention. */
export function resolveMentionIds(text: string, threads: RefTarget[]): number[] {
  const known = new Set(threads.map((thread) => thread.id));
  const ids: number[] = [];
  const seen = new Set<number>();
  for (const ref of scanRefs(text)) {
    if (!known.has(ref.id) || seen.has(ref.id)) continue;
    seen.add(ref.id);
    ids.push(ref.id);
  }
  return ids;
}
~~~

But the exported send/pending path accepts caller-provided mentions unchanged:

app/src/threads/store.ts:144-150

~~~typescript
export function sendThreadMessage(threadId: number, body: string, mode: SendMode, attachments: Blob[], mentions: number[], artifactIds: number[]): void {
  if (!threadState.ready) throw new Error("Reconnect before sending to a Thread.");
  const references = [...new Set(artifactIds)].sort((a,b) => a-b);
  if (references.length > 16 || references.some(id => !Number.isSafeInteger(id) || id < 0)) throw new Error("A message supports at most 16 valid artifact references.");
  const pending: PendingMessage = { clientId: crypto.randomUUID(), threadId, body, attachments, mentions, artifactIds: references, mode, failed: false };
  setThreadState(draft => { draft["pending"] = (rows => [...rows, pending])(draft["pending"]); });
  transmitMessage(pending);
}
~~~

The composer derives unique mentions from body text, but that is a UI
convention rather than an authority boundary:

app/src/components/chat/Composer.tsx:90-97,132-136

~~~typescript
// the composed text stays the only record of what was
// cited, so mentions is re-derived from the body at send time.
const picker = createThreadRefPicker({
  getEl: () => textRef,
  value,
  setValue,
  threads: () => props.threads ?? [],
});

// The body IS the mention list: every #id still standing in the text at
// send time, resolved against the live field.
props.onSend(body, mode, blobs, resolveMentionIds(body, props.threads ?? []), artifactId === undefined ? [] : [artifactId]);
~~~

Native and debug ingress preserve or forward raw Vec values:

crates/hirsel-client-core/src/client.rs:15-35

~~~rust
pub struct SendThreadMessageRequest {
    pub thread_id: u64,
    pub attachments: Vec<String>,
    pub body: String,
    pub mentions: Vec<u64>,
    pub artifact_ids: Vec<u64>,
}
~~~

crates/hirsel-client-ffi/src/lib.rs:377-392

~~~rust
pub fn send_thread_message(
    &self,
    thread_id: u64,
    body: String,
    attachments: Vec<String>,
    mentions: Vec<u64>,
    artifact_ids: Vec<u64>,
) -> SendReceipt {
    let mut request = core::SendThreadMessageRequest::new(thread_id, body);
    request.thread_id = thread_id;
    request.attachments = attachments;
    request.mentions = mentions;
~~~

crates/hirsel-host/src/debug.rs:81-94

~~~rust
struct OwnerMessageRequest {
    artifact_ids: Vec<u64>,
    #[serde(default)]
    client_id: Option<String>,
    body: String,
    thread_id: u64,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default)]
    mentions: Vec<u64>,
~~~

crates/hirsel-host/src/debug.rs:344-360

~~~rust
async fn owner_message(
    State(state): State<AppState>,
    Json(request): Json<OwnerMessageRequest>,
) -> Result<Json<OwnerMessageResponse>, DebugError> {
    let client_id = request
        .client_id
        .unwrap_or_else(|| format!("debug-{}", Uuid::new_v4()));
    let submission = state
        .submit_thread_message(
            client_id,
            request.thread_id,
            request.body,
            request.attachments,
            request.mentions,
~~~

Finally, the accepted list is expanded without deduplication into model input:

crates/hirsel-host/src/lash_runtime/thread_queue.rs:138-143

~~~rust
if let Some(message_id) = turn.message_id
    && let Some(message) = self.tools.storage().chat_message(message_id).await?
    && !message.mentions.is_empty()
{
    turn.body.push_str(&format!("\n[Explicitly referenced Threads: {}. References do not change the owning Thread; use threads.read for their context.]",message.mentions.iter().map(|id|format!("#{id}")).collect::<Vec<_>>().join(", ")));
}
~~~

### Concrete invalid state and reachability

For an existing Thread 42, this accepted message is representable and
reachable:

~~~text
body = "Please inspect #42"
mentions = [42, 42]
~~~

The web composer normally produces [42], but the public browser store method,
native client-core request, FFI method, debug JSON endpoint, and raw
SendThreadMessage protocol can carry [42,42]. The host existence loop accepts
both occurrences and the JSON column stores both. There is no maximum length,
so [42,42,...] is also representable. The queue then emits #42 twice (or
arbitrarily many times) into the turn body. The persisted message's
semantically resolved citation set and the execution context consequently
depend on transport entry point rather than one invariant.

Unknown ids are rejected by the host, so this finding is specifically about
duplicate and unbounded valid ids, not the existing unknown-reference guard.
It is reachable through the listed non-web paths; no live data was inspected.

### Duplicate-truth check

No duplicate truth write path was found. The body is the human-authored
content, while mentions is the resolved accepted-target snapshot. The
composer explicitly says the body is the source for deriving mentions, and
thread_messages.rs captures the accepted values atomically so recovery does
not reread edited skills or text. Those are distinct semantics, and the
stored snapshot is deliberate. The defect is that the snapshot has no
ordered-unique invariant and is amplified without normalization, not that
two writers update one fact independently.

### Reproducible consumer census

The targeted query used to trace mention representation and amplification was:

~~~sh
rg -n -H 'mentions|resolveMentionIds|Explicitly referenced Threads' \
  app/src/lib/thread-ref.ts app/src/components/chat/Composer.tsx \
  app/src/threads/store.ts app/src/threads/ThreadMessages.tsx \
  app/src/threads/types.ts app/src/protocol.ts \
  crates/hirsel-proto/src/chat.rs crates/hirsel-proto/src/client.rs \
  crates/hirsel-client-core/src/client.rs crates/hirsel-client-core/src/store.rs \
  crates/hirsel-host/src/protocol.rs crates/hirsel-host/src/thread_commands.rs \
  crates/hirsel-host/src/storage/chat.rs \
  crates/hirsel-host/src/storage/thread_messages.rs \
  crates/hirsel-host/src/lash_runtime/thread_queue.rs \
  crates/hirsel-host/src/debug.rs
~~~

Observed result: 46 matching lines in 15 files. The matching file set was:

~~~text
app/src/components/chat/Composer.tsx
app/src/lib/thread-ref.ts
app/src/protocol.ts
app/src/threads/store.ts
app/src/threads/types.ts
crates/hirsel-client-core/src/client.rs
crates/hirsel-client-core/src/store.rs
crates/hirsel-host/src/debug.rs
crates/hirsel-host/src/lash_runtime/thread_queue.rs
crates/hirsel-host/src/protocol.rs
crates/hirsel-host/src/storage/chat.rs
crates/hirsel-host/src/storage/thread_messages.rs
crates/hirsel-host/src/thread_commands.rs
crates/hirsel-proto/src/chat.rs
crates/hirsel-proto/src/client.rs
~~~

The command includes every direct mention consumer needed for this finding;
the remaining dispatch-listed chat and runtime files were read and had no
additional mention behavior.

### Smallest credible target

Use one canonical internal value and one normalized durable association while
preserving the existing JSON array wire shape.

At the protocol/domain layer, add an ordered-unique MentionIds value in
hirsel-proto:

~~~rust
#[serde(transparent)]
pub struct MentionIds(Vec<u64>);
~~~

Its constructor and serde conversion retain first occurrence order and
discard later duplicates; empty is valid. Its only way to expose iteration or
serialization is the canonical ordered sequence. Use MentionIds for:

- ChatMessage.mentions;
- ClientToHost::SendThreadMessage.mentions;
- hirsel-client-core SendThreadMessageRequest.mentions,
  PendingSend.mentions, and ConfirmedMessage.mentions;
- host owner-submission and accepted-message internals.

Keep the external JSON and FFI parameter shape as arrays so this is not a
wire-format or generated-binding rename. The FFI, debug, and raw protocol
boundaries convert through MentionIds immediately. A duplicate input therefore
has compatibility-preserving canonical output [42], never a duplicate local,
durable, or execution value. The browser resolver already has this
first-occurrence behavior; sendThreadMessage must apply the same constructor
to direct callers.

In current.sql, remove the JSON mentions column from chat_messages and make
the accepted citation association relational:

~~~sql
CREATE TABLE message_mentions (
    message_id INTEGER NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position >= 0),
    mentioned_thread_id INTEGER NOT NULL REFERENCES threads(id),
    PRIMARY KEY (message_id, position),
    UNIQUE (message_id, mentioned_thread_id)
);
~~~

The source thread is already owned by chat_messages.thread_id and is not
duplicated in this table. message_id plus position owns the ordered list;
message_id plus mentioned_thread_id makes a duplicate target impossible.
Foreign keys keep both the source message and cited Thread live. Empty
mentions means no rows.

In thread_messages.rs, canonicalize once before existence validation, insert
one message_mentions row per position in the same transaction as the chat
row, and leave append_thread_chat's agent tool-summary path with zero mention
rows. In chat.rs, remove JSON-column parsing and hydrate MentionIds with
mentioned_thread_id ordered by position for get_chat_message, all_chat,
recent_chat, and thread_detail. The existing delete transaction gets cascade
cleanup from the new foreign key; an explicit delete is acceptable if the
schema keeps manual cleanup conventions. thread_queue consumes the canonical
projection, so context contains each target once in accepted order.

This target removes the invalid state at every durable and internal layer:
the value type canonicalizes transport input, the host retains the same
accepted snapshot across retries, and the table cannot store duplicate or
unbounded JSON list entries. It keeps body text as content and mentions as
the intentionally captured resolution snapshot. It also avoids duplicating
the source thread id.

### Smallest credible affected files and interfaces

The minimum coherent implementation set is:

- crates/hirsel-proto/src/chat.rs and client.rs: MentionIds and the two
  serialized message/request fields;
- crates/hirsel-client-core/src/client.rs and store.rs: request, pending,
  confirmed-message state, and pending-to-wire conversion;
- crates/hirsel-client-ffi/src/lib.rs: array-to-MentionIds conversion while
  retaining the generated external array shape;
- crates/hirsel-host/src/storage/current.sql, storage/thread_messages.rs,
  storage/chat.rs, and lash_runtime/thread_queue.rs: schema, acceptance,
  projection, and model-context consumption;
- crates/hirsel-host/src/protocol.rs and debug.rs: boundary conversion;
- app/src/protocol.ts, app/src/threads/store.ts, and
  app/src/components/chat/Composer.tsx: wire typing and direct-caller
  normalization;
- focused host/client/web tests adjacent to those paths.

No change is required to ChatAuthor, ToolCallSummary, ref ownership, or the
ConversationEntry exact-ID/timestamp join.

### Regression, cutover, and validation

The current schema is a direct current-schema source in this snapshot; no
migration or live database was inspected. Removing the JSON column and
introducing message_mentions therefore requires a deliberate current-schema
cutover. If deployed databases exist, implementation must transform existing
JSON in first-seen order, dedupe it, validate target Thread ids, and only then
drop the old column; this report does not claim that any existing row is
malformed. The normalized table also changes query shape and requires checking
that deletion and restart/replay remain atomic.

Required validation, not run here:

- MentionIds unit tests for empty input, [3,3,2] -> [3,2], first-seen order,
  serde round-trip, and malformed/non-integer wire values;
- host storage tests proving duplicate native/raw/debug input produces one
  accepted target, order [3,2] is preserved, unknown ids still fail, and
  the retry returns the same canonical message;
- direct schema/query tests proving there is no JSON mentions column and
  message_mentions rejects duplicate (message_id, mentioned_thread_id) and
  duplicate positions;
- all_chat, recent_chat, chat_message, and thread_detail hydration tests for
  empty, one, and several mentions;
- deletion/restart tests proving association cleanup and replay do not create
  duplicate rows;
- queue/context tests proving each accepted target is rendered once;
- client-core, FFI, raw protocol, debug endpoint, and browser direct-caller
  tests proving the same canonical array crosses every ingress;
- existing web resolver tests remain green and preserve first-seen order.

Existing evidence demonstrates only the safe browser behavior:
app/src/lib/thread-ref.test.ts:96-101 covers ordered deduplication, and
crates/hirsel-host/src/storage/threads_tests.rs:50-109 covers a single
validated mention in an owner message. The host chat tests contain no
duplicate-mention fixture; no test was executed in this audit.

## Additional inspected leads intentionally skipped

These were read far enough to decide, but are not recommendations:

- ToolCallSummary is a deliberate compact outcome projection. The agent
  append path persists the supplied name/ok summaries and the UI renders them
  only for non-owner messages. Losing full tool-event identity is either
  tracked in the adjacent web/activity work or outside this cluster; no new
  C03 defect was established.
- append_thread_chat accepts ChatAuthor::Owner even though it has no
  client_messages row. The shipped tool caller uses Agent, and the only
  Owner use found was test/fixture-shaped. A broad author capability/API
  redesign without a reachable production write path was not promoted.
- ref is a same-thread message anchor, not a citation ownership field.
  thread_messages.rs validates the anchor's thread, and conversation.ts
  joins final turns by exact message id. No guessed timestamp or mention
  ownership was found.
- ConversationEntry ordering uses exact IDs plus timestamps for independent
  activity placement. This is an intentional presentation ordering rule, not
  a duplicate message/activity owner.
- Artifact ids are canonicalized and bounded in the owner path. Their
  existence, association, and blob semantics belong to the artifact/blob
  clusters; they were not reopened as C03 findings.
- text.rs contains only short_label. It has no durable or wire state and no
  useful simplification connected to this cluster.
- client_id is intentionally absent from the chat_messages row and optional
  on ChatMessage for agent messages. C03-01 preserves that side-table
  ownership; moving it into every message would create, not remove,
  denormalization.

## Audit log and constraints honored

Read-only commands used included:

- git rev-parse HEAD HEAD^{tree} and git status --porcelain before and after;
- rg --files to establish the repository surface;
- bounded rg -n symbol queries for client_messages, client_id, mentions,
  resolveMentionIds, tool_calls, ref, ChatAuthor, ChatMessage, and
  ToolCallSummary;
- nl -ba and sed on every assigned file, each shared definition, the listed
  consumers, and the focused tests;
- read-only inspection of the assigned exclusions and neighboring worker
  reports for deduplication.

No source edit, test, build, install, migration, commit, push, issue write,
live-data/config read, provider call, process action, or session action was
performed. The only intended write is this assigned report outside the
workspace. Counts in the two finding sections are the observed results of the
exact commands shown there, not estimates.

## Final verification and handoff (under 300 words)

The required post-audit commands were run after the report write:

~~~text
$ git rev-parse HEAD HEAD^{tree}
3ee0621a603659ab0168f565b99012b642415419
a4aac830c45398a66591f2c44b707aaf3cef281b

$ git status --porcelain
(empty)
~~~

The workspace source is unchanged at the expected commit and tree. No tests
were run; validation requirements are listed under each finding.

Evidence path: /tmp/hirsel-combined-audit/workers/C03-CONVERSATION.md.
Verdict: recommend exactly two fixes. C03-01 is the smaller schema-only
cardinality correction for a latent scalar-reader failure. C03-02 is the
broader reachable ordered-unique mention invariant and normalized storage
correction. Both are high confidence. The complete source quotes, concrete
states, consumer counts, cutover risks, skips, and required validation are
above. HEAD and tree match the expected snapshot, and git status is empty.

First fix: C03-02, because duplicate mentions are reachable from native/raw senders and amplify every accepted citation into Agent context.
