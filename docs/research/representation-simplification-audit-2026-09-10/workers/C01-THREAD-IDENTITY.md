# C01-THREAD-IDENTITY — Thread identity, hierarchy, lifecycle, attention and read

## Findings

### F01 — Persisted action requests have two independent Thread identities

**Verdict: recommend. Confidence: medium.** The normal `handle_thread_action`
path is internally consistent today, but the durable JSON boundary permits an
action snapshot for one Thread to be accepted and executed as a turn for
another Thread. The final representation should have one durable owner: the
outer `OwnerTurn.thread_id`.

#### Evidence by layer

- The durable domain object has one identity, `Thread.id`:

  > `crates/hirsel-proto/src/thread.rs:13-16` — `pub struct Thread { pub id: u64, pub parent_thread_id: Option<u64>, ... }`

- The accepted runtime envelope has a second, outer identity used for
  admission and routing:

  > `crates/hirsel-host/src/lash_runtime/runtime.rs:15-20` — `OwnerTurn` contains `pub thread_id: u64` and `pub thread_action: Option<ThreadActionContext>`.

  > `crates/hirsel-host/src/lash_runtime/thread_lanes.rs:164-172` — recovery reads pending payloads, deserializes `OwnerTurn`, takes `let id = request.thread_id`, and obtains the lane from that id.

- The nested accepted-action snapshot duplicates the identity:

  > `crates/hirsel-host/src/lash_runtime/thread_action.rs:6-22` — `ThreadActionSnapshot` is serialized into the request and includes `pub id: u64` alongside the historical fields and `revision`.

  > `crates/hirsel-host/src/lash_runtime/thread_action.rs:25-40` — `From<Thread>` copies `thread.id` into the nested snapshot.

- The command path constructs both values, but only because it happens to use
  the same `current` value:

  > `crates/hirsel-host/src/thread_commands.rs:123-127` — it loads `current` for the requested outer `id`.

  > `crates/hirsel-host/src/thread_commands.rs:224-236` — it passes outer `id` to `submit_addressed_turn` and puts `current.clone().into()` into `ThreadActionContext.thread`.

- The storage boundary accepts an untyped JSON request and validates only the
  nested revision, not the nested identity:

  > `crates/hirsel-host/src/storage/thread_messages.rs:35-45` — `append_thread_owner_request` accepts `thread_id: u64` and `request: &serde_json::Value`.

  > `crates/hirsel-host/src/storage/thread_messages.rs:144-157` — action validation extracts `action.thread.revision`, compares it with `threads::get(&tx, thread_id)?.revision`, and checks lifecycle state; there is no `action.thread.id == thread_id` check.

  > `crates/hirsel-host/src/storage/thread_messages.rs:159-167` — the receipt stores the nested action payload, while the revision increment updates the outer function-argument `thread_id`.

  > `crates/hirsel-host/src/storage/thread_messages.rs:197-218` — the persisted `OwnerTurn` JSON is assigned `request["thread_id"] = thread_id`, the turn row is inserted with that same outer id, and the request payload is then stored.

- The schema therefore preserves the two pieces in different places:

  > `crates/hirsel-host/src/storage/current.sql:11` — `thread_action_receipts` stores only `client_id` and an opaque `payload` containing the nested action.

  > `crates/hirsel-host/src/storage/current.sql:12-15` — `thread_requests` derives its indexed `thread_id` from `json_extract(payload,'$.thread_id')` and orders by that id.

- The prompt and a concrete runtime consumer disagree on which copy is
  authoritative:

  > `crates/hirsel-host/src/lash_runtime/turn.rs:56-65` — the prompt labels the outer `turn.thread_id` as the owning Thread, then serializes the nested action and tells the agent to preserve identity.

  > `crates/hirsel-host/src/lash_runtime/scripted.rs:328-346` — the scripted action path updates `context.thread.id`, not `turn.thread_id`.

#### Concrete invalid combination and write path

Create two active Threads A and B at the same revision (fresh rows both have
`revision = 1`). Call the existing `append_thread_owner_request` boundary with
`thread_id = A` and a request whose `thread_action.thread` is the serialized
snapshot of B. The nested revision equals A's revision, so the checks at
`thread_messages.rs:149-156` pass. The transaction then:

1. records B's action snapshot in `thread_action_receipts` at lines 160-163;
2. increments A at lines 164-167;
3. writes the visible message and `thread_turns.thread_id` as A at lines
   178 and 213;
4. overwrites the request's outer `thread_id` with A at line 202 and persists
   the request at lines 217-220.

On recovery, `thread_lanes.rs:167-184` routes the request using A. The
scripted action consumer then updates B at `scripted.rs:339`. This is a
reachable write path through the crate's generic JSON storage method (and is
straightforward to exercise from an in-crate caller), although no current
production command supplies the mismatch: `handle_thread_action` constructs
both copies from the same `current` Thread at `thread_commands.rs:123-127` and
`232-236`. It is therefore a latent boundary defect, not a claim about a live
row.

This is also a duplicate-truth write path: the outer owner is set and used for
the message, turn, revision and request index, while the nested owner is
independently supplied and stored in the receipt. No write updates or checks
the nested `id` against the outer owner.

#### Consumer query and result

From the repository root, the following targeted query was run:

```text
rg -n 'ThreadActionSnapshot|ThreadActionContext|thread_action|context\.thread\.id|request\["thread_id"\]|turn\.thread_id' \
  crates/hirsel-host/src/lash_runtime/thread_action.rs \
  crates/hirsel-host/src/lash_runtime/runtime.rs \
  crates/hirsel-host/src/thread_commands.rs \
  crates/hirsel-host/src/storage/thread_messages.rs \
  crates/hirsel-host/src/lash_runtime/turn.rs \
  crates/hirsel-host/src/lash_runtime/scripted.rs | wc -l
```

Result: **34 matching lines across 6 files**. The relevant consumers are the
outer-owner prompt/routing paths (`turn.rs:56`, `thread_lanes.rs:167-184`) and
the nested-id mutation (`scripted.rs:328-346`). Web/native Thread projections
consume `Thread.id` and are not additional owners of the persisted action
identity.

#### Proposed end-state representation

- Keep `Thread.id: u64` as the identity of the durable row.
- Keep `OwnerTurn.thread_id: u64` as the sole identity in the accepted and
  recovered turn envelope.
- Remove `id` from `ThreadActionSnapshot`; retain only the historical action
  context (`title`, `description`, `instrument`, attention/lifecycle fields,
  timestamps and `revision`). `ThreadActionContext` then cannot carry a second
  owner.
- Keep `thread_requests.thread_id` as the generated/indexed projection of the
  outer request JSON, and keep `thread_action_receipts` as an action receipt;
  neither payload contains a nested Thread id.
- Make the scripted consumer target `turn.thread_id`. At the storage boundary,
  deserialize/validate the action shape before inserting it so a malformed
  action cannot bypass the single-owner representation.

The target removes the duplicate owner rather than relying on two equal values
forever. During a clean current-schema cutover, the minimum containment check
is `nested_id == thread_id` before any receipt/message/turn write; that guard is
useful for diagnosing old or malformed rows but is not the final ownership
model.

#### Smallest affected surface

Owned lead files are `crates/hirsel-host/src/lash_runtime/thread_action.rs`
(type and serialization fixture) and
`crates/hirsel-host/src/thread_commands.rs` (construction remains outer-owned).
The boundary must be updated in
`crates/hirsel-host/src/storage/thread_messages.rs`; the direct runtime
consumer is `crates/hirsel-host/src/lash_runtime/scripted.rs`, and the prompt
serialization is `crates/hirsel-host/src/lash_runtime/turn.rs`. The recovery
router in `thread_lanes.rs` should remain outer-owned and only needs regression
coverage. No web/native protocol or SQL DDL change is required for the target.

#### Regression, cutover and validation risk

- `ThreadActionSnapshot` has `serde(deny_unknown_fields)` at
  `thread_action.rs:8-9`. Removing `id` changes the JSON shape; any pending
  current-schema action request containing the old nested id must be drained,
  explicitly rejected, or otherwise handled by the authorized clean cutover.
  The snapshot rules allow no compatibility/migration shim in shipped source.
- The owner prompt must retain the outer `turn.thread_id`; otherwise the agent
  loses the explicit identity instruction. The scripted path must still clear
  attention/update the same outer Thread, preserving the current action
  behavior.
- The existing historical serialization fixture at
  `thread_action.rs:49-71` needs its expected shape updated.

Additional inspection-only validation required before implementation is
accepted:

1. Add a storage regression with two same-revision Threads and a mismatched
   action snapshot; assert rejection occurs before any receipt, message, turn,
   or revision write.
2. Preserve the existing valid action/replay and revision-conflict behavior,
   and assert recovery routes by the outer id.
3. Exercise the scripted action consumer with distinct A/B rows and assert
   only A changes.
4. Add a serialization/restart case covering the chosen current-schema
   cutover behavior for pending requests.

No tests or application code were executed during this audit.

#### Existing test/fixture coverage

The condition is **not** demonstrated by an existing negative fixture.
`crates/hirsel-host/src/lash_runtime/thread_action.rs:49-71` demonstrates
historical field capture and intentionally excludes live summary projections.
`crates/hirsel-host/src/storage/threads_tests.rs:298-350` tests action receipt
replay and conflicting payload rejection, but constructs the nested snapshot
from the same `t` passed as the outer id at lines 301-306. The recovery action
test at `crates/hirsel-host/src/lash_runtime/thread_recovery_tests.rs:206-235`
also uses the public same-Thread path. No live values or rows were read.

## Explicit no-finding inventory and skips

Every assigned whole-file owner and shared definition was inspected. No
additional recommendation is made for the following areas:

| Owned area | Result and reason for skip |
| --- | --- |
| `crates/hirsel-host/src/lash_runtime/thread_action.rs` | F01 is the only defect; the exclusion of `running_turn`, queue count, terminal turn and activity fields is intentional historical capture and is covered by lines 49-71. |
| `crates/hirsel-host/src/storage/thread_mutations.rs` | `ThreadRef` resolution, caller validation, mutation receipts, and typed tri-state icon/showcase updates are coherent (`thread_mutations.rs:57-194`). No second mutable owner or invalid combination was found. |
| `crates/hirsel-host/src/storage/thread_summary.rs` | Running/queued/terminal/activity fields are query-time projections from turns, messages and activities (`thread_summary.rs:20-50`), not duplicate stored facts. Existing chronology and lifecycle-independence tests cover the intended model. |
| `crates/hirsel-host/src/storage/threads.rs` | Row decoding, creation, lifecycle updates, root-only pinning, instrument validation and revision increments were inspected. The positional projection is a maintenance hazard but no current invalid state or material drift was established; the attention fallback is only relevant to an externally corrupted row, since the DDL constrains values. |
| `crates/hirsel-host/src/thread_commands.rs` | Public action construction uses one `current` row for both identities (`thread_commands.rs:123-127,224-236`). Lifecycle dimensions remain independent as required by ADR0016/exclusions. F01 is the downstream generic-boundary gap. |
| `crates/hirsel-host/src/storage/thread_icons.rs` | Omitted/null/value icon patch semantics and validation are explicit (`thread_icons.rs:7-65`); existing icon round-trip, replay and revision tests cover them. Existing #8 is excluded. |
| `crates/hirsel-host/src/storage/thread_icons_tests.rs` | Full file inspected; valid Unicode, invalid values, owner/agent round trips and replay/revision guards are covered. No distinct regression. |
| `crates/hirsel-host/src/storage/thread_pins_tests.rs` | Full file inspected; root-only pin behavior and inert legacy child pins are covered. Existing #12 is explicitly excluded. |
| `crates/hirsel-host/src/storage/thread_summary_tests.rs` | Full file inspected; query-time execution summary, activity chronology, lifecycle/read independence, deletion fallback and timestamp tie-breaking are covered. No stored-summary duplicate was found. |
| `crates/hirsel-host/src/storage/threads_tests.rs` | Full file inspected; create/idempotency, hierarchy, durable turns/activity, lifecycle controls and action receipts are covered. The same-id action fixture is evidence for F01's missing negative case, not a separate finding. |
| `crates/hirsel-host/src/tools/thread_summary_tests.rs` | Full file inspected; unopened execution transitions and factual message/activity recency are covered without conflating lifecycle state. No new defect. |

Shared definitions were also inspected:

- `crates/hirsel-host/src/storage/current.sql:1-11`: `threads`, the
  `threads_parent` index and `thread_action_receipts`; the self-parent check,
  foreign key and root/child storage shape are consistent with the hierarchy
  contract. `thread_requests` lines 12-15 were read as the F01 persistence
  consumer.
- `crates/hirsel-proto/src/thread.rs:5-11`: strict two-state
  `ThreadAttention`; `:13-44`: durable `Thread` plus deliberately independent
  execution/activity projections; `:81-96`: `ThreadBrief` and `ThreadDetail`
  read shapes. No state collapse or duplicate source of truth was found.

### Deliberate skips across consumers and adjacent owners

- `app/src/threads/tree.ts` has seen-set guards for malformed ancestry/cycles;
  creation only chooses an existing parent and there is no normal parent update
  path. No reachable hierarchy cycle was found.
- Web thread navigation/shell/status/actions keep pin, settlement, attention,
  read and execution as independent dimensions. This matches the explicit
  product exclusion; no UI copy or projection issue is reported here.
- `crates/hirsel-client-core/src/store.rs` applies revision ordering to the
  durable Thread while accepting separately changing execution/activity
  projections. That is intentional bounded denormalization, not a second
  owner.
- `crates/hirsel-host/src/storage/thread_read.rs` uses independent bounded
  cursors for messages, turns and activities; this is a read contract, not an
  identity duplicate.
- `crates/hirsel-host/src/tools/threads.rs` republishes durable rows with
  query-time summaries after messages, activities and turns. No stored summary
  write path was found.
- Direct background-request identity handling in
  `crates/hirsel-host/src/storage/thread_activity.rs:47-96` and delegation
  insertion are adjacent-owner surfaces, not owned by this cluster. They were
  read only for context and are not reported or co-owned here.
- Unknown attention text falling through to `Quiet` in
  `crates/hirsel-host/src/storage/threads.rs:31-34` was considered. The table
  has `CHECK(attention IN ('quiet','needs_owner'))` at
  `current.sql:6-8`, all normal writes use `threads::attention` at
  `threads.rs:67-71`, and no reachable bad-row writer was found. This remains
  latent corruption hardening, below the two-finding budget.
- Nullable `client_id` on delegated child rows and legacy child pin data are
  intentional/final-contract cases covered by the exclusions and adjacent
  owners, not invalid Thread identity.

## Audit log and snapshot verification

- Read the prescribed cluster spec and `/tmp/hirsel-combined-audit/exclusions.md`.
- Inspected all 11 assigned files in full, all six shared definitions, and the
  listed host/proto/client/web consumers; expanded only around the F01 identity
  path and related concrete leads.
- Used only read-only `git`, `rg`, `nl`, `sed`, `wc` and text inspection. No
  tests, builds, installs, migrations, application execution, live-data/config
  reads, commits, pushes, or delegation were performed.
- Expected snapshot before audit: HEAD
  `3ee0621a603659ab0168f565b99012b642415419`; tree
  `a4aac830c45398a66591f2c44b707aaf3cef281b`; `git status --porcelain` was
  empty.
- The same HEAD/tree and empty source status were rechecked after inspection
  and report generation. The source checkout is unchanged.

Fix first: **F01 — make `OwnerTurn.thread_id` the sole persisted action owner.**
