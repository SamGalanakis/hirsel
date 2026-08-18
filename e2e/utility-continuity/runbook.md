# Utility Continuity E2E

> **Read [../RULES.md](../RULES.md) first** — task-surface pre-flight, isolation, Abort/RCA, and reporting rules apply.

Automated core: `cd app && npm run e2e:task-margins` → [latest report](../reports/task-margins-latest.md).

## Purpose

Prove Processes, Settings, and Canvas are temporary utilities, never destinations: each overlays or docks beside the same task world, and closing it restores the exact task focus and draft.

## Persona and Probe

Act as Sam inspecting background work mid-decision. Open `deploy 4821` and type the unsent draft `compare blast radius` before summoning utilities.

## Phase 0 — Pre-Flight and Isolation

1. Start the seeded mock and Vite per [the shared rules](../RULES.md). Require seeded processes.
2. Use a fresh 1440×1000 browser context, authenticate, open `deploy 4821`, and enter the probe draft without sending.
3. Record task id, focus state, draft, task scrollTop, and active element.

## Phase 1 — Processes

1. Open the overflow menu, then `Processes`.
2. Require `[data-slot="processes-panel"]`, literal running/finished group labels, and no second conversation or nested-task navigation.
3. Close with Escape. Require the same focused deploy task, exact unsent draft, and prior task scrollTop.
4. Reopen Processes and activate `Ask Hirsel to stop` on a running Sub-agent. Require the utility to close and the same standing composer to receive the exact process-id/label prefill while Task focus remains unchanged; no new destination may mount.

## Phase 2 — Settings

1. Preserve the current task focus, open `Settings`, and require `[data-slot="settings-panel"]`.
2. Toggle theme once and require the document theme changes. Close with the labelled close control and require the same Task and composer value.
3. Reopen Settings through `Model settings`; require the Models heading is within the inspector viewport as the landing target, then close.

## Phase 3 — Canvas When Available

1. If `Canvas` is absent from the overflow, record `not seeded` and mark this phase not applicable rather than fabricating a view.
2. If present, open it and require exactly one canvas utility. Close it and require task-focus/draft continuity as above.

## Phase 4 — Phone Utility Semantics

1. Repeat Processes at 390×844.
2. Require a modal dialog with honest `aria-modal`, trapped Tab focus, a 44px close/back target, and body/task controls unavailable to Tab until close.
3. Close and require focus restoration plus unchanged task focus and draft.

## Evidence

In addition to the provenance required by [the shared report format](../RULES.md#report-format), record before/after Task id, selected Task `aria-pressed`, composer `data-focused`, exact draft/prefill value, scrollTop, active element, utility role/aria-modal, Models landing bounds, Canvas present/N/A result, screenshots for desktop and phone, and console/page/request errors.

## Scorecard

| Gate | Objective evidence | Result |
| --- | --- | --- |
| Processes temporary | Panel mounts alone and closes to same state | |
| Ask-to-stop | Same composer receives prefill | |
| Settings temporary | Correct section; same state on close | |
| Canvas temporary | Same state on close, or honest N/A | |
| Phone modal | Focus trap, honest semantics, 44px close | |

**Aggregate:** every applicable gate passes. Capture the first broken transition and Abort/RCA without repairing during the run.
