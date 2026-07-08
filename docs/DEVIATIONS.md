# Scaffold Deviations

This file records places where the implementation intentionally differs from the spike reports or the full slice-1 target.

## Lash Runtime

The `lash_runtime` module is an isolation facade, but it does not yet construct `LashCore::rlm_builder`, `RlmProtocolPluginFactory`, sqlite lash stores, the Anthropic provider, runtime processes, trigger store, or lashlang `ToolProvider` bindings.

Reason: slice-1 server scaffolding needed a compileable host with storage, WebSocket, debug, driver, and smoke-test behavior first. Lash's public API is alpha, and the full wiring should land inside this module without leaking alpha types across the rest of the host.

Current behavior:

- Without `ANTHROPIC_API_KEY`, Owner turns append an Agent-authored error Chat message instead of crashing.
- With `HIRSEL_DRIVER=fake`, delegation requests exercise the Sub-agent Driver lifecycle and file a requires-response Inbox Item when the fake driver reaches a terminal event.
- With `ANTHROPIC_API_KEY`, the scaffold can answer the smoke prompt containing `pong`, but it is not yet a real RLM turn through lash.

Required follow-up:

- Add path dependencies on `/workspace/code/lash` crates in `hirsel-host`.
- Move all direct lash alpha API usage into `lash_runtime`.
- Build one session named `agent` at boot with the prompt in `prompts/agent.md`.
- Replace the scaffold turn handler with the sequential lash turn pump.
- Convert `tools` into a real lash `ToolProvider` with lashlang bindings.

## Runtime Processes And Wakes

Sub-agent process state is currently tracked in Hirsel Host debug memory, not in lash Runtime Processes, and terminal events notify the scaffold Agent facade directly.

Required follow-up:

- Register Sub-agent CLIs as lash Runtime Processes with `RecoveryDisposition::OwnerBound`.
- Append terminal Process Events and rely on lash Process Wake delivery to wake the `agent` session.
- Preserve ADR-0004 by never mechanically restarting dead Sub-agents.
