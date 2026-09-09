# Independent Codex implementation review

Reviewed assembled `/workspace/code/hirsel-skills/crates/hirsel-drivers/src/codex.rs` read-only on 2026-09-09. This reviewer implemented Claude/shared, not Codex. Existing test results were not rerun during this review. Focus: controls, root/turn filtering, completion ordering, startup ownership and cancellation.

## Initial finding: server-request response blocks the supervisor — resolved

`read_codex_stdout` at initial-review lines 573-584 upgrades a strong session and awaits `reject_server_request` inside its stdout `select!` branch. That helper waits for the stdin mutex and writes under the 30-second control timeout (`:131-151`). While that await is pending, the outer `child.wait()` and 500ms drain-deadline branches cannot be polled.

A native server request arriving while a caller owns stdin for a blocked large write can therefore stall independent child-exit observation until the command deadline. An inherited stdin pipe can keep the write blocked even after the direct child dies. The strong session reference also remains owned during this wait. This defeats the intended separation between native control backpressure and child supervision.

Recommended correction: supervise server-response writing independently, or keep direct-child exit/cancellation selection active while that write waits. Bound and cancel any new response task so moving the await does not introduce a detached writer. A hermetic peer can expose the case by reading the first byte of a large follow-up, emitting a native server request, retaining stdin in a descendant and exiting the direct child.

Reported immediately to root and the Codex implementer. **Resolution independently rechecked** in final `/workspace/code/hirsel-codex-control` source: the reader stores at most one boxed pending response-write future and polls it in the same `select!` as direct-child exit and the drain deadline. The future captures an `Arc` to stdin plus timeout/response data, not a strong session owner. Direct-child exit cancels the pending write; final cleanup explicitly drops any remaining write before failing/closing the session. No detached writer task was introduced.

The new `native_reply_backpressure_does_not_block_direct_child_exit_supervision` peer test waits until a 2 MiB caller write owns stdin, asks the peer to emit a native server request, observes the unsupported-request progress barrier, then triggers direct-child exit with a descendant retaining stdin. It requires terminal failure, writer rejection and retirement within two seconds and separately checks parent/descendant death. This reviewer inspected the test and production correction; the implementing agent executes it. Root assembles these final worker files centrally before final checks.

Root identified one additional drain-ordering edge, also rechecked after correction: buffered native requests must not trigger outgoing writes after direct exit, and a broken reply write must not discard a valid final notification buffered behind it. Final code skips new native responses after exit/reply failure and gives reply-write failure the same bounded 500ms stdout drain. `get_or_insert` preserves an already-earlier deadline; pending response state is dropped before cleanup. The new `failed_native_reply_still_drains_a_valid_final_result` fixture closes stdin, sends a native request, then sends the final root result; its assertion requires the valid result to win. This resolves the edge without another worker or unbounded buffer.

**Final review verdict:** no unresolved blocking finding in this scoped Codex review.

## Other inspected properties

- Pending requests use generated numeric IDs, cancellation cleanup and method-specific response correlation. Server requests are discriminated by `method` and cannot consume same-ID response waiters. Errors propagate to the addressed caller; transport failures/timeouts close and fail the run.
- Initialization waits for successful `initialize`, sends `initialized`, then waits for correlated root `thread/start` and first `turn/start` before publishing a registry handle. Root identity does not come from unsolicited child thread notifications.
- Immediate ProcessGroup ownership, explicit uncommitted StartupGuard and a weak reader reference protect ordinary failed/cancelled startup. Stderr drains during initialization. Retirement fails/kills before awaiting stdin closure.
- Follow-up uses `turn/steer` with `expectedTurnId`, validates the acknowledged turn id, and never sends another `turn/start`. Interrupt snapshots the active root turn. Known stale-turn notifications cannot replace an active turn or settle it.
- Notifications must name the root thread. An explicit item turn id must match the active turn; terminal must name the matching active turn. First terminal wins through shared EventHub, preserving valid completion against late child exit. A `turn/start` response arriving after valid terminal does not revive active state.
- Child exit and stdout EOF have bounded normal cleanup; the issue above is the exceptional inline-write path that bypasses that independent select while awaited.

No other concrete blocking defect identified in these inspected paths. This review does not claim every hypothetical malformed provider sequence is accepted or every native CLI release was exercised.
