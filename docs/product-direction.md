# Product direction: Spaces and Tasks

Accepted 2026-09-09. [ADR 0016](adr/0016-threads-own-conversation.md) adopts the T3 Code domain boundaries and supersedes this document's earlier global-conversation/Task-margin model.

[ADR 0018](adr/0018-spaces-and-tasks.md), accepted 2026-09-10, refines that identity into ongoing Spaces and finishable Tasks. Both may be roots; Spaces contain either kind and Tasks contain Tasks only.

## Domain

A **Thread** owns one durable subject and its conversation. **Messages** are things said in that Thread. A **Turn** is one Agent execution with queued/running/terminal state. **Activity** records facts about work or execution. The **instrument** is the current generated interface attached to the Thread. These are distinct records and responsibilities.

The Owner uses one composer addressed to the focused Space or Task. Each agent execution may access its own conversation and descendants, with focused child assignments and upward reports. Humans can browse the whole tree. A coordinator is an ordinary root conversation; citations provide references without duplicating message ownership or granting access.

## State

Thread creation immediately establishes visible identity. Neither messages nor decisions are prerequisites. Attention (quiet/needs Owner), read state, archived/snoozed visibility, and Task open/done lifecycle are independent of turn execution. Reading, replying, an activity update or a completed execution never settles work. Spaces have no completion state. Task completion and reopening are explicit Owner actions; automatic inactivity or PR-based settlement is not adopted.

The Agent may update a Thread's title, description, attention and instrument from any wake. A generated continue action advances the existing instrument. An explicit Owner complete action is valid only for a Task. Identity, conversation and previous activity survive both. Current instrument revisions fence stale action submissions.

## Execution and recovery

Hirsel retains Lash, SQLite and native Sub-agent Drivers. It does not adopt T3's framework, event-sourcing architecture or per-provider worktree controls merely to reproduce the domain model. Each Thread has its own execution lane and accepted context; conversations are durable product records.

Owner requests persist with their owning Thread before admission. Per-Thread FIFO admission and bounded cross-Thread concurrency preserve each accepted request. Both send modes honor that boundary. Cancelling queued work removes only that request; stopping an active turn requires its owning Thread. Interrupted execution is visible after restart and is never inferred to mean the Thread is settled. Recovery decisions remain Agent/Owner judgment.

## Product surface

The primary inventory contains Spaces and Tasks sharing durable Thread identity. There are no notification-kind filters deciding which work exists. Information, questions and digests are content or activity associated with work. Background housekeeping cannot create invisible work or finish visible work. Native and web clients use the same identities, state and reconnect contract. Existing process and settings controls remain operational surfaces.

[PRODUCT.md](../PRODUCT.md), [DESIGN.md](../DESIGN.md), and [CONTEXT.md](../CONTEXT.md) describe the current behavior. Earlier ADRs remain historical evidence except where ADR 0016 or ADR 0018 explicitly supersedes them.
