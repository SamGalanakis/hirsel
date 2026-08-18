# Model Selection Runbook

## Purpose

Prove only the wire and durability tier of runtime model selection: a main-model change broadcasts
`model_changed` and appears in a fresh `hello_ok.model`; a Sub-agent catalog-row change broadcasts
`subagent_models_changed`; both selections persist across SIGTERM and reboot; one invalid model id
returns an error without mutating the active selection. Registry validation remains unit-test scope.

Fully mechanical: `HIRSEL_AGENT=scripted`, `HIRSEL_PROVIDER=codex`, `HIRSEL_DRIVER=fake`, a
disposable data dir, and a free port chosen from 3330. `hello-snapshot.cjs` uses Node ≥22's built-in
WebSocket from any neutral cwd.

## Execute

```bash
/workspace/code/hirsel-rbcov/e2e/protocol-compatibility/model-selection/run.sh
```

## Gate 1: main-model wire state

`POST /debug/set-model` selects `gpt-5.6-sol/high`. Require a matching `model_changed` in
`/debug/broadcasts`, then open a brand-new `/ws` connection and require the same selection in
`hello_ok.model.current`.

## Gate 2: Sub-agent catalog wire state

Toggle the existing `claude-opus-5` row to `enabled:false` with `default_variant:"high"` through
`POST /debug/subagent-models`. Require the returned catalog and a `subagent_models_changed`
broadcast to carry that row.

## Gate 3: one invalid selection

Submit one unknown main-model id. The endpoint must return its JSON error, `/debug/health` must
still report `gpt-5.6-sol/high`, and the runbook makes no attempt to re-prove the full registry
validation matrix.

## Gate 4: persistence and reconnect replay

Stop the host with SIGTERM and reboot on the same data dir. Require the main selection from
`/debug/health`, the Sub-agent row from `/debug/subagent-models`, and both values again in a fresh
wire-level `hello_ok`.

## Success Gates

- Gate 1: `model_changed` + fresh `hello_ok.model` agree on `gpt-5.6-sol/high`.
- Gate 2: `subagent_models_changed` carries the toggled catalog row.
- Gate 3: one invalid model errors without changing the selection.
- Gate 4: both selections survive SIGTERM + reboot and replay on `/ws`.

## Report

Record the selected model/variant, the toggled provider/model row, the invalid error body, the
broadcast matches, and the before/after `hello_ok` fields.
