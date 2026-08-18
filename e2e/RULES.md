# Task E2E rules

This directory's primary suite covers the current Task Margins product surface:
one global Hirsel, flat Task dives, generated Task instruments, and temporary
utilities. Historical wire and backend-only scenarios are isolated under
[`protocol-compatibility/`](protocol-compatibility/README.md).

## Automated runs

From `app/`:

```bash
npm run e2e:task-margins
npm run e2e:task-host
```

`e2e:task-margins` boots an isolated mock plus Vite on controlled ports, polls
readiness, and drives the shared desktop, 390px phone, and 320px narrow suite
for all four Task Margins runbooks. It captures outgoing WebSocket frames,
browser errors, request failures, and screenshots, then replaces
[`reports/task-margins-latest.md`](reports/task-margins-latest.md) and its JSON
twin.

`e2e:task-host` is the production-boundary adaptive Task proof. It builds and
launches the real Rust Host from a neutral temporary directory with the
deterministic scripted global Agent, launches Vite separately, polls both
services, and drives a generated non-settling action through Agent
recomposition, terminal settlement, reload, and reopen. It replaces
[`reports/task-host-latest.md`](reports/task-host-latest.md), its JSON twin, and
the Host-run screenshot.

`e2e:task-host-external-smoke` is an optional, explicit-cost supplement. Its
default check-only mode never calls a provider; see the
[`external model smoke runbook`](external-model-smoke/runbook.md). It is not a
substitute for the deterministic Host gate.

## Primary scenario index

- [`task-continuity`](task-continuity/runbook.md) — global start, deliberate
  Task dive, Task/global composer scope, interleaved attribution, and return
  without context loss.
- [`generated-task-ui`](generated-task-ui/runbook.md) — constrained JSON
  instruments, adaptive same-Task stages, action payloads, settlement, reload,
  and reopen. This runbook owns the real-Host adaptive proof as well as its
  Task Margins gates.
- [`utility-continuity`](utility-continuity/runbook.md) — Processes, Settings,
  and Canvas as temporary utilities that return to the same Task, composer
  scope, and draft.
- [`task-responsive-keyboard`](task-responsive-keyboard/runbook.md) — one Task
  inventory, desktop roving keys, phone containment, touch targets, focus
  restoration, and honest shortcut presentation.
- **Host adaptive proof** —
  [`task-host-runner.mjs`](task-host-runner.mjs) drives the real Rust
  Host/global-Agent/reducer/storage path specified by
  [`generated-task-ui`](generated-task-ui/runbook.md), with evidence in the
  [`latest Host report`](reports/task-host-latest.md).

These four runbooks plus the Host-adaptive path are the whole primary index.
Compatibility runbooks are governed by their own
[`RULES.md`](protocol-compatibility/RULES.md).

## Evidence boundary

Drive the real web app in a clean browser context against either the declared
dev mock or the debug Host. Assert visible semantics, keyboard focus, layout
containment, and the exact outgoing frame or authoritative Host update named by
the runbook.

A run is void if the asserted behavior came from DOM injection, direct store
mutation, fabricated network state, or reading implementation source in place
of observed behavior.

## Pre-flight

1. Record checkout revision and dirty state; prove Vite serves that checkout.
2. Use the automated runners unless the runbook explicitly calls for a manual
   diagnostic pass. They choose and own their service ports.
3. Use a fresh browser context and a unique non-secret development token.
4. Record viewport sequence, color scheme, reduced-motion preference, console
   errors, page errors, and failed requests.
5. Use mouse, keyboard, touch emulation, and observable network frames only.
6. Use exact runbook viewports. A resized screenshot without interaction is
   not evidence.

## Isolation and polling

Host fixtures run from neutral temporary working/data directories and must not
inherit the repository as runtime state. Every asynchronous gate polls an
observable condition with a deadline; fixed sleeps are not proof. Runners own
their process groups and must leave no Host, Vite, or browser process behind.

## Abort conditions

Abort on a service error, malformed frame, unexpected browser error, failed
request, timeout, incorrect geometry/focus state, or evidence fabricated
outside the declared path.

Do not repair the application while claiming to execute a runbook. Fixes and
fresh proof runs are separate operations.

## Report format

Preserve:

- revision and dirty state;
- start/completion timestamps;
- exact invocation and service kinds;
- browser and viewport environment;
- screenshot paths;
- outgoing frames and authoritative Host observations;
- every passed/failed gate; and
- the failing command/frame, last observed state, and RCA.
