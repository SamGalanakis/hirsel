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

Schema 14 adds `message_task_focus(message_id PK/FK chat_messages, task_thread_id FK threads, snapshot_json TEXT NOT NULL CHECK object)`. `TaskFocus` is optional on `SendThreadMessage` and `ChatMessage`. Client-core carries it through pending state, retry, reconnect and confirmed snapshots; FFI callers that do not support focus send `None`.

## Material state and headlines

Reserved for slice 3. Task state, findings, headline rollups and visit baselines will extend this decision without changing Space-chat identity.

## Questions and request presentation

Reserved for slice 4. Durable questions and Owner request cards will implement the escalation policy stated above.

## Coordination delivery and joins

Reserved for slice 5. State delivery and declared joins replace per-report conversational wakeups.

## Board and status

Reserved for slice 6. The board and derived status remain projections of durable Host facts.

## Supersession

This amends ADR 0016: top-level Space conversations are the Owner's Space-chat landing, and later state delivery/joins supersede automatic parent follow-up per report. It amends ADR 0017 by making an explicitly stored focus snapshot another reference into existing durable material, never a second owner or transcript. It does not supersede ADR 0023: one Native execution surface and the universal tool profile remain authoritative. ADRs 0018, 0020 and 0022 remain authoritative for topology, executor events and reach; in particular, ADR 0022 remains the sole authority model.
