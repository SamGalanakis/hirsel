# hirsel-client-core

Shared Rust transport and state for Hirsel clients. Durable Threads own messages,
turns and activity. Citations (`mentions`) navigate to other Threads and never
change message ownership. Thread 0 is the globally aware Orchestrator.

The core preserves complete Thread instruments and independent lifecycle,
attention and read state. Open responses are request-correlated and filtered by
message ownership; reconnect refreshes opened histories. Message echoes reconcile
by client ID and Thread ID, including offline retries and attachment IDs. Streams
use durable turn IDs and sequence numbers to reject late or duplicate deltas.

Native commands create/open Threads, send owned messages, submit revision-bound
instrument actions, settle/reopen/read/snooze/archive, and stop an addressed turn.
UniFFI exposes the same state without interpreting instrument semantics. Android
renders the constrained text, metadata, form and choice catalog; embedded custom
views explicitly require the web app. Blob uploading remains a separate transport
capability; received attachment metadata and outbound attachment IDs are retained.

Thread actions return a client request ID and carry the caller-captured history and
Thread. The host acknowledges an applied action with that same ID and address, and
echoes the ID on failure. Native lifecycle events expose successful acknowledgements;
protocol errors retain their request ID so UI shells can display a failure only in
the action's owning context. `ThreadOpened` exposes the successful open request ID
to native shells as well. Android retains bounded pending ownership for create,
open, Related and action requests, then removes it on exact success, failure,
timeout or history reset. Late correlated errors are ignored; only errors without
a request ID remain global.

Explicitly saved URL and Thread references are exposed as `ClientSnapshot.related_items`,
separate from canonical artifact references. Opening or reconnecting loads the
complete per-Thread list independently of message pagination; live snapshots
replace that list and reject foreign histories and older Related revisions, including
late detail responses. Metadata revisions never gate Related snapshots: metadata3
followed by Related2 advances Related1 without changing metadata3. Equal Related
revisions remain valid.

`add_thread_related(history_id, thread_id, target, title)` and
`remove_thread_related(history_id, thread_id, item_id)` return a client request ID.
Callers must capture the history ID together with the addressed Thread before
any delayed action; the API sends that explicit history unchanged so the host
can reject stale commands after reset. `ThreadRelatedChanged` observer events
acknowledge successful requests, and `ProtocolError.client_id` correlates host
errors. Agent changes have no client request ID. These operations do not send
messages, start work, or change artifact identity.

`ThreadRelatedTarget` is either a URL or the `(history_id, thread_id)` tuple.
Thread targets do not change hierarchy or access. Native in-app navigation requires
an online snapshot, exact history match and an existing target, using the same guard
as notifications. Canonical textual references are ordinary relative Markdown
links such as `[Thread #0](/t/0?history=550e8400-e29b-41d4-a716-446655440000)`.
The client has no configured reachable web base and does not construct absolute
share URLs, re-pair from references or register a new native link scheme.
