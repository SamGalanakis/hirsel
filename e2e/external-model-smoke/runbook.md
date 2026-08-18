# Optional External-Model Host Smoke

> This is a manual, cost-bearing supplement. The credential-free real-Host proof in
> [`../generated-task-ui/runbook.md`](../generated-task-ui/runbook.md) remains the required gate.

## Contract

The smoke launches the production Rust Host with `HIRSEL_AGENT=lash`, seeds the same
validated adaptive Task fixture, submits the same required form payload through
`AppState::handle_event_action`, and polls persisted Host state until the external Agent
uses `events.recompose` on that exact Task id and Anchor.

It performs at most one external model turn, binds only to loopback, runs from a neutral
temporary directory, and deletes its temporary database and process group. Reports record
provider/credential-source presence but never credential values, raw provider responses,
tokens, or prompts.

## Safety

Check availability without spending:

```sh
cd app
npm run e2e:task-host-external-smoke
```

Execute only with explicit authority and an already configured provider:

```sh
HIRSEL_EXTERNAL_SMOKE=1 HIRSEL_SMOKE_PROVIDER=codex npm run e2e:task-host-external-smoke -- --run
```

Use `HIRSEL_SMOKE_PROVIDER=anthropic` only when `ANTHROPIC_API_KEY` is already present.
The Codex path requires the existing private `~/.codex/auth.json`. Never paste either
credential into a command or report.

Evidence is replaced at
[`../reports/task-host-external-smoke-latest.md`](../reports/task-host-external-smoke-latest.md)
and its JSON twin. A `NOT_EXECUTED` report is the only honest result when authority,
provider selection, or credentials are absent.
