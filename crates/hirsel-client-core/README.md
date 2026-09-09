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
