# Independent T3 source verification

2026-09-09. Read `exclusions.md`, then independently read every T3 source range in `t3-mechanisms.md` plus relevant helpers and test setup. Local `/tmp/ref-t3code` HEAD is `6c583620ff7ad3235b135af7107c0543467eecfa`. No app or tests were run; no implementation or runtime state was changed. This verifies T3 evidence only, not Hirsel findings or external issue-discovery counts.

## Passed

- All pinned references resolve to the quoted source ranges. The glossary/doctrine, quiet creation projection, separate shell attention/lifecycle fields, SQL inventory filter, shared ID index and reducer, and sidebar classification support their stated narrow claims.
- Persistent projection writes occur before publication: engine transaction `OrchestrationEngine.ts:250–318`; `ProjectionPipeline.ts:1946–1972` executes projectors/cursor writes before returning deferred attachment cleanup. The socket upsert/removal commentary and implementation match this ordering. This source inspection establishes ordering, not delivery guarantees across crashes.
- Snapshot test `ProjectionSnapshotQuery.test.ts:933–1025` directly inserts a settled, unarchived projection row and asserts shell/read-model membership and settlement fields. It is a query test, not creation-to-client integration.
- Replay socket test injects 20 busy-thread events and one new-thread event; assertions require both IDs, one busy-thread fetch, and replay limit 50. Live test injects the same burst after synchronization and requires both IDs with fewer than 20 busy-thread fetches. Both use engine/query doubles. Reducer test separately verifies insertion and sequence advancement.
- Automatic settlement is confirmed by `ThreadSettlementPolicy.ts` and `ThreadSettlementReactor.ts:60–83`, subject to settings and eligibility checks. No automatic-settlement policy is recommended for Hirsel.
- `contract-review.md` T3 claims also pass: `projector.ts:320–365` creates identity with null latest turn/session and empty messages/activities; `projector.test.ts:43–101` asserts the cited quiet fields. This is an in-memory projector test. `projector.ts:379–398` changes archive metadata without settlement fields (archive also clears title regeneration). `uiStateStore.ts:22–47,426–445` separates visit preferences; `214–269,445–474` confirms localStorage persistence and UI-only visit updates. Do not generalize web-local persistence into a claim about every T3 client.

## Corrections applied to the T3 brief

1. Creation does not require an ID absent from all history: `commandInvariants.ts:154–172` allows reusing a soft-deleted ID. The brief now says no non-deleted thread with that ID. Activity requires a record; this helper does not require that record to be unarchived or undeleted.
2. Rollback test uses a fake projection pipeline (`OrchestrationEngine.test.ts:1558–1592`). Its real SQLite event-store assertions establish rollback/retry for turn start; they do not assert projection-row rollback, socket silence, or failed thread creation. Added the setup citation and limit.
3. Automatic settlement wording now states eligibility/settings conditions. Extended attention-wake citation through `decider.ts:1796`, including the emitted `thread.unsettled` and returned event pair; this occurs when an approval/input request meets a non-null settlement override.

## Limits

No inspected test proves natural-language/provider command choice, full creation-to-sidebar convergence, or Hirsel's groceries path. Those are proposed acceptance requirements. Separate durable entity inventory from attention; preserve Hirsel's existing persistence-before-upsert ordering rather than claiming that ordering is newly missing. The contract-review T3 paragraphs were verified but not edited.
