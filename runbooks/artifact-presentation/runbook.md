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

The route-free open lands in the client-created ordinary **Home** Space chat, with its one
recipient labelled in the composer; the scenario creates its
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

## Judged run — 2026-09-21

Source `e565e97` with a clean tree, provider `codex`, model `gpt-5.6-sol`
reasoning variant `medium` from `hello_ok`, one model turn. Evidence:
`runbook-evidence-2/all-run1/artifact-presentation`, with the no-model sweep in
`runbook-evidence-2/probes`. Objective result: **ABORT**, in the desktop
showcase sweep. Judged verdict: **FAIL — a product fault in the Space chat's
own layout, not a runbook expectation that drifted**.

This turn emitted a single reasoning summary, so the
[lash#1769](https://github.com/Ascending-AI/lash/issues/1769) blocker did not
bite and the reasoning-integrity gate passed on its own terms. It remains a
blocker for any turn that emits two or more.

| Item | Verdict | What passed it |
|---|---|---|
| Space chat landing | PASS | `result.json` `landedSpaceChatId: 1`; the scenario's own Space chat opened empty on all three surfaces at `schemaVersion 18` |
| Four real creations | PASS | exactly four `artifacts_create` `tool_start`/`tool_done` pairs, all `ok`, and exactly four `artifact_upsert` frames, all inside one turn |
| Exact stored formats | PASS | SQLite: `markdown`/null/null, `image`/`image/svg+xml`/null, `solid`/null/null, `html`/null/null, each holding its exact content including the leading space and the trailing newline |
| Rendered trace | PASS | `10-created-formats.png` shows the run card's steps — one Agent code cell and four `artifacts_create` rows in arrival order, each with its own content summary, `ok` mark and duration — matching the canonical events exactly, `traceGated: false` |
| Per-call payload | PASS | `10-created-formats-call-0..3` each opened one call in turn, and its `tool_start.input` and `tool_done.result` text appeared in the shared panel for that call id |
| Reasoning integrity | PASS | the turn streamed one reasoning block, `**Planning four exact artifact creations**` under a single `block_id`, rendered once |
| Desktop preview sweep | PASS for the formats reached | HTML and Markdown both defaulted to `Rendered`, rendered their actual result, switched to `Source` by keyboard with focus retained, showed the byte-exact stored content with no iframe and no side effect, downloaded identical bytes under the original name from both modes, and reset to `Rendered` on reopen |
| Desktop showcase sweep | **FAIL** | the HTML showcase passed; the run then aborted opening the second artifact's **Open with**. Playwright's own log names the cause: `<aside aria-label="Space board">… intercepts pointer events`. Once any pane stands to the right of a Space chat at 1440x900, the board is laid over the conversation instead of sharing the width with it |
| Phone sweep | NOT REACHED | the run aborted before 390x844 |

**Aggregate.** Creation, storage, the trace and the presentation contract itself
are truthful for everything the run reached. The Space chat's layout is not: a
Space chat beside any right-hand pane loses the right edge of its own
conversation to the board.

### The layout fault, exactly

Reproduced with no model at all (`probes/probe-overlap.mjs`, evidence
`probes/overlap1/`), at 1440x900 with the inventory docked:

| | `main[data-thread-id]` | `aside[aria-label="Space board"]` | overlap |
|---|---|---|---|
| no right-hand pane | x 352 → 886 | x 894 → 1428 | 0px |
| showcase open | x 352 → 768 | x **651** → 943 | **117px** |

The Space chat column has to give width to the showcase, the conversation
refuses to shrink past its 26rem reading minimum (`split:min-w-[26rem]`), and
it overflows underneath `ThreadBoard`, which paints on top. `overlap1/
with-showcase.png` shows the result: the board's own heading overprints the
chat header, and the composer's `Remove artifact context` control is covered —
`document.elementFromPoint` over its centre returns the board, a real click
never lands, while the same button dispatched programmatically works. The same
picture appears in two real-model runs that had nothing to do with each other:
`all-run1/artifact-presentation/desktop-html-showcase-source.png` and
`all-run1/artifact-creation/40-natural-cat-preview.png`.

Stage: client rendering/layout. `SpaceChatShell` gives the chat and the board
one flex row each with `flex-1`, and `ThreadShell`'s pane budget
(`sideCollapsed`, `fourPanes`) only ever weighs the utility region against the
showcase — the board was added to the row without a share of that budget. Not
worked around here, and no oracle was relaxed to get past it.
