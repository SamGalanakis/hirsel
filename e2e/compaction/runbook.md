# Compaction Runbook

## Purpose

Prove the Agent can compact itself with `continue_as`, that the operation is visible through debug broadcasts or committed tool-call summaries, and that a pre-compaction fact remains available afterward.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/compaction/run.sh
```

The runner uses `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake`.

## Gates

- The Agent acknowledges the pre-compaction fact.
- The Owner asks the Agent to compact immediately.
- A post-compaction question correctly recalls the pre-compaction fact.
- `/debug/broadcasts` or `/debug/chat` shows a visible `continue_as`/control/compaction tool event.

On this branch, `prompts/agent.md` documents `control.continue_as`, but the host tool definition list does not expose a matching tool. If recall works but no control event is visible, this runbook fails mechanically for missing compaction surface.
