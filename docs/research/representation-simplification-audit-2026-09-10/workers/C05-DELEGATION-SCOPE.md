# C05-DELEGATION-SCOPE audit

Date: 2026-09-10
Cluster: Child delegation, caller identity, history fencing, execution bindings, and scoped tools
Audit mode: read-only schemasmash plus audit-your-codebase pass

## Verdict

Recommend one material fix: remove the redundant `child_thread_id` column from
`thread_delegations` and derive it from the canonical `thread_turns.thread_id`
through `child_turn_id`.

No second finding clears the materiality bar. The remaining apparent concerns
are either already enforced at the storage boundary, deliberate independent
dimensions, immutable projections rather than duplicate authority, or an
explicit product/operational boundary.

## Snapshot and constraints

The fixed repository snapshot was verified before inspection:

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
TREE  a4aac830c45398a66591f2c44b707aaf3cef281b
STATUS  empty (`git status --porcelain`)
```

The audit read the named specification and exclusions, `CONTRIBUTING.md`,
`CLAUDE.md`, every owned file listed below, and the specified consumer context.
No source files, tests, builds, dependencies, database contents, live
configuration, providers, commits, or branches were changed or invoked.

## Coverage inventory

| ID | Owned surface inspected | Key definitions and behavior | Result |
| --- | --- | --- | --- |
| C05-A | `crates/hirsel-host/src/lash_runtime/scoped_tools.rs`; `crates/hirsel-host/src/lash_runtime/thread_schemas.rs` | `ScopedThreadTools`, scoped catalog validation, delegate/send/report dispatch, `DelegateInput`, assignment resolution, JSON schemas | No independent finding. Delegate acceptance is a consumer of F-01's `DelegatedTurn`; the command-level `child_thread_id` selector remains necessary. |
| C05-B | `crates/hirsel-host/src/storage/thread_delegation.rs`; shared `thread_delegations` and `thread_reports` definitions in `crates/hirsel-host/src/storage/current.sql` | Child enqueue, idempotent delegation receipt, child report receipt, artifact authorization, parent activity/outbox publication | F-01 recommended. `thread_reports` already uses `child_turn_id` as its canonical child identity and is otherwise sound. |
| C05-C | `crates/hirsel-host/src/storage/thread_scope.rs`; shared `thread_execution_bindings` definition in `current.sql` | `ThreadRef`, `ThreadCaller`, history validation, subtree authorization, binding/revocation, execution guards, cancellation | No independent finding. Boundary validation and history fencing are coherent. |
| C05-D | `crates/hirsel-host/src/storage/thread_read.rs` | Independent message/turn/activity cursors, scoped detail, delegation brief lookup, related data | No finding. Independent cursors are intentional and covered by bounded-pagination tests. |
| C05-E | `crates/hirsel-host/src/thread_tool_bridge.rs`; `crates/hirsel-host/src/thread_tool_bridge/telemetry.rs` | Bridge capability, launch identity, frozen tool catalog, first-invoker fencing, request receipts, telemetry guards, cancellation completion | No independent finding. Retry, bridge fencing, and telemetry completion have distinct purposes. |
| C05-F | `crates/hirsel-host/src/tools/session.rs`; `crates/hirsel-host/src/tools/threads.rs` | Session/tool-surface reconciliation, handoff seed, thread/turn/activity publication, terminal child report publication | No finding. These are adapters/projections and do not introduce a second delegation authority. |
| C05-G | `crates/hirsel-host/src/lash_runtime/resource_tests.rs`; `crates/hirsel-host/src/storage/thread_scope_tests.rs`; `crates/hirsel-host/src/thread_tool_bridge/artifact_tests.rs`; `crates/hirsel-host/src/thread_tool_bridge/tests.rs` | Scope, delegation, history reset, artifact references, bridge replay/cancellation, telemetry, publication regression coverage | Existing coverage supports the skips and F-01 cutover; additions are listed below. |

## Finding F-01: `thread_delegations` stores child identity twice

**Verdict: recommend. Confidence: high. Materiality: medium/high.**

### Exact source evidence

The canonical relationship is already present in `thread_turns`:

`crates/hirsel-host/src/storage/current.sql:16-22`

```sql
CREATE TABLE thread_turns (
    id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id INTEGER NOT NULL REFERENCES threads(id),
    requester_thread_id INTEGER REFERENCES threads(id),
    requester_turn_id INTEGER REFERENCES thread_turns(id),
    ...
    CHECK(requester_turn_id IS NULL OR requester_thread_id IS NOT NULL));
```

The delegation receipt independently stores both the thread and a turn whose
row already names that thread:

`crates/hirsel-host/src/storage/current.sql:133-136`

```sql
CREATE TABLE thread_delegations (
    requester_turn_id INTEGER NOT NULL REFERENCES thread_turns(id), operation_id TEXT NOT NULL, payload TEXT NOT NULL,
    child_thread_id INTEGER NOT NULL REFERENCES threads(id), child_turn_id INTEGER NOT NULL REFERENCES thread_turns(id),
    PRIMARY KEY(requester_turn_id,operation_id));
```

The Rust result type also returns both values, because callers need both, but it
does not make the database copies independent authorities:

`crates/hirsel-host/src/storage/thread_delegation.rs:17-21`

```rust
pub(crate) struct DelegatedTurn {
    pub thread_id: u64,
    pub turn_id: u64,
}
```

The child turn is created with the selected child thread:

`crates/hirsel-host/src/storage/thread_delegation.rs:23-34`

```rust
c.execute("INSERT INTO thread_turns(thread_id,requester_thread_id,requester_turn_id,state,started_at) VALUES(?1,?2,?3,'queued',?4)", ...)?;
let turn_id = c.last_insert_rowid() as u64;
```

The delegation receipt then writes the same child value again:

`crates/hirsel-host/src/storage/thread_delegation.rs:102-115`

```rust
let turn_id = enqueue(&tx, child, ..., Some(caller.thread_id), Some(caller.turn_id), false)?;
...
tx.execute("INSERT INTO thread_delegations(requester_turn_id,operation_id,payload,child_thread_id,child_turn_id) VALUES(?1,?2,?3,?4,?5)", ...)?;
```

Both replay paths trust the two stored columns independently:

`crates/hirsel-host/src/storage/thread_delegation.rs:81-85`

```rust
SELECT payload,child_thread_id,child_turn_id FROM thread_delegations ...
return Ok(DelegatedTurn{thread_id,turn_id});
```

`crates/hirsel-host/src/storage/thread_delegation.rs:213-221`

```rust
SELECT payload,child_thread_id,child_turn_id FROM thread_delegations ...
Ok(DelegatedTurn { thread_id, turn_id })
```

The accepted result is consumed as both a thread and a turn by the scoped-tool
publication path:

`crates/hirsel-host/src/lash_runtime/scoped_tools.rs:257-272`

```rust
.thread_detail(accepted.thread_id, None, 1)
...
.filter(|t| t.id == accepted.turn_id)
...
.filter(|a| a.turn_id == Some(accepted.turn_id))
```

### Invalid representable state

The schema permits this combination:

```text
threads.id = 7 and threads.id = 8 both exist
thread_turns.id = 42 has thread_id = 8
thread_delegations.child_thread_id = 7, child_turn_id = 42
```

Both foreign keys pass. There is no constraint requiring
`thread_delegations.child_thread_id = thread_turns.thread_id` for the selected
`child_turn_id`. A replay can therefore return thread 7 with turn 42, while a
subsequent `thread_detail(7)` cannot find turn 42. This is a concrete invalid
cross-layer state even though the current production writer does not currently
create it.

Reachability is currently latent, not observed on the normal API path:
`delegate_thread` selects `child`, passes it to `enqueue`, receives that
inserted turn ID, and writes both values in one transaction. The defect is that
the state remains representable to any future writer, direct SQL fixture, or
corrupting path, and every normal write still maintains two copies of one fact.
No current one-copy-only update path was found; the risk is the existence of
two mutable storage authorities rather than a demonstrated current divergence.

### Consumer inventory query

The following reproducible query was run over the host source and host tests:

```text
rg -n 'thread_delegations|DelegatedTurn|delegate_thread|delegation_receipt|child_thread_id|accepted\.(thread_id|turn_id)' crates/hirsel-host/src crates/hirsel-host/tests --glob '*.rs'
```

Result: **40 matches**. Relevant consumers are:

- receipt deletion during history reset (`storage.rs:112`);
- delegation receipt lookup, child selection, and accepted-result publication
  (`scoped_tools.rs:233-276`);
- the storage writer and both replay readers (`thread_delegation.rs:56-121,
  207-224`);
- input/schema selectors and their contract tests (`scoped_tools.rs:471-526`,
  `subagent_models.rs:355-377`, `thread_scope_tests.rs:79-125,243-250`);
- child-report provenance, which is an activity snapshot derived from the
  turn and is not the duplicate receipt authority
  (`thread_delegation.rs:165-181`);
- nested-runtime verification, which already keys reports by child turn
  (`tests/nested_runtime.rs:280-294`).

### Recommended target representation

Keep the command input and public result shape, but make the durable receipt
store only the accepted child turn:

```sql
CREATE TABLE thread_delegations (
    requester_turn_id INTEGER NOT NULL REFERENCES thread_turns(id),
    operation_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    child_turn_id INTEGER NOT NULL REFERENCES thread_turns(id),
    PRIMARY KEY(requester_turn_id,operation_id));
```

Change the two receipt reads to join the canonical turn row, conceptually:

```sql
SELECT d.payload, t.thread_id, d.child_turn_id
FROM thread_delegations AS d
JOIN thread_turns AS t ON t.id = d.child_turn_id
WHERE d.requester_turn_id = ?1 AND d.operation_id = ?2
```

The insert becomes `(... requester_turn_id, operation_id, payload,
child_turn_id ...)`. `DelegatedTurn { thread_id, turn_id }` remains unchanged
as the internal/public result contract, populated from the joined turn on
replay and from the already selected `child` on the new-write path.

Do **not** remove `Delegation.child_thread_id`: at
`thread_delegation.rs:10-16` and `scoped_tools.rs:471-526` it is an input
selector for reusing an existing direct child, not a second persisted receipt
fact. Do **not** remove `child_thread_id` from the immutable `child_report` or
`delegation_received` activity payloads: those are provenance/UI snapshots.

### Smallest credible change set

1. `crates/hirsel-host/src/storage/current.sql`: remove only
   `thread_delegations.child_thread_id`.
2. `crates/hirsel-host/src/storage/thread_delegation.rs`: update the insert and
   both receipt queries/destructuring to join `thread_turns`.
3. `crates/hirsel-host/src/storage/thread_scope_tests.rs`: retain the existing
   replay/payload-mismatch assertions and add a receipt-shape/derived-child
   assertion.
4. `crates/hirsel-host/tests/nested_runtime.rs`: verify the accepted child
   result and report path still use the joined turn identity; its report query
   already uses `child_turn_id`.

No public tool schema, `Delegation` input, `DelegatedTurn` output, activity
payload, report receipt, or execution-binding interface needs to change.

### Why this removes the defect

There is one durable source for the child thread: `thread_turns.thread_id`.
The receipt's `child_turn_id` identifies the accepted turn, and the join derives
the corresponding thread every time. A mismatched pair cannot be stored, and a
future writer cannot update one child identity without the other because only
the turn reference exists in the receipt.

### Regression, cutover, and validation risk

- This is a current-schema cutover. The project exclusions state that the
  shipped schema is current-only and does not invite compatibility shims or
  gratuitous migration work. If an already-persisted database must be upgraded,
  that rollout/migration requires a separate authorized decision; it should not
  be silently invented in this audit.
- Both replay paths must use the join. Missing either query would make fresh
  delegations work while retries or preflight receipt checks fail.
- The scoped publication path must continue returning a thread and turn that
  belong together; otherwise the defect reappears at the API boundary even
  after the table is corrected.
- Existing validation: `thread_scope_tests::delegation_is_atomic_idempotent_and_reports_once_after_hidden_parent_resumes`
  covers same-payload replay, changed-payload rejection, direct-child scope,
  child requester linkage, terminal reporting, and one report after repeated
  terminal transitions. `tests/nested_runtime.rs:280-294` checks the child
  turn linkage and report receipt. The bridge and reset suites cover the
  surrounding history/capability fences.
- Additional validation required after implementation, but not run here:
  inspect the final `thread_delegations` columns; exercise fresh and replayed
  delegation through both `delegate_thread` and `delegation_receipt`; assert
  the returned `thread_id` equals the selected turn's `thread_id`; and run the
  focused host storage/nested-runtime tests plus the repository's required
  workspace gates.

## Explicit skips and no-finding areas

### Caller identity and history fencing

`thread_scope.rs:19-27` exposes the five-field `ThreadCaller` capability, while
`current.sql:141-144` persists the binding's history/session/execution/turn
identity and revocation bit. This could look like duplicate state, but the
production constructors are concentrated in `thread_scope.rs:236-283`, and
`validate_caller` (`thread_scope.rs:45-63`) checks the binding against the
current history, bound turn/thread, running state, revocation, and cancellation
before scoped operations. The source consumer query

```text
rg -n 'ThreadCaller|caller\.(history_id|session_id|execution_id|thread_id|turn_id)|execution_caller|bind_thread_execution|revoke_thread_execution|validate_caller' crates/hirsel-host/src --glob '*.rs'
```

returned **213 matches**, but no production literal constructor outside the
owner was found. Forged field combinations are latent behind crate visibility,
not a demonstrated reachable write path, and making the capability opaque
would be an API-hardening refactor rather than a materially simpler current
design. Existing reset, revocation, cancellation, and stale-writer tests cover
the actual boundary. Skipped.

### Thread references and independent pagination

`thread_scope.rs:7-98` intentionally supports both numeric IDs and relative
descendant paths, with subtree authorization and bounded path depth.
`thread_read.rs:7-24` intentionally carries separate before-cursors for
messages, turns, and activities. Collapsing either representation would merge
independent scope or pagination dimensions. The scope tests cover peer denial,
hierarchy resolution, and reaching all three collections exactly once. Skipped.

### Reports, activities, mutation receipts, and artifacts

`thread_reports` (`current.sql:137-140`) is keyed by `child_turn_id` and stores
payload plus its immutable activity ID; `thread_delegation.rs:165-195` writes
the report activity, receipt, parent request, and revision in one completion
transaction. The activity payload is a parent-facing projection and the receipt
is an idempotency authority, so they are not interchangeable duplicate truth.
The same distinction applies to mutation receipts and artifact backlinks. No
one-copy-only update path or invalid representable combination was found in
the owned surfaces. Skipped.

### Execution revocation, cancellation, bridge replay, and telemetry

The normal execution guard and the telemetry completion guard intentionally have
different lifetimes: a cancelled/revoked execution must reject new work while
an already-started tool may still need one terminal completion event. The
bridge's capability, launch identity, first-invoker fence, frozen catalog, and
request receipts likewise serve different replay and authority boundaries.
`resource_tests.rs` and `thread_tool_bridge/tests.rs` cover cancelled access,
in-flight completion, reset history fencing, duplicate calls, and exactly-once
telemetry. No finding.

### Dynamic catalog and dispatch shape

`scoped_tools.rs:33-43,317-322` validates against a fresh settings/plugin
catalog per scoped invocation, while `thread_tool_bridge.rs` freezes the
catalog for a launched bridge. That apparent duplication is the intended
settings-refresh versus bridge-session contract. The dispatch match is broad,
but a style/line-count refactor would violate the audit threshold. Skipped.

### CLI working directory and worktree isolation

`scoped_tools.rs:478-526` accepts an execution `cwd`, and the CLI drivers use it
as their process/config workspace. The product direction explicitly declines
per-provider worktree controls (`docs/product-direction.md:19`); the prompt
contract documents `cwd` as the working directory and separately tells callers
that parallel repository workers need separate worktrees
(`prompts/agent.md:15,22`). The host scope contract is thread/descendant
authorization, not workspace isolation. Adding a worktree manager here would
report a settled product/operational boundary as a new data-model finding.
Skipped.

### Test-only owned files

`resource_tests.rs`, `thread_scope_tests.rs`, `artifact_tests.rs`, and
`thread_tool_bridge/tests.rs` were inspected as behavior/fixture surfaces. They
already cover resource scope, delegation replay/reporting, history reset,
artifact exactness, bridge replay/fencing, cancellation, and telemetry. No test
fixture introduces an independent source of delegation identity. The only
missing regression shape relevant to F-01 is the post-cutover assertion that
replay derives the thread from the child turn; it is listed above rather than
reported as a second finding.

## Exclusions honored

I did not restate tracked outcomes #2-#14 or #16 as new findings, did not merge
thread pin/settlement/attention/visibility/read/execution dimensions, did not
propose artifact ownership changes, automatic delegated-work restart, native
driver removal, transport unification, or a worktree manager, and did not add a
compatibility migration. No excluded behavior was used to inflate the result.

## Audit log and handoff

- Read the exact C05 specification and exclusions before source inspection.
- Verified the fixed HEAD/tree and empty worktree before inspection.
- Inspected all 13 whole-file owners and the four named shared SQL definitions;
  read-only consumer context included `storage.rs`, `thread_execution.rs`,
  `executor.rs`, CLI drivers, web thread references, and the specified tests.
- Ran bounded `rg`, `nl`, and `sed` queries only. No test, build, install,
  migration, provider, live-config, issue, commit, or push operation was run.
- Post-report source verification, run after this report was written, returned
  the same fixed snapshot and an empty status:

  ```text
  HEAD  3ee0621a603659ab0168f565b99012b642415419
  TREE  a4aac830c45398a66591f2c44b707aaf3cef281b
  STATUS  empty (`git status --porcelain`)
  ```

### Required handoff

Recommendation: implement F-01 as a current-schema cutover, then run the
focused delegation/replay and nested-runtime validation plus the repository's
normal workspace gates. The source snapshot is confirmed unchanged; the report
is the only deliverable written by this worker.
