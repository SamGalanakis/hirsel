# Multi-turn Memory Runbook

## Purpose

Prove ordinary conversation continuity with the real Codex Agent: facts from earlier Owner messages are recalled in a later turn, and the same session memory survives a host restart on the same data dir.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/multi-turn-memory/run.sh
```

The runner uses `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake`; fake driver is present only to avoid real Sub-agent work and is not used by the scenario.

## Gates

- Message 3 correctly recalls facts introduced in messages 1 and 2.
- After SIGTERM restart on the same data dir, message 4 correctly recalls the same facts.
- No `Agent turn failed` message appears in the transcript.

Wrong recall is a prompt-behavior miss. Lost history after restart or `Agent turn failed` is a mechanical failure.
