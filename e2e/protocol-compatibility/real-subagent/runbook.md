# Real Sub-agent Runbook

## Purpose

Prove the day-to-day delegation path with the real Codex Sub-agent Driver instead of the fake driver: the Agent delegates a repo fix, the Codex process is visible with the requested model, progress is observable, the terminal wake reaches the Agent, and the repo test passes. A second scenario proves Owner-requested interruption reaches the driver and leaves the process in a terminal non-done state.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/protocol-compatibility/real-subagent/run.sh
```

The runner chooses a free loopback port after printing `ss -tlnp`, uses `/tmp/hirsel-e2e-real-subagent-*`, starts `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=real`, and kills only the host PID it starts.

## Gates

Scenario A:

- `/debug/health` reports `ok: true`.
- A throwaway `/tmp` git repo starts with a failing `python3 -m unittest -v`.
- The Agent uses `subagents.spawn`; `/debug/processes` shows `kind: "subagent"`, `agent: "codex"`, and `model: "gpt-5.5"`.
- `/debug/broadcasts` accumulates multiple `process_upsert` events for the same process.
- The process reaches `state: "done"`.
- After terminal state, the Agent reports the outcome in Chat or a Ping.
- The repo test passes after the Sub-agent finishes.

Scenario B:

- The Agent starts a long-running Codex Sub-agent with `model: "gpt-5.5"`.
- The Owner asks to stop that process by id.
- The process reaches terminal `state: "cancelled"` or `state: "failed"` and not `done`.

Any model refusal or failure to choose the requested tool after the host exposes the right tools is a prompt-behavior miss. Missing process rows, missing model, no terminal transition, or tests still failing are mechanical failures.
