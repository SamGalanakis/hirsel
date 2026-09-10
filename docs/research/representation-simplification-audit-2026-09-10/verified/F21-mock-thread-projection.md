# F21 — mock create replay erases current Thread status

Recommend, low priority, high confidence. C27 worker F1. Coordinator reopened makeThread, summary, broadcast, create_thread receipt replay, turn lifecycle and web equal-revision upsert. Re-ran status-field consumer query: 23 matching lines.

Mock makeThread stores empty derived status alongside base fields. Broadcast derives the actual summary, but first/replayed thread_created sends the raw record. After a turn runs/completes, retrying the accepted create ID returns stale empty running/terminal/activity fields at the same revision; the web accepts that revision. This is a supported mock protocol operation and invalidates development/contract evidence, not a claimed production Rust defect.

Target one mock base Thread record and one summary projection for every wire emission, including initial/replayed creation. Keep immutable create request fingerprint plus resulting Thread ID in its existing test-world requests map; regenerate the response from current record. Do not add a generic fixture repository/type framework or alter product wire semantics. Existing artifact replay already regenerates its summary. Scope mock-server.mjs and post-turn duplicate-create contract regressions, including running/completed and reconnect summary agreement. Audit ran no tests. Coordinate parent history-contract changes to this fixture after their handoff.

Independent materiality qualification: the requests map has no source-enforced bound. Do not claim bounded storage or introduce eviction that breaks idempotency. Add rename-then-replay to ensure immutable original create input, rather than current mutable title, controls the receipt fingerprint. This is fixture correctness, not a production Rust defect.
