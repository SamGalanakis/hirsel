# Threads own conversation and work

> **Refined 2026-09-10:** [ADR 0018](0018-spaces-and-tasks.md) introduces Space/Task kinds, constrained child kinds and Task-only completion. The universal settlement statements below are superseded; common conversation ownership remains.

Accepted 2026-09-09 following the T3 Code prospect and the Owner's explicit decision to adopt its domain model.

Threads replace notification-shaped Tasks as the durable work identity. Each Message belongs to exactly one Thread; Turns and Activity belong to that same subject. Projects and their coordinators are ordinary root Threads. A parent can delegate focused work into child conversations; no separate Project or Namespace entity exists. Humans see the entire tree. Each agent execution can access only its own Thread and descendants, and a child reports upward to its actual requester without reading parent or peer transcripts. A citation is a reference, never a second owner of a Message.

Thread creation establishes visible identity before an execution, decision, or generated instrument exists. Attention, execution, read state, visibility, and active/settled lifecycle are separate dimensions. Settlement and reopening are explicit. Reading, replying, a successful Turn, or a quiet update never settles a Thread. Automatic inactivity/PR settlement is not adopted.

Hirsel retains Lash, SQLite, native Sub-agent Drivers, and generated instruments. This adopts T3 Code's domain boundaries, not its runtime implementation, provider/worktree options, or automatic settlement policy. See docs/research/prospect-task-model-2026-09-09/ for pinned source evidence.

This supersedes the global-transcript/Task-margin ownership rule in the earlier product direction, the typed Event work-object basis of ADR-0012, and the visible Task terminology in ADR-0004/0009/0013. ADR-0004's prohibition on mechanical workflow/retry policy remains. Durable Thread records are product context, not execution specifications.

The runtime accepts one current schema and protocol. It creates new stores directly and refuses nonempty mismatched stores without migration. A separately reviewed operator cutover can preserve unchanged current rows in a fresh schema after a full backup; it is not a shipped import path. Each store has a stable history_id. Clients invalidate identity-bound cache and queued operations when that identity changes, keeping plain unsent text for explicit draft recovery.

Nested execution is addressed by an immutable history/session/execution binding,
validated within resource transactions. Numeric IDs and relative paths do not
grant access. A child receives its accepted brief and explicit artifact
references; artifacts retain globally shared current content and no ownership
or revisions. Model-facing backlinks are filtered to visible Threads. Human
submission can explicitly attach up to 16 distinct existing artifact IDs and
atomically grant their references through the accepted owner message.

Each accepted turn captures execution settings. Lash has independent lazy
Thread sessions; Claude/Codex use a fresh CLI session per accepted turn with the
same scoped host tool surface. Durable FIFO admission, terminal outcome,
upward report, reference links and parent follow-up share SQLite authority.
Parent completion does not abandon children or settle any Thread. Hidden
parents retain reports while automatic follow-up waits for visibility.

A Turn's chronological prose, reasoning, code and tool timeline is durable
Thread history rather than an ephemeral client projection. Every producer
commits through one per-turn sequence before broadcast; live delivery and
`open_thread` replay therefore share `(turn_id, seq)` identity. Tool rows retain
bounded verbatim JSON input/result payloads with explicit truncation alongside
their concise labels. Historical turns from before this contract continue to
show only their truthful message, activity and tool-summary records.

Reset changes history identity, drains owned runtime tasks and CLI work, clears
session/bridge/View projections, and initializes fresh scoped sessions. Delayed
View submissions and blob metadata carry their captured history. Blob directory
cleanup excludes new-history uploads until it completes. No old global Lash
checkpoint or obsolete event/task projection is imported into this runtime.
