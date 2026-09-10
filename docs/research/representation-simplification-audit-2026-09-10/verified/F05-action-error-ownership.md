# F05 — Navigation action failures lose their owning Thread

Recommend; medium priority/high confidence. Owner C15-WEB-THREADS. Full worker evidence: ../workers/C15-WEB-THREADS.md F1. Independently reopened ThreadClientMessage/threadAction, host error client_id extraction/dispatch, error reducer, ThreadError, navigation and shell consumers. Exact consumer query reproduced34 matches.

Trigger: focused conversation A; invoke a connected action from navigation row B; host rejects B's action (stale icon revision, invalid operation, unknown Thread). ThreadAction carries no client_id. The uncorrelated error becomes `{operation:"request",detail}` with no threadId, and ThreadError intentionally renders that global error under A and in navigation. The UI cannot associate failure with B's originating action. Disconnected action failure already carries threadId, so the current single-Thread/offline fixtures do not cover this.

Target: correlated transient action identity retaining captured history, Thread and action. Require client_id on the action frame, return a targeted success result, echo the ID on failure, and settle the precise request on success/error/timeout/reset. Extend the existing bounded web request owner/map rather than introduce a parallel map if possible. Render action errors only for their owning Thread; genuine global protocol errors remain global. No durable action table or auto-replay policy is needed.

Scope: web thread request/error types/store/UI and corresponding Rust/wire/FFI conversions. This overlaps F02/#19's ThreadAction addressing contract; coordinate the same wire cutover or dependent implementation, while retaining distinct acceptance criteria (F02 prevents wrong-history writes, F05 attributes results correctly).

Regression: A focused/B action failure, two concurrent B actions, success settling only its request, genuine global error unchanged, reset/late result cannot attach to reused history IDs. Existing request/error/ThreadError tests are the closest fixtures. No tests executed by audit; source3ee/treea4aac830 unchanged.

Known C15 findings from root fresh feature review are not new: stale same-history merge overwrites terminal and tool-summary IDs are lost (#13), child pin projection (#12). They remain tracked/inflight under root implementation.
