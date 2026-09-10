# C06-ARTIFACTS audit

Snapshot checked before report write:

```text
HEAD: 3ee0621a603659ab0168f565b99012b642415419
tree: a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain: empty
```

Scope was read-only. No tests, builds, application code, live data, migrations,
or source edits were run. One finding is accepted; no second finding met the
materiality bar.

## F1 — artifact format, MIME, and filename form contradictory states

**Verdict: medium-confidence, reachable representation defect.** The product
already has three semantic content modes, but the mode and its download
metadata are independently writable at every boundary. A live
`hirsel.artifacts_create` call can publish, for example,
`{kind:"html", mime:"text/markdown", filename:"readme.md", content:"# x"}`.
The server accepts it: `ArtifactDraft` stores independent fields
[`crates/hirsel-host/src/storage/artifacts.rs:8-17`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/crates/hirsel-host/src/storage/artifacts.rs#L8),
and validation only checks each string independently
[`crates/hirsel-host/src/storage/artifacts.rs:28-45`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/crates/hirsel-host/src/storage/artifacts.rs#L28).
The tool schema makes `mime` and `filename` unconstrained optional properties
next to the `kind` enum
[`crates/hirsel-host/src/lash_runtime/tool_defs.rs:68-74`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/crates/hirsel-host/src/lash_runtime/tool_defs.rs#L68).
The human debug path accepts the same `ArtifactDraft`
[`crates/hirsel-host/src/debug/artifacts.rs:7-13`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/crates/hirsel-host/src/debug/artifacts.rs#L7).

The mismatch is observable rather than theoretical:

The relevant producer/storage and protocol definitions are literal independent
fields:

```text
crates/hirsel-host/src/storage/artifacts.rs:9-16
pub(crate) struct ArtifactDraft {
    pub title: String,
    pub kind: ArtifactKind,
    pub mime: String,
    pub filename: Option<String>,
    pub content: String,
    pub expected_content: Option<String>,
}

crates/hirsel-host/src/lash_runtime/artifact_tools.rs:48-60
let mime = optional_string(args, "mime")?.unwrap_or_else(|| {
    match kind {
        ArtifactKind::Solid => "text/jsx",
        ArtifactKind::Html => "text/html",
        ArtifactKind::File => "text/plain",
    }
    .into()
});
filename: optional_string(args, "filename")?,

crates/hirsel-host/src/storage/current.sql:113-116
kind TEXT NOT NULL, mime TEXT NOT NULL, filename TEXT, content TEXT NOT NULL,

crates/hirsel-proto/src/artifact.rs:14-22
pub struct ArtifactSummary {
    pub kind: ArtifactKind,
    pub mime: String,
    pub filename: Option<String>,
    ...
}
```

- The SQL row preserves all three independent values
  [`crates/hirsel-host/src/storage/current.sql:113-116`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/crates/hirsel-host/src/storage/current.sql#L113).
- The protocol flattens them into independent wire fields
  [`crates/hirsel-proto/src/artifact.rs:13-30`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/crates/hirsel-proto/src/artifact.rs#L13).
- Preview dispatches on `kind`: files are escaped/possibly Markdown, HTML is
  inserted as HTML, and Solid is compiled
  [`app/src/artifacts/document.ts:10-16`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/app/src/artifacts/document.ts#L10);
  the preview component independently uses the same discriminator
  [`app/src/artifacts/ArtifactPreview.tsx:25-38`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/app/src/artifacts/ArtifactPreview.tsx#L25).
- Markdown recognition requires `kind=file` and then uses MIME or filename
  [`app/src/artifacts/markdown.ts:9-14`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/app/src/artifacts/markdown.ts#L9).
- Download uses the stored MIME and explicit filename, but derives a default
  extension from `kind`
  [`app/src/artifacts/download.ts:3-8`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/app/src/artifacts/download.ts#L3).

The consuming branches are correspondingly split:

```text
app/src/artifacts/document.ts:12-16
if (artifact.kind === "file") ... isMarkdownArtifact(artifact) ...
if (artifact.kind === "html") ... artifact.content ...
if (!compiled) throw new Error("Solid artifact has not been compiled.");

app/src/artifacts/download.ts:4-7
new Blob([artifact.content], { type: artifact.mime });
link.download = artifact.filename ?? `${artifact.title}.${...artifact.kind...}`;
```

Thus the example previews as raw HTML, is not recognized as Markdown, and
downloads with Markdown metadata/name. The converse
`{kind:"solid", mime:"text/plain", filename:"x.txt"}` compiles as Solid but
downloads as plain text. Both combinations pass current validation and the
tool input schema. Existing fixtures only use compatible pairs (the helper in
`crates/hirsel-host/src/storage/artifacts/tests.rs:2-10` always constructs
HTML/text-html); no existing test demonstrates rejection or normalization of
these combinations.

Reproducible consumer search:

```text
$ rg -n 'artifact\.kind|kind === "solid"|kind === "html"|kind === "file"|artifact\.mime|artifact\.filename|isMarkdownArtifact' app/src/artifacts crates/hirsel-host/src/lash_runtime crates/hirsel-host/src/storage/artifacts.rs crates/hirsel-proto/src/artifact.rs
14 matching lines
```

The broader representation search over the producer, storage, protocol, and
preview files (`rg -n 'kind|mime|filename|content' ...`) returned 69 matching
lines. These searches identify separate consumers of the same untagged
metadata; they do not imply that every match needs changing.

**Duplicate-truth check.** No separate content owner was found. Current
content is stored once in `artifacts.content`; `message_artifacts`,
`activity_artifacts`, `turn_output_artifacts`, and `threads.showcased_artifact_id`
are references/staging associations, not copies of content. The
`artifact_operations` payload is an idempotency receipt, not another content
record. Publication updates the artifact and its card/reference in one
transaction [`crates/hirsel-host/src/storage/artifacts.rs:167-230`](https://github.com/SamGalanakis/hirsel/blob/3ee0621a603659ab0168f565b99012b642415419/crates/hirsel-host/src/storage/artifacts.rs#L167).
F1 is invalid state, not a duplicate-write finding.

**Smallest credible target representation.** Preserve the three product modes,
but make the format a discriminated union and normalize it once at the
server/tool boundary:

```rust
enum ArtifactFormat {
    Solid { filename: Option<SafeFilename> },
    Html  { filename: Option<SafeFilename> },
    File  { mime: Mime, filename: Option<SafeFilename> },
}
struct ArtifactDraft {
    title: String,
    format: ArtifactFormat,
    content: String,
    expected_content: Option<String>,
}
```

`Solid` and `Html` derive their canonical download MIME (`text/jsx` and
`text/html`); only `File` carries arbitrary MIME. A supplied filename is a
safe download name, not a second format discriminator. The protocol/TypeScript
shape should use the same tagged union (with `mime` required only by `file`),
and `artifacts_create` should use `oneOf` branches that reject `mime` for
`solid`/`html`. The current table can encode the same invariant without a
generic blob: retain `kind`, make `mime` nullable, and add checks equivalent to
`kind IN ('solid','html') => mime IS NULL` and `kind='file' => mime IS NOT NULL`;
derive the first two MIME values in conversion. If the wire cutover prefers
flattened fields, the Rust constructor and SQL checks are still required.

Smallest affected interfaces/files are the owned draft/operation/debug paths,
`crates/hirsel-proto/src/artifact.rs`, the `artifacts` DDL, tool definitions,
and `app/src/artifacts/{types.ts,download.ts,markdown.ts,document.ts}`. The
showcase ID/reference code does not need to change. This target makes
`html+text/markdown` and `solid+text/plain` unrepresentable while retaining
arbitrary MIME/filename for genuine files, removing the current cross-consumer
semantic split without adding versions or ownership.

**Risk and validation.** This is a schema/protocol cutover under the settled
schema4-only policy. Update exact-catalog validation, tool JSON fixtures,
Rust/protocol fixtures, and UI types together; inspect any pre-existing rows
before enabling the SQL checks. Add a matrix covering all accepted/rejected
format combinations, round-trip wire decoding, Markdown detection, preview
dispatch, and download MIME/extension behavior. Existing artifact atomicity,
CAS, replay, scope, showcase, and preview-isolation tests remain required.
Inspection only: none of these validations were executed here.

## Coverage and explicit skips

Every owned whole-file was inspected:

| Owned path | Coverage result |
| --- | --- |
| `crates/hirsel-host/src/debug/artifacts.rs` | Thin human publication adapter; shares the draft contract in F1; no independent state. |
| `crates/hirsel-host/src/lash_runtime/artifact_tools.rs` | Create/edit/replay operation path inspected; atomic receipt/card behavior is sound; unconstrained create metadata is F1. |
| `crates/hirsel-host/src/storage/artifacts.rs` | Draft validation, summary/get, scoped reads, CAS, receipt, and transaction inspected; F1 is the only accepted defect. |
| `crates/hirsel-proto/src/artifact.rs` | Flattened summary/content conversion inspected; participates in F1; no second protocol defect. |
| `crates/hirsel-host/src/storage/artifacts/tests.rs` | Atomic create/edit/replay, CAS, invalid input, persistence, and reset fixtures inspected; all metadata pairs are compatible, so add F1 matrix. |
| `crates/hirsel-host/src/storage/thread_showcase.rs` | Showcase is one current artifact ID plus timestamp/revision handling; it does not duplicate artifact content or metadata. |
| `crates/hirsel-host/src/storage/thread_showcase_tests.rs` | Scope/grant/removal, replay, CAS, replacement/clear, broadcasts, and parser fixtures inspected; no content-format ownership. |

The exact shared definitions in `crates/hirsel-host/src/storage/current.sql`
were inspected: `artifacts:113`, `message_artifacts:117`,
`message_artifacts_by_artifact:120`, `artifact_operations:121`,
`activity_artifacts:127`, `activity_artifacts_by_artifact:131`, and
`turn_output_artifacts:154`. Join tables, indexes, and turn-output staging
were not reported as duplicate truth because they represent distinct
association/staging lifecycles. `artifact_operations.message_id` receipt
survival after a manually deleted card was considered, but normal production
deletion only targets queued owner messages and no reachable shipped path
demonstrates the condition; it is not promoted to a finding.

Read-only consumer coverage included the host artifact protocol handlers,
`crates/hirsel-client-core/src/{client.rs,store.rs}`, activity/delegation and
completion joins, and the app artifact preview/download/Markdown stores and
tests. The native client-core viewer gap is the explicitly excluded #10.
Iframe self-navigation is the explicitly excluded #11. Mutable global current
content, explicit reference grants, scoped backlinks, shared ownership, and
the absence of versions are settled ADR0017 behavior and were not reported.

Final post-write verification:

```text
HEAD: 3ee0621a603659ab0168f565b99012b642415419
tree: a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain: empty
```

Source remained unchanged.
