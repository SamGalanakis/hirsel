# Product runbook rules

Read this file before running any scenario under `runbooks/`. These are
agent-judged product checks against the shipping web app and Host, not another
unit-test framework.

## Layers and evidence

Use both layers without confusing their claims:

- Existing Rust, frontend, and `e2e/*.mjs` tests are deterministic contract
  evidence. They may use scripted providers and debug fixture routes.
- Product runbooks drive the real browser surface through the real model-facing
  runtime. The Agent judges what the Owner can actually see. A scripted reply,
  direct database mutation, debug artifact publication, or green CI result
  cannot satisfy a real-model runbook gate.

For every conversation-changing step reconcile three surfaces:

1. the rendered DOM, including visible row order and exact result text;
2. the authenticated `open_thread` snapshot and captured WebSocket frames;
3. the isolated Host's `hirsel.sqlite` rows.

Record stable message, turn, tool-call, and artifact IDs. A disagreement between
surfaces is a failure even if each surface looks internally consistent.

## Isolation and model calls

- Run only against a fresh data directory and an unused loopback port. Port
  `3076` is forbidden. Never copy Owner history.
- The Host must serve the production frontend built from the same checkout.
- Use the configured real provider and record the model and reasoning variant
  from `hello_ok`. Missing or unusable provider authentication is a blocker,
  not permission to substitute a fake response.
- The complete battery is bounded to five initial turns: two chronology turns,
  two tool turns, and one artifact turn. Rerun only a failed scenario after a
  relevant fix. Do not add retries that spend more model calls.
- Preserve only sanitized evidence. Never record auth frames, tokens, provider
  credentials, or unrelated configuration.

## Driving and waiting

- Use actual controls. DOM injection, client-store mutation, direct tool calls,
  and debug publication routes do not prove product behavior.
- Use unique, noncoincidental markers for every run.
- Poll explicit predicates with deadlines: running/queued/terminal turn states,
  expected rendered text, and stable row counts. Fixed sleeps do not decide
  whether work finished.
- Screenshots accompany assertions; they do not replace them. Capture every
  named checkpoint with the relevant rows scrolled into view.
- Individual tool payloads may expand in place. A whole-turn disclosure hiding
  reasoning, tool calls, or results fails the inline chronology contract.

## Abort and report

Stop the current scenario on the first browser error, timeout, Host exit,
unexpected terminal state, answer-key mismatch, or cross-surface disagreement.
Capture the failing command/gate, last DOM extract and screenshot, frames,
`open_thread` snapshot, database extract, and Host status. Diagnose the stage:
browser event handling, wire projection, turn execution, tool realization,
storage, or rendering. Do not repair the product during that run.

The runner reports `OBJECTIVE_PASS` only for its deterministic predicates. It
also records `scorecardStatus: "NOT_JUDGED"`; that status cannot be promoted by
the runner. After inspecting the screenshots and extracts, an Agent fills the
runbook scorecard with the exact rendered element or backend row that passed
each gate and records a separate judged verdict. Teardown only the process
group the runner started and retain its evidence directory.

## Runner

From the repository root:

```bash
just product-runbook all
just product-runbook chat-chronology
just product-runbook tool-execution
just product-runbook artifact-creation
```

`all` uses three separate empty stores and no more than five model turns. Set
`HIRSEL_RUNBOOK_ARTIFACTS` to choose the evidence root. The runner prints that
path even when a scenario aborts.
