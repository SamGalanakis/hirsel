# Product scenario: chat chronology, queue, settle, reload

> Read [../RULES.md](../RULES.md) first.

**Purpose.** Prove that a queued Owner message and both turns' streamed work,
tool calls, results, and replies appear in the order they actually happened.
Settling and reloading must not reorder or duplicate anything.

**Real model calls:** two.

## Golden rules

1. Queue the second message only after the first turn is visibly running.
2. The accepted second request immediately shows its own quiet **Queued** row,
   beside that Owner message, and survives reload before activation.
3. The second turn's working row starts after the first reply and therefore
   renders below that reply. The already-queued Owner message may correctly
   precede the first reply because the Owner sent it earlier.
4. Reasoning, progress, tool invocation/result, and reply stay inline at their
   event positions. There is no turn-wide `Work details` gate.
5. Completed tool rows retain their distinct command/subject while collapsed
   and state the outcome in plain language. Expanded shell results lead with
   stdout/stderr and retain the exact wire envelope under **Raw result**.
6. After both turns settle, reload preserves the full per-turn event sequence:
   stable event/call identities, ordering, kind, and available payload—not only
   the outer message and turn IDs.

## Phase 0 — start empty

**Do:** Run `just product-runbook chat-chronology`. The runner boots an empty
owned Host and creates one Thread named with the run nonce.

**Expect:** `00-empty.png` shows the selected empty Thread, and DOM, wire, and
SQLite extracts contain no messages or turns for its ID.

## Phase 1 — queue during real work

**Do:** Send the first marker prompt. It requires a short real `shell.run`, so
the runner can poll the visible running state. While it is running, send the
second marker prompt through the same composer.

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

**Do:** Poll both turns terminal, with the two exact final markers visible;
then reload the Thread.

**Expect:** one Owner and one Agent message per turn, two distinct turns, no
duplicate reply, and the same ordered reasoning/progress/tool/reply timeline
before and after reload. Compare stable event and call identities plus every
available payload; equality of only the four message IDs is insufficient. Save
`30-settled.png`, `31-reloaded.png`, and `result.json`.

## Scorecard

| Item | Objective gate | Verdict | Evidence |
|---|---|---|---|
| Empty scope | selected Thread has zero messages/turns on all three surfaces | | `00-*` |
| Real queue | second send accepted while first turn is visibly running | | `10-*`, frames |
| Queued reload | queued row and exact ID survive reload before activation | | `11-queued-reloaded-*` |
| Handoff chronology | reply one precedes turn two's working row | | `20-handoff.png`, DOM IDs |
| Inline work | turn events are visible without a whole-turn disclosure | | `20-*`, `30-*` |
| Tool identity | collapsed rows retain distinct subjects and plain outcomes; expanded result leads with payload and keeps Raw result | | `30-*`, `31-*` |
| Settled identity | two Owner + two Agent messages and two terminal turns agree across surfaces | | `30-*` |
| Reload timeline | event/call identities, order, kind, and available payload are equal before and after reload | | `30-*`, `31-*`, `result.json` |

**Aggregate:** did the conversation remain a truthful chronological timeline
through queueing, handoff, settlement, and reload?
