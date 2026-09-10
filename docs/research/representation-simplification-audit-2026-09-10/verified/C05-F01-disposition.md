# C05-F01 disposition — skip as low materiality

Worker correctly identifies a redundant immutable child_thread_id beside child_turn_id in thread_delegations. I reopened current.sql:133–136 and thread_delegation.rs's sole insert and both replay readers; the current writer obtains the child turn from enqueue(child) and inserts both references within the same transaction. No one-copy-only update, independently mutable identity, existing branching complexity or reachable divergence exists. Thread-to-turn ownership is immutable after creation.

Removing the field would exchange one inserted scalar for joins in both replay queries and require an exact-schema cutover. The worker's40-site count includes command selectors and activity provenance that would not change; actual representation consumers are the one insert and two replay queries. This is a true latent schema redundancy but does not meet the materially useful simplification threshold for a repository-wide recommendation. Current deliberate atomically owned projection is coherent; do not migrate merely because direct SQL could fabricate an inconsistent row.

No accepted new finding from C05. Full coverage and other explicit skips remain in workers/C05-DELEGATION-SCOPE.md. Preserve this candidate/disposition for fresh materiality review. No tests or live rows read; unchanged source3ee/treea4aac830.
