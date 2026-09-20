# Space chats, material state and coordination delivery

Accepted 2026-09-20.

## Space-chat and worker roles

Decision recorded 2026-09-20: a Space's own Thread conversation is a Space chat. This applies to top-level and nested Spaces. The Owner lands in a top-level Space chat, and Space chats coordinate and dispatch work to Tasks. A Task's conversation is where its worker does the work. The Host derives one factual role line from the Thread's current kind and appends it to the Owner-overridable Agent prompt.

These roles are guidance and presentation, never Host enforcement. Every Thread has the universal tool surface from ADR 0023, and any Thread may select Native, Claude CLI or Codex CLI. Changing a Thread between Space and Task changes the next identity block; it does not change tools, rotate a session, reinterpret accepted execution settings or confer authority.

Reach, grants and root under ADR 0022 remain the only authority model. A role line, focus snapshot, tool availability, evidence or successful execution never widens reach or supplies authorization.

The Space chat dispatches through the atomic `threads.delegate` operation and does not perform the delegated work itself. It resolves the intended Task before acting, asks one short question when no confident target exists, and needs no Task for plain conversation. Spending, external commitments and irreversible choices remain Owner decisions.

## Landing, recipient, focus and pairing

Route-free entry restores `hirsel.last-project.<history_id>` when it still names an active top-level Space. Otherwise the web client calls the idempotent `EnsureHomeProject { client_id, history_id }` operation. The Host atomically creates one ordinary positive-ID top-level Space named Home, records it as `meta['project_chat:home_thread_id']`, and creates no root grant. Explicit malformed, missing and old-history links retain their refusal behaviour and never fall back to Home. The internal wire and storage names are compatibility names, not Owner vocabulary.

The client keeps three facts separate and labels all three at the composer:

- `projectRecipientId`: the top-level Space whose Space chat receives a message;
- `taskFocus`: an optional explicit Task snapshot attached to the next Space-chat message;
- `workerPairingId`: the Task whose worker the Owner has stepped into.

Navigation remains `threads/store.ts::focusedId`; it grants none of the three meanings above. Drafts remain keyed by history and the actual message recipient.

“Talk about this” on a Task opens its owning Space chat and stages `TaskFocus { task_thread_id, snapshot }`. The snapshot is bounded title, brief and current instrument summary, not a transcript. It is stored atomically with the Owner message in `message_task_focus` and replayed into the accepted input. The Host rechecks that the Task is within the Space chat's existing reach; focus never widens reach.

“Step in” opens a Task's own conversation and addresses that worker. While work is running, the visible send action says “Send after current turn”; Stop remains separate.

## Storage and protocol

Schema 14 added `message_task_focus(message_id PK/FK chat_messages, task_thread_id FK threads, snapshot_json TEXT NOT NULL CHECK object)`. `TaskFocus` is optional on `SendThreadMessage` and `ChatMessage`. Client-core carries it through pending state, retry, reconnect and confirmed snapshots; FFI callers that do not support focus send `None`.

## Durable effects

Schema 15 adds `thread_effect_receipts`. Every accepted-turn Thread or artifact
create, send, delegation, read, edit or refusal records a receipt in the same
transaction as its effect. Identity is `(turn_id, operation_id, effect_index)`:
two actual probes remain two facts, while replay of one operation does not
duplicate either its activity or receipt. Targets are closed Thread, artifact
or root variants and preserve an attempted inaccessible identifier. Refusal
details exist exactly for refused effects.

`ThreadDetail.effects` reloads receipts for its bounded turn page and
`ThreadEffectsChanged` publishes a complete live projection for one source
turn through the existing turn-ingest sequencing barrier. Receipts are durable
facts. Open, Archive, Cancel queued and Stop are current Host projections,
refreshed as the target changes rather than persisted as promises. The new
Owner-only `CancelThreadTurn` operation carries the history, exact target
Thread and turn, and expected state, with a correlated acknowledgement; it can
therefore stop delegation work without guessing from an Owner message.

The web renders pills outside the collapsible trace, including during running
work and for failed or empty-final turns. Refused Thread pills reuse Reach and
spell out subtree scope without retrying the operation. `owner_fence` offers no
ineffective ordinary grant, and artifact refusals do not invent an owning
Space. This adds no authority beyond ADR 0022.

## Material state and headlines

Schema 16 adds `thread_state`, `thread_state_artifacts` and the append-only `thread_state_changes` audit stream, plus a positive `artifacts.revision`. Every Thread receives state atomically at creation so Spaces can carry parent rollups, while the product presents the state card on Tasks. `ThreadState.revision` is independent from `Thread.revision`. Reads, read receipts and telemetry do not advance it; material lifecycle, instrument, showcase, steering, terminal-result and referenced-artifact changes do.

`threads.state` checkpoints one reachable Task with revision compare-and-swap. It normalizes ASCII whitespace to single spaces and requires a nonempty headline of at most 12 words and 240 UTF-8 bytes. Findings and artifact references are bounded. A conflict returns the expected and actual state revisions plus the current state and records no effect. A committed write records exactly one `edited` effect receipt. Artifact edits advance the artifact revision and each state that explicitly references it.

A Thread retains `own_headline` even while it has children. The displayed `headline` for a parent is Host-derived: a bounded direct-child count plus the numerically lowest child whose active subtree has the highest fixed status precedence (`needs you`, `failed`, `running`, `queued`, `blocked`, `done`, `idle`). Current running or queued work takes precedence over an older terminal result on the same Thread. Archived subtrees do not participate. No child prose is selected, and no rollup settles a parent. The material mutation, every changed ancestor headline and every corresponding change row commit in one SQLite transaction, then `ThreadUpsert` publishes the changed state.

Schema 16 remains a complete-layout cutover with no in-place migration. Existing schema-15 data requires a separately reviewed offline backup and cutover; startup refuses it without modification.

## Questions and request presentation

Reserved for slice 4. Durable questions and Owner request cards will implement the escalation policy stated above.

## Coordination delivery and joins

Reserved for slice 5. State delivery and declared joins replace per-report conversational wakeups.

## Board and status

Reserved for slice 6. The board and derived status remain projections of durable Host facts.

## Supersession

This amends ADR 0016: top-level Space conversations are the Owner's Space-chat landing, and later state delivery/joins supersede automatic parent follow-up per report. It amends ADR 0017 by making an explicitly stored focus snapshot another reference into existing durable material, never a second owner or transcript. It does not supersede ADR 0023: one Native execution surface and the universal tool surface remain authoritative. ADRs 0018, 0020 and 0022 remain authoritative for topology, executor events and reach; in particular, ADR 0022 remains the sole authority model.
