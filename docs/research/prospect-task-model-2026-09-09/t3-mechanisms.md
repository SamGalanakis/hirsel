# T3 Code: durable work and visibility mechanisms

Read-only source inspection, 2026-09-09. Read `exclusions.md` first. Reference checkout `/tmp/ref-t3code`, verified HEAD `6c583620ff7ad3235b135af7107c0543467eecfa`. Tests below were inspected, not executed. Paths and line ranges refer to this checkout; linked sources are pinned to that commit.

## Domain boundary

T3 defines a **thread** as durable project conversation/work history and a **turn** as one user-to-agent cycle ([`AGENTS.md:54–56`][glossary]). This is an analogy for durable identity, not evidence that T3 implements Hirsel's first-class Tasks. Its written doctrine favors the smallest sufficient model, requires all clients/entry points to follow wire contracts, and calls for visible reverse transitions ([`AGENTS.md:33–37`][taste], [`AGENTS.md:67–74`][surfaces]).

## Concrete mechanisms

1. **Creation and activity are different commands with different invariants.** `thread.create` requires an existing project and no non-deleted thread with that ID, then emits `thread.created`; it permits recreating a soft-deleted ID. `thread.activity.append` requires an existing thread record. An activity therefore cannot stand in for creating the durable object. Sources: [`apps/server/src/orchestration/decider.ts:352–384`][create], [`1745–1771`][activity], [`commandInvariants.ts:154–172`][creation-invariant]. Hirsel delta: a work-creation tool should have a typed durable Task result; notification emission must not be a successful substitute for work creation. This enforces the already settled Task concept rather than introducing a host workflow abstraction.

2. **Persisted identity exists without a question, running agent, or activity.** `thread.created` immediately upserts the thread projection with its ID/title and empty attention state: no latest turn, zero pending approvals/user inputs, no actionable plan. Sources: [`apps/server/src/orchestration/Layers/ProjectionPipeline.ts:597–632`][projection]. The shell contract carries lifecycle separately from pending input/approval flags and background liveness ([`packages/contracts/src/orchestration.ts:692–731`][shell]). Hirsel delta: an open, informational, quiet Task must remain an inventory member; attention describes that member rather than deciding whether it exists.

3. **Commit precedes publication; clients receive projected entity state.** Engine dispatch appends events, applies persistent projections, and records its accepted command receipt inside one SQL transaction. It publishes events only after that transaction returns, then returns the committed sequence ([`apps/server/src/orchestration/Layers/OrchestrationEngine.ts:250–318`][transaction]). The socket layer converts a thread change into a current shell upsert or explicit removal; its comment relies on projection-before-publication to distinguish absence from an uncommitted insert ([`apps/server/src/ws.ts:839–875`][wire]). The shared client reducer upserts by thread ID and rejects stale sequence updates ([`packages/client-runtime/src/state/shellReducer.ts:12–42`][reducer]). Hirsel delta: create/update should commit the durable Task before emitting its client update, and tool success should name that durable Task. A SQLite row plus typed upsert can satisfy this; adopting an append-only event architecture is unnecessary.

4. **Inventory and presentation state have separate selectors.** Active shell SQL excludes deleted/archived rows, not settled rows or rows without actionable content ([`apps/server/src/orchestration/Layers/ProjectionSnapshotQuery.ts:529–567`][query]). Shared atoms index snapshot threads directly by ID ([`packages/client-runtime/src/state/threadShell.ts:58–81`][index]). The web sidebar partitions those thread entities into pinned/active/snoozed/settled; its default branch retains each row as active ([`apps/web/src/components/Sidebar.tsx:2474–2527`][sidebar]). The distinction to adopt is entity inventory versus view classification. Do not import the shelves or T3's lifecycle states into Hirsel by default.

## Tests worth translating

- **Snapshot membership:** a persisted settled thread must remain in the live shell snapshot with its settlement fields. Actual assertions: [`ProjectionSnapshotQuery.test.ts:1009–1025`][snapshot-test]. Hirsel counterpart: open/done Task identity remains in the appropriate inventory after fresh snapshot/reconnect, including informational Tasks.
- **Live membership amid noise:** replay coalescing emits the new thread along with a busy existing thread; a second test asserts this after the live synchronization marker. Actual assertions: [`apps/server/src/server.test.ts:9617–9627`][replay-test], [`9721–9728`][live-test]. Shared client tests also assert a new shell upsert adds its ID ([`shellReducer.test.ts:125–137`][reducer-test]). Hirsel counterpart: create groceries through the authorized tool, observe that same ID in both a fresh snapshot and the live client's real Task selector, then emit unrelated notifications and verify membership remains.
- **No partial success:** an injected projection-pipeline failure during a multi-event turn-start command leaves only preexisting events; retry produces exactly the intended two new events ([`OrchestrationEngine.test.ts:1645–1675`][rollback-test]). This test uses a fake projection pipeline ([`1558–1592`][rollback-setup]); its assertions establish event-store rollback and retry, not actual projection-row rollback or absence of socket publication. Hirsel counterpart: failed creation cannot report success or leave only a notification masquerading as the Task.

These are layered proofs, not an inspected end-to-end natural-language-to-sidebar test. The socket tests inject engine/query doubles. Do not claim they prove the provider chooses the right command; Hirsel must add that tool-facing contract explicitly.

## Weaknesses and things not to copy

- T3 supports automatic settlement after inactivity/PR closure, subject to policy settings and eligibility guards. That would change Hirsel's explicit open/done settlement ruling and should not be imported ([`docs/user/thread-sidebar.md:75–92`][settlement-doc]). Activity can also generate an un-settle transition for a pending approval/question when a settlement override is present ([`decider.ts:1773–1796`][attention-wake]); its separation of dimensions is useful, but it does not keep them behaviorally independent.
- Its durable thread includes conversation, provider/session choices, branches/worktrees, and many presentation overrides. Hirsel keeps one global Agent and flat Tasks; copying a thread per Task would alter its settled product boundary. The extra optional compatibility fields in the contract are not a target data model ([`packages/contracts/src/orchestration.ts:610–664`][full-thread]).
- T3's sidebar itself is not a guarantee against all disappearance: project filters and archived state intentionally exclude rows. The narrower verified claim is that ordinary informational activity is not the source of inventory membership ([`Sidebar.tsx:2474–2527`][sidebar]).
- Receipt/sequence machinery is justified by T3's event-sourced server. Retain the simpler invariant in Hirsel; do not adopt Effect, reactors, replay, automatic settlement, or additional destinations to obtain it.

## Ranked adoptable mechanisms

1. Separate typed durable Task creation from activity/notification emission; creation must return the Task identity.
2. Make Task inventory membership depend on durable lifecycle, independently of attention, response affordances, or the latest event kind.
3. Commit Task state before publishing its typed client upsert; make fresh snapshots and live updates converge on the same entity shape and selector.
4. Enforce the groceries path with creation-to-snapshot/live-selector assertions, including quiet/info-only work, notification bursts, reconnect, explicit settlement, and failed creation. Keep behavioral tests scoped to the actual gaps.

[glossary]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/AGENTS.md#L54-L56
[taste]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/AGENTS.md#L33-L37
[surfaces]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/AGENTS.md#L67-L74
[create]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/decider.ts#L352-L384
[activity]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/decider.ts#L1745-L1771
[projection]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/ProjectionPipeline.ts#L597-L632
[shell]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/packages/contracts/src/orchestration.ts#L692-L731
[transaction]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/OrchestrationEngine.ts#L250-L318
[wire]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/ws.ts#L839-L875
[reducer]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/packages/client-runtime/src/state/shellReducer.ts#L12-L42
[query]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/ProjectionSnapshotQuery.ts#L529-L567
[index]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/packages/client-runtime/src/state/threadShell.ts#L58-L81
[sidebar]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/web/src/components/Sidebar.tsx#L2474-L2527
[snapshot-test]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/ProjectionSnapshotQuery.test.ts#L1009-L1025
[replay-test]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/server.test.ts#L9617-L9627
[live-test]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/server.test.ts#L9721-L9728
[reducer-test]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/packages/client-runtime/src/state/shellReducer.test.ts#L125-L137
[rollback-test]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/OrchestrationEngine.test.ts#L1645-L1675
[settlement-doc]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/docs/user/thread-sidebar.md#L75-L92
[attention-wake]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/decider.ts#L1773-L1796
[full-thread]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/packages/contracts/src/orchestration.ts#L610-L664
[creation-invariant]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/commandInvariants.ts#L154-L172
[rollback-setup]: https://github.com/pingdotgg/t3code/blob/6c583620ff7ad3235b135af7107c0543467eecfa/apps/server/src/orchestration/Layers/OrchestrationEngine.test.ts#L1558-L1592
