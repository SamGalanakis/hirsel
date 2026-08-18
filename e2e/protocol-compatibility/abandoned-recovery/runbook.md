# Abandoned Recovery Runbook

## Purpose

Exercise ADR-0004's recovery promise: an in-flight Sub-agent that dies with the host should not be mechanically resurrected. On reboot, it should surface as abandoned, wake the Agent, and let the Agent exercise judgment.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/protocol-compatibility/abandoned-recovery/run.sh
```

The runner uses `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake` with a long fake fixture, SIGKILLs only the host PID it started, then reboots on the same data dir.

## Gates

- A fake-driver Sub-agent is started through the real Lash Agent and visible in `/debug/processes`.
- The host is killed while the process is still running.
- After reboot, no new running Sub-agent process is mechanically spawned.
- `/debug/processes` surfaces the orphan as `state: "abandoned"`.
- The Agent is woken and emits Chat or Ping output that references the orphaned/abandoned work.

If no respawn occurs but no abandoned row is visible, the no-respawn gate passes and abandoned visibility fails mechanically.
