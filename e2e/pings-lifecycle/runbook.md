# Pings Lifecycle Runbook

## Purpose

Prove the complete Ping lifecycle: required name and description fields, reply-driven automatic resolution, lifecycle-neutral mentions, explicit Owner resolution, and Agent resolution of a moot Ping.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/pings-lifecycle/run.sh
```

Scenario A uses `HIRSEL_AGENT=scripted HIRSEL_DRIVER=fake` for deterministic delegation and lifecycle transitions. Scenario B uses `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake` to exercise the real `pings.send` and `pings.resolve` tools.

## Gates

Scenario A:

- Scripted delegation sends an open `requires_response: true` Ping with a Quick Reply, non-empty `name`, and non-empty `description`.
- An Owner message whose `ref` equals the Ping Anchor changes it to `status: "done"` and broadcasts `ping_upsert`; no Agent resolve call is needed.
- A second Owner message carries the Ping id in `mentions` with no `ref`; the mentioned Ping remains open.
- `POST /debug/resolve-ping` changes the second Ping to done and broadcasts `ping_upsert`.

Scenario B:

- The real Agent sends a named Ping with a non-empty description via `pings.send`.
- An unreferenced Owner message declares it moot, so automatic reply resolution cannot satisfy the gate.
- The Agent calls `pings.resolve`; the Ping becomes done and the committed tool summary contains `pings_resolve`.

Real-Agent failure to choose either Ping tool is a prompt-behavior miss. Missing lifecycle state or broadcasts are mechanical failures.
