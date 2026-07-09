# Scaffold Deviations

This file records intentional differences between this scaffold branch and the full slice-1 target.

## Current Status

The previous Lash Runtime and Runtime Processes deviations are resolved in the scaffold branch:

- `hirsel-host` embeds `LashCore::rlm_builder` with the RLM protocol factory, SQLite session/process/trigger stores, file attachments, process env store, and `InlineEffectHost`.
- The default Agent mode is a real Lash RLM session named `agent`; `prompts/agent.md` is installed as a session prompt contribution.
- `HIRSEL_PROVIDER=anthropic|codex` selects the Lash provider. Missing credentials degrade to an Agent-authored error Chat message while the host stays up.
- Hirsel tools are registered as a Lash `ToolProvider` with Lashlang bindings for Pings, Sub-agents, and shell. Chat replies are not a tool; committed Agent turn output is appended as the Agent-authored Chat message.
- Sub-agent starts create Lash Runtime Processes with `RecoveryDisposition::OwnerBound`; driver terminal events append terminal Process Events and enqueue Lash `ProcessWake` work for the `agent` session. Spawns accept an optional model, and `subagents.wait` can await a Sub-agent Runtime Process to terminal output inside the current lashlang turn.
- Hirsel contributes a host-owned `timer.Schedule` trigger source. Timer registrations live in Lash's SQLite trigger store; the host scheduler emits `timer.Tick` occurrences and relies on trigger target processes to `wake` the `agent` session.
- Owner turns and queued process wakes are drained by a sequential Lash queued-turn pump.
- WebSocket `agent_activity` comes from Lash session observation.

## Intentional Test Double

`HIRSEL_AGENT=scripted` remains as a deterministic no-credentials test double for the delegation-loop runbook. It is not the product path and should not be used as evidence that RLM prompt/tool behavior works.

## Remaining Work

No scaffold deviation currently blocks the slice-1 Lash embedding target. Prompt quality and product behavior can still need tuning after real LLM runs, especially around when the Agent chooses a Ping vs Chat.
