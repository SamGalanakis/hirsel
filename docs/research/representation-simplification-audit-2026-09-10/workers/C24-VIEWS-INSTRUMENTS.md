# C24-VIEWS-INSTRUMENTS audit

## Verdict

Two materially useful, independently verified recommendations are accepted:

| ID | Priority | Recommendation | Reachability | Confidence |
| --- | --- | --- | --- | --- |
| C24-01 | High | Reject duplicate `form` field names at the host template boundary. | Direct inline `hirsel.views_show` specs and file templates reach `templates::validate`; the browser collapses duplicate names into one submitted map key. | High |
| C24-02 | Medium | Remove `TemplateStore.cache`, which is written by resolution but never read by resolution and is replaced before every list read. | Every template resolve currently performs the useless cache write; `views_list_templates` is the only public list consumer. | High |

No other C24 finding met the materiality bar. C18 findings F13 (advertised `chat` placement) and F14 (view ordering) are explicitly owned by the web-view cluster; C14 F03 owns view broadcast-dedupe behavior. Those are not repeated here.

## Snapshot, constraints, and integrity

The requested fixed snapshot was verified before inspection:

```text
HEAD:  3ee0621a603659ab0168f565b99012b642415419
tree:  a4aac830c45398a66591f2c44b707aaf3cef281b
status: empty (`git status --porcelain`)
```

The supplied spec and `/tmp/hirsel-combined-audit/exclusions.md` were read first. This was read-only inspection only: no source edits, tests, builds, installs, application execution, migrations, commits, pushes, live-data/config reads, provider calls, process actions, or session actions. The assigned report outside the source checkout is the only intended write.

## Coverage contract

Every assigned file was read in full. Consumer paths were inspected as reads only; they remain outside C24 ownership.

| ID | Exact owned boundary inspected | Key definitions / behavior | Consumers and tests followed | Status |
| --- | --- | --- | --- | --- |
| C24-A | `crates/hirsel-host/src/json_spec.rs:1-224` | Shared scalar, enum, boolean, range, and allowed-key validators; shared tone/state/field vocabularies. | `templates/spec.rs`, `thread_instrument.rs`, their unit tests. | skip |
| C24-B | `crates/hirsel-host/src/templates.rs:1-275` | Module exports, bundled paths, binding/spec/store/view tests. | `TemplateStore`, `ViewManager`, bundled template fixtures. | skip |
| C24-C | `crates/hirsel-host/src/templates/bind.rs:1-140` | Whole-string and embedded bindings, root/current lookup, `#each` expansion. | `TemplateStore::resolve`, `ViewManager::show/update`, binding tests. | skip |
| C24-D | `crates/hirsel-host/src/templates/spec.rs:1-286` | Closed template component grammar; table/choice/field/form validation. | `TemplateStore::resolve`, inline `ViewManager::show`, `ViewRenderer` form/value-map consumer, catalog docs. | recommend: C24-01 |
| C24-E | `crates/hirsel-host/src/templates/store.rs:1-171` | `TemplateFile`, `TemplateSummary`, `TemplateStore`, reload/list/resolve and cache. | `tools/views.rs`, scoped `views_list_templates`, template reload/list tests. | recommend: C24-02 |
| C24-F | `crates/hirsel-host/src/templates/views.rs:1-469` | `ViewSource`, `PatchOperation`, `ActiveView`, `ViewManager`, history fence, JSON Patch implementation. | `tools/views.rs`, scoped view tools, host snapshots, websocket events, `view_event_tests.rs`, web view consumers. | skip; adjacent C18/C14 findings deduped |
| C24-G | `crates/hirsel-host/src/thread_instrument.rs:1-623` | Closed Thread-instrument grammar, action/field contract derivation, constrained payload validation. | `storage/threads.rs`, `thread_commands.rs`, web and Android renderers, instrument tests. | skip |
| C24-H | `crates/hirsel-host/src/tools/views.rs:1-63` | Thin `ToolSuite` show/update/clear/list delegates and private lookup. | `lash_runtime/scoped_tools.rs`, tool tests. | skip; no independent owner logic |
| C24-I | `crates/hirsel-proto/src/view.rs:1-11` | Wire `ViewInstance` identity, thread, placement, and resolved JSON spec. | `HostToClient`, hello snapshot/dedupe, TypeScript protocol/store/renderers. | skip; placement is C18-owned |
| C24-J | `templates/CATALOG.md:1-64`; `templates/{decision,pr-summary,status-digest,table-report,task-progress}.json` | Catalog envelope/binding/component contract and five shipped templates. | `TemplateStore` seed resolution test and web renderer. | recommend: C24-01 docs; no fixture finding |
| C24-K | `crates/hirsel-host/src/view_event_tests.rs:1-159` | Delayed view callback and history-reset regression fixture. | `AppState::handle_view_event`, `ViewManager::bound_view/clear_all`. | skip; existing history fence is coherent |

### Conversion and ownership map

```text
template file / inline spec
  -> bind_spec + templates::validate
  -> ViewManager::ActiveView / hirsel_proto::ViewInstance
  -> HostToClient::ViewUpsert / hello snapshot
  -> web ViewRenderer and Canvas selector

Thread instrument JSON
  -> storage::threads::validate_instrument
  -> stored Thread.instrument
  -> thread_commands::validate_action
  -> web/Android ThreadInstrument action submission
```

The Rust template validator and Thread-instrument validator share only the primitive kernel in `json_spec.rs`; their component vocabularies are separate product tiers. Thread pin/settlement/attention/visibility/read/execution dimensions remain independent per the supplied exclusions.

## Finding C24-01 — form field names are not unique before conversion to a name-keyed payload

### Verdict and concrete state

**Recommend; high confidence.** A template form can contain two fields with the same non-empty `name`. That is reachable through an inline `views_show` spec or a file template because the host validates each field independently but never validates uniqueness at the form boundary.

Example accepted by the current host validator:

```json
{
  "type": "form",
  "action": "submit",
  "fields": [
    { "type": "field", "name": "answer", "label": "First", "kind": "text", "value": "A" },
    { "type": "field", "name": "answer", "label": "Second", "kind": "text", "value": "B" }
  ]
}
```

This is not merely a hypothetical malformed peer frame. `scoped_tools::views_show` accepts an arbitrary object `spec` and forwards it (`crates/hirsel-host/src/lash_runtime/scoped_tools.rs:324-348`); the inline branch binds and calls the template validator (`crates/hirsel-host/src/templates/views.rs:88-91`).

### Evidence at each affected layer

The field validator establishes only per-field non-empty names and kind/value rules:

`crates/hirsel-host/src/templates/spec.rs:172-203`

```rust
required_string(object, "name", at)?;
required_string(object, "label", at)?;
let kind = required_enum(object, "kind", &VIEW_FIELD_KINDS, at)?;
...
match (kind, object.get("options")) { ... }
```

The form validator validates each array item and its component type, but has no set/map of names:

`crates/hirsel-host/src/templates/spec.rs:222-237`

```rust
let fields = object
    .get("fields")
    .and_then(Value::as_array)
    .ok_or_else(|| anyhow::anyhow!("{at}.fields must be an array"))?;
for (index, field) in fields.iter().enumerate() {
    let field_at = format!("{at}.fields[{index}]");
    validate_node(field, &field_at)?;
    if field.get("type").and_then(Value::as_str) != Some("field") {
        anyhow::bail!("{field_at} must be a field component");
    }
}
```

The catalog defines the submission as a map keyed by field name but does not
state the uniqueness invariant:

`templates/CATALOG.md:57-58`

```text
field: required name ...
form: ... Submission emits ... data containing values keyed by field name.
```

The web conversion has one `Record` map and writes each declaration into it:

`app/src/views/ViewRenderer.tsx:586-606`

```ts
const seed: Record<string, unknown> = {};
for (const f of fields) {
  const name = str(rec.name);
  if (!name) continue;
  seed[name] = fieldSeed(rec);
}
...
setValues((prev) => ({ ...prev, [name]: value }));
```

Both rendered controls then read the same entry and submission emits only one
property (`app/src/views/ViewRenderer.tsx:608-618`, `:600-606`). With the
example above, the second seed overwrites `"A"` with `"B"`; editing either
control writes the same `answer` key, so the two declared fields cannot carry
independent values.

### Reachability, existing evidence, and duplicate truth

- Reachable write path: inline `spec` -> `ViewManager::show` -> `validate` -> `ViewInstance` -> web `FormNode`; file templates use the same validator through `TemplateStore::resolve`.
- No live values, database rows, or application runs were inspected. Existing shipped templates contain no `form` component. Existing renderer fixtures use distinct field names (`app/src/views/ViewRenderer.test.tsx:240-248`, `:274-276`); they do not demonstrate the bad state.
- The analogous Thread-instrument contract already rejects duplicate names while deriving its action contract (`crates/hirsel-host/src/thread_instrument.rs:522-535`, with the enforcement in `:166-180`). That is evidence of the intended invariant, not a co-owned fix.
- There is no durable second writer: the defect is the lossy array-to-map conversion. The concrete one-copy-without-the-other write is `seed[name] = ...` and later `{ ...prev, [name]: value }`; duplicate declarations overwrite/collide before the `view_event` payload is produced.

Reproducible consumer query:

```sh
rg -n -S 'seed\[name\]|values\(\)\[name\]|setValues.*\[name\]|keyed by.*field.*name' \
  app/src/views/ViewRenderer.tsx templates/CATALOG.md
# 5 matches
```

### Target representation and smallest scope

Keep the external ordered JSON shape because field order is presentation order,
but make its invariant explicit and enforce it before publication:

```text
Form {
    action: ActionName,
    fields: Vec<Field>,       // ordered, each Field.name is unique and non-empty
}
FormValues: Map<FieldName, FieldValue>  // exactly one entry per field name
```

The smallest credible cutover is:

1. In `templates/spec.rs::validate_form`, collect validated field names in a
   `BTreeSet` (or equivalent) and reject the second occurrence with a pathful
   error. Keep the existing ordered JSON array and client map.
2. Document `name` uniqueness in `templates/CATALOG.md`.
3. Add a host validator test for duplicate names and a focused renderer/contract
   regression proving that a valid form still emits all distinct keys. No wire
   rename, storage migration, or new runtime abstraction is required.

This deletes a representable state that the downstream map cannot preserve,
rather than adding a second normalization layer.

### Risk and validation

The cutover changes duplicate-name inputs from an apparently successful view to
a clear validation error. There are no current bundled form fixtures with
duplicates, but callers that intentionally repeated a name would need an
explicit array/multi-value contract; the current client cannot support that
meaning. Validate, without running here:

- `templates::validate` rejects two fields with the same name and accepts two
  distinct names;
- inline and file-backed `views_show` reject the duplicate before a
  `ViewUpsert` is published;
- renderer submission still preserves text/textarea/number/toggle/select values
  for distinct names;
- existing `cargo test -p hirsel-host` and focused web renderer tests pass.

Confidence: **high**.

## Finding C24-02 — TemplateStore maintains an ineffective duplicate cache

### Verdict and concrete state

**Recommend; high confidence, medium materiality.** The filesystem is the
template source. `TemplateStore::resolve` reads the selected file directly,
validates/binds it, and then writes the resulting `TemplateFile` into
`cache`. No resolve path reads `cache`. `list` first performs a full `refresh`,
which replaces the entire cache, and only then reads it to produce summaries.
The cache is therefore neither a resolution cache nor an independently useful
snapshot; every successful resolve adds an async lock/write and an otherwise
unobservable duplicate copy.

### Evidence at each affected layer

The representation contains both the directory authority and a cached copy:

`crates/hirsel-host/src/templates/store.rs:13-32`

```rust
struct TemplateFile { id: String, title: String, params_schema: BTreeMap<String, String>, spec: Value }

pub struct TemplateStore {
    dir: Arc<PathBuf>,
    cache: Arc<RwLock<BTreeMap<String, TemplateFile>>>,
}
```

Listing always refreshes before reading the cache:

`crates/hirsel-host/src/templates/store.rs:48-60`

```rust
pub async fn list(&self) -> anyhow::Result<Vec<TemplateSummary>> {
    self.refresh().await?;
    Ok(self.cache.read().await.values().map(...).collect())
}
```

Resolution reads the filesystem path and writes the cache, but never reads it:

`crates/hirsel-host/src/templates/store.rs:62-80`

```rust
let path = self.dir.join(format!("{template_id}.json"));
let template = read_template(&path).await?;
...
let resolved = bind_spec(&template.spec, &params)?;
validate(&resolved)?;
self.cache.write().await.insert(template.id.clone(), template);
Ok(resolved)
```

The refresh path reconstructs a new `BTreeMap` from directory entries and
replaces the cache wholesale (`crates/hirsel-host/src/templates/store.rs:83-113`).
The only public consumer is the list delegate
(`crates/hirsel-host/src/tools/views.rs:52-55` ->
`crates/hirsel-host/src/lash_runtime/scoped_tools.rs:408-415`); view resolution
uses `TemplateStore::resolve` through `ViewManager` and receives the returned
resolved `Value`, not the cache.

Reproducible owned-cache query:

```sh
rg -n -S 'cache|refresh|read_template' crates/hirsel-host/src/templates/store.rs
# 11 matches
```

The matches show three cache definitions/writes/reads, direct filesystem reads
at resolve and refresh, and no cache read in `resolve`. The public call-site
search is:

```sh
rg -n -S 'views_list_templates|templates\(\)\.list|TemplateStore::load|\.resolve\(' \
  crates/hirsel-host/src/tools/views.rs \
  crates/hirsel-host/src/lash_runtime/scoped_tools.rs \
  crates/hirsel-host/src/templates.rs \
  crates/hirsel-host/src/plugins/tests.rs \
  crates/hirsel-host/src/lash_runtime/tests.rs
# 20 matches
```

### Reachability, existing evidence, and duplicate truth

- Every successful template resolution reaches the cache write. A file can
  change or disappear after that write, so the cache and filesystem can differ,
  but no production read relies on the cached copy.
- `templates.rs:77-115` explicitly tests that a file edit is observed by a
  later `resolve`; `templates.rs:118-192` tests listing and resolving the five
  bundled templates. No fixture or test demonstrates a cache hit, because no
  resolve path has one.
- There is no independent writer that makes the cache authoritative. The
  duplicate truth is latent and useless: the directory is read for resolution,
  while the cache is only rebuilt for list and discarded on the next refresh.

### Target representation and smallest scope

Use one authoritative owner and a transient parse result:

```text
TemplateStore {
    dir: Arc<PathBuf>,
}

read_templates(dir) -> BTreeMap<TemplateId, TemplateFile>  // transient validation/list result
resolve(id, params) -> resolved ViewSpec                    // direct file read, no cache write
list() -> Vec<TemplateSummary>                              // map/vector derived from one refresh
```

The smallest cutover is confined to `crates/hirsel-host/src/templates/store.rs`:

- retain the existing directory scan and filename/id checks in a helper that
  returns the transient map (or summaries directly);
- have `load` invoke the helper once to preserve current startup validation;
- have `list` derive summaries from one helper result;
- delete `cache`, its `RwLock`, the resolve-time insert, and the cache read.

`TemplateStore`'s public methods and `TemplateSummary` remain unchanged, so
`tools/views.rs`, scoped tools, `ViewManager`, the wire shape, and clients need
no API migration. Dynamic reload remains direct filesystem reread behavior.

This removes a duplicate in-memory owner, an async lock, and a write on every
resolution without moving template parsing into a new abstraction or changing
reload semantics.

### Risk and validation

The main regression risk is accidentally weakening the current `load`/directory
validation while removing the cache. Preserve the existing `refresh` checks in
the transient helper. There is no persisted data, wire, or migration concern.
Validate, without running here:

- startup still rejects malformed JSON, invalid IDs, filename/ID mismatch, and
  duplicate IDs;
- `list` returns the same sorted summaries and sees directory edits;
- repeated `resolve` sees edits without restart and no cache write remains;
- existing template lifecycle, scoped tool, and host workspace tests pass.

Confidence: **high**.

## Explicit skips and deduplication

- `json_spec.rs` is a settled shared primitive kernel. Its use by both
  validators is deliberate and already centralizes the shared scalar/enum
  rules; no new type would remove a current invalid state.
- `bind.rs` has one binding traversal with explicit root/current semantics and
  existing interpolation/`#each` coverage. No duplicate source of truth was
  found.
- Other `templates/spec.rs` shapes are closed at their appropriate layer:
  table column keys are unique and rows are checked against the declared key
  set (`:118-152`); select options are conditional on `kind` (`:198-203`);
  component properties are rejected by `allowed`. `optionSet.selected` and
  display scalars were not tightened because no current product invariant says
  a selection must be present or typed beyond the declared scalar contract.
- `thread_instrument.rs` derives action/field constraints from the current
  validated JSON and rejects duplicate action/field names, unknown payload
  keys, undeclared choices, and oversized data. `settles` is an intentional
  independent action dimension. The overlapping component names with template
  views are two intentionally different render/control tiers, not duplicate
  ownership; `json_spec.rs` is the shared primitive boundary.
- `ViewManager`'s `ActiveView` keeps raw source/params/patches plus the resolved
  `ViewInstance.spec` as a deliberate materialized client projection. No
  mutation path was found that updates one persisted copy without recomputing
  the other. Per-view `history_id` is needed to return the old identity from
  `bound_view` before a reset; `view_event_tests.rs:64-97` exercises that
  delayed-callback fence, so it must not be collapsed into the current manager
  history.
- `PatchOperation`'s optional `from`/`value` fields are the RFC 6902 input
  boundary; operation-specific late checks are local and the surrounding tool
  schema already closes the operation vocabulary. No material state bug was
  accepted within this cluster.
- C18 F13 (chat placement), C18 F14 (map/order versus browser insertion
  order), C14 F03 (view broadcast dedupe), existing #2-14 outcomes, #16, #18,
  and planned #10 were read as exclusions/deduplication context and are not
  repeated.
- No cross-cutting pattern spanning three independent clusters was promoted.

## Priority and dependencies

1. **C24-01 first:** it blocks a valid-looking view before the client silently
   loses one field, has a reachable producer path, and requires only a host
   validation invariant plus a focused regression.
2. **C24-02 second:** it is independent and low-risk; remove the ineffective
   cache while preserving the existing directory validation and reload tests.

The findings do not depend on one another. C24-01's catalog/test update and
C24-02's store-local cutover can be reviewed separately.

## Audit log and independent audit-of-audit passes

1. Read the assigned spec, exclusions, repository contribution instructions,
   and both invoked audit skill instructions.
2. Verified expected HEAD/tree and clean source status before inspection.
3. Enumerated the exact owned files with `rg --files`, read every owned file in
   full with numbered lines, and traced each public interface to its major
   producer/consumer/test path.
4. Reopened both accepted findings, reproduced their consumer queries and
   counts, and checked current fixtures for the concrete bad states.
5. Coverage pass: all 16 assigned files are present in the table; no generated,
   frontend, Android, protocol, or test boundary was omitted from the read-only
   consumer map.
6. Duplication/ownership pass: removed placement/order and protocol-dedupe
   candidates as C18/C14-owned; retained only the form validator invariant and
   the ineffective host cache.
7. Materiality/over-abstraction pass: rejected generic typed component
   rewrites, enum/string cleanup, and cache redesigns; both accepted targets
   delete an actual invalid/lossy state or unused duplicate owner.
8. Schema-completeness pass: each finding has verdict, exact evidence at each
   affected layer, reachability, existing fixture status, consumer query/count,
   target representation, scope, risk, and validation requirements.
9. Dependency/priority pass: the form invariant precedes the independent local
   cache removal; neither requires a migration or a cross-cluster wire change.

Final source integrity is rechecked after this report is written:

```text
HEAD:  3ee0621a603659ab0168f565b99012b642415419
tree:  a4aac830c45398a66591f2c44b707aaf3cef281b
status: empty (`git status --porcelain`)
```
