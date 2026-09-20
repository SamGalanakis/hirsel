# Product scenario: artifact creation through chat

> Read [../RULES.md](../RULES.md) first.

**Purpose.** Prove that an Owner can request a reusable file in ordinary chat,
the real Agent actually calls `artifacts.create`, and the exact result becomes
listed, previewable, and downloadable.

**Real model calls:** two.

## Named natural-language regression

Keep the ordinary request `Make a picture of a cat artifact` as a distinct
regression. On source `dbbb8d3a99cdaf238feb3c6bd9a997dffe78fb55`, the recorded
real-model turns completed with prose apologies, zero TypeScript cells, zero
tool events, and zero durable calls even though `artifacts_create` was in the
tool catalog. The sanitized evidence is
`/tmp/hirsel-artifact-failure-trace.json` (SHA-256
`16475bb862148db73d628cab958584c8dc978d5e9b2a5fb03e001f75999a8a9d`).

The automated scenario first spends one turn on an exact file receipt so
content, preview, and download have deterministic answer keys. It then spends
the explicitly authorized sixth battery turn on the exact ordinary request
`Make a picture of a cat artifact`; the old failure evidence does not satisfy
that final acceptance gate.

## Golden rules

1. Scripted/debug publication proves only component contracts and cannot pass
   this scenario. Require a real `artifacts.create` start/done pair from the
   addressed turn.
2. `completed` is not an answer key. Require the generated artifact's exact
   title, filename, MIME type, and noncoincidental content.
3. The artifact ID must agree across tool result, `artifact_upsert`, Agent
   message reference, authenticated Thread snapshot, global artifact list, and
   SQLite. `artifacts.create` commits the mutation, an Agent message and the
   reference together (ADR 0017), so the referencing message is that receipt
   inside the same turn, not the turn's closing sentence.
4. Preview and download use actual UI controls. The downloaded bytes must equal
   the requested UTF-8 content exactly.

## Phase 0 — start empty

**Do:** Run `just product-runbook artifact-creation`.

**Expect:** the route-free open lands in the bootstrapped **Home** Space chat
with its one recipient labelled; the scenario's own Space chat
then has no artifacts, messages or turns. Capture `00-empty.png` and all
baseline extracts.

## Phase 1 — request through chat

**Do:** Ask the real Agent to create one `file` artifact using the exact title,
filename, MIME type, and content in the prompt.

**Expect:** the turn emits matched `artifacts.create` start/done events. The
Agent reply references the resulting artifact card; the card's ID matches the
wire and disk artifact. Capture `10-created.png`.

## Phase 2 — list, preview, download

**Do:** Open All artifacts through the UI, select the exact title, inspect the
preview, and use Download.

**Expect:** All artifacts opens as its own named pane; the global list contains
exactly one matching artifact; the preview shows the exact content; the
suggested filename is exact; downloaded bytes are byte-for-byte equal. Capture
`20-listed.png` and `21-preview.png`, and retain the downloaded file.

## Phase 3 — reload

**Do:** Return to the conversation and reload.

**Expect:** the same artifact card ID is still attached to the Agent message
and opens the same content. Save `30-reloaded.png` and `result.json`.

## Phase 4 — natural cat artifact

**Do:** Clear any artifact the earlier preview staged as composer context —
the regression is the ordinary request alone, and an attached example would
soften it — then send exactly `Make a picture of a cat artifact` through the
composer.

**Expect:** the real Agent emits a matched `artifacts.create` start/done pair
and creates exactly one new rendered artifact. Its durable content contains a
graphical image surface, the Agent reply references the same artifact ID, and
opening its card through the UI shows an actual SVG, canvas, or image rather
than escaped source text. The kind and MIME are evidence, but the rendered
browser result is the deciding gate. Save `40-natural-cat-preview.png` and
`41-natural-cat-*`.

## Scorecard

| Item | Objective gate | Verdict | Evidence |
|---|---|---|---|
| Space chat landing | route-free open lands in Home with its recipient labelled | | `00-*`, `result.json` |
| Real creation | matched real `artifacts.create` start/done call | | frames |
| Exact stored result | title, kind, MIME, filename, and content match on wire and SQLite | | store/open snapshot |
| Conversation reference | an Agent message of that turn carries the same artifact ID and a visible card | | `10-created.png` |
| Global listing | exact artifact appears in All artifacts | | `20-listed.png` |
| Preview/download | exact text renders and downloaded bytes match | | `21-preview.png`, download |
| Reload identity | same message/card/artifact IDs and content survive reload | | `30-*`, `result.json` |
| Natural cat | the ordinary request alone, with no staged artifact, creates one referenced, durable, viewable cat artifact | | `40-*`, `41-*`, frames |

**Aggregate:** did an ordinary Owner chat request produce a real, exact,
reusable artifact rather than a prose claim that a tool failed or completed?

## Judged run — 2026-09-20

Source `ed16130`, provider `codex`, model `gpt-5.6-sol` variant `medium`, two
model turns. Evidence: `all-run2/artifact-creation`. Objective result:
**OBJECTIVE_PASS**. Judged verdict: **pass, except the named natural-language
regression, which this run did not put to the test**.

| Item | Verdict | What passed it |
|---|---|---|
| Space chat landing | PASS | `result.json` `landedSpaceChatId: 1`; the composer names its Space recipient and no worker, with placeholder `Message Space chat Runbook artifact-9b4e1d99` |
| Real creation | PASS | matched `artifacts_create` `tool_start`/`tool_done` with `ok: true` on one call id, in the addressed turn |
| Exact stored result | PASS | SQLite row 1: title `Runbook receipt artifact-9b4e1d99`, kind `file`, mime `text/plain`, filename `receipt-artifact-9b4e1d99.txt`, content `HIRSEL-ARTIFACT-artifact-9b4e1d99\n` |
| Conversation reference | PASS | Agent message 2 of turn 1 carries `artifact_ids: [1]` and renders the card; `10-created-dom.json` entry 1 shows `Artifact: Runbook receipt artifact-9b4e1d99` with its card, above the closing reply |
| Global listing | PASS | `20-listed.png` shows the one matching row in the All artifacts pane |
| Preview/download | PASS | `21-preview.png` shows the exact content; the download's suggested filename and bytes matched exactly (`receipt-artifact-9b4e1d99.txt` retained) |
| Reload identity | PASS | `30-reloaded-*` keeps the same message, card and artifact ids and the same content |
| Natural cat | NOT PROVEN | the artifact WAS created and rendered — SQLite row 2 `Cozy Cat Picture`, kind `image`, mime `image/svg+xml`, referenced by Agent message 5, drawn as a real SVG in `40-natural-cat-preview.png` — but the send carried `artifact_ids: [1]`, because the earlier preview staged the receipt as composer context. The regression asks for the ordinary request ALONE; an attached example softens it. The runner now clears that context first |

**Aggregate.** An ordinary chat request produced a real, exact, reusable
artifact rather than a prose claim, and the cat request produced a genuine
rendered image. The one gate still owed is the cat request with nothing
attached to it.
