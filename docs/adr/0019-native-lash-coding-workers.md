# Native Lash coding workers use dedicated RLM sessions

Accepted 2026-09-11 for [#56](https://github.com/SamGalanakis/hirsel/issues/56).

`threads.delegate` may select `agent: "lash"` for a focused coding worker. This is an in-process Lash TypeScript RLM session with a narrow tool profile, separate from the coordinator's RLM session. `agent: "host"` remains the full coordinator; `claude` and `codex` remain external CLI workers.

The worker exposes exactly four Hirsel-owned model-callable operations: `read`, `edit`, `write`, and `exec_command`, bound in RLM programs as `files.read`, `files.edit`, `files.write`, and `shell.exec`. Hirsel owns the tool implementations and does not depend on Lash's coding-tool crate. The coordinated Lash runtime/provider family remains pinned to `47e6e23764939c790961fbe2905ee08ff5373a95`; the later tool-only pin is removed. There are no browser, web, planning, delegation, Thread-management, artifact-publication, or plugin tools in this profile. The runtime verifies the opened session's effective catalog, not only the provider's declared manifests. The accepted cwd is a default base, not a filesystem sandbox. Truncated text reads return both a line and UTF-8 byte cursor so one long line can be reconstructed without skipped bytes.

Each accepted turn durably captures a credential-free provider route snapshot, model, provider-default variant, canonical cwd, and versioned tool profile. The default route is the configured `openrouter` instance and the default model is `deepseek/deepseek-v4.1-flash`. Credentials remain private configuration indirection. Removing or retargeting a provider after acceptance refuses that queued turn; it never silently falls back. Rotating only a credential preserves the accepted route.

A Task retains its native-worker preference and conversation across follow-ups. Native worker sessions use a generation namespace distinct from coordinator sessions. First native use and a return after another backend seed bounded unseen conversation from only that Task; a provider, model, cwd, or tool-profile change rotates the worker generation and seeds bounded recent Task conversation. Selector-free follow-ups resolve this effective backend before admission, so native artifact refusal and explicit skill expansion cannot be bypassed. The bootstrap contains the accepted brief, applicable repository instructions, and explicitly expanded skills; it does not receive unrelated conversations or coordinator guidance.

Verified model metadata is route-specific, rather than inferred from the Owner-chosen provider id. The large context/output limits and image capability apply only to the exact official OpenRouter base URL plus `deepseek/deepseek-v4.1-flash`. Other free-text model ids retain conservative metadata; acceptance validates their shape, not availability or capabilities at the configured endpoint.

The worker reuses Hirsel's durable FIFO, shared admission capacity, turn identities, inline timeline, terminal outbox, and parent-report path. A successful worker turn reports once to its requesting parent but does not mark the Task done. Startup interrupts native worker turns that were running at shutdown rather than replaying uncertain shell effects. A direct Lash drive that exits without a settled report also abandons that session generation before terminal delivery, so a later follow-up cannot drive its uncertain pending input. Runtime failures remain failed with a bounded diagnostic in the child timeline and terminal parent report; resource cleanup does not reclassify them as cancellation. Hirsel retains every admitted file operation and host-owned command task independently of its transient result consumer. Cancellation and history reset freeze tool admission, cancel active command tokens, join retained work, reap owned one-shot command process groups, and revoke the turn's execution binding before stale callbacks can act.

Native command execution is supported on Linux hosts with `pidfd_open` and readable procfs process metadata. Hirsel preflights both capabilities before spawning, starts `/bin/sh` without a login profile in a fresh session/process group, keeps the direct leader unreaped while terminating that group on every terminal path, waits until its ordinary members are no longer runnable, then reaps the leader and drains its output. This preserves the direct exit status while preventing same-group descendants from outliving successful, nonzero, cancelled, timed-out, failed-reader, or externally terminated calls. Processes that deliberately escape the owned group are outside this initial guarantee. Other or unsupported hosts fail closed before spawning a command rather than silently providing weaker cleanup.

Artifact references are rejected before native-worker acceptance until an explicit bounded materialization contract exists. They are never silently discarded or exposed through broad artifact access.

## Revision (2026-09-13)

The native worker now runs the same RLM protocol posture as the coordinator: the TypeScript dialect with process and trigger abilities. Its four-tool coding profile remains deliberately narrow, and Hirsel creates no default processes. One runtime posture prevents coordinator and worker protocol behavior from drifting, while RLM code cells make the worker's program visible in chat through the existing `CodeStart` and `CodeDone` timeline events; tool calls inside those cells retain their `ToolStart`, `ToolDone`, and `tool_completed` projections.

## Superseded (2026-09-13)

Superseded by [ADR-0023](0023-one-native-execution.md). The separate
native-worker session, its narrow four-tool profile, its own session generation
namespace and its artifact refusal are deleted: one Native session per Thread
now carries the full Thread tool set and the four coding operations together.
This record stands as history.
