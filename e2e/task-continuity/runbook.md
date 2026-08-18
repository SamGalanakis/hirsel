# Global and Task Continuity E2E

> **Read [../RULES.md](../RULES.md) first** — task-surface pre-flight, isolation, Abort/RCA, and reporting rules apply.

Automated core: `cd app && npm run e2e:task-margins` → [latest report](../reports/task-margins-latest.md).

## Purpose

Prove that Hirsel starts ambient, a task dive changes only the subject, and the same composer follows focus without creating another conversation destination.

## Persona and Probe

Act as Sam moving from morning orientation into one production decision. Use exact probe `Summarize what matters across everything` globally and `What would make this unsafe?` in the deploy task.

## Phase 0 — Pre-Flight and Isolation

1. Start the mock and Vite exactly as specified in [the shared rules](../RULES.md); record PIDs and checkout revision.
2. Open a fresh 1440×1000 browser context, clear storage, enter a unique non-empty token, and wait for `Connected` plus the seeded task inventory.
3. Record console errors and the initial `hello_ok`. Any error frame or stale deployment is Abort/RCA.

## Phase 1 — Ambient Resting State

1. Without selecting a task, require one `[data-slot="ambient-field"]` and one `Tasks` navigation, with no ambient title or mode label.
2. Require no `[data-slot="task-field"]`, no composer placeholder, and `data-focused=false` on the composer shell.
3. Count each seeded task name across the visible resting surface. Each appears once, in the task navigation; a second inventory fails.
4. Send the global probe. In the outgoing `send_message`, require `ref: null` and no task id added to `mentions`.

## Phase 2 — Deliberate Dive

1. Select `deploy 4821` from the task navigation.
2. Require one screen-reader Task identity, the generated question `Ship build 4821 to production?` as the larger visible semantic heading, and literal status `blocked on you` in the selected Task row. The main field must not repeat identity or status.
3. Require the selected Task row to carry `aria-pressed=true`, the composer shell to carry `data-focused=true`, and no scope control.
4. Send the task probe. Require the outgoing `send_message` to carry the task anchor as `ref` and task id in `mentions`; it must not carry a side-session scope.

## Phase 3 — Clear and Restore Focus

1. Activate the focused `deploy 4821` row again. Require the task field to disappear, the ambient field to remain untitled, and the composer shell to carry `data-focused=false`.
2. Send `Compare this with the auth task`. Require an ordinary ambient `send_message` with `ref: null` and no task id injected.
3. Activate `deploy 4821` again. Require the same task to return focused through the same single composer.

## Evidence

In addition to the provenance required by [the shared report format](../RULES.md#report-format), record screenshots for Phases 1–3, task id/anchor, the three complete outgoing `send_message` frames, visible headings/statuses, inventory counts, focus owner after each focus transition, and console/page/request errors.

## Scorecard

| Gate | Objective evidence | Result |
| --- | --- | --- |
| Ambient default | Untitled ambient field; no task field or mode UI | |
| One inventory | One Tasks navigation; each task name once at rest | |
| Deliberate dive | Generated question and literal status visible | |
| Task send | Main frame contains anchor + task mention, no side scope | |
| Ambient aside | Clearing focus produces an unanchored send | |
| Return | Same task restores through the same composer | |

**Aggregate:** every gate passes. On any failure, capture the exact action, DOM/network evidence, and last good state, then Abort/RCA without repair.
