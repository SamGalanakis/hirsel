# Task state

Read [the runbook rules](../RULES.md). Run `just product-runbook task-state` only against a fresh store, unused loopback port other than 3076, matching production frontend and the configured real provider. Budget: one Space-chat coordination turn and one worker turn. Never retry a model call to pass.

## Owner-visible outcome

A Task rests with its material state above the conversation: a normalized headline of at most 12 words, findings and its explicitly referenced artifacts. The worker updates state through `threads.state`; it does not edit the instrument or complete the Task. The owning Space shows a deterministic child-count rollup and remains open until the Owner explicitly completes it.

## Scenario

1. Land in Home, create one child Task and record both initial state revisions.
2. Ask the Space chat to delegate one bounded evidence-gathering job to that Task. The worker must publish one small Markdown artifact and checkpoint a unique headline plus two unique findings and that artifact.
3. While the worker runs, verify the Task and owning Space receive live state updates. The Space headline must name the direct child and a Host-derived status, not copy the worker headline.
4. Open the Task. Verify the state card precedes the conversation, shows the exact normalized headline and findings, opens the referenced artifact, and displays the material revision. Verify the Task and Space remain incomplete.
5. Ask the worker to edit the artifact once. Verify its revision and the referencing Task-state revision each advance, with no Owner read or telemetry action advancing either material state.
6. Create a second Space with existing reach to the Task. Change the Task from the first Space. Verify the second Space chat gets one coalesced “Changed by <Space>” line but no automatic turn. On its next explicit message, verify the accepted context contains the bounded before/after digest. Revoke reach before a later admission and verify the later content is absent.
7. Reload. Verify the authenticated `hello_ok`, `open_thread`, rendered state card and SQLite rows agree on state, artifact IDs, revisions, change causes and the latest accepted context.

## Evidence and verdict

Retain sanitized screenshots before and after reload, authenticated frames, `open_thread` snapshots, and exact rows from `thread_state`, `thread_state_artifacts`, `thread_state_changes`, `thread_change_deliveries`, `thread_change_cursors`, `thread_turn_contexts`, `artifacts` and `thread_effect_receipts`. The `threads.state` operation must have one `edited` receipt. Do not run another model turn merely to obtain digest evidence. Record `OBJECTIVE_PASS` only for deterministic predicates; a reviewing Agent separately judges that the headline is useful, findings are material, rollup prose is factual, and no parent was automatically completed.
