# Product scenario: rendered and source artifact presentation

## Purpose

Prove that an Owner can inspect the exact stored source of renderable artifacts
without executing it, then return to the normal rendered result. Cover both the
ordinary Artifact preview and a Thread showcase at desktop and phone widths.

## Stable controls and selectors

- Presentation group: `[data-slot="artifact-presentation-toggle"]`, accessible
  name `Artifact presentation`.
- Mode buttons: `Rendered` and `Source`; the selected button has
  `aria-pressed="true"` and `data-presentation-mode="rendered|source"`.
- Inert source container: `[data-slot="artifact-source"]`.
- Ordinary viewer: `[data-slot="artifact-preview"]` / `Artifact preview`.
- Showcase viewer: `[data-slot="thread-showcase"]` / `Thread showcase`.

## Scenario

The route-free open lands in the bootstrapped **Home** Space chat, with Space,
Focus and Worker separately labelled in the composer; the scenario creates its
own Space chat and works there. All four `artifacts.create` calls belong to one
turn, which shares one payload panel, so each call is opened and captured in
its own right.

Use isolated disposable Host data and the production web build. Create real
HTML, Markdown, SVG, and Solid artifacts with distinctive leading whitespace,
HTML-looking text, and a trailing newline. Do not use port 3076.

Run `just product-runbook artifact-presentation`. The scenario uses one Owner
turn and requires four successful real `artifacts.create` calls; scripted
publication or direct database mutation does not satisfy it.

For each format in the ordinary viewer and showcase:

1. Open the artifact and require `Rendered` to be selected by default. Verify
   the format's actual rendered result.
2. Focus and activate `Source` using the keyboard. Require the same button to
   retain focus, the iframe/rendered tree to be absent, and the source
   container's `textContent` to equal the stored content byte-for-byte.
3. For HTML and Solid, include an observable side effect in source and prove it
   does not run while Source is selected. For Solid, prove no compiler worker is
   started in Source mode.
4. Refresh the same artifact's content and require Source to remain selected.
   Change to a different artifact, then close and reopen the viewer, and require
   Rendered to be restored each time.
5. Activate `Rendered` and verify the current content renders. Download from
   both modes and require the original filename, MIME type, and bytes.
6. Repeat at 390 x 844. Require a clear title row and the mode, Download,
   actions, and Back controls to remain within the viewport with no horizontal
   overflow.

Capture one desktop and one phone screenshot with the Source mode selected in
each surface, plus machine-readable assertions for content equality, focus,
non-execution, reset/refresh behavior, downloads, and viewport overflow.

## Known upstream blocker

The reasoning-integrity gate cannot pass on a Native session while the model
emits more than one reasoning summary in a turn. Lash gives every anonymous
reasoning delta in a model call the same fallback correlation id and carries no
block boundary, so the Host cannot tell a new block from the next chunk
([Ascending-AI/lash#1769](https://github.com/Ascending-AI/lash/issues/1769),
found at pin `47e6e23764939c790961fbe2905ee08ff5373a95`). Hirsel does not work
around this, and the gate is not loosened: a Native run that produces two or
more reasoning summaries fails here until the Lash pin carries the fix. Claude
CLI and Codex CLI turns take block identity from their own event streams and
are unaffected.

## Judged run — 2026-09-20

Source `ed16130`, provider `codex`, model `gpt-5.6-sol` variant `medium`, one
model turn. Evidence: `presentation-run3/artifact-presentation`. Objective
result: **ABORT**, at the reasoning-integrity gate. Judged verdict: **FAIL — a
product fault in how consecutive reasoning blocks are rendered**.

| Item | Verdict | What passed it |
|---|---|---|
| Space chat landing | PASS | `result.json` `landedSpaceChatId: 1`; the scenario's own Space chat opened empty on all three surfaces with `schemaVersion 14` |
| Four real creations | PASS | exactly four `artifacts_create` `tool_start`/`tool_done` pairs, all `ok`, and exactly four `artifact_upsert` frames |
| Exact stored formats | PASS | SQLite: `html`/null/null, `markdown`/null/null, `image`/`image/svg+xml`/null, `solid`/null/null, each with the exact content including its leading space and trailing newline |
| Rendered trace | PASS | `10-created-formats-call-*.png` show the run card's steps — one Agent code cell and four `artifacts_create` rows in arrival order, each with its own content summary, `ok` mark and duration — matching the canonical events exactly |
| Per-call payload | PASS | each call was opened in turn and its `tool_start.input` and `tool_done.result` text appeared in the shared panel for that call id |
| Reasoning integrity | **FAIL** | the turn streamed three separate reasoning blocks (`**Clarifying artifacts.create usage constraints**`, `**Preparing precise artifacts.create calls**`, `**Ensuring content formatting with spacing and newlines**`). They are concatenated with no separator, so the Owner reads one unbroken run — `Clarifying artifacts.create usage constraintsPreparing precise artifacts.create callsEnsuring content formatting with spacing and newlines` (`10-created-formats-call-0.png`) — and the adjoining `**` markers form `****`, which destroys the emphasis |
| Presentation sweep | NOT REACHED | the run aborted before the desktop and phone Rendered/Source sweep |

**Aggregate.** Creation, storage and the trace are truthful. The turn's thinking
is not: three distinct thoughts are presented as one sentence. That is a
rendering fault, not a runbook expectation that drifted.
