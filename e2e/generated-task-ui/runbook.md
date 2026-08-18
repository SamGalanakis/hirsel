# Generated Task Instrument E2E

> **Read [../RULES.md](../RULES.md) first** — task-surface pre-flight, isolation, Abort/RCA, and reporting rules apply.

Automated core: `cd app && npm run e2e:task-margins` → [latest report](../reports/task-margins-latest.md).

Production-boundary proof: `cd app && npm run e2e:task-host` → [latest Host report](../reports/task-host-latest.md), [JSON](../reports/task-host-latest.json), and [screenshot](../reports/task-host-latest.png). This path launches the real Rust Host and WebSocket/reducer/storage stack with the deterministic scripted global Agent, so it needs no credentials or external model network and performs no client-side Event upsert.

## Purpose

Prove that constrained JSON UI is the Task's primary instrument: distinct question/choice and field/submit shapes render directly, a non-settling action recomposes the same Task into the next stage, a second action settles, and reopen honestly restores the prior actionable stage.

## Persona and Probe Bank

| Task | Exact action | Objective effect |
| --- | --- | --- |
| `deploy 4821` | Choose `Ship now`, then `Promote to 100%` | first `event_action { action: "advance", data.choice: "A" }` keeps Task open at canary stage; second `{ action: "choose", data.choice: "A" }` settles |
| `auth pr` | Enter reviewer `Sam`, activate `Open PR` | `event_action { action: "submit", data.reviewer: "Sam" }`; settled state appears |

## Phase 0 — Pre-Flight and Isolation

1. Start a fresh seeded mock and Vite instance per [the shared rules](../RULES.md); record mock stdout so action frames are observable.
2. Use a fresh 1440×1000 browser context and authenticate with a unique non-empty token.
3. Confirm both tasks are present and no console error exists.

## Phase 1 — Choice Instrument

1. Open `deploy 4821` and require the generated question to be visually/semantically above the supporting copy and choices.
2. Require choices to be flat rows, `Recommended` to be plain text rather than a capsule, and every choice to have a computed hit height of at least 44px.
3. Activate `Ship now`. Require the exact Task id/Anchor/selection/scope to remain while the instrument recomposes to `Canary is healthy. Promote production?`; require literal success plus `5% canary · 0 errors · p95 184ms`, and no `Task decided` yet.
4. In the outgoing frame require the exact event id, `advance`, and `{choice:"A", label:"Ship now"}` payload.
5. Activate `Promote to 100%`. Require the exact `choose` payload, `Task decided`, and a visible `Reopen` action.
6. Reload and require the Task remains done. Activate `Reopen`; require the canary/promotion stage—not the initial ship stage—to be interactive again and require `event_action <id> reopen`.

## Phase 2 — Field Instrument and Shortcut Truth

1. Open `auth pr`. Require its field/submit composition to differ from the choice task while sharing type and spacing tokens.
2. Require no generated keyboard token such as `⌘↵` unless it is actually wired. The seeded UI shows none.
3. Enter `Sam` in `Reviewer`, activate `Open PR`, and require settled state.
4. Require mock stdout to show `submit` with `{"reviewer":"Sam"}`.

## Phase 3 — Status Without Color

1. Open `nightly backup`.
2. Disable color in browser emulation or inspect in grayscale.
3. Require the literal state `success` and label `0 errors`; the dot alone cannot carry meaning.

## Evidence

In addition to the provenance required by [the shared report format](../RULES.md#report-format), record task ids, resolved UI text/roles, computed heading sizes and control heights, screenshots before/after settle, complete mock action frames, reload result, status literals, and console/page/request errors.

## Scorecard

| Gate | Call evidence | Visible effect | Result |
| --- | --- | --- | --- |
| Adaptive choice | `event_action advance` with A | Same Task shows canary status/promotion stage, remains open | |
| Promotion | `event_action choose` with A | Settled generated state + Reopen | |
| Reopen | `event_action reopen` | Prior canary action stage restored after reload | |
| Form | `event_action submit` with reviewer | Settled state | |
| Shortcut truth | No unwired accelerator shown | Submit remains keyboard reachable | |
| Non-color status | State literal present | Meaning survives grayscale | |

**Aggregate:** frame evidence and visible effect must both pass. Also record console, page, failed-request, and HTTP ≥400 response errors. On failure, capture action/log/DOM state and Abort/RCA without repair.
