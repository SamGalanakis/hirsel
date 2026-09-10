# C02 TURN LIFECYCLE — combined schemasmash / simplification review

## Snapshot and method

Reviewed the supplied read-only worktree at `HEAD 3ee0621a603659ab0168f565b99012b642415419` (`tree a4aac830c45398a66591f2c44b707aaf3cef281b`). The source tree was clean before the review. No source edits, tests, builds, installs, network calls, live data/config reads, provider calls, or session/process actions were performed.

The complete owned surface was inspected: `crates/hirsel-host/src/lash_runtime/{bridges.rs,cli_turn.rs,lifecycle.rs,runtime.rs,runtime_tasks.rs,scripted.rs,thread_lanes.rs,thread_queue.rs,timeline.rs,turn.rs}`, `crates/hirsel-host/src/storage/{thread_activity.rs,thread_completion.rs,thread_execution.rs,thread_requests.rs}`, `crates/hirsel-proto/src/turn.rs`, tests `crates/hirsel-host/src/lash_runtime/{cli_delivery_tests.rs,thread_recovery_tests.rs,thread_tests.rs,upgrade_tests.rs}`, and `crates/hirsel-host/tests/nested_runtime.rs`. The assigned shared definitions in `storage/current.sql` (request, turn, activity, execution and cancellation tables/indexes) and `hirsel-proto/{thread.rs,client.rs}` (`ThreadActivity`, `ThreadTurnState`, `ThreadTurn`, `SendMode`) were also inspected. Major consumers were read at the request → durable turn → lane/driver → observation/timeline/activity → wire path.

## One new material finding

### C02-01 — Existing Host child follow-ups silently lose the child’s stored backend

**Verdict: recommend a small production fix; high confidence.** This is a reachable duplicate-truth/precedence defect. A Host child preference is persisted, but a later follow-up snapshots the changing global Host default instead of that child preference. The result is a turn whose immutable execution record disagrees with the child’s documented “stored execution backend.”

**Contract and input path.** The generated delegation contract says an existing child with no selectors “keeps its accepted backend” (`crates/hirsel-host/src/subagent_models.rs:350-355`). `threads_send` deliberately constructs such a request with `execution: None` (`crates/hirsel-host/src/lash_runtime/scoped_tools.rs:231-249`); the equivalent selector-free `threads_delegate` branch also returns `None` (`scoped_tools.rs:478-487`). The public tool description says follow-up work uses the child’s “stored execution backend” (`crates/hirsel-host/src/lash_runtime/tool_defs.rs:123-130`).

**Write and conversion evidence.** Explicit `agent:"host"` resolves to the current Host execution (`scoped_tools.rs:488-501`). `Storage::delegate_thread` writes that `ThreadExecution` to the one-row-per-child `thread_execution_preferences` table (`crates/hirsel-host/src/storage/thread_delegation.rs:86-111`; `crates/hirsel-host/src/storage/current.sql:148`), then `enqueue` calls `capture` before creating the request (`thread_delegation.rs:23-44`). The capture function reads the preference (`crates/hirsel-host/src/storage/thread_execution.rs:27-39`) but preserves only `ThreadExecution::Cli`; every `Host` preference falls through to `meta.host_execution_default` (`thread_execution.rs:40-51`). It then serializes the selected value as the immutable per-turn snapshot in `thread_turn_execution` (`thread_execution.rs:53-57`; `current.sql:149`).

The global value is independently rewritten on startup and when the live Host model changes (`crates/hirsel-host/src/lash_runtime/runtime.rs:127-144`; `thread_lanes.rs:59-75`). Therefore this concrete sequence is representable and reachable: delegate child C with `agent:"host"` while global Host is `{provider P0, model M0}`; restart or change the Host model so the global value is `{provider P1, model M1}`; send a follow-up to C with no selectors. C’s preference row remains `{P0,M0}`, while its new `thread_turn_execution` row becomes `{P1,M1}`. The lane dispatch consumes that snapshot and applies its provider/model (`crates/hirsel-host/src/lash_runtime/thread_lanes.rs:164-196`; `thread_queue.rs:144-161`), so the child can run on a different provider/model without an explicit reassignment. Existing Host turns already snapshotted remain unchanged.

This is the required duplicate-truth write path: `delegate_thread` writes the child copy at `thread_delegation.rs:99-100`, while `refresh_execution_default` updates the global copy at `thread_lanes.rs:68-74`; no operation updates the child row when the global copy changes. The bug is the conversion precedence in `capture`, which treats a persisted Host child choice as if it were absent. No malformed-row assumption is needed.

**Reproducible consumer counts and existing coverage.** On this snapshot:

```text
rg -n 'thread_execution_preferences|ThreadExecution::Host|capture\(' \
  crates/hirsel-host/src/storage/thread_execution.rs \
  crates/hirsel-host/src/storage/thread_delegation.rs \
  crates/hirsel-host/src/lash_runtime/scoped_tools.rs \
  crates/hirsel-host/src/lash_runtime/thread_lanes.rs \
  crates/hirsel-host/src/lash_runtime/thread_queue.rs \
  crates/hirsel-host/src/lash_runtime/runtime.rs
```

Result count: **8**. The corresponding test query

```text
rg -n 'thread_execution_preferences|turn_execution|ThreadExecution' \
  crates/hirsel-host/src/lash_runtime/*tests.rs \
  crates/hirsel-host/src/storage/*tests.rs
```

has **1** result, the CLI fixture at `crates/hirsel-host/src/lash_runtime/cli_delivery_tests.rs:97`; no existing test/fixture demonstrates a persisted Host preference surviving a global-default change. The required validation is inspection plus a hermetic storage/runtime regression: persist Host P0/M0 for a child, change the global Host default to P1/M1, enqueue a selector-free child follow-up, and assert its `thread_turn_execution` is P0/M0; also retain the existing CLI-preference and no-preference Host cases. Do not use a provider call.

**Target representation and scope.** Keep the existing tagged `ThreadExecution` enum (`thread_execution.rs:7-20`) and the two tables: `thread_execution_preferences` remains the durable per-child backend choice, and `thread_turn_execution` remains the immutable per-turn snapshot. Make `capture` obey the invariant “if a preference row exists, copy that exact `ThreadExecution` into the turn snapshot; otherwise use the global Host default,” i.e. match `Some(preferred) => Some(preferred)` for both Host and CLI. No schema migration or interface change is required. The smallest production change is `storage/thread_execution.rs`, with a focused test in the storage/delegation or owned recovery test surface. This removes the invalid combination where a no-selector follow-up has a child Host preference and a different global Host snapshot. The expected risk is limited to making already-persisted Host choices effective; a child whose stored provider is unavailable will now reach the existing explicit provider check (`thread_queue.rs:148-151`) rather than silently switching providers. Existing CLI behavior and already accepted turn snapshots are unchanged.

## Known tracked issue, explicitly not a new finding

F01/#16 is independently verified and in flight at the root: startup interrupts running turns (`crates/hirsel-host/src/lash_runtime/runtime.rs:103-106`), pending requests remain FIFO ordered (`crates/hirsel-host/src/storage/thread_requests.rs:39-44`), and the CLI lane skips a non-Queued head without removing it (`crates/hirsel-host/src/lash_runtime/thread_lanes.rs:164-176`), blocking later requests. It is recorded here only for coverage and is excluded from the recommendation.

## Explicit no-finding / skip areas

- `current.sql` turn/activity/cancellation tables and the `ThreadActivity`, `ThreadTurnState`, `ThreadTurn`, and `SendMode` definitions: the orthogonal state dimensions and durable cancellation intent have reachable writers/readers; no additional invalid state with a complete production path met the materiality threshold.
- `thread_activity.rs`, `thread_completion.rs`, bridges, timeline, and `turn.rs`: terminal idempotency, activity keys, observation ordering, fallback tool/code IDs, and sequence publication were read through their consumers. The existing terminal-delivery correction and recovery fixtures cover the relevant retry shape; no new duplicate outcome or sequencing defect was established.
- `runtime_tasks.rs`: `Option<Vec<JoinHandle>>` is an intentional open/stopped epoch marker; stop drains the owned generation before reset. No simplification finding.
- `OwnerTurn.turn_id: Option<u64>` and queued/accepted conversion: preaccept drafts and stored-turn validation make the option representable; replacing it would require an invasive producer cutover and no complete invalid path was established. No finding.
- Scripted/Lash queue recovery, cancellation, and nested runtime tests: durable request rows, in-memory projections, and cancellation intents have coordinated recovery paths; no independent defect beyond tracked F01 was verified.
- Existing issue outcomes #2–#14 and the exclusions file were not rediscovered. In particular, CLI terminal delivery/retry and native driver findings remain tracked or already corrected.

## Final source check

After writing this report, the source worktree remained at `HEAD 3ee0621a603659ab0168f565b99012b642415419`, tree `a4aac830c45398a66591f2c44b707aaf3cef281b`, with empty `git status --porcelain`. The report is outside the source worktree. No tests/builds were run.
