# E2E Runbook Rules

Read this before running any scenario in `e2e/`. These are agent-driven runbooks, not scripts. You drive the Hirsel Host debug HTTP endpoints with `curl`, poll observable state, and judge whether the system produced the expected behavior.

## What You're Testing

You are testing Hirsel, not the tester model. A run is void if the asserted behavior came from you inventing state, manually writing the expected response, or relying on a visible transcript instead of the Hirsel Host debug surface.

## Debug Surface

Run scenarios only with `HIRSEL_DEBUG=1`; debug routes must be bound on `127.0.0.1`.

- `POST /debug/reset` wipes Chat, Pings, process debug state, and starts from a clean session.
- `POST /debug/upload { "name": "...", "mime": "...", "data_b64": "..." }` stores a blob and returns its Blob JSON.
- `POST /debug/owner-message { "client_id": "optional-stable-id", "body": "...", "ref": null | message_id, "attachments": ["blob-id"], "mentions": [ping_id], "mode": "send" | "next_turn" }` injects an Owner Chat message through the same host ingress path as the WebSocket; optional `mentions` add Ping context without changing lifecycle, while only `ref` to a Ping Anchor auto-resolves it.
- `POST /debug/open-side-chat { "ping_id": ... }` opens or resumes a live side chat and returns its scope id and transcript.
- `POST /debug/side-message { "sc": "side:...", "body": "...", "mentions": [ping_id] }` submits a side-scoped Owner message; poll `/debug/side-chats` for the reply.
- `POST /debug/conclude { "sc": "side:..." }` drafts the Owner's conclusion without adding the draft to the transcript.
- `POST /debug/confirm-conclusion { "sc": "side:...", "text": "..." }` posts the anchor-refed Owner conclusion to main Chat, auto-resolves the Ping, and closes the side chat.
- `POST /debug/read-ping { "ping_id": ... }` marks a Ping read and broadcasts `ping_upsert`.
- `POST /debug/resolve-ping { "ping_id": ... }` explicitly moves a Ping to done and broadcasts `ping_upsert`.
- `POST /debug/cancel-turn` cooperatively interrupts the active Agent turn and broadcasts `agent_activity` idle.
- `POST /debug/cancel-queued { "client_id": "..." }` cancels an unclaimed queued Owner message, deletes its Chat row, and broadcasts `msg_removed`; if it was already claimed, the endpoint returns an error.
- `POST /debug/create-monitor { "cmd": "...", "every_secs": 30, "wake_on": "changed" | "exit_zero" | "exit_nonzero" | "regex", "pattern": "...", "label": "..." }` creates a persisted monitor for deterministic monitor runbooks.
- `GET /debug/chat` returns persisted Chat messages.
- `GET /debug/pings` returns persisted Pings, including required `name` and `description` fields.
- `GET /debug/broadcasts` returns the recent debug-recorded host broadcasts, including `msg`, `msg_removed`, `turn_event`, `process_upsert`, and cancellation `agent_activity` events emitted through the debug/WebSocket ingress path.
- `GET /debug/processes` returns v1.4 `ProcessInfo` rows for Sub-agents and monitors: `id`, `kind`, `label`, `agent`, `model`, `state`, timestamps, and `summary`.
- `GET /debug/side-chats` returns only live side chats with their scoped transcripts.
- `GET /debug/health` returns basic host health and the latest Chat message id.
- `get_blob_url` returns a short-lived, blob-scoped signed URL for `GET /blob/{id}`. `Authorization: Bearer ...` remains a migration path, but owner tokens are never accepted in query strings. Images are served inline; other MIME types are served as attachments.

Agent event tools are `events.judgment { question, context?, options, unblocks?, view? }`,
`events.notify { name, description, content_md? }`, and
`events.summary { name, description, content_md | ui }`; tool summaries use the internal names
`events_judgment`, `events_notify`, and `events_summary`. `pings.send` remains a deprecated alias for
judgment/info events, with tool-summary name `pings_send`; `pings.resolve { ping_id }` remains the
compatibility lifecycle tool with summary name `pings_resolve`. A scripted or real Agent must supply
the required non-empty event fields; runbooks must never synthesize them outside the Agent tool path.

## Scenario Index

- `attachments` - protocol v1.1 upload, replay, blob fetch, and scripted attachment-note plumbing.
- `attachment-agent-behavior` - real Codex Agent behavior over image/text attachments.
- `abandoned-recovery` - ADR-0004 abandoned Sub-agent recovery after SIGKILL/reboot.
- `compaction` - Agent-initiated context compaction via `continue_as` and post-compaction recall.
- `delegation-loop` - fake-driver delegation, terminal event, Ping question, Quick Reply, auto-resolution, and acknowledgement.
- `pings-lifecycle` - named/described Pings, reply auto-resolution, neutral mentions, and explicit Owner/Agent resolution.
- `ping-read` - Ping read-state round trip and restart persistence.
- `monitors` - monitor creation, process visibility, wake, and restart survival.
- `multi-turn-memory` - real Codex conversation recall before and after host restart.
- `real-subagent` - real Codex Sub-agent spawn, progress, completion, and interruption.
- `restart-persistence` - real Agent persistence over repeated host restarts.
- `send-queue-cancel` - send/next-turn queueing and active-turn cancellation.
- `side-chats` - side-chat loop: seeded open, scoped conversation, resume, conclude, confirm, reply-driven Ping resolution, teardown, and main-Agent reaction.
- `timers` - timer trigger source registration and wake.
- `turn-timeline` - live turn timeline ordering and tool event summaries.
- `channel-discipline` - real-Agent surface choice: a warm result is answered in Chat, a cooled-off result becomes a Ping, a pure acknowledgment is filed nowhere, and nothing is ever double-filed.
- `delegation-hygiene` - delegation note before the spawn, no redundant sibling sessions (`subagents.list`/`subagents.prompt` reuse), and one working directory per Sub-agent.
- `interruption-and-reporting` - blocked work sends exactly one `requires_response` Ping then moves on without nagging, and a decision-carrying completion is a single outcome-phrased Ping (never split report + question).
- `recovery-judgment` - ADR-0004 judgment layer over `abandoned-recovery`: no mechanical respawn after reboot, a nudge re-spawns only what the Agent still wants, and Owner-cancelled work stays dead.
- `daily-driver` - the whole SCOPE Slice-1 loop (warm Chat exchange → delegation + note → progress → one `requires_response` Ping with Quick Replies → auto-resolving tap → acknowledgement) chained end-to-end in one continuous session with a single reset.
- `event-queue` - the ADR-0012/0013 typed event lifecycle: a real-Agent judgment with the blessed card `ui`, choose delivering an anchor-refed reply, taste-store `record_rule`, the scheduled digest summary, judgment-only push, and the Done-toggle reopen (plus the no-`until` snooze rejection).
- `event-snooze-sweep` - the wave-3 lifecycle additions, fully mechanical: durable snooze validation (`until` required, presets named on error), host-timer returns with judgment re-push, restart-surviving returns, unsnooze, and the `clear_finished_events` sweep over the real `/ws` wire stamping `archived_at` while open judgments survive.
- `interactive-orchestration` - the keep-chat-interactive guarantees: a delegation turn ends while the Sub-agent still runs, a warm question is answered mid-flight, and a long Sub-agent report reaches the Agent untruncated.

## Neutral Working Directories

Never let anything under test touch or inherit from the hirsel checkout:

- Host instances under test run with their working directory in the scenario's `/tmp` workdir (invoke the prebuilt binary by absolute path; the repo is reference material, not a runtime location).
- Every delegation instruction in an owner message MUST name an explicit throwaway workdir (e.g. "in /tmp/hirsel-e2e-<scenario>-work, which you may create") — an unguided Agent defaults its Sub-agent `cwd` into the host's cwd, and a full-auto CLI running inside the hirsel repo inherits the Owner's personal CLAUDE.md and can write into the checkout.
- Runbook executors likewise run from a neutral directory and reference the repo read-only by absolute path.

## Poll, Don't Sleep

Every async gate must be checked by polling debug state. Do not `sleep` and assume progress. Use short polling intervals and a clear timeout; each poll should inspect the current JSON and decide whether the gate has matched, is still pending, or has failed.

## Gate Objectively

Before judging wording, prove the state transition happened:

- A delegated run must show a process in `/debug/processes`.
- A Sub-agent completion must show `kind: "subagent"` and terminal `state: "done"`.
- A tool-using Agent turn must persist non-empty `tool_calls` on the Agent Chat message and emit `turn_event` `tool_start`/`tool_done` broadcasts while running.
- An Owner question must appear as a Ping with non-empty `name` and `description` and `requires_response: true`.
- A Quick Reply response must be an Anchor-refed Owner Chat message.
- The Agent acknowledgement must be a persisted Agent Chat message after the Owner reply.

## Abort Triggers

Stop immediately on any of:

- an HTTP error from the debug surface;
- malformed JSON;
- a process terminal status of failed when the scenario expected success;
- a gate that never matches within the scenario timeout;
- evidence that you, the tester, created the asserted state instead of the system.

On abort, report the failing curl command, response body, last observed debug state, and the specific gate that failed.

## Report Format

On success, report each gate with the observed id or JSON field that proved it. On abort, report RCA and stop; do not repair the system as part of a runbook execution.
