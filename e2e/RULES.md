# E2E Runbook Rules

Read this before running any scenario in `e2e/`. These are agent-driven runbooks, not scripts. You drive the Hirsel Host debug HTTP endpoints with `curl`, poll observable state, and judge whether the system produced the expected behavior.

## What You're Testing

You are testing Hirsel, not the tester model. A run is void if the asserted behavior came from you inventing state, manually writing the expected response, or relying on a visible transcript instead of the Hirsel Host debug surface.

## Debug Surface

Run scenarios only with `HIRSEL_DEBUG=1`; debug routes must be bound on `127.0.0.1`.

- `POST /debug/reset` wipes Chat, Inbox, process debug state, and starts from a clean session.
- `POST /debug/upload { "name": "...", "mime": "...", "data_b64": "..." }` stores a blob and returns its Blob JSON.
- `POST /debug/owner-message { "body": "...", "ref": null | message_id, "attachments": ["blob-id"] }` injects an Owner Chat message through the same host ingress path as the WebSocket; `attachments` is optional and defaults to `[]`.
- `GET /debug/chat` returns persisted Chat messages.
- `GET /debug/inbox` returns persisted Inbox Items.
- `GET /debug/processes` returns Sub-agent process records and normalized events.
- `GET /debug/health` returns basic host health and the latest Chat message id.
- `GET /blob/{id}?token=...` returns blob bytes; `Authorization: Bearer ...` is also accepted. Images are served inline; other MIME types are served as attachments.

## Poll, Don't Sleep

Every async gate must be checked by polling debug state. Do not `sleep` and assume progress. Use short polling intervals and a clear timeout; each poll should inspect the current JSON and decide whether the gate has matched, is still pending, or has failed.

## Gate Objectively

Before judging wording, prove the state transition happened:

- A delegated run must show a process in `/debug/processes`.
- A Sub-agent completion must show a terminal normalized event.
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
