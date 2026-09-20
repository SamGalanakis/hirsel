# Space chats, essentials only

Accepted 2026-09-20. Simplified 2026-09-21.

## Decision

Every Space conversation is a Space chat that coordinates and dispatches. Every Task conversation is its worker. The Host derives that Role line from the Thread kind. Roles change guidance and presentation only: every Thread keeps every tool and may use any backend.

Reach, grants and root from ADR 0022 are the whole authority model. Hirsel adds no authority from roles, focus, evidence, status, pills or orchestration state. Fan-out, fan-in, batching, child waits and question handling belong in the agent's TypeScript code mode, using programs or processes on typed `thread.*` triggers, `threads.report`, instruments and ordinary Thread reads.

Route-free entry restores the last active top-level Space for the current history, otherwise opens the lowest-ID active top-level Space, otherwise creates an ordinary Space named Home through `create_thread`. Explicit malformed, unavailable and wrong-history links retain their existing refusal behavior. “Talk about this” opens the Task's Space chat and inserts `#<id>` into its draft. “Step in” opens the Task itself; sending during work visibly queues after the current turn, and Stop remains separate.

## Headline and status

Schema 18 stores `own_headline`, rolled-up `headline`, `previous_headline`, `headline_revision` and `last_seen_headline_revision` on `threads`. `threads.state` accepts a target and one headline. Rust and total SQL CHECKs enforce normalized nonempty text of at most 12 words and 240 bytes. Setting it moves the displayed headline to `previous_headline` and advances the revision.

Parents roll up deterministic child counts and Host facts with fixed precedence and numeric-ID tie-breaking. Rollups never copy child prose and never complete a Task.

Each Thread exposes one derived status and a reason: needs you, running, queued, hung, sleeping or idle. Hung means a running turn has emitted no durable event for `threads.hung_after_minutes` (default 10). It is suspicion only; no status cancels or retries work.

## Presentation

Effect pills are a web projection of already-durable tool timeline events and refusal activities. Malformed or truncated payloads produce no pill. Pills show created, sent to, delegated, read, edited and refused; the only action is Open. A grantable Thread refusal may open Reach, whose grant covers the named subtree and never retries. `owner_fence` cannot be fixed by a subtree grant, and an artifact has no Space Hirsel may guess.

The board is a client selector over the loaded Thread inventory, filtered to one top-level Space. It groups Needs you, Changed since you looked, and the rest. One client message advances displayed headline baselines independently of `read`. Desktop places Space chat and board side by side; phone uses Chat and Board tabs. Task view orders headline/status, instrument or showcased artifact, children with headlines, then the existing timeline.

When a turn changes a Thread under another top-level Space, the Host adds one ordinary, per-turn-coalesced activity to that Space chat: `Changed by #<id>: <what>`. No digest or context is injected; agents reread state when needed.

## Considered and removed (2026-09-21)

The implementation briefly included effect-receipt storage and exact-turn cancellation, outside-change digests and persisted admission context, Task-focus snapshots, wide Task state/findings/checkpoints/artifact revisions, managed Home lifecycle, a question ladder, merged request cards, declared joins and board-visit tables. All were removed. They duplicated durable timelines or ordinary Thread state, created new authority-shaped concepts, or moved orchestration out of code mode. The store is wiped at deployment; there is no migration or compatibility path.

## Supersession

This amends ADR 0016 only for Space-chat landing and presentation. It retracts ADR 0017's former Task-focus amendment. ADR 0023's universal execution surface and ADR 0022's authority model remain authoritative. No report batching, digest delivery or Host join contract supersedes their existing behavior.
