# Product scenario: responsive Thread workspace

## Owner-visible outcome

At wide CSS viewports, Threads is an in-flow dock beside a readable conversation and a bounded artifact preview. It opens by default without taking focus, stays open while selecting a Thread, and remembers an explicit desktop close. At narrower widths, Threads returns to a keyboard-contained modal drawer and artifacts use the available side pane or a full-screen phone pane. Resizing preserves the selected Thread and artifact. Primary turn status metadata stays on one compact line.

## Deterministic saved-fixture check

This layout scenario makes no model calls. It copies the saved `artifact-presentation` state into a new evidence directory, boots that copy with the scripted/fake Host on an unused non-3076 port, selects its real stored SVG artifact through the UI, and checks 2048, 1440, 1024, 768, and 390 CSS-pixel viewports plus live resize, focus, persistence, and the unaddressed overview. It also adds four ancestors to the copied fixture and disconnects the isolated browser so the longer atomic “Last known: Working · 1 queued” status is checked against both its Thread row and inventory bounds without overlapping row actions.

Use an archived state directory from a successfully judged `artifact-presentation` product run. If no saved fixture exists, `just product-runbook artifact-presentation` creates one under its printed evidence path according to [the product runbook rules](../RULES.md); that separate scenario uses its documented one-turn model budget and is not part of this deterministic layout check.

Build the exact checkout under the workspace feature graph, then run:

```bash
cd app && npm run build
cd .. && cargo build --workspace --all-targets
HIRSEL_RESPONSIVE_FIXTURE=/tmp/hirsel-product-runbooks-final-acceptance-presentation/artifact-presentation/state \
  node e2e/workspace-responsive.mjs
```

`HIRSEL_RESPONSIVE_EVIDENCE` can name the output directory. The runner records `window.innerWidth` and `devicePixelRatio`; those CSS viewport metrics, rather than screenshot bitmap width, decide the responsive mode. It refuses a missing saved fixture or live port 3076, writes a screenshot for every checkpoint, and prints `OBJECTIVE_PASS` only after all geometry, overflow, focus, resize, selection, and persisted-choice assertions pass.

## Agent judgment

Inspect the dark-theme screenshots together in one bounded pass. Confirm that the dock, center surface, and artifact read as one coordinated workspace at 2048 and 1440; that the 1024 side-by-side state remains usable; that 768 and 390 are intentional single-pane views; that the overview remains legible with an artifact open; and that turn outcome plus age reads as one compact status rather than a broken grid of words. Record the evidence directory and any failed exact assertion.
