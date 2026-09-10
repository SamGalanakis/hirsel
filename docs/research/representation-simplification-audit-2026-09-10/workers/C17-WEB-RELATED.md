# C17-WEB-RELATED

## Verdict

No new finding is recommended. The owned cluster has zero verified fixes at this
snapshot. The review found several plausible representation leads, but each was
either producer-constrained, an intentional orthogonal state, a harmless local
projection, or the excluded C07 malformed Related-item/envelope case. No tests,
builds, application code, live values, database rows, providers, or network
services were accessed.

## Snapshot and scope

- Expected HEAD: `3ee0621a603659ab0168f565b99012b642415419`
- Observed HEAD before review: `3ee0621a603659ab0168f565b99012b642415419`
- Expected tree: `a4aac830c45398a66591f2c44b707aaf3cef281b`
- Observed tree before review: `a4aac830c45398a66591f2c44b707aaf3cef281b`
- Status before review: clean (`git status --porcelain` produced no output).
- Observed HEAD after review: `3ee0621a603659ab0168f565b99012b642415419`
- Observed tree after review: `a4aac830c45398a66591f2c44b707aaf3cef281b`
- Status after review: clean (`git status --porcelain` produced no output);
  `git diff --stat` was empty.
- Owned files: the 12 exact files named by the dispatch; no source files were
  edited. `app/src/threads/types.ts`, Markdown/ThreadRef consumers, WebSocket
  lifecycle, and Rust protocol/storage are read-only conversion context.

## Coverage contract and dispositions

| ID | Exact owned boundary | Definitions and conversion/consumer coverage | Disposition |
|---|---|---|---|
| C17-R1 | `app/src/lib/thread-ref.ts` | `RefTarget` (5), `RefQuery` (34-39), `RefSpan` (139-143), query detection/filter/insertion, `scanRefs` (109-119), mention resolution (126-135), and rendering split (148-159). Consumers: `useThreadRefPicker.ts` (75-101), `Composer.tsx` (132-136), `ThreadRef.tsx` (12-13); adjacent grammar tests are `app/src/lib/thread-ref.test.ts`. | Skip: picker queries are intentionally name-capable while committed refs are numeric; composed text remains the only mention source. No contradictory state or duplicate persisted fact found. |
| C17-R2 | `app/src/lib/thread-url.ts` | `ThreadTarget`/`ThreadLinkResult` (2-3), local-origin parser (6-18), path/URL/reference constructors and click policy (20-25). Consumers: `RelatedList.tsx` (67-69), `RichLink.tsx` (21-30), `threads/store.ts` (35-52, 187-195); adjacent tests are `app/src/lib/thread-url.test.ts`. | Skip: the result union distinguishes non-local, incomplete, invalid, and valid local links; current-history checks are applied at route, render, and mutation boundaries. |
| C17-R3 | `app/src/related/url.ts`, `app/src/related/LinkIcon.tsx` | `WebLink` (2-6), `webLink` validation/recognition (8-26), bare-label predicate (28-30), and total icon map (4-6). Consumers: `RichLink.tsx` (26-30, 45-50), `RelatedList.tsx` (23-27, 36-37), `store.ts` (56-65). Cross-layer normalization: host `canonical_url` (Rust `thread_related.rs:16-39`); storage union/uniqueness (`current.sql:158-168`). | Skip: `WebLink` is a pure presentation projection; no second mutable writer for its derived label/kind/site exists. |
| C17-R4 | `app/src/related/store.ts`, `app/src/related/context.tsx` | `RelatedOrigin`, `RelatedList`, `relatedState`, `Pending`, `reads` (store.ts:6-20), request/reset/finish lifecycle (21-52), target identity (53-69), revision-gated receive and message conversion (71-87), and context (context.tsx:3-4). Consumers: `RelatedList.tsx`, `RichLink.tsx`, `ThreadShell.tsx:79`, WebSocket client `ws/client.ts:366-375`, and the four owned Related tests. | Skip: list cache is numerically keyed but its invalidation owner is explicit: history reset clears it (34-38), every mutation/read is history-checked (41, 48, 54, 61), and the WebSocket hello path calls reset on a changed history. Revision and items are replaced together. |
| C17-R5 | `app/src/related/RelatedList.tsx`, `app/src/related/RichLink.tsx` | `SavedReference` (RelatedList.tsx:18-45), `RelatedList` input/add/load/UI state (47-88), `copyLink`, `RichLink` props/target derivation/render/actions (RichLink.tsx:15-60). Consumers: Markdown (`components/Markdown.tsx:28-35, 78-84`) and ThreadRef (`threads/ThreadRef.tsx:12-23`). | Skip: `RichLink`'s optional typed target is only supplied by two producers that construct it from the same target used to build `href`; Markdown producers omit it. A mismatch is an API-level latent combination, not a reachable producer defect at this snapshot. |
| C17-T | `app/src/related/RichLink.test.tsx`, `store.test.ts`, `thread-links.test.tsx`, `url.test.ts` | Existing fixtures cover generic/authored/reference links, unsafe/lookalike URLs, image/code exclusion, click/copy/save actions, history replacement, snapshot revision ordering, retries, typed Thread targets, and URL normalization. | Skip: tests demonstrate valid paths and guarded history/revision behavior; none demonstrates a current invalid or duplicate state. Tests were inspected, not executed. |

All 12 dispatch-owned files were present and read. `rg --files app/src/lib app/src/related`
also confirmed the adjacent `thread-ref.test.ts` and
`thread-url.test.ts` are separate consumer/test context, not omitted owned
definitions. No shared named definition was assigned to this cluster.

## Candidate verification and rejected findings

### 1. `targetKey` null collision — excluded / not a new finding

The frontend identity function returns a nullable key:

> `app/src/related/store.ts:53-58`
> `return origin.historyId === historyId() && (relatedState.lists[origin.threadId]?.items.some(item => targetKey(item.target) === targetKey(target)) ?? false);`
> `const url = webLink(target.url)?.url; return url ? \`url:${url}\` : null;`

Therefore two malformed URL targets would compare as equal through `null`.
That state is not reachable through the owned write path: `addRelatedItem`
rejects a URL unless `webLink` accepts it and sends the normalized URL
(`store.ts:60-66`), while the host canonicalizes HTTP(S), rejects credentials,
backslashes, controls, and overlong values (`thread_related.rs:16-39`) before
the SQL union/uniqueness constraints (`current.sql:158-168`). The protocol
type is a raw `string` (`app/src/threads/types.ts:44-50`), so the collision is
latent only for malformed/untrusted snapshots or hand-built fixtures. That is
the coordinator's explicitly excluded C07 malformed Related-item case, with no
producer or race witness. Existing fixtures contain only accepted URLs
(`store.test.ts:9`, `RichLink.test.tsx:9`, `url.test.ts:5-16`). Not reported.

### 2. History omitted from the list-map key — intentional invalidation, not a finding

`relatedState` is `Record<number, RelatedList>` (`store.ts:7-9`), but the cache
is not allowed to survive a history change: `resetRelated` rejects pending work
and replaces `lists` with `{}` (`store.ts:34-38`); incoming broadcasts reject a
foreign history (`store.ts:76-80`); delayed detail reads carry captured history
and thread identity (`store.ts:15-18, 81-85`). The WebSocket hello owner invokes
the reset when `acceptHistory` reports a new history (`ws/client.ts:366-371`).
The existing old-history test demonstrates the complete guard (`store.test.ts:41-48`),
and the rendered-link test demonstrates no title hydration across replacement
(`thread-links.test.tsx:30-34`). Adding history to the key would duplicate the
already-owned invalidation protocol without deleting a reachable state.

### 3. Related loading flags — valid state machine, not a finding

`RelatedList` contains `loaded`, `loading`, and `error` beside the snapshot
(`store.ts:7-8`). The apparently redundant combinations are deliberate: a
refresh can retain existing items while loading, and a failed refresh can retain
stale items while surfacing an error. Writes are centralized in `finish`,
`loadRelated`, and `receive` (`store.ts:21-27, 47-51, 71-75`). The retry test
demonstrates the intended stale-items-plus-error transition and recovery
(`store.test.ts:50-55`). A tagged load union would be a possible future
refinement, but no current invalid combination or materially simpler ownership
was verified.

### 4. `RichLink` target plus `href` — latent API redundancy below threshold

`RichLink` accepts `href` and optional `target` (`RichLink.tsx:19`), then gives a
thread target precedence and derives the canonical href from it (`21-30`). The
only target-bearing producers construct the target and href together
(`threads/ThreadRef.tsx:13, 21-23`); Markdown link producers do not pass target
(`components/Markdown.tsx:33-35, 82-84`). `rg -F '<RichLink' app/src` finds
four render sites, only two target-bearing. The mismatch is representable in a
public component prop but has no reachable source in this snapshot; removing
the prop would be a small API cleanup, not a material verified fix.

### 5. Parser/projection duplication — no contradictory behavior

`detectRefQuery` intentionally accepts name queries (`thread-ref.ts:10-13,
45-51`), while `scanRefs` intentionally recognizes only numeric committed refs
(`109-119`), and `resolveMentionIds`/`splitThreadRefs` share the latter
(`126-159`). Existing grammar tests cover both halves (`thread-ref.test.ts:37-53,
95-115`). `parseThreadLink` and `webLink` also have distinct domains: local
Thread identity versus generic HTTP(S) presentation. No duplicated mutable
owner or current conversion disagreement was found.

## Independent consumer-query evidence

The final targeted pass re-ran fixed-string searches across `app/src`:

- `targetKey(`: 2 matches (definition plus `hasRelatedTarget` comparison).
- `hasRelatedTarget(`: 7 matches (definition, production callers, and tests).
- `addRelatedItem(`: 7 matches (definition, production callers, and tests).
- `loadRelated(`: 5 matches (definition, two production callers, and tests).
- `handleRelatedMessage(`: 10 matches (definition, WebSocket dispatch, and tests).
- `parseThreadLink(`: 12 matches; `webLink(`: 12 matches.
- `filterThreadCandidates(`: 12 matches; `detectRefQuery(`: 13 matches;
  `resolveMentionIds(`: 6 matches; `splitThreadRefs(`: 5 matches.

These are the full local consumers of the owned interfaces found by the
specified bounded searches. The cross-layer search found the same tagged target
at the Rust protocol, host storage, client-core, FFI, and TypeScript layers;
their conversions are read-only context and showed no owned layer drift.

## Audit log and final ranking

1. Verified expected HEAD/tree and a clean status before inspection.
2. Read `exclusions.md`, every exact owned source/test file, all discovered
   frontend consumers, WebSocket history lifecycle, and Rust target/storage
   conversion/DDL context.
3. Re-opened candidate definitions and reran consumer queries independently.
4. Applied the C07 exclusion and rejected the remaining candidates for lack of
   a reachable invalid state, duplicate writer, or material simplification.
5. No recommendation is ranked because no candidate cleared the threshold.

First fix: none; no new C17 finding survived independent verification.
