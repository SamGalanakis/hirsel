# Project chats product runbook

Follow [`../RULES.md`](../RULES.md). This is a bounded real-provider product check. Do not run it as part of deterministic validation or without explicit approval for the model calls.

## Owner-visible outcome

Route-free entry opens the last project for this isolated history, or the ordinary Home project when none exists. The project composer separately labels the project recipient, Task focus and worker pairing. Plain conversation stays in the project chat without creating work.

From a Task, **Talk about this** returns to its project chat and stages a bounded focus snapshot. The next message reaches the project chat, names the focused Task and cannot reach a Task outside that project's existing reach. **Step in** addresses the Task worker itself. While that worker runs, Stop and the visible **Send after current turn** action remain separate.

The project chat dispatches implementation to a Task worker. It does not receive or execute coding, shell, direct sub-agent or execution-capable plugin tools. The worker receives its role, accepted brief and full worker profile.

## Isolated setup

- Use a fresh data directory, disposable repository and an unused loopback port other than 3076.
- Build the Host and web app from the exact reviewed checkout.
- Record the chosen provider/model and source revision without recording credentials.
- Do not grant Home root reach.

## Bounded scenario

Spend at most three model turns: one plain project-chat exchange, one project-chat delegation request and one worker result. Do not retry automatically.

1. Open `/`. Verify the Host creates exactly one positive-ID top-level Home Space, persists `project_chat:home_thread_id`, creates no root grant and returns the same Space when bootstrap is repeated.
2. Send plain conversation. Verify no Task, coding operation or direct sub-agent execution is created.
3. Create a child Task through the supported controls, open it, and verify the composer labels the worker pairing.
4. Return with **Talk about this**. Verify Project, Focus and Worker labels remain distinct and the accepted message stores one bounded `message_task_focus` row.
5. Ask the project chat to make one tiny, verifiable repository change. Verify it delegates atomically to the Task worker and stays responsive.
6. While the worker runs, verify Stop remains separate and the send button reads **Send after current turn**. Queue one message only if the explicit call budget allows it.
7. Verify the worker performs the change, reports a concise result and the project chat gives the Owner a standalone answer rather than pasting the report.
8. Reload route-free and confirm the last project reopens. Probe a malformed, wrong-history and missing explicit link; each must retain the unavailable-link behavior and must not open Home.

## Evidence

Retain sanitized screenshots of project and Task composers, authenticated frames, `open_thread` snapshots, the exact Home/meta/grant/focus rows, role-specific advertised tool names, refused execution probes and result JSON. Record `OBJECTIVE_PASS` only when every deterministic predicate holds. A reviewing Agent separately judges whether the project chat remained conversational and dispatch-first.
