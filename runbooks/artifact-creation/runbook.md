# Product scenario: artifact creation through chat

> Read [../RULES.md](../RULES.md) first.

**Purpose.** Prove that an Owner can request a reusable file in ordinary chat,
the real Agent actually calls `artifacts.create`, and the exact result becomes
listed, previewable, and downloadable.

**Real model calls:** one.

## Named natural-language regression

Keep the ordinary request `Make a picture of a cat artifact` as a distinct
regression. On source `dbbb8d3a99cdaf238feb3c6bd9a997dffe78fb55`, the recorded
real-model turns completed with prose apologies, zero TypeScript cells, zero
tool events, and zero durable calls even though `artifacts_create` was in the
tool catalog. The sanitized evidence is
`/tmp/hirsel-artifact-failure-trace.json` (SHA-256
`16475bb862148db73d628cab958584c8dc978d5e9b2a5fb03e001f75999a8a9d`).

That evidence is reused for this named regression; the automated scenario
below spends its single turn on an exact file receipt so content, preview, and
download have deterministic answer keys. Do not spend an extra model call on
the cat request in the five-turn battery.

## Golden rules

1. Scripted/debug publication proves only component contracts and cannot pass
   this scenario. Require a real `artifacts.create` start/done pair from the
   addressed turn.
2. `completed` is not an answer key. Require the generated artifact's exact
   title, filename, MIME type, and noncoincidental content.
3. The artifact ID must agree across tool result, `artifact_upsert`, Agent
   message reference, authenticated Thread snapshot, global artifact list, and
   SQLite.
4. Preview and download use actual UI controls. The downloaded bytes must equal
   the requested UTF-8 content exactly.

## Phase 0 — start empty

**Do:** Run `just product-runbook artifact-creation`.

**Expect:** the new store has no artifacts and the selected Thread has no
messages or turns. Capture `00-empty.png` and all baseline extracts.

## Phase 1 — request through chat

**Do:** Ask the real Agent to create one `file` artifact using the exact title,
filename, MIME type, and content in the prompt.

**Expect:** the turn emits matched `artifacts.create` start/done events. The
Agent reply references the resulting artifact card; the card's ID matches the
wire and disk artifact. Capture `10-created.png`.

## Phase 2 — list, preview, download

**Do:** Open All artifacts through the UI, select the exact title, inspect the
preview, and use Download.

**Expect:** the global list contains exactly one matching artifact; the preview
shows the exact content; the suggested filename is exact; downloaded bytes are
byte-for-byte equal. Capture `20-listed.png` and `21-preview.png`, and retain
the downloaded file.

## Phase 3 — reload

**Do:** Return to the conversation and reload.

**Expect:** the same artifact card ID is still attached to the Agent message
and opens the same content. Save `30-reloaded.png` and `result.json`.

## Scorecard

| Item | Objective gate | Verdict | Evidence |
|---|---|---|---|
| Real creation | matched real `artifacts.create` start/done call | | frames |
| Exact stored result | title, kind, MIME, filename, and content match on wire and SQLite | | store/open snapshot |
| Conversation reference | Agent message carries the same artifact ID and visible card | | `10-created.png` |
| Global listing | exact artifact appears in All artifacts | | `20-listed.png` |
| Preview/download | exact text renders and downloaded bytes match | | `21-preview.png`, download |
| Reload identity | same message/card/artifact IDs and content survive reload | | `30-*`, `result.json` |

**Aggregate:** did an ordinary Owner chat request produce a real, exact,
reusable artifact rather than a prose claim that a tool failed or completed?
