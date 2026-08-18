# Task Responsive and Keyboard E2E

> **Read [../RULES.md](../RULES.md) first** — task-surface pre-flight, isolation, Abort/RCA, and reporting rules apply.

Automated core: `cd app && npm run e2e:task-margins` → [latest report](../reports/task-margins-latest.md).

## Purpose

Prove the one task world remains navigable without a labyrinth across desktop and phone: one inventory, efficient roving keys, contained horizontal scrolling, honest shortcuts, visible focus, and touch-safe controls.

## Persona and Probe

Use keyboard only on desktop, then touch emulation only on phone. The probe is to open the second task, act globally without closing it, and return to the first task.

## Phase 0 — Pre-Flight and Isolation

1. Start the seeded mock and Vite per [the shared rules](../RULES.md).
2. Use a fresh context with reduced motion enabled. Authenticate and record console errors.

## Phase 1 — Desktop Keyboard at 1440×1000

1. Press `g`, then `t`. Require focus in the sole `Tasks` navigation.
2. Press ArrowDown. Require focus and selection to move to the next task, its content scrollTop to reset to zero, and `aria-pressed=true` to move with focus.
3. Press End, Home, ArrowUp, and ArrowRight. Require deterministic wrap/boundary behavior with no focus loss.
4. Press `/`; require focus in `Message Hirsel`. Press Tab until focus has left the text field, then press `?`, inspect the shortcut sheet, and close with Escape. `?` must remain ordinary text while the composer itself owns focus.
5. Require no shortcut claims a generated submit accelerator. Press `g p`, close Processes, then `g s`, close Settings; focus remains usable.

## Phase 2 — One Inventory

1. Clear focus by activating the selected Task again.
2. Require exactly one `[data-slot="task-index"]`; each task name occurs once as inventory, with no duplicate ambient list or count-led dashboard.
3. Require literal statuses in the buttons' visible text and accessible names.

## Phase 3 — Phone at 390×844

1. Resize exactly to 390×844 and reload into global state.
2. Require `document.documentElement.scrollWidth <= 390`; only the task strip may scroll horizontally.
3. Swipe the task strip, open each task, and require the strip remains within viewport bounds, content is one column, and the composer remains at the thumb edge.
4. Measure every Task button, generated choice, attach, Send, send-options, overflow, and utility close target. Each must be at least 44px in its actionable dimension.
5. Clear and restore focus through the same Task strip item.

## Phase 4 — Narrow Floor at 320×700

1. Resize to 320×700.
2. Enter a 180-character composer draft and attach no files. Require no document-level horizontal overflow, the composer stays within 320px, the text area grows/scrolls, and only the Task strip owns horizontal overflow.
3. With reduced motion, require no required content depends on animation completion.

## Evidence

In addition to the provenance required by [the shared report format](../RULES.md#report-format), record focus sequence, selected ids, scrollTop after navigation, shortcut sheet text, DOM inventory counts, every named bounding box/hit size, document/task-strip scroll widths, desktop/phone/narrow screenshots, and console/page/request errors.

## Scorecard

| Gate | Objective evidence | Result |
| --- | --- | --- |
| Roving navigation | Focus, selection, and scroll reset agree | |
| Shortcut truth | Every displayed shortcut works; no phantom accelerator | |
| One inventory | One task index; no duplicate overview list | |
| Phone containment | No document overflow; strip owns horizontal scroll | |
| Touch targets | All named controls meet 44px | |
| 320px floor | Composer/task remain usable | |

**Aggregate:** every gate passes. Capture viewport, focused element, bounding box, and screenshot on the first failure, then Abort/RCA without repair.
