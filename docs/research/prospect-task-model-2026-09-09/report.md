# Prospect: make Tasks durable work, separate from activity

2026-09-09. Design recommendation, not an implementation decision. Reference: local checkout of [pingdotgg/t3code at `6c583620`](https://github.com/pingdotgg/t3code/tree/6c583620ff7ad3235b135af7107c0543467eecfa). Scope: work identity, attention/lifecycle, and creation-to-client visibility. Hirsel was inspected in the working tree with the Lash upgrade present.

## Verdict

**Give Task its own producer and client contract. Treat events as things that happen, rather than categories of work.** Hirsel already defines a durable Task in its product language. The implementation still makes notification categories determine whether work appears, needs attention, and can be cleared. The recommendation brings those contracts into agreement.

For “add a task for buying groceries,” successful creation should mean: a Task ID was persisted, the same ID appears in the live inventory and after reconnect, and it remains open until an explicit settlement action. No artificial decision or digest is needed.

## What T3 Code actually does

T3's comparable durable entity is a **thread**, not a demonstrated equivalent of Hirsel's Task. Its [`thread.create`](https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/decider.ts#L352-L384) creates identity; [`thread.activity.append`](https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/decider.ts#L1745-L1771) requires an existing identity. The initial [thread projection](https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/ProjectionPipeline.ts#L597-L632) has no latest turn, pending approval, pending question, or actionable plan. Quiet work still exists.

Its shell queries and reducers operate on those entities. Activity and attention classify existing work. Source tests cover both [snapshot membership](https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/ProjectionSnapshotQuery.test.ts#L1009-L1025) and [new live membership amid other traffic](https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/server.test.ts#L9721-L9728). Those tests are useful layered examples, not proof of natural-language tool selection.

## Verified improvement batch, ranked

| Priority | Current gap | Recommended outcome | Independent convergence |
| --- | --- | --- | --- |
| 1 | The Agent chooses judgment, FYI, or digest when creating work. Info is hidden; Summary is visible but retains awareness cleanup rules. | Explicit `tasks.create` returns durable work identity independently of attention or content tone. | Reference reader, doctrine review, contract review |
| 2 | Read, open Summary becomes eligible for Clear finished; the host then archives it as Done. Undo only unarchives it. | Read never makes work finished. Bulk clear operates on explicitly Done Tasks and returns the actual affected IDs for accurate Undo. | Doctrine review, contract review |
| 3 | Agent `events.recompose` is restricted to the current generated-action Task and cannot change kind/response classification. | Agent can update a named Task's instrument and attention during ordinary orchestration while retaining ID, Anchor, and settlement. | Doctrine review, contract review |
| 4 | Host, web selectors, and native projection prove different local contracts. Structured message mentions are also missing from persisted/broadcast conversation. | Test the real producer-to-inventory path, complete native Task projection, and durable Task references in conversation. | Doctrine review and contract review converge on client coverage; contract review identifies the attribution seam |

Evidence and exact local source lines are in [doctrine-review.md](doctrine-review.md) and [contract-review.md](contract-review.md). Important qualifications: Summary can create non-decision work today; merely reading does not immediately complete it. The action-only restriction concerns the Agent tool, not all internal storage callers. Single-Task Anchor attribution still works; the separate gap concerns structured cross-Task mentions.

The host already persists Event creation before broadcasting its upsert and snapshots durable Events. Keep that behavior; T3's transaction/publication ordering is corroborating design evidence, not a newly discovered missing Hirsel feature.

## Recommended model

| Concept | Meaning | Effect on the Task inventory |
| --- | --- | --- |
| Task | Durable work with ID, Anchor, description, generated instrument, and Open/Done settlement | Creates a stable inventory member; explicit snooze/archive govern presentation |
| Attention | Mutable Task state: quiet or needs Owner, with blocking information where useful | Changes emphasis and the needs-you count; does not create or complete work |
| Activity/event | Something happened: progress, information, a result, or a Task change, optionally referring to a Task | Updates context; does not allocate another Task implicitly |

Read/unread is observation, independent of both attention and settlement. Process remains execution, independent of all three. Keep one global Agent and conversation, with Task margins selected by durable Anchor and persisted mentions.

Agent-facing creation/update should be explicit:

```text
tasks.create(description, instrument?, attention?) -> task_id
tasks.update(task_id, description?, instrument?, attention?) -> same task_id
```

Creation defaults to Open and quiet, with a minimal valid instrument. Host-owned Anchor resolution remains authoritative. Update cannot change settlement or identity. Existing Owner action handling retains explicit continue/complete/reopen intent and generated-action target validation. This proposal does not grant the Agent a new implicit completion authority.

“What groceries?” becomes attention on the same Task; answering can clear attention while leaving the work Open. A shopping-list update changes that Task's instrument. Marking it done settles it. Reading any of these leaves settlement unchanged.

Use the existing conversation/activity channel for FYIs and summaries, with a Task reference when relevant. This does not require a new activity database, separate inbox, or event-sourcing architecture. A first-class Task wire/domain type is the recommended boundary; physical table naming is secondary. Retire overlapping legacy producer semantics as part of the eventual cutover, rather than leaving two equally authoritative ways to create work.

## Proof required for implementation

1. Create ordinary groceries through the actual tool executor. Require the returned ID in storage, a live upsert, a fresh snapshot, and web/shared-native inventory projections.
2. Read it, converse about it, emit unrelated FYIs, reconnect, and run Clear finished. It stays Open. Explicitly completed work clears, with the affected IDs and Undo agreeing across host and client.
3. Change quiet → needs Owner → quiet from ordinary Agent turns. Preserve ID and Anchor; continuing a generated stage stays Open; explicit complete/reopen follows existing authority.
4. Send a message citing Tasks A and B through the host. Both live delivery and replay preserve those references and margin membership. Native actions retain the Task identity and structured action rather than sending only text.

## Decisions and exclusions

This realizes existing Task identity and explicit-settlement decisions. It changes ADR-0012's remaining typed-Event storage/wire basis and awareness-dismissal behavior. Reconcile ADR-0004's historical absolute no-Task-table wording with its current product clarification while retaining its rejection of Host workflow/retry machinery. Agent updates outside generated-action turns are an explicit extension of the producer contract. Preserve current archive-implies-Done behavior, but remove read-based completion; independent archiving of open work would be a separate product choice.

Considered, not adopted: revealing all Info rows (would admit housekeeping); using Summary for all ordinary work (retains the unsafe lifecycle); creating a new Task whenever attention changes (breaks continuity); T3 thread-per-session conversations, automatic inactivity/PR settlement, sidebar shelves, Effect, and event sourcing (unnecessary or conflicting with Hirsel's decisions).

The [exclusion map](exclusions.md) records current ADRs, prior design work, and tracker discovery. No open GitHub issue or Hirsel-matching open Linear issue was found; Linear discovery was scoped to Hirsel searches, not the entire shared workspace. Existing native feature lag is documented, so this report requires data/command consistency for the cutover rather than proposing a new mobile architecture.

## Verification record

Separate reader, doctrine, and undercovered-contract reviews produced the findings. Independent verifiers checked source paths, line ranges, characterizations, and limitations: [T3 verification](verification-t3.md), [Hirsel verification](verification-hirsel.md). Reference tests were inspected, not executed; this research adds no claim that T3's whole suite passes. No application code, live Tasks, or tracker items were changed for this prospect.
