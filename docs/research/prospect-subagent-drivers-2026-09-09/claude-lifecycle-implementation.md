# Claude native controls and shared lifecycle implementation

Implementation worktree `/workspace/code/hirsel-driver-lifecycle`, based on `160b6dedea265d9d8f9ad182fc4425f70fbd2475`. Covers GitHub #2 (native acknowledgements), #4 (lifetime) and Claude's capability-specific portion of #5 (follow-up semantics). Codex files are owned by the separate control implementer.

## Behavior

- Native `claude -p --input-format stream-json --output-format stream-json` remains the transport, with full-auto permissions. Adds `--replay-user-messages` and a UUID on each input. Startup/follow-up succeeds only when the native peer replays that UUID; unrelated user/tool-result UUIDs cannot acknowledge it. Initial spawn publishes no registry handle until its receipt arrives.
- `interrupt` registers a separate control request ID, waits for the matching `control_response`, surfaces provider rejection, and times out after 30 seconds. Pending waiters are removed on cancellation/timeout and rejected on session closure. Writes are inside the same bounded wait.
- This is **CLI receipt**, not evidence that a model consumed or acted on the input. Claude has no Codex `expectedTurnId` equivalent in this interface. Hirsel refuses input after the first observed terminal and rejects an unacknowledged input when completion wins the race. It creates no follow-up queue or second run. A CLI-acknowledged input can still lose a race with model completion; public tool wording must retain that distinction.
- The session owns its process group immediately after spawn; stderr drains before waiting for startup receipt. The reader owns the `Child` and only a weak session reference, so cancellation of construction drops the owning guard and kills the process tree. Tokio's child kill-on-drop backstops direct-child cleanup.
- Reader observes direct child exit separately from stdout. It allows 500ms for final stdout after exit, then kills remaining owned descendants. EOF while the child remains alive kills/reaps it; reap is bounded. Exit status 0 without a provider result is failure. Valid final output wins over a later nonzero exit.
- Shared EventHub admits one terminal, suppresses later events, replays that terminal to late subscribers, and ends their stream afterward. Existing progress-history storage is otherwise unchanged.
- Retirement kills the group and rejects waiters **before** acquiring the stdin mutex, so a blocked large write cannot hold cleanup hostage for its request timeout.
- Independent review also corrected supervisor completion to abort its owned stderr-drain task and publish terminal before the final pending-waiter rejection. This closes detached-pipe and admission-ordering gaps.

## Protocol evidence

Installed `claude --help` documents `--replay-user-messages` as re-emitting stdin user messages to stdout for acknowledgement (root inspected, no inference). Official [TypeScript reference](https://code.claude.com/docs/en/agent-sdk/typescript) defines optional input UUID and required replay UUID/`isReplay`; [checkpointing documentation](https://code.claude.com/docs/en/agent-sdk/file-checkpointing) documents replay-user-messages and response UUIDs. This implements the native wire directly; no Claude SDK dependency or subscription credential migration was introduced.

## Hermetic verification

Python native peers run through the actual private `spawn_command` transport seam, avoiding global PATH/environment mutation and any provider credentials/model calls. Scenarios: matched input echo plus unmatched-ID noise; matched interrupt error; ignored control times out/removes its waiter; stderr-heavy startup; clean exit 0 with a descendant retaining stdout; valid late final bytes after parent exits 7; startup timeout and caller cancellation kill both parent/descendant; result wins over follow-up echo; blocked stdin writer cannot prevent retirement; immediate duplicated result replays exactly one first terminal to a late subscriber.

Final combined driver checks used the completed Codex implementation copied from `/workspace/code/hirsel-codex-control` plus this Claude/shared implementation. Obsolete `finish_child`/`emit_child_failure` helpers were removed after both new supervisors replaced them.

- `cargo test -p hirsel-drivers`: **32 passed, 0 failed, 2 ignored** (real Claude/Codex paid CLI smokes remain ignored). Includes eight Claude peer tests and ten Codex peer tests. Log `/tmp/hirsel-driver-lifecycle-tests.log`.
- `cargo clippy -p hirsel-drivers --all-targets -- -D warnings`: exit 0. Log `/tmp/hirsel-driver-lifecycle-clippy.log`.
- `cargo fmt --all -- --check`: exit 0.
- `git diff --check`: exit 0.

The last combined rerun completed in 2.23s with all 32 tests passing. An earlier combined run exposed a peer-fixture scheduling issue: a 150ms Codex startup timeout could kill Python before it wrote its PID file. The Codex implementer increased timeout margins; Claude cancellation now waits for child/descendant readiness before aborting, and its startup timeout has a one-second margin. These change fixture scheduling, not production deadlines or assertions. All eight Claude peer tests also passed independently after the refinements. No live model calls were used.

Owned patch is `/tmp/hirsel-driver-lifecycle.patch`, containing only `claude.rs`, `claude_tests.rs`, `shared.rs`, `types.rs`. Copied Codex files are deliberately excluded; apply the separate `/tmp/hirsel-codex-control.patch` alongside it. No main-workspace edits or live provider calls were made.
