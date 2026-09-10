# C04-BLOBS — attachment/blob representation audit

## Ranked findings

1. **Recommend — declared MIME is an open string that directly selects inline delivery.** A reachable upload can store arbitrary bytes as `image/svg+xml` (or another `image/*` value) and the signed blob route will serve them as an inline document. This is a host delivery-boundary defect with a small, local fix.
2. **Recommend — the blob file path is duplicated in SQLite and durable turn JSON even though it is derivable from the blob ID.** The normal writer keeps the copies equal, but relocation/restoration or any metadata drift leaves a valid-looking row and durable request pointing at a stale or arbitrary host path. This is a broader storage/runtime cutover.

## Finding 1 — declared MIME selects unsafe inline delivery

### Verdict and condition

Recommend fixing. The upload path accepts a caller-declared MIME string and opaque bytes, while the route treats every string beginning with `image/` as safe to display inline. A concrete invalid combination is:

```text
name = "payload.svg"
mime = "image/svg+xml"
data = bytes containing an SVG document with active markup/script
```

This combination is reachable through the current web upload flow: the browser sends the declared `File.type` and base64 bytes over the WebSocket; the host validates size/base64 but does not validate the bytes against the MIME; a user can then open the returned blob URL. `window.open` navigates a new top-level document, not an `<img>` element. The exact browser/deployment security impact depends on browser policy and deployment headers, but the unsafe inline state is certain from the source. No live values or tests were run.

### Evidence across the representation layers

- The wire upload has free-form MIME plus raw base64: `app/src/protocol.ts:240-248`:

  > `export interface UploadBlobMsg {`
  > `  type: "upload_blob";`
  > `  client_id: string;`
  > `  name: string;`
  > `  mime: string;`
  > `  data_b64: string;`

- The web client sends the staged name and MIME unchanged with the encoded file: `app/src/components/chat/useAttachments.ts:174-181`:

  > `const b64 = await fileToBase64(pf.file);`
  > `const blob = await client.uploadBlob(pf.clientId, pf.name, pf.mime, b64);`

- The host normalizer only trims and supplies a fallback; it does not close or validate the MIME set: `crates/hirsel-host/src/attachments.rs:32-38`:

  > `pub fn normalize_mime(mime: &str) -> String {`
  > `    let mime = mime.trim();`
  > `    if mime.is_empty() {`
  > `        "application/octet-stream".to_string()`
  > `    } else {`
  > `        mime.to_string()`

- The WebSocket host stores that value after only the base64/size check: `crates/hirsel-host/src/protocol.rs:544-558`:

  > `let data = decode_blob_data_b64(&data_b64)?;`
  > `...`
  > `sanitize_blob_name(&name),`
  > `normalize_mime(&mime),`
  > `data`

- The declared MIME is persisted as an unconstrained `TEXT`: `crates/hirsel-host/src/storage/current.sql:45-52`:

  > `CREATE TABLE blobs (`
  > `  id TEXT PRIMARY KEY,`
  > `  name TEXT NOT NULL,`
  > `  mime TEXT NOT NULL,`
  > `  size INTEGER NOT NULL,`
  > `  path TEXT NOT NULL,`
  > `  created_ts TEXT NOT NULL`
  > `);`

- The route uses the same value for `Content-Type` and for the inline/download decision: `crates/hirsel-host/src/blob_route.rs:113-124,136-148`:

  > `let data = tokio::fs::read(&stored.path).await?;`
  > `response_headers.insert(CONTENT_TYPE, content_type_header(&stored.blob.mime));`
  > `response_headers.insert(`
  > `    CONTENT_DISPOSITION,`
  > `    content_disposition_header(&stored.blob.name, &stored.blob.mime),`
  > `);`
  > `let disposition = if mime.starts_with("image/") {`
  > `    "inline"`
  > `} else {`
  > `    "attachment"`

- The web classifier also explicitly treats SVG as an image and all `image/*` values as image attachments: `app/src/components/chat/paste.ts:38-47,60-65`:

  > `"image/svg+xml": "svg",`
  > `...`
  > `if (mime.startsWith("image/")) return "image";`

- The user-facing action opens the signed URL in a new window: `app/src/threads/ThreadMessages.tsx:50-52`:

  > `onClick={() => { void getClient()?.getBlobUrl(blob.id).then(url => window.open(url, "_blank", "noopener,noreferrer")); }}`

The wire `Blob` itself is only metadata (`crates/hirsel-proto/src/chat.rs:13-19`: `id`, `name`, `mime`, `size`); it does not introduce a second MIME owner. The defect is the interpretation of one open field, not a copy-sync failure.

### Duplicate truth and reachability

There is **no duplicate-truth write path** for this finding: the MIME is one persisted field propagated into the wire projection and response headers. The problem is that a free-form declared value is also being used as a delivery-policy state.

The condition is reachable from an authenticated upload and attachment click; it does not require direct database mutation. Existing source tests demonstrate only the normal `image/png` inline and `text/plain` attachment cases. They do not demonstrate or reject SVG bytes, an HTML payload mislabeled as an image, or a MIME/payload mismatch. No tests were run.

### Consumer query and result

Reproducible bounded propagation query:

```sh
rg -n 'mime\.starts_with\("image/"\)|image/svg\+xml|normalize_mime|uploadBlob|window\.open\(url' \
  crates/hirsel-host/src/attachments.rs \
  crates/hirsel-host/src/blob_route.rs \
  crates/hirsel-host/src/protocol.rs \
  app/src/components/chat/paste.ts \
  app/src/components/chat/useAttachments.ts \
  app/src/threads/ThreadMessages.tsx \
  app/src/ws/client.ts \
  app/src/protocol.ts | wc -l
```

Result: **8** matching propagation/interpretation sites across the host and web client. This is a source query only; it was not an application execution or live-data test.

### Target representation and smallest credible scope

Keep `Blob.mime: String` as declared file metadata for compatibility with arbitrary downloads, but derive a host-only closed delivery state at the route boundary:

```text
BlobDisposition = InlineRaster | Attachment
```

`InlineRaster` must come from an exact allowlist of supported non-active raster formats (the current web list can supply the initial list: PNG, JPEG, GIF, WebP, AVIF, BMP, TIFF). `image/svg+xml` and every unrecognized `image/*` value must be `Attachment`. Do not persist an `inline` boolean: disposition is derived policy, not another fact. For `Attachment`, return `Content-Disposition: attachment` and add `X-Content-Type-Options: nosniff`; retain or safely fallback the declared `Content-Type` according to the chosen download policy.

Smallest credible implementation boundary: `crates/hirsel-host/src/blob_route.rs` for the closed policy and headers, `crates/hirsel-host/src/attachments.rs` if normalization is tightened, and the existing route/WebSocket tests for regression coverage. `app/src/components/chat/paste.ts` may stop advertising SVG as an inline image, but server-side delivery must enforce the invariant even for non-web clients. No DB or wire schema change is required for this minimum fix.

This removes the representable “active-looking bytes + inline image policy” state and prevents a prefix test from acting as an extensible delivery state machine. The cutover risk is user-visible: SVG and future image formats may download rather than display, so the allowlist must be an explicit product decision. Required validation, not run here: test SVG/script bytes and mislabeled HTML with `image/*` are downloads; test each allowed raster remains inline; test `nosniff`; retain signed-URL authorization and existing text behavior.

**Confidence: High** for the behavioral finding; the severity of any cross-origin script effect remains deployment/browser-policy dependent.

## Finding 2 — persisted and durable absolute paths duplicate the blob ID

### Verdict and condition

Recommend fixing as a clean storage/runtime representation cutover. `Storage` owns one blob directory and creates each file at `<blobs_dir>/<uuid>`, so the SQL `path` column is derivable from `id`. Nevertheless, the absolute path is stored in SQLite, returned in `StoredBlob`, serialized into durable `thread_requests.payload`, and later trusted by the route and runtime.

A concrete invalid state is a row such as `{id: "A", path: "/new-data/blobs/B"}` or a valid row copied with its database from `/old-data` to `/new-data` while retaining `/old-data/blobs/A`. The schema accepts the state, `stored_blob_from_row` trusts it, and readers can fail to find the file or read a path outside the current blob root. The relocation case is reachable by opening/restoring the same SQLite database under a different data directory; an in-process normal upload does not create the mismatch. No live mismatch was inspected or executed.

### Evidence across the representation layers

- The schema stores `path` beside the ID and metadata: `crates/hirsel-host/src/storage/current.sql:45-52`:

  > `id TEXT PRIMARY KEY,`
  > `...`
  > `size INTEGER NOT NULL,`
  > `path TEXT NOT NULL,`
  > `created_ts TEXT NOT NULL`

- The writer derives the path from the generated ID, then constructs a `StoredBlob` containing both values: `crates/hirsel-host/src/storage/blobs.rs:25-47`:

  > `let id = Uuid::new_v4().to_string();`
  > `let path = self.blobs_dir.join(&id);`
  > `...`
  > `blob: Blob {`
  > `    id: id.clone(),`
  > `    ...`
  > `},`
  > `path: path.clone(),`

- It persists both copies: `crates/hirsel-host/src/storage/blobs.rs:53-65`:

  > `INSERT INTO blobs (id, name, mime, size, path, created_ts)`
  > `VALUES (?1, ?2, ?3, ?4, ?5, ?6)`
  > `...`
  > `record.path.to_string_lossy(),`

- Every metadata read selects `path`, and the row conversion accepts it directly as a `PathBuf`: `crates/hirsel-host/src/storage/blobs.rs:135-145,164-205`:

  > `SELECT b.id, b.name, b.mime, b.size, b.path, b.created_ts`
  > `...`
  > `pub struct StoredBlob {`
  > `    pub blob: Blob,`
  > `    pub path: PathBuf,`
  > `    pub created_ts: DateTime<Utc>,`
  > `}`
  > `path: PathBuf::from(row.get::<_, String>(4)?),`

- The current `Storage` instance has a canonical root derived from its data directory: `crates/hirsel-host/src/storage.rs:70-85`:

  > `let data_dir = absolute_path(data_dir)?;`
  > `let blobs_dir = data_dir.join("blobs");`
  > `...`
  > `blobs_dir: Arc::new(blobs_dir),`

- Accepted durable requests serialize the entire queried attachment record, including `StoredBlob.path`: `crates/hirsel-host/src/storage/thread_messages.rs:196-219`:

  > `request["attachments"] =`
  > `    serde_json::to_value(super::blobs::message_attachments(&tx, id)?)?;`
  > `...`
  > `INSERT INTO thread_requests(client_id,payload) VALUES(?1,?2)`

- Recovery deserializes that path-bearing type: `crates/hirsel-host/src/lash_runtime/runtime.rs:13-27`:

  > `pub struct OwnerTurn {`
  > `    ...`
  > `    pub attachments: Vec<StoredBlob>,`
  > `    pub mode: SendMode,`
  > `}`

- Runtime input and HTTP delivery trust the path rather than resolving from the ID: `crates/hirsel-host/src/lash_runtime/turn.rs:19-30,46-54` and `crates/hirsel-host/src/blob_route.rs:113-123`:

  > `let bytes = tokio::fs::read(&attachment.path)`
  > `...`
  > `attachment.path.display(),`
  > `...`
  > `let data = tokio::fs::read(&stored.path).await?;`

The current writer has no path-only update API. A bounded source search found only `INSERT INTO blobs` at `blobs.rs:55` and deletes at `blobs.rs:76` and `storage.rs:117`; no `UPDATE blobs` exists. Thus this is a latent/operational mismatch, not an observed normal-write divergence. It is nevertheless duplicate ownership: the ID plus the current `Storage.blobs_dir` determines the location, while the row and durable JSON independently carry another location fact.

### Consumer query and result

Reproducible production-consumer query, excluding tests:

```sh
rg -n --glob '*.rs' --glob '!**/tests.rs' \
  'StoredBlob|stored\.path|attachment\.path|message_attachments\(&tx, id\)' \
  crates/hirsel-host/src/blob_route.rs \
  crates/hirsel-host/src/lash_runtime \
  crates/hirsel-host/src/storage/thread_messages.rs | wc -l
```

Result: **6** matching source references in the bounded route, runtime, and durable-request consumers. The storage queries and `StoredBlob` definition above were inspected separately as the owned representation sources.

### Target representation and smallest credible scope

Make location ownership singular:

```text
SQLite blobs row:       id, name, mime, size, created_ts       (no path)
durable OwnerTurn:      attachment blob IDs or immutable Blob metadata (no PathBuf)
host-only resolved file: { Blob metadata, path = storage.blobs_dir.join(blob.id) }
```

The preferred durable shape is `OwnerTurn.attachments: Vec<Blob>` (or a dedicated immutable `BlobRef` with exactly `id`, `name`, `mime`, `size`), preserving the current acceptance snapshot without persisting a machine path. At execution/read time, `Storage` resolves each ID against its current `blobs_dir` and returns a non-serialized `ResolvedBlobFile`/equivalent to the route and runtime. If IDs alone are chosen, the resolver must enforce that the referenced blob still exists before execution. `Blob` remains the existing wire metadata type; no path should cross the wire.

Smallest credible implementation boundary: `crates/hirsel-host/src/storage/current.sql`, `crates/hirsel-host/src/storage/blobs.rs`, and a host resolver at the `Storage` boundary; then update `crates/hirsel-host/src/blob_route.rs`, `crates/hirsel-host/src/storage/thread_messages.rs`, and `crates/hirsel-host/src/lash_runtime/{runtime.rs,turn.rs}` plus their fixtures. The route/runtime files are consumers/coordination points, not additional owned definitions. Orphan detection should compare directory entries to IDs and `self.blobs_dir.join(id)`, not to a path copied from SQLite. A clean cutover is required because the repository has no shipped schema-migration path for this audit slice; existing stores are outside this report’s scope.

This removes the invalid ID/path pair, makes data-directory relocation restore the file location from current configuration, prevents a DB path from escaping the blob root, and avoids exposing a stale host path through durable JSON. It preserves the current agent-visible path behavior by rendering the freshly resolved path at execution time.

Regression/cutover risks: durable request JSON changes shape and must be read atomically with the accepted message; recovery, reset/history fencing, duplicate-upload cleanup, and orphan diagnostics must continue to work. Existing stores containing `path` cannot be silently assumed compatible with the new schema. Required validation, not run here: store/reopen under the same directory; copy a DB and blob files under a new data directory and verify route/runtime resolution uses the new root; verify a queued request round-trips without `path`; verify missing IDs and orphan files fail/log through the intended paths.

Existing tests demonstrate only the valid relation: `crates/hirsel-host/src/storage/blobs/tests.rs:4-41` writes `<blobs_dir>/<id>` and asserts the path/file relationship, while `crates/hirsel-host/src/lash_runtime/tests.rs:379-426,905-916` constructs fixture `StoredBlob` values with explicit paths and reads them. No fixture tests a moved data directory, a mismatched DB row, or durable recovery after relocation. No tests were run.

**Confidence: High** for the duplicate representation and its consumers; the relocation scenario is operational rather than a normal upload-path failure.

## Coverage contract and explicit skips

The complete owned-file scope was inspected: `crates/hirsel-host/src/attachments.rs`, `crates/hirsel-host/src/blob_route.rs`, `crates/hirsel-host/src/storage/blobs.rs`, `crates/hirsel-host/src/storage/blobs/tests.rs`, and the imports/helpers/enclosing plumbing in `crates/hirsel-proto/src/chat.rs`. The shared exact definitions inspected were `crates/hirsel-host/src/storage/current.sql:45` (`blobs`), `:53` (`client_blobs`), `:57` (`message_attachments`), and `crates/hirsel-proto/src/chat.rs:14` (`Blob`); the explicitly excluded `ChatAuthor`, `ChatMessage`, and `ToolCallSummary` definitions were not treated as owned. Relevant consumers were read for propagation only: host chat/thread storage and runtime, the WebSocket protocol, app attachment staging/paste/drop/rendering, client-core, and FFI.

| Area | Result | Reason |
|---|---|---|
| Base64/size/name normalization | Skip except MIME policy above | Size is bounded at both current upload entry points; names reduce to a basename and control characters; no distinct representation defect was found. |
| `client_blobs` | Skip | `client_id` is an intentional idempotency receipt. The existing integration shape returns the first blob for a repeated client ID, even when the second request differs; this is an existing behavior, not a new finding. |
| `message_attachments.position` | Skip | The write enumerates positions and the read orders by position. `ChatMessage.attachments` is a projection of the join, not a second persisted attachment list. No product invariant currently forbids repeated blob IDs. |
| `Blob.id`, `name`, and `size` | Skip | UUID generation, name sanitization, unsigned size conversion, UI display, and agent metadata use are coherent in the inspected paths. A new ID newtype or size source would be speculative. |
| Signed URL fields and signer | Skip | The HMAC binds blob ID and expiry; route authorization verifies the signed tuple. `BlobUrl` transport fields are correlated and host-created atomically; no material divergence was found. |
| Web upload lifecycle, `PendingFile`, drag/drop, and generic UI attachment state | Skip | The upload lifecycle is a closed union and current tests cover its normal transitions. `PendingFile` has local convenience copies and optional text/line fields, but its private construction currently supplies coherent values; treating future `Partial` misuse as a finding would be hypothetical. The generic UI primitive is not the current blob source of truth. |
| Client-core/native FFI | Skip / adjacent ownership | These project the wire `Blob` metadata and send attachment IDs; no independent blob storage owner or duplicate fact was found in the bounded read. |
| Orphan logging/reset cleanup | Skip | Crash-orphan cleanup and operational lifecycle are adjacent host-ops concerns; the representation audit found no additional schema/type defect beyond the path ownership problem above. |

The exclusions file was read before reporting. Its existing tracked outcomes were not repeated, including the planned artifact viewer work; no distinct regression in this cluster was found. No third cross-cutting finding is promoted: the two findings are the two material representations identified, and no stylistic, hypothetical, or line-saving cleanup is included.

## Audit guard

- Initial repository snapshot: `HEAD=3ee0621a603659ab0168f565b99012b642415419`, tree=`a4aac830c45398a66591f2c44b707aaf3cef281b`, `git status --porcelain` empty.
- The audit used bounded read-only inspection (`rg`, `nl`/`sed`, `git` metadata/history/blame where useful). No source files, tests, builds, fixtures, databases, providers, sessions, commits, or branches were changed.
- Final repository snapshot after writing this report: `HEAD=3ee0621a603659ab0168f565b99012b642415419`, tree=`a4aac830c45398a66591f2c44b707aaf3cef281b`, `git status --porcelain` empty.
