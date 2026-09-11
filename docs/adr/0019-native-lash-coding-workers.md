# Native Lash coding workers use dedicated standard sessions

Accepted 2026-09-11 for [#56](https://github.com/SamGalanakis/hirsel/issues/56).

`threads.delegate` may select `agent: "lash"` for a focused coding worker. This is an in-process Lash standard tool-calling session, separate from the coordinator's RLM session. `agent: "host"` remains the full coordinator; `claude` and `codex` remain external CLI workers.

The worker exposes exactly four model-callable operations: `read`, `edit`, `write`, and `exec_command`. `exec_command` is the model-facing name bound to Lash's semantic `shell.exec` operation. There are no background-process, browser, web, planning, delegation, Thread-management, artifact-publication, or plugin tools in this profile. The runtime verifies the opened session's effective catalog, not only the provider's declared manifests. The accepted cwd is a default base, not a filesystem sandbox.

Each accepted turn durably captures a credential-free provider route snapshot, model, provider-default variant, canonical cwd, and versioned tool profile. The default route is the configured `openrouter` instance and the default model is `deepseek/deepseek-v4.1-flash`. Credentials remain private configuration indirection. Removing or retargeting a provider after acceptance refuses that queued turn; it never silently falls back. Rotating only a credential preserves the accepted route.

A Task retains its native-worker preference and conversation across follow-ups. Native worker sessions use a generation namespace distinct from coordinator sessions. A provider, model, cwd, or tool-profile change rotates the worker generation and seeds it from only that Task's visible recent conversation. The bootstrap contains the accepted brief, applicable repository instructions, and explicitly expanded skills; it does not receive unrelated conversations or coordinator guidance.

The worker reuses Hirsel's durable FIFO, shared admission capacity, turn identities, inline timeline, terminal outbox, and parent-report path. A successful worker turn reports once to its requesting parent but does not mark the Task done. Startup interrupts native worker turns that were running at shutdown rather than replaying uncertain shell effects. A direct Lash drive that exits without a settled report also abandons that session generation before terminal delivery, so a later follow-up cannot drive its uncertain pending input. Cancellation and history reset freeze tool admission, cancel and reap owned one-shot command process groups, and revoke the turn's execution binding before stale callbacks can act.

Artifact references are rejected before native-worker acceptance until an explicit bounded materialization contract exists. They are never silently discarded or exposed through broad artifact access.
