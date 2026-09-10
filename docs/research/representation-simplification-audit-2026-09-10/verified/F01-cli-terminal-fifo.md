# F01 — Terminal CLI request remains the head of its Thread FIFO

Tracked implementation: [GitHub #16](https://github.com/SamGalanakis/hirsel/issues/16), root-owned separate worktree. Root independently verified the same mechanism.

Verdict: recommend. Priority: high. Confidence: high from complete reachable source path, no execution/live rows inspected. Authoritative owner C02-TURN-LIFECYCLE. Independently verified by accountable subplanner against feature HEAD `3ee0621a603659ab0168f565b99012b642415419`, tree `a4aac830c45398a66591f2c44b707aaf3cef281b`.

## Concrete failure

A CLI turn starts, then the host restarts before completion. Startup marks the turn interrupted and leaves its request row. The request query still returns it first for that Thread. The admission loop claims that Thread's FIFO position, then rejects the interrupted CLI turn without retiring its request. Every later request to that Thread is skipped on every poll. This does not need provider failure or a malformed database; ordinary host restart is enough.

## Evidence and affected representations

- `crates/hirsel-host/src/storage/current.sql:13–16`: `thread_requests` stores `client_id`, opaque JSON `payload`, generated `thread_id` and `report_triggered`; no structural turn identity or turn-state eligibility. `thread_turns` separately owns `state` and `finished_at` at lines 17–24.
- `crates/hirsel-host/src/lash_runtime/runtime.rs:104–107`: startup invokes `interrupt_unfinished_thread_turns`, publishes the changed turns, then starts the poller.
- `crates/hirsel-host/src/storage/thread_activity.rs:187–199`: selects running turns, invokes `finish(...Interrupted...)`, commits. `finish` at line 296 onward updates `thread_turns` and reports upward; it does not remove corresponding requests.
- `crates/hirsel-host/src/storage/thread_requests.rs:39`: `SELECT r.client_id,r.payload FROM thread_requests r JOIN threads t ON t.id=r.thread_id WHERE r.report_triggered=0 OR (...) ORDER BY r.id`. Eligibility has no join to turn state.
- `crates/hirsel-host/src/lash_runtime/thread_lanes.rs:165–176`: `if !seen.insert(id) || self.cli.lock().await.contains_key(&id) { continue; }`, followed by `if turn.state != hirsel_proto::ThreadTurnState::Queued { continue; }` in the CLI branch.
- Normal completion differs: `storage/thread_completion.rs:109` deletes request rows for the turn. Startup uses the lower-level `finish` path instead. Lash's `thread_queue.rs:91–108` explicitly cancels a terminal turn's pending Lash input and removes its request; the CLI branch never enters this cleanup path.
- Canonical wire `hirsel-proto/src/thread.rs::ThreadTurnState` already represents Interrupted correctly. Web/native receive terminal state correctly; no wire/client type change is needed for the narrow fix.

This is duplicate authority: physical request-row presence means admission-pending to the query, while the durable turn state says execution is terminal. The exact producer writing one without the other is startup interruption. The invalid combination is reachable, not merely nullable-field speculation.

## Consumer blast radius

Reproducible query: `rg -n 'pending_thread_requests\(|interrupt_unfinished_thread_turns\(|remove_thread_request\(|DELETE FROM thread_requests' crates/hirsel-host/src`.

The important semantic consumers are the Thread registry poller, Lash admission, Lash unowned-input reconciliation and scripted recovery; the reproducible query returns 45 sites including definitions and tests (full output in F01-consumers.txt). One canonical storage/admission fix should cover CLI restart without creating alternate backend-specific truth.

## Target and narrow scope

Make queued/admittable work a projection of the durable turn lifecycle, not request existence alone. Preserve one required accepted turn identity in the stored request representation, and derive scheduler eligibility from that turn's state. If completed requests must remain temporarily for Lash input cancellation, expose that cleanup ownership separately from admission eligibility; do not let a retained cleanup receipt occupy FIFO admission. Rust OwnerTurn/SQL request reading are implementation boundaries; existing wire/client terminal enums stay unchanged.

Smallest credible scope: `storage/thread_requests.rs`, `lash_runtime/thread_lanes.rs`, and the existing restart/admission tests. If consolidating terminal ownership into storage, include `thread_activity.rs`/`thread_completion.rs` and remove superseded cleanup branches only after checking Lash cancellation receipt requirements. A schema change is not necessary for the immediate behavior fix; a required request turn FK would be a separate evidence-driven representation tightening, not an excuse for a broader rewrite.

## Risks and validation required

Do not restart interrupted delegated work (ADR0004). Preserve queued later inputs, upward reports, report-triggered visibility gating and strict per-Thread FIFO. Do not delete a Lash ownership receipt before cancelling its pending input, because that could orphan an input.

Required regression: hermetic accepted CLI A plus queued B in same Thread; simulate persisted running A and fresh runtime startup; A becomes Interrupted exactly once and is never relaunched; B becomes admitted; another Thread still progresses. Add a case with terminal retained request preceding a newer request; preserve existing Lash interrupted-input cancellation and queued-cancellation tests. No tests executed by audit.

Existing coverage inspected: `storage/threads_tests.rs` explicitly removes a stored request after turn completion; existing `thread_recovery_tests.rs` covers Lash recovery/cancellation, not this CLI head-of-line case. Existing #2–6 driver settlement/delivery outcomes do not own durable Thread FIFO restart admission. Fresh C02 worker independently confirmed this mechanism and completed its bounded coverage. Final fresh materiality review remains pending; root reports a separate implemented regression fix with passing Rust/static/hooks.
