# Spec: Space chats

2026-09-20. Accepted for implementation 2026-09-20. **Proposed** and **Decided** tags record
how each part was settled. Mocks:
<https://claude.ai/artifact/Vq4vrmZnL7M6LbX6CHZq28>.

## Summary

Every Space has one long-running **Space chat**.
The Owner lands in a top-level Space chat. The chat dispatches work to **workers** in child
Tasks and never does the work itself. Everything a reply touched is shown as
**pills** on that reply. A single **board** beside the chat shows every
Space's work, ordered by what changed since the Owner last looked. Workers
raise **questions** upward; the chat answers what it can and escalates the rest
to the Owner as request cards.

## Goals

- Minimal management overhead: no choosing among many Threads, no filing, no
  status reading, no context plumbing by hand.
- A real conversation with ample context remains the front door.
- Token cost sits in bounded worker sessions, not in a transcript that grows
  with tool output.

## Non-goals

- A new response UI. One openui card per request is good enough.
- A routing classifier or any small-model steward in front of the chat.
- New authority mechanisms. Space reach, Space-to-Space grants and the
  root grant (ADR 0022) are the whole model.
- Anything taken from firstmate beyond prompt wording.

## Concepts

**Space chat.** A Space's own Thread conversation. It decides for
itself whether a turn concerns a Task, and if so makes the tool calls. Plain
conversation makes none. Role is guidance and presentation only: any Space may
select any backend and has the same tools as any other Thread. (Decided.)

**Task.** Unchanged in structure: a Thread with its conversation, turn
timeline, instrument, artifacts and children. What changes is its resting
view; see Surfaces. (Proposed.)

**Worker.** The session that executes a Task: Native, Claude CLI or Codex CLI,
on the model the job needs. It reads the Task's brief and state, not the
Space chat.

**State and headline.** Each Task carries structured state (instrument,
artifacts, findings). Every state change carries a headline of at most 12
words. A parent's headline is a deterministic rollup of its children.

**Question.** Something a worker needs answered to proceed. A record on the
Task, not a chat message. See Questions. (Proposed.)

**Pill.** A rendering, on a chat reply, of one Thread or artifact that reply
touched: created, sent to, delegated, read, edited, refused. Pills offer only
actions that are true at that moment (cancel queued, stop, archive, open). A
refused pill offers today's Space grant, with its subtree scope spelled out,
and revoke. (Decided.)

**Board.** One global, read-mostly list of work across Spaces. Default
order: questions for the Owner, then Tasks changed since the last visit with
before and after headlines, then unchanged. A Task's status is derived from
Host facts, with the reason shown: running, hung (running, no events for N
minutes), needs you, until a registered wake, waiting on a named input, or
idle with nothing scheduled. Filter by Space; toggle to group by status.
(Decided, except the derived statuses, Proposed.)

## Surfaces

**Space view.** Chat-first. Space chat on the left, board on the right
(desktop); Chat and Board tabs (phone). The rail switches Spaces; a star
marks root.

**Task view.** (Proposed.) Same parts as today's Thread, reordered so state
comes first:

1. Headline, derived status and its reason.
2. Open questions for this Task.
3. State: the instrument, or the showcased artifact.
4. Children, with rolled-up headlines.
5. Timeline: the worker's conversation and turn cards, as today.

The Task's composer is **step in**: it addresses the worker directly, is
labelled as such, and queues behind the current turn. The default way to
discuss a Task is **talk about this**, which loads its state into the owning Space
chat as explicit focus. Space recipient, Task focus and worker pairing are
three separate, always-labelled pieces of state.

## Questions

(Proposed.) Questions climb a ladder, and most should stop before the Owner.

1. A worker raises a question on its Task: the question, options, its
   recommendation, and whether it blocks the worker.
2. The Host delivers open questions to the requester (normally the Space
   chat), grouped by Task and batched with its other pending wakes, not one
   turn per question.
3. The chat answers what it can from what the Owner has already said, standing
   preferences and current state. Each such answer records its reason.
4. The chat escalates the rest to the Owner as openui request cards, merging
   duplicates across Tasks ("three Tasks ask about budget" is one card).
5. The board's top band shows only escalated questions, grouped by Task. Each
   Task also shows "answered for you · N", with the answer, the reason and an
   override. An override re-wakes the worker.

Prompt policy, not an authority mechanism: the chat does not answer questions
about spending, external commitments or anything irreversible. It escalates
them.

## Host behaviour (no model)

- Headline rollups.
- Declared joins: a parent wakes once when its declared inputs are all ready;
  a failed, cancelled or Owner-steered input blocks the join visibly.
- Reports update Task state and rollups. Only a satisfied join, an open
  question or an explicit escalation wakes a chat.
- Outside changes: when another Space changes yours, your chat shows one
  activity line and receives a short digest of unconsumed changes before its
  next turn.
- Derived status, including hung detection.
- Visit baseline for "changed since last visit", separate from read state.

## Prompt changes

`prompts/agent.md`: a dispatch-first role for Space chats; resolve the
target and say it; owner-facing language (outcome, consequence, decision; the
final message stands alone; never paste worker output); evidence is not
authorization; simplest path first; the question-answering policy above.

## What this changes

- PRODUCT.md "no default recipient": the Owner lands in a Space chat, last
  used or Home.
- ADR 0016 per-report parent follow-up: replaced by state delivery and joins.
- ADR 0017 conversation-first resting surface: Tasks rest on state; Spaces
  rest on chat plus board.

## Decisions so far

- 2026-09-19. State is a primitive alongside task, session and decision.
- 2026-09-20. Request response UI is solved by openui cards; the problem is
  management.
- 2026-09-20. The main chat is long-running on a medium model with ample
  context; do not starve it.
- 2026-09-20. No routing classifier; the chat routes by its own tool calls,
  shown as pills.
- 2026-09-20. One chat per top-level Space; some hold root.
- 2026-09-20. No authority mechanisms beyond Space-level grants. Accepted
  risk: asking before an irreversible cross-Space action is the model's
  judgement, not a Host guarantee.
- 2026-09-20. From firstmate, prompt wording only. Declared wait kinds
  rejected: the Host can derive status.

## Resolved 2026-09-20

1. Task view order: accepted.
2. Only the requester answers a question, normally the Space chat. One hop;
   intermediate parent Tasks pass questions through.
3. A blocking question wakes the chat immediately; non-blocking ones wait for
   the next batch.
4. The Owner lands in the last Space used; Home when there is none.
5. Done, split and merge proposals originate in the chat, so they go straight
   to the Owner as request cards and skip the ladder.

## Build order

1. Space chat contract: prompts, role guidance, recipient and focus
   kept distinct.
2. Pills from structured effect receipts.
3. Task state, headlines, rollups, change digests.
4. Questions ladder and "answered for you".
5. State delivery and joins.
6. Board ordering, derived status, Task view reorder, phone flow.
