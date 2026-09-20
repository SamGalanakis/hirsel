# Project chats, material state and coordination delivery

Accepted 2026-09-20.

## Project chat and worker roles

A top-level Space is a project and its own Thread conversation is the project chat. The Owner lands in that conversation and the project chat coordinates work. Every other Thread runs as a worker. The Host derives this role from current topology, appends one factual role line to the Owner-overridable Agent prompt, and captures `ToolProfile::{ProjectChat, Worker}` with every accepted turn.

Project chats stay on Native execution. Their advertised and executable surface keeps coordination, inspection, artifacts, instruments and request presentation, while excluding native coding operations, `shell_run`, direct sub-agent execution and execution-capable plugin tools. A plugin must explicitly declare an inspection-only tool before a project chat can receive it; execution is the conservative default. Workers retain the existing surface. The profile contributes to the Native tool-surface fingerprint, so changing a Thread between project chat and worker rotates the session rather than reinterpreting it. A CLI selection for a project chat is refused with a visible reason.

An accepted turn keeps its captured profile through queueing, admission, every tool dispatch and any process body it starts. A project-chat turn converted to a Task therefore remains unable to run coding tools; a worker turn converted to a top-level Space retains its worker tools until that accepted turn finishes. This symmetric rule prevents mutable topology from reinterpreting accepted work, while the next accepted turn captures the Thread's new role. The admitted catalog is rebuilt from the same capture, and dispatch resolves the durable caller before checking it, so catalog membership and execution authority cannot disagree.

The project chat dispatches through the atomic `threads.delegate` operation and does not perform the delegated work. It resolves the intended Task before acting, asks one short question when no confident target exists, and needs no Task for plain conversation. Focus and evidence never confer authority. Spending, external commitments and irreversible choices remain Owner decisions.

## Landing, recipient, focus and pairing

Route-free entry restores `hirsel.last-project.<history_id>` when it still names an active top-level Space. Otherwise the web client calls the idempotent `EnsureHomeProject { client_id, history_id }` operation. The Host atomically creates one ordinary positive-ID top-level Space named Home, records it as `meta['project_chat:home_thread_id']`, and creates no root grant. Explicit malformed, missing and old-history links retain their refusal behaviour and never fall back to Home.

The client keeps three facts separate and labels all three at the composer:

- `projectRecipientId`: the top-level Space whose project chat receives a project message;
- `taskFocus`: an optional explicit Task snapshot attached to the next project message;
- `workerPairingId`: the non-project Thread whose worker the Owner has stepped into.

Navigation remains `threads/store.ts::focusedId`; it grants none of the three meanings above. Drafts remain keyed by history and the actual message recipient.

“Talk about this” on a Task opens its owning project chat and stages `TaskFocus { task_thread_id, snapshot }`. The snapshot is bounded title, brief and current instrument summary, not a transcript. It is stored atomically with the Owner message in `message_task_focus` and replayed into the accepted input. The Host rechecks that the Task is within the project chat's existing reach; focus never widens reach.

“Step in” opens a Task's own conversation and addresses that worker. While work is running, the visible send action says “Send after current turn”; Stop remains separate.

## Storage and protocol

Schema 14 adds `message_task_focus(message_id PK/FK chat_messages, task_thread_id FK threads, snapshot_json TEXT NOT NULL CHECK object)`. `TaskFocus` is optional on `SendThreadMessage` and `ChatMessage`. Client-core carries it through pending state, retry, reconnect and confirmed snapshots; FFI callers that do not support focus send `None`.

## Material state and headlines

Reserved for slice 3. Task state, findings, headline rollups and visit baselines will extend this decision without changing project-chat identity.

## Questions and request presentation

Reserved for slice 4. Durable questions and Owner request cards will implement the escalation policy stated above.

## Coordination delivery and joins

Reserved for slice 5. State delivery and declared joins replace per-report conversational wakeups.

## Board and status

Reserved for slice 6. The board and derived status remain projections of durable Host facts.

## Supersession

This amends ADR 0016: top-level Space conversations are project chats, and later state delivery/joins supersede automatic parent follow-up per report. It amends ADR 0017 by making an explicitly stored focus snapshot another reference into existing durable material, never a second owner or transcript. It amends ADR 0023: one Native session remains, but its tool profile depends on the captured project-chat or worker role instead of every Native Thread universally receiving coding tools. ADRs 0018, 0020 and 0022 remain authoritative for topology, executor events and reach.
