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
