# Host terminal delivery and startup handoff

Implemented for [issue #6](https://github.com/SamGalanakis/hirsel/issues/6) and the host handoff portion of [issue #4](https://github.com/SamGalanakis/hirsel/issues/4). Implementation worktree: `/workspace/code/hirsel-terminal-delivery`, based on snapshot `160b6de`. No real providers or existing sessions were used by these tests.

## Delivery invariant

Receiving a terminal event no longer acknowledges its handling. The [receiver](../../../crates/hirsel-host/src/tools.rs) suppresses a process only after explicit acknowledgement. The [runtime bridge](../../../crates/hirsel-host/src/lash_runtime/bridges.rs) keeps one delivery future per process, independently of other completions. An append failure retries the same request and replay key; delays start at 100 ms and double to a 3.2-second cap. Retries continue until success or cancellation; they never restart delegated work.

After append succeeds, direct wake delivery retries using the receipt already obtained, without appending another terminal. A successful direct enqueue or accepted fork dispatch permits acknowledgement and removes the pending future. In-flight duplicates are suppressed separately from acknowledged results. Aborting the bridge or closing its input drops its pending appends/backoff futures without acknowledging unsuccessful handling; retained events remain available to a replacement subscriber.

Fork dispatch retains its existing meaning: acceptance into the asynchronous triage dispatcher, not durable completion of the fork. This change does not redesign that subsystem or add a provider restart policy.

## Required wake draft fields

The actual wake test exposed another defect in the existing direct-delivery helper. It supplied a replay source key but omitted the structural process source that pinned Lash `10af7f4` requires in `QueuedWorkBatchDraft::validate_process_wake_source`. The store therefore rejected the draft even after an injected transient failure was removed.

The final implementation adds the public `.with_process_wake_source(process_id, sequence)` and `.with_authority(wake.authority)` builders. It preserves the existing source key and delivery policy. Lash's internal `process_wake_batch_draft` helper is crate-private in this revision and is not called by Hirsel.

## Startup ownership

[Host startup](../../../crates/hirsel-host/src/tools/subagents.rs) acquires a rollback guard immediately after the driver returns a handle. It subscribes before inserting a host record, then keeps ownership through insertion, persistence and pump installation. A handoff error retires the acquired handle. An inserted record becomes failed history rather than remaining Running. Caller cancellation invokes the same cleanup in a separate task so cancellation does not interrupt retirement/persistence. Cleanup failures are logged; the original startup failure remains the returned error. Successful pump installation disarms the guard without an intervening await.

## Validation

`cargo test -p hirsel-host terminal_ -- --nocapture` passed: **16 passed, 0 failed**, including seven new tests in [terminal_delivery_tests.rs](../../../crates/hirsel-host/src/lash_runtime/terminal_delivery_tests.rs) and [subagent_handoff_tests.rs](../../../crates/hirsel-host/src/tools/subagent_handoff_tests.rs):

1. Receiver redelivery before acknowledgement and duplicate suppression afterward.
2. Injected append failure, then successful SQLite append with a lost receipt, followed by retry: one durable terminal and a settled runtime wait.
3. One failing delivery does not block another process; abort preserves the unhandled result for replay.
4. Direct wake-store failure plus duplicate publication: one terminal append, two wake-store attempts, one queued wake.
5. Event-subscription failure retires the acquired handle without publishing a host record, including a retirement-error path.
6. An injected SQLite rejection of the Running row retires the handle and retains only failed history.
7. Cancelling the acquired-handle owner finishes retirement and persists failed history.

The tests use actual receiver/bridge code, SQLite-backed process append, a real in-memory Lash wake store with one injected open failure, and an instrumented driver for host handoff. The handoff fixture constructs ToolSuite directly to exclude unrelated startup recovery. `cargo clippy -p hirsel-host --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed. The coordinator owns the final combined workspace checks.
