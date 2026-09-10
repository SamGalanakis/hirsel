# C03 — conversation proposals

Coordinator verdict: skip both as proposed. Worker ../workers/C03-CONVERSATION.md. Independently reopened message/receipt writer, reverse-correlation readers, mentions storage admission, web resolver, native projection and execution-context expansion.

C03-01 contains a decisive factual error: it says rusqlite query_row errors on multiple rows. Cargo.lock pins rusqlite0.37.0; local dependency source lib.rs647–648 explicitly says rows after the first are ignored, and query_one is the different API that rejects multiple rows. Current chat.rs uses query_row. No production writer creates two receipt IDs for one message: it allocates a new row and inserts one mapping atomically, while retry reads the existing mapping. The schema permits a latent ambiguous mapping, but the claimed transcript failure is false and the witness requires direct SQL/future writer. Do not add a schema constraint based on that claim.

C03-02: raw/native API callers may submit duplicate valid mention IDs; web composer already deduplicates. thread_queue.rs140–142 emits their short #id labels only, not duplicated conversation contents or reads. Thus the worker's amplification language overstates the consequence. JSON payload length drives linear validation/rendering; the proposed relational table does not itself bound row count, so it also fails its claimed unbounded-input target. No ordinary shipped composer path or material runtime failure was found. A host dedup helper could be a tiny future cleanup if desired, but a proto value type, SQL association table, FFI/core/write/read rewrite and schema cutover are disproportionate. Preserve accepted citation snapshots and explicit-reference semantics.

No tests/application code executed. Dependency inspection was read-only local source, not a network/provider read. These deciding corrections must be visible to the fresh materiality reviewer rather than silently copying the worker recommendations.

Coordinator reran the exact recorded consumer queries without shell evaluation: matching-line counts [121, 46].
