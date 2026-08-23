# Protocol compatibility E2E index

These runbooks preserve backend, operational, and historical wire coverage
outside the primary Task Margins product suite. Read
[`RULES.md`](RULES.md) before running one.

The directory move is organizational only: scripts remain runnable and keep
their original scenario semantics. Use the paths below, not their former
top-level `e2e/<scenario>` locations.

## Storage, transport, and conversation

- [`attachments`](attachments/runbook.md) — upload, replay, signed blob fetch,
  and stored-path plumbing.
- [`attachment-agent-behavior`](attachment-agent-behavior/runbook.md) — real
  Agent behavior over image and text attachments.
- [`multi-turn-memory`](multi-turn-memory/runbook.md) — real conversation recall
  before and after restart.
- [`scrollback-history`](scrollback-history/runbook.md) — browser auto-reveal,
  correlated Host history paging, prepend anchoring, bounded eviction, and the
  truthful jump back to latest.
- [`restart-persistence`](restart-persistence/runbook.md) — Agent persistence
  across repeated Host restarts.
- [`send-queue-cancel`](send-queue-cancel/runbook.md) — injection, next-turn
  queueing, queued cancellation, and active-turn cancellation.
- [`turn-timeline`](turn-timeline/runbook.md) — ordered live timeline and tool
  summary frames.
- [`compaction`](compaction/runbook.md) — visible `continue_as` and
  post-compaction recall.

## Historical Ping, Event, View, and side-session contracts

- [`delegation-loop`](delegation-loop/runbook.md) — fake-driver delegation,
  terminal event, lifecycle-neutral Ping/Quick Reply, acknowledgement, and explicit settlement.
- [`pings-lifecycle`](pings-lifecycle/runbook.md) — named Pings, reply
  neutrality, neutral mentions, and explicit resolution.
- [`ping-read`](ping-read/runbook.md) — read-state round trip and restart
  persistence.
- [`side-chats`](side-chats/runbook.md) — retained scoped-session and
  conclude/confirm flow.
- [`event-queue`](event-queue/runbook.md) — typed Event lifecycle, judgment,
  taste rule, digest, push, and reopen.
- [`event-snooze-sweep`](event-snooze-sweep/runbook.md) — durable snooze,
  timer return, unsnooze, and finished sweep.
- [`event-archive-undo`](event-archive-undo/runbook.md) — archive,
  unarchive/reopen, and restart persistence.
- [`views-lifecycle`](views-lifecycle/runbook.md) — standalone View
  show/update/action/clear and reconnect replay.
- [`push-discipline`](push-discipline/runbook.md) — token registration and
  judgment-only push behavior.

## Agent orchestration and operations

- [`abandoned-recovery`](abandoned-recovery/runbook.md) — no mechanical
  Sub-agent resurrection after Host death.
- [`recovery-judgment`](recovery-judgment/runbook.md) — Agent judgment over
  abandoned work.
- [`real-subagent`](real-subagent/runbook.md) — real Codex delegation,
  progress, completion, and interruption.
- [`delegation-hygiene`](delegation-hygiene/runbook.md) — delegation note,
  session reuse, and worktree hygiene.
- [`interactive-orchestration`](interactive-orchestration/runbook.md) — main
  conversation remains interactive while delegated work runs.
- [`channel-discipline`](channel-discipline/runbook.md) — historical
  Chat-versus-Ping output choice.
- [`interruption-and-reporting`](interruption-and-reporting/runbook.md) —
  single interruption and outcome reporting discipline.
- [`monitors`](monitors/runbook.md) — monitor creation, process visibility,
  wake, and restart.
- [`timers`](timers/runbook.md) — Host-owned timer trigger and Agent wake.
- [`model-selection`](model-selection/runbook.md) — model broadcasts,
  validation, and persistence.

The historical coverage audit is retained as
[`normal-user-coverage-report.md`](normal-user-coverage-report.md).
