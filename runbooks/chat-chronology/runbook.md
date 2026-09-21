# Product scenario: chat chronology, queue, settle, reload

> Read [../RULES.md](../RULES.md) first.

**Purpose.** Prove that a queued Owner message and both turns' streamed work,
tool calls, results, and replies appear in the order they actually happened.
Settling and reloading must not reorder or duplicate anything.

**Real model calls:** two.

## Golden rules

1. Queue the second message only after the first turn is visibly running.
2. The second message is sent with the composer's visible **Send after current
   turn** action, which stands beside Stop while the first turn runs. The
   accepted request immediately rests as its own quiet run card marked
   `queued`, beside that Owner message, and survives reload before activation.
3. The second turn's working row starts after the first reply and therefore
   renders below that reply. The already-queued Owner message may correctly
   precede the first reply because the Owner sent it earlier.
4. Reasoning, the Agent's program cells, tool invocation/result, and the reply
   stay inline at their event positions inside the turn's run card. No
   disclosure stands over a whole turn's trace.
5. A finished turn rests as a closed run card; reopening the card shows its
   trace. With its payload still closed, each tool row retains its distinct
   command/subject and states the outcome through its own status mark,
   announced as `ok` or `failed`, never as a transport status. An opened shell
   result leads with stdout/stderr and retains the exact wire envelope under
   **Raw result**. One step's payload is open at a time within a turn; rows in
   different turns open independently.
6. After both turns settle, reload preserves the full per-turn event sequence:
   stable event/call identities, ordering, kind, and available payload—not only
   the outer message and turn IDs. How long a step took is the one thing the
   Host does not own: the client measures it between the arrival of that step's
   own start and done frames, so a replayed trace carries no clock and shows
   none. A reloaded step row must therefore read exactly as the live row did
   minus that one duration, and never a changed subject, outcome or payload.
7. Each reasoning phrase the turn emitted is rendered once, with its own block
   boundary intact. This is the same gate `artifact-presentation` states, and
   the same [Ascending-AI/lash#1769](https://github.com/Ascending-AI/lash/issues/1769)
   Native blocker applies to it: a Native turn that emits two or more reasoning
   summaries fails here until the Lash pin carries block identity. Hirsel does
   not work around it and the gate is not loosened.

## Phase 0 — start empty

**Do:** Run `just product-runbook chat-chronology`. The runner boots an empty
owned Host, opens `/`, and creates one Space chat named with the run nonce.

**Expect:** the route-free open lands in the client-created ordinary **Home** Space chat
with the composer's one recipient label shown; the new Space
chat's composer reads `Message Space chat <name>`. `00-empty.png` shows the
selected empty Thread, and DOM, wire, and SQLite extracts contain no messages
or turns for its ID.

## Phase 1 — queue during real work

**Do:** Send the first marker prompt. It requires a short real `shell.run`, so
the runner can poll the visible running state. While it is running, send the
second marker prompt through the same composer's **Send after current turn**
action; Stop stays separate.

**Expect:** both Owner messages have distinct durable IDs; the Thread's
authoritative queue count increases while the first turn remains the only
running turn. The second request's already-durable turn ID arrives immediately
in `queued` state and a visible **Queued** row is placed beside its message.
Reload before activation preserves that row and exact ID. `10-queued.png` and
`11-queued-reloaded.png` show the live and reloaded states.

## Phase 2 — handoff ordering

**Do:** Poll until turn one is terminal and turn two is running.

**Expect:** the first reply is visible before the second turn's working row.
That working row carries turn two's stable ID. Its inline timeline contains the
events emitted so far. Save `20-handoff.png` and the DOM/wire/store extracts.

## Phase 3 — settle and reload

**Do:** Poll both turns terminal, with the two exact final markers visible.
Reopen both run cards, read the collapsed tool rows, then open each call's
payload — the two calls live in different turns, so both
panels stand open — then reload the Thread and open them again.

**Expect:** one Owner and one Agent message per turn, two distinct turns, no
duplicate reply, and the same ordered reasoning/progress/tool/reply timeline
before and after reload. Compare stable event and call identities plus every
available payload; equality of only the four message IDs is insufficient. Save
`30-settled.png`, `31-reloaded.png`, and `result.json`.

## Scorecard

| Item | Objective gate | Verdict | Evidence |
|---|---|---|---|
| Space chat landing | route-free open lands in Home with its recipient labelled | | `00-*`, `result.json` |
| Empty scope | selected Thread has zero messages/turns on all three surfaces | | `00-*` |
| Real queue | second send accepted while first turn is visibly running | | `10-*`, frames |
| Queued reload | queued row and exact ID survive reload before activation | | `11-queued-reloaded-*` |
| Handoff chronology | reply one precedes turn two's working row | | `20-handoff.png`, DOM IDs |
| Inline work | turn events are visible without a whole-turn disclosure | | `20-*`, `30-*` |
| Tool identity | collapsed rows retain distinct subjects; the status mark announces `ok`; the open panel leads with payload and keeps Raw result | | `30-*`, `31-*` |
| Settled identity | two Owner + two Agent messages and two terminal turns agree across surfaces | | `30-*` |
| Reasoning integrity | each reasoning phrase renders once with its block boundary intact | | `30-*`, `result.json` |
| Reload timeline | event/call identities, order, kind, and available payload are equal before and after reload, a step row differing only by the client-measured duration | | `30-*`, `31-*`, `result.json` |

**Aggregate:** did the conversation remain a truthful chronological timeline
through queueing, handoff, settlement, and reload?

## Judged run — 2026-09-20

Source `ed16130` (runbook/runner edits in the tree), provider `codex`, model
`gpt-5.6-sol` reasoning variant `medium` from `hello_ok`, two model turns.
Evidence: `chronology-run4/chat-chronology`. Objective result: **ABORT** at the
settled tool-identity gate. Judged verdict: **incomplete — no product fault
found in what ran**.

| Item | Verdict | What passed it |
|---|---|---|
| Space chat landing | PASS | route-free open resolved to `/t/1`, `main[data-thread-id="1"]` named Home; the composer named Home as recipient with no worker (`result.json` `landedSpaceChatId: 1`) |
| Empty scope | PASS | `00-empty-dom.json` `entries: []`; `00-empty-thread.json` no messages/turns; `00-empty-store.json` no rows, `schemaVersion 14` |
| Real queue | PASS | second message sent with the composer's `Send after current turn` control while `Stop the agent` was present and turn 1 was still `running`; `thread_upsert.queued_turn_count` rose before any terminal turn frame |
| Queued reload | PASS | `11-queued-reloaded-dom.json` entry `data-execution-turn="2"`, `data-outcome="queued"`, run-card header accessible name `queued`, visible word `queued`; `turns[1] = {owner_message_id: 2, state: "queued"}` on the `open_thread` snapshot and in SQLite |
| Handoff chronology | PASS | `20-handoff-dom.json` index 2 is agent message 3 (`HIRSEL-CHAT-FIRST-…`), index 3 is the running turn 2 — the reply precedes the newer working row |
| Inline work | PASS | every entry `traceGated: false` in `10-*`, `20-*`, `30-*`; the running turn shows its steps in place |
| Tool identity | NOT PROVEN | the run aborted here: a settled run card rests closed, so the collapsed rows were not in the DOM when they were read. Runner fixed to reopen the cards first; not re-run within the model budget |
| Settled identity | PASS | `30-settled-store.json` two Owner and two Agent messages, turns 1 and 2 both `completed`; agent bodies are exactly the two markers |
| Reload timeline | PARTIAL | live frames, the `open_thread` timelines and the SQLite `thread_turn_events` rows are identical and strictly ordered for both turns (5 events each: reasoning, code_start, tool_start, tool_done, code_done). The browser before/after-reload comparison was not reached |

**Aggregate.** Everything the run reached was a truthful chronological
timeline, and the queued state was honest on all three surfaces before and
after reload. The abort was a stale runner expectation, not a product fault.

## Judged run — 2026-09-21

Source `227d48e` (two unrelated runbook files carried judged-run text in the
tree; the runner and this scenario were clean), provider `codex`, model
`gpt-5.6-sol` reasoning variant `medium` from `hello_ok`, two model turns after
a runner fix — four for the day, the first two lost to the runner opening only
one run card. Evidence: `runbook-evidence-2/chronology-run2/chat-chronology`.
Objective result: **OBJECTIVE_PASS**. Judged verdict: **pass — the conversation
stayed a truthful chronological timeline through queueing, handoff, settlement
and reload**.

| Item | Verdict | What passed it |
|---|---|---|
| Space chat landing | PASS | `result.json` `landedSpaceChatId: 1`; the route-free open resolved to `/t/1` and the composer's context row read `Recipient` / `Home`, with the new Space chat's composer named `Message Space chat Runbook chatchro-868fa8a7` |
| Empty scope | PASS | `00-empty-dom.json` `entries: []`, `00-empty-thread.json` no messages or turns, `00-empty-store.json` no rows at `schemaVersion 18` |
| Real queue | PASS | the second message was sent with the composer's `Send after current turn` control while `Stop the agent` stood beside it and turn 1 was still `running`; `thread_upsert.queued_turn_count` rose before any terminal turn frame |
| Queued reload | PASS | `11-queued-reloaded-dom.json` entry 3: `data-execution-turn="2"`, `data-outcome="queued"`, run-card header accessible name `queued`, visible text `·queued` — after a reload, and with turn 1 still `running` in entry 1. The same `{owner_message_id: 2, state: "queued"}` stands on the `open_thread` snapshot and in SQLite |
| Handoff chronology | PASS | `20-handoff-dom.json` order is Owner 1, Owner 2, Agent message 3 (`HIRSEL-CHAT-FIRST-…`), then the running turn 2 row — the older reply precedes the newer working row, and the queued Owner message correctly precedes both |
| Inline work | PASS | every entry `traceGated: false` in `10-*`, `20-*`, `30-*`; the running turn showed its steps in place |
| Tool identity | PASS | both settled cards reopened and kept distinct subjects: `shell_run cmd: sleep 8; printf 'HIRSEL-CHAT-FIRST-chatchro-868fa8a7'` and `… 'HIRSEL-CHAT-SECOND-…'`, each `statusLabel: "ok"` with no `ok status 0`. Each open panel leads `Output\n<marker>\n\nExit status\n0`, then `Input`, and keeps the bounded wire envelope under `Raw result` |
| Settled identity | PASS | two Owner and two Agent messages, turns 1 and 2 both `completed`, on DOM, `open_thread` and SQLite alike; the Agent bodies are exactly the two markers |
| Reasoning integrity | PASS | one reasoning block per turn — `Confirming single shell.run execution` and `Planning variable naming for call result` — each rendered once. Two single-block turns, so [lash#1769](https://github.com/Ascending-AI/lash/issues/1769) did not bite; it remains a blocker for any turn that emits two or more summaries |
| Reload timeline | PASS | live frames, the `open_thread` timelines and the SQLite `thread_turn_events` rows are identical and strictly ordered for both turns (5 events each: reasoning, code_start, tool_start, tool_done, code_done). The browser projection was finally compared too: every entry, row identity, status mark, open state and payload is equal before and after reload, and the only text that changed is the four measured durations (`8.2s`, `8.0s`, `3.1s`, `3.0s`), which the client times from its own frames and a replayed trace does not carry |

**Aggregate.** Yes. Nothing reordered, nothing duplicated, and everything the
Host owns came back identical after a reload.
