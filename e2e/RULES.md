# E2E Runbook Rules

Read this before running any scenario in `e2e/`. These are agent-driven runbooks, not scripts. You drive the Hirsel Host debug HTTP endpoints with `curl`, poll observable state, and judge whether the system produced the expected behavior.

## What You're Testing

You are testing Hirsel, not the tester model. A run is void if the asserted behavior came from you inventing state, manually writing the expected response, or relying on a visible transcript instead of the Hirsel Host debug surface.

## Debug Surface

Run scenarios only with `HIRSEL_DEBUG=1`; debug routes must be bound on `127.0.0.1`.

- `POST /debug/reset` wipes Chat, Inbox, process debug state, and starts from a clean session.
- `POST /debug/upload { "name": "...", "mime": "...", "data_b64": "..." }` stores a blob and returns its Blob JSON.
- `POST /debug/owner-message { "client_id": "optional-stable-id", "body": "...", "ref": null | message_id, "attachments": ["blob-id"], "mode": "send" | "next_turn" }` injects an Owner Chat message through the same host ingress path as the WebSocket; `client_id`, `attachments`, and `mode` are optional, and `mode` defaults to `send`.
- `POST /debug/open-side-chat { "item_id": ... }` opens or resumes a live side chat and returns its scope id and transcript.
- `POST /debug/side-message { "sc": "side:...", "body": "..." }` submits a side-scoped Owner message; poll `/debug/side-chats` for the reply.
- `POST /debug/conclude { "sc": "side:..." }` drafts the Owner's conclusion without adding the draft to the transcript.
- `POST /debug/confirm-conclusion { "sc": "side:...", "text": "..." }` posts the anchor-refed Owner conclusion to main Chat, archives the item, and closes the side chat.
- `POST /debug/read-item { "item_id": ... }` marks an Inbox Item read and broadcasts `inbox_upsert`.
- `POST /debug/cancel-turn` cooperatively interrupts the active Agent turn and broadcasts `agent_activity` idle.
- `POST /debug/cancel-queued { "client_id": "..." }` cancels an unclaimed queued Owner message, deletes its Chat row, and broadcasts `msg_removed`; if it was already claimed, the endpoint returns an error.
- `POST /debug/create-monitor { "cmd": "...", "every_secs": 30, "wake_on": "changed" | "exit_zero" | "exit_nonzero" | "regex", "pattern": "...", "label": "..." }` creates a persisted monitor for deterministic monitor runbooks.
- `GET /debug/chat` returns persisted Chat messages.
- `GET /debug/inbox` returns persisted Inbox Items.
- `GET /debug/broadcasts` returns the recent debug-recorded host broadcasts, including `msg`, `msg_removed`, `turn_event`, `process_upsert`, and cancellation `agent_activity` events emitted through the debug/WebSocket ingress path.
- `GET /debug/processes` returns v1.4 `ProcessInfo` rows for Sub-agents and monitors: `id`, `kind`, `label`, `agent`, `model`, `state`, timestamps, and `summary`.
- `GET /debug/side-chats` returns only live side chats with their scoped transcripts.
- `GET /debug/health` returns basic host health and the latest Chat message id.
- `GET /blob/{id}?token=...` returns blob bytes; `Authorization: Bearer ...` is also accepted. Images are served inline; other MIME types are served as attachments.

There is no debug HTTP route for owner-side Inbox archive/delete in this branch. Runbooks that need to prove owner archive semantics must use the canonical WebSocket `archive_item` frame from `app/PROTOCOL.md`, then gate on `/debug/inbox` and `/debug/broadcasts`.

## Scenario Index

- `attachments` - protocol v1.1 upload, replay, blob fetch, and scripted attachment-note plumbing.
- `attachment-agent-behavior` - real Codex Agent behavior over image/text attachments.
- `abandoned-recovery` - ADR-0004 abandoned Sub-agent recovery after SIGKILL/reboot.
- `compaction` - Agent-initiated context compaction via `continue_as` and post-compaction recall.
- `delegation-loop` - fake-driver delegation, terminal event, Inbox question, Quick Reply, acknowledgement.
- `inbox-lifecycle` - requires-response reply flow, owner archive via WebSocket, and Agent archive of a moot item.
- `inbox-read` - Inbox read-state round trip and restart persistence.
- `monitors` - monitor creation, process visibility, wake, and restart survival.
- `multi-turn-memory` - real Codex conversation recall before and after host restart.
- `real-subagent` - real Codex Sub-agent spawn, progress, completion, and interruption.
- `restart-persistence` - real Agent persistence over repeated host restarts.
- `send-queue-cancel` - send/next-turn queueing and active-turn cancellation.
- `side-chats` - protocol v2.0 side-chat loop: seeded open, scoped conversation, resume, conclude, confirm, idempotent archive, teardown, and main-Agent reaction (scripted + real variants).
- `timers` - timer trigger source registration and wake.
- `turn-timeline` - live turn timeline ordering and tool event summaries.

## Poll, Don't Sleep

Every async gate must be checked by polling debug state. Do not `sleep` and assume progress. Use short polling intervals and a clear timeout; each poll should inspect the current JSON and decide whether the gate has matched, is still pending, or has failed.

## Gate Objectively

Before judging wording, prove the state transition happened:

- A delegated run must show a process in `/debug/processes`.
- A Sub-agent completion must show `kind: "subagent"` and terminal `state: "done"`.
- A tool-using Agent turn must persist non-empty `tool_calls` on the Agent Chat message and emit `turn_event` `tool_start`/`tool_done` broadcasts while running.
- An Owner question must appear as an Inbox Item with `requires_response: true`.
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
