# C16-WEB-ARTIFACTS

## Findings

### F1. The iframe sandbox policy has two independent production representations

**Verdict:** Recommend. **Priority:** first. **Confidence:** high. **State:** latent code-level defect; the current values happen to match.

The preview security policy is declared in one place but the live iframe does not consume it:

```text
app/src/artifacts/document.ts:5
export const ARTIFACT_SANDBOX = "allow-scripts";

app/src/artifacts/document.ts:6
// No same-origin permission, credentials, tool handles, or host capabilities.

app/src/artifacts/ArtifactPreview.tsx:48
<iframe ref={node => { frame = node; }} title={props.artifact.title} srcdoc={document()} sandbox="allow-scripts" referrerpolicy="no-referrer" class="h-full min-h-64 w-full border-0 bg-white" />

app/src/artifacts/document.test.ts:8
expect(ARTIFACT_SANDBOX).toBe("allow-scripts");

app/src/artifacts/ArtifactPreview.test.tsx:42
expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
```

`ARTIFACT_SANDBOX` is not consumed by production code. The live DOM attribute is a separate literal. A normal security-policy edit can therefore update `document.ts` and its test while leaving the actual iframe permission unchanged, or update the JSX while leaving the exported/tested policy stale. The mismatch is not currently reachable through artifact data; it is reachable through ordinary maintenance and is security-relevant because the sandbox controls the opaque execution surface.

**Reproducible reference audit:**

```text
$ rg -n 'ARTIFACT_SANDBOX|sandbox="allow-scripts"' app/src/artifacts
app/src/artifacts/document.ts:5:export const ARTIFACT_SANDBOX = "allow-scripts";
app/src/artifacts/ArtifactPreview.tsx:48:... sandbox="allow-scripts" ...
app/src/artifacts/document.test.ts:2:import { ARTIFACT_CSP, ARTIFACT_SANDBOX, artifactDocument } from "./document";
app/src/artifacts/document.test.ts:8:    expect(ARTIFACT_SANDBOX).toBe("allow-scripts");

$ rg -n 'ARTIFACT_SANDBOX|sandbox="allow-scripts"' app/src/artifacts | wc -l
4

$ rg -n 'ARTIFACT_SANDBOX' app/src/artifacts --glob '!*.test.*' | wc -l
1
```

**Duplicate-truth write path:** this is a duplicate code representation, not a server/storage/wire write. `document.ts:5` is one policy value and `ArtifactPreview.tsx:48` is the independently edited consumer; the two test assertions validate their separate copies rather than their relationship.

**Target representation and smallest cutover:** add one small client-only policy module, for example `app/src/artifacts/preview-policy.ts`, containing the sole `ARTIFACT_SANDBOX` value. Import it from both `document.ts` and `ArtifactPreview.tsx`; update the two tests so the document policy retains an explicit approved-value assertion and the rendered iframe assertion compares its attribute with the shared policy. Do not statically import `document.ts` into `ArtifactPreview.tsx`: `ArtifactPreview.tsx:23` deliberately lazy-loads `./document`, and `document.ts:2` imports the virtual runtime. A tiny policy module preserves that lazy boundary. No Rust, protocol, database, or artifact-schema change is required.

This removes the invalid state in which the declared security boundary and the actual browser boundary disagree, while retaining one explicit test for the approved permission and one integration-shaped assertion for the DOM consumer.

**Validation required (not run):** retain the document/CSP tests; render `ArtifactPreview` and assert `sandbox` equals the shared constant; statically verify one production policy literal and both production consumers; smoke Solid, HTML, Markdown, and file previews; confirm the document module remains dynamically loaded. Existing tests independently demonstrate the exported constant and the current DOM value, but no fixture demonstrates that they cannot diverge. No tests or builds were executed per the task constraints.

**Regression/cutover risk:** importing the heavy document module statically would change loading/bundle behavior, which is why the policy module must remain small. Changing the permission itself could alter preview behavior; this finding does not propose changing `allow-scripts`, only making its existing authority unambiguous.

### F2. Markdown Source mode is not scoped to the selected artifact

**Verdict:** Recommend. **Priority:** second. **Confidence:** medium. **State:** reachable desktop UI state defect.

The panel keeps a bare view-mode boolean while artifact selection is independently replaced:

```text
app/src/artifacts/ArtifactSurface.tsx:43
return <Show when={artifactState.selectedId !== null}><ArtifactPanel /></Show>;

app/src/artifacts/ArtifactSurface.tsx:47
const [source, setSource] = createSignal(false);

app/src/artifacts/ArtifactSurface.tsx:75
<Show when={artifactState.opened && isMarkdownArtifact(artifactState.opened)}><button class={button} aria-pressed={source() ? "true" : "false"} onClick={() => setSource(value => !value)}>{source() ? "Preview" : "Source"}</button></Show>

app/src/artifacts/ArtifactSurface.tsx:82
<Show when={artifactState.opened}>{artifact => <div class="min-h-0 flex-1 overflow-auto"><ArtifactPreview artifact={source() && isMarkdownArtifact(artifact()) ? { ...artifact(), mime: "text/plain", filename: "source.txt" } : artifact()} ... /></div>}</Show>

app/src/artifacts/store.ts:56-60
export function openArtifact(id: number) {
  setArtifactState({ selectedId: id, loading: true, error: null, opened: null });
  if (latestOpenRequest) finish(latestOpenRequest);
  latestOpenRequest = crypto.randomUUID();
  send({ type: "open_artifact", client_id: latestOpenRequest, artifact_id: id }, id);
}

app/src/threads/ThreadShell.tsx:96
<Show when={!showRelated()} fallback={<Show when={props.globalArtifacts} fallback={<RelatedList origin={origin} />}><ArtifactList onResume={props.onConversation} /></Show>}>

app/src/threads/ThreadShell.tsx:209-210
<Show ...>{focused => <ThreadConversation ... />}</Show>
<ArtifactSurface />
```

`ArtifactPanel` is not keyed by artifact identity. `openArtifact` changes `selectedId` and clears the loaded artifact, but never resets `source`. On desktop, `ArtifactSurface.tsx:53-60` uses `panel.show()` rather than `showModal()`, so the artifact list remains usable while the panel stays mounted. A user can therefore:

1. Open Markdown artifact A and select **Source** (`source === true`).
2. Select non-Markdown artifact B. The source button is hidden and B renders normally, but the stale `source === true` remains in the mounted panel.
3. Select Markdown artifact C. The source button returns as **Preview**, and line 82 immediately projects C as `text/plain`/`source.txt`, so C opens in Source mode without the user selecting it.

The hidden `source === true` paired with B is the invalid representable combination; its observable consequence is the surprising automatic Source view for C. The condition is reachable from the global artifact list/card flow at `ThreadShell.tsx:96` and the panel/list sibling arrangement at `ThreadShell.tsx:209-210`.

**Reproducible reference audit:**

```text
$ rg -n 'source\(\)|setSource' app/src/artifacts/ArtifactSurface.tsx app/src/artifacts/*.test.tsx
app/src/artifacts/ArtifactSurface.tsx:47:  const [source, setSource] = createSignal(false);
app/src/artifacts/ArtifactSurface.tsx:75:        ... aria-pressed={source() ? "true" : "false"} ... onClick={() => setSource(value => !value)} ...
app/src/artifacts/ArtifactSurface.tsx:82:      ... artifact={source() && isMarkdownArtifact(artifact()) ? ... : artifact()} ...

$ rg -n 'source\(\)|setSource' app/src/artifacts/ArtifactSurface.tsx app/src/artifacts/*.test.tsx | wc -l
3

$ rg -n 'name: "Source"|setSource|aria-pressed' app/src/artifacts/*.test.tsx | wc -l
0
```

**Duplicate-truth write path:** none at the domain, server, or wire layer. This is an unscoped UI state issue: `ArtifactSurface.tsx:47` owns the mode, while `store.ts:57` writes a new selected-artifact identity without an associated mode transition. The selected artifact and the view mode are two legitimate dimensions, but the mode must be identity-bound rather than global to the panel instance.

**Target representation and smallest cutover:** represent the local view as an identity-bearing value, such as `{ artifactId: number; mode: "preview" | "source" }`, and derive Source mode only when `artifactId === artifactState.opened?.id` and the opened artifact is Markdown. On every new `selectedId`, initialize that identity to `mode: "preview"`; on close, clear it. The existing button may then toggle only the current identity-bearing value. This changes `ArtifactSurface.tsx` and adds a transition regression in `ArtifactSurface.test.tsx`; no store protocol or backend change is needed. Keying/remounting the panel is an alternative only if the framework semantics are verified, but identity-bearing state makes the invariant explicit and protects against future remount changes.

This removes both the hidden Source flag for another artifact and the automatic Source projection after a later Markdown selection. It preserves Source as an intentional per-artifact UI dimension.

**Validation required (not run):** add a desktop interaction test for Markdown A → Source → non-Markdown B → Markdown C, asserting B is not Source and C starts in Preview; test direct Markdown A → Markdown B selection; test close/reopen resets mode; retain assertions for `aria-pressed` and the `text/plain`/`source.txt` source projection. Existing artifact-list/open/retry tests demonstrate selection and loading transitions, but no existing test/fixture mentions Source mode. No tests or builds were executed per the task constraints.

**Regression/cutover risk:** the first load of every newly selected artifact will intentionally begin in Preview, and switching identity can recreate the iframe. Focus, loading/error states, mobile dialog behavior, and the existing Markdown-only guard need the added interaction coverage. There is no persistence or wire migration.

## Cross-cutting result

No cross-cutting finding is promoted. F1 is confined to the browser security-policy consumer boundary; F2 is confined to local artifact-panel view state. The underlying artifact model intentionally keeps `kind`, `mime`, `filename`, current content, and explicit thread references as separate contracts. The C06 format/mime/filename candidate was independently rejected as insufficiently material and as requiring an unestablished product invariant in `/tmp/hirsel-combined-audit/verified/C06-F1-disposition.md`; it is not duplicated here.

The persistent Thread showcase and temporary artifact preview remain separate intentionally: the showcase pointer/content flow is covered by `showcase-actions.ts`, `showcase-store.ts`, `ShowcaseSurface.tsx`, and `showcase.test.tsx`, while the temporary preview uses `artifactState` and `ArtifactSurface.tsx`. Existing exclusions for the native artifact viewer (#10), iframe self-navigation (#11), current mutable artifact content/versioning, explicit references, and deliberate Thread dimensions were treated as settled scope, not new findings.

## Owned-file coverage and explicit skips

Every file in the C16 ownership list was read with numbered source, including its tests. “Skip” below means inspected and no materially useful, in-scope representation/ownership finding accepted.

| Owned file | Inspected surface | Result |
|---|---|---|
| `app/artifact-preview.config.ts` | preview build/config exports | Skip: configuration is consistent with the isolated `srcdoc` preview; no independent representation defect. |
| `app/src/artifacts/ArtifactPreview.tsx` | opaque iframe, worker lifecycle, retry/error/dismiss handling | F1: sandbox consumer. |
| `app/src/artifacts/ArtifactSurface.tsx` | card/list/panel, source toggle, desktop/mobile dialog lifecycle | F2: unscoped Source mode. |
| `app/src/artifacts/compiler-environment.ts` | worker environment initialization | Skip: narrow production environment setup; no duplicate domain state. |
| `app/src/artifacts/compiler.ts` | Solid compiler and source-size guard | Skip: controlled compile path and explicit limit. |
| `app/src/artifacts/compiler.worker.ts` | worker request/result/error conversion | Skip: bounded `{code}`/`{error}` conversion; stale ownership is handled by the preview. |
| `app/src/artifacts/document.ts` | CSP, sandbox export, HTML/document construction, script escaping | F1: exported sandbox policy is not the live consumer. Other document paths skip. |
| `app/src/artifacts/draft-context.ts` | history/thread-keyed draft artifact handoff and local persistence | Skip: in-memory state is authoritative and storage is its persistence backing, per the contract. |
| `app/src/artifacts/markdown.ts` | Markdown classification, escaping, inert links, rendering/style | Skip: classification/rendering has explicit escaping and tests; no new representation issue. |
| `app/src/artifacts/runtime-entry.ts` | browser Solid/runtime module entry | Skip: controlled runtime bundle boundary. |
| `app/src/artifacts/runtime-plugin.ts` | Vite virtual runtime plugin and bundle cache | Skip: controlled build-time runtime generation; no material ownership defect. |
| `app/src/artifacts/store.ts` | global summaries/open artifact, request IDs, stale guards, reset/upsert | Skip except F2 consumer evidence: request and stale-response transitions are guarded and tested; it does not own local Source mode. |
| `app/src/artifacts/types.ts` | artifact summary/full artifact and client/server message unions | Skip: wire shapes align with the Rust protocol; no representable invalid combination accepted here. |
| `app/src/artifacts/vendor.d.ts` | virtual module/type declarations | Skip: declarations only. |
| `app/src/artifacts/ArtifactPreview.test.tsx` | iframe isolation and frame-owned dismiss message | F1 evidence: independently asserts current sandbox literal; no coupling fixture. |
| `app/src/artifacts/ArtifactSurface.test.tsx` | artifact navigation/list/retry/backlink flows | F2 evidence: selection flows covered, Source transition absent. |
| `app/src/artifacts/ShowcaseSurface.tsx` | persistent showcase surface, picker, replacement/removal/actions | Skip: persistent showcase is intentionally separate from temporary preview. |
| `app/src/artifacts/artifact-message-context.test.tsx` | preview-to-composer context staging and replacement/removal | Skip: tests intentional draft/context semantics; no artifact representation defect. |
| `app/src/artifacts/document.test.ts` | CSP/sandbox contract, compiled Solid document, script escaping, file rendering/errors | F1 evidence: tests exported policy, not live iframe coupling. |
| `app/src/artifacts/download.ts` | Blob download and filename fallback | Skip: format/mime/filename mismatch was reviewed under C06 and rejected; no independent C16 finding. |
| `app/src/artifacts/markdown.test.ts` | Markdown classification/rendering/security fixtures | Skip: fixtures cover the owned Markdown behavior; no new finding. |
| `app/src/artifacts/showcase-actions.ts` | history/revision/connection-guarded showcase commands | Skip: guards match intentional persistent showcase contract. |
| `app/src/artifacts/showcase-store.ts` | showcase tuple selection, refresh, stale response/disconnect handling | Skip: separate persistent artifact state and stale guards are tested. |
| `app/src/artifacts/showcase.test.tsx` | showcase actions, revision/history guards, replacement/removal, stale reads | Skip: intentional showcase behavior; no duplicate temporary-preview ownership. |
| `app/src/artifacts/store.test.ts` | list/open errors, stale requests, global IDs, newest summaries | Skip: store transitions and stale guards are covered; no Source-mode fixture. |

## Conversion and consumer coverage

The owned definitions were traced through the relevant consumers and wire/storage layers:

- `app/src/protocol.ts` embeds the artifact client/server unions; `app/src/ws/client.ts` attaches the artifact transport. The client transport is best-effort, while request completion/error/stale ownership remains in `store.ts` and `showcase-store.ts`.
- `app/src/threads/ThreadShell.tsx`, `ThreadMessages.tsx`, `ThreadWork.tsx`, and `RelatedList.tsx` consume artifact cards/lists and backlink data. `app/tools/artifact-smoke.tsx` consumes `ArtifactPreview` independently.
- `crates/hirsel-proto/src/artifact.rs`, `crates/hirsel-proto/src/client.rs`, and `crates/hirsel-proto/src/host.rs` were checked against `types.ts`; Solid/HTML/file kinds and summary/full-artifact flattening align.
- `crates/hirsel-host/src/protocol.rs` and `crates/hirsel-host/src/storage/artifacts.rs` were checked for list/open/upsert and artifact validation/publication ownership. `current.sql`, `thread_showcase.rs`, `thread_scope.rs`, and `thread_commands.rs` were checked for links, showcase selection, authorization, and publication. No C16 backend or wire finding survived the exclusions and deduplication check.

## Verification record

The expected snapshot was verified before the report write:

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  [empty]
```

The post-write snapshot matches the pre-write snapshot:

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  [empty]
```

No source files were edited. Tests, builds, installs, migrations, provider calls, live-data/config reads, and session actions were not performed, as required by the C16 specification.
