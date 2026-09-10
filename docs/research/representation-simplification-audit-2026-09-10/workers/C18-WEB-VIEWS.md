# C18-WEB-VIEWS audit

## Verdict

Two materially useful, high-confidence findings are confirmed at the fixed snapshot:

| ID | Priority | Finding | Reachability |
| --- | --- | --- | --- |
| C18-01 | High | The host accepts and broadcasts `chat` placements, but the current client contract and renderer cover only `canvas`; a successful `chat` show becomes invisible. | Directly reachable through the advertised `hirsel.views_show` tool. |
| C18-02 | High | View order is an implicit insertion order in the browser but lexicographic `BTreeMap` order in reconnect snapshots; updates also move only the browser copy. | Directly reachable with two custom non-empty instance IDs or any same-ID update. |

No source files were edited. Tests and builds were not run; no application code was executed; live services and live data were not accessed.

## Snapshot and method

```text
HEAD  3ee0621a603659ab0168f565b99012b642415419
tree  a4aac830c45398a66591f2c44b707aaf3cef281b
git status --porcelain  [empty]
```

The supplied spec and exclusions were read first. This was a read-only pass over the exact C18 ownership boundary, using the `schemasmash` questions (invalid representable states, duplicate truth, and amplification) together with `audit-your-codebase` coverage discipline. Existing F03/#20 view-cache dedupe and root #22 view-only catalog bypass are excluded and are not reported.

## Coverage contract

The complete contents of the assigned files were inspected:

```text
app/src/components/views/CanvasSurface.tsx
app/src/views/ThreadInstrument.tsx
app/src/views/ViewRenderer.tsx
app/src/views/nodes.tsx
app/src/views/tokens.ts
app/src/views/ThreadInstrument.test.tsx
app/src/views/ViewRenderer.test.tsx
```

`CanvasSurface` was followed into `ThreadShell`, store selectors/reducer, and the websocket dispatch path. `ViewRenderer` was followed through its form, field, registry, and event paths. `ThreadInstrument`'s `viewSlot` ignores its optional `node.view` payload, but no current producer or documented consumer uses that field; it is a latent skip rather than a promoted finding. `nodes.tsx` and `tokens.ts` contain the intentional closed catalog/shared nodes and token maps; no independent material defect was found. All assigned tests were read; none exercises multi-view reconnect order or a chat placement.

Bounded consumer searches and counts:

```sh
rg -n -S 'placement|ViewPlacement|canvasViews|views_show' app/src/protocol.ts app/src/store app/src/components/views/CanvasSurface.tsx app/src/views/ViewRenderer.tsx crates/hirsel-host/src/templates/views.rs crates/hirsel-host/src/lash_runtime/tool_defs.rs crates/hirsel-host/src/lash_runtime/scoped_tools.rs templates/CATALOG.md | wc -l
# 48
rg -n -S 'BTreeMap<String, ActiveView>|active.values()|slice().reverse()|oldest-first|newest-first|instance_id' crates/hirsel-host/src/templates/views.rs app/src/store/selectors.ts app/src/components/views/CanvasSurface.tsx app/src/protocol.ts app/src/store/reducer.ts app/src/store/reducer.views.test.ts | wc -l
# 43
```

## Finding C18-01 — advertised `chat` placement is silently unrenderable

### Concrete state and reachability

`hirsel.views_show` describes itself as supporting canvas or chat and its input schema permits both (`crates/hirsel-host/src/lash_runtime/tool_defs.rs:214-238`). The host validator independently accepts both and only rejects other strings (`crates/hirsel-host/src/templates/views.rs:257-262`). A successful show stores the supplied placement and publishes it (`views.rs:95-104, 245-253`).

The client protocol type was narrowed to `ViewPlacement = "canvas"` (`app/src/protocol.ts:75-85, 481-489`), but websocket JSON is only cast to `ServerMessage` at runtime (`app/src/ws/client.ts:306-307`). The reducer can therefore store a host-sent `chat` value, while `canvasViews` drops every placement whose value is not `canvas` (`app/src/store/selectors.ts:12-18`). The only thread utility surface mounted by `ThreadShell` is `CanvasRail`/`CanvasSheet` (`app/src/threads/ThreadShell.tsx:208-213`); the audited app has no chat-view consumer. The same unrendered value is representable in the Rust wire type, whose placement is an unconstrained `String` (`crates/hirsel-proto/src/view.rs:5-10`; `crates/hirsel-proto/src/host.rs:131-136`).

Concrete sequence: an agent calls the advertised tool with `placement: chat`. Host validation succeeds, `ViewUpsert` is sent, the browser dispatches it, and state contains the view; `canvasViews` filters it out, so no renderer, button, or error appears. The existing host contract test explicitly locks in this state by asserting both canvas and chat are valid (`crates/hirsel-host/src/lash_runtime/tests.rs:1229-1243`).

### Duplicate truth and target

There is one host active-view writer; the defect is cross-layer contract drift, not an additional database writer. The same placement is independently advertised, validated, serialized, typed, and filtered, with no end-to-end consumer for chat.

For the current shipped UI, make Canvas the sole placement end to end:

* remove chat from the `views_show` schema/description and `validate_placement`, and make the Rust protocol placement a closed canvas value (or shared constant);
* keep `ViewPlacement`/`ViewUpsertMsg`/selectors/renderers canvas-only and update the host contract test and placement documentation together;
* if chat is a required product surface instead, add an explicit chat owner, selector, and renderer before widening the shared contract.

The canvas-only cutover is process-local and has no stored-data migration, but it turns a previously accepted request into a clear validation error. Validate schema, host rejection, TypeScript typecheck, and an end-to-end show event; none was run here. Confidence: high.

## Finding C18-02 — view ordering diverges between live updates and snapshots

### Concrete state and reachability

The host stores active views in `BTreeMap<String, ActiveView>` (`crates/hirsel-host/src/templates/views.rs:39-45`). A supplied `instance_id` only has to be non-empty (`views.rs:95-98`), and show inserts by ID (`views.rs:113-123`). `snapshot()` returns `active.values()` in map-key order (`views.rs:208-214`), and reconnect `hello_ok` uses that snapshot (`crates/hirsel-host/src/protocol.rs:250-265`). Neither the Rust/host wire shape nor TypeScript `ViewInstance` carries an order field (`crates/hirsel-proto/src/view.rs:6-10`, `host.rs:131-136`, `app/src/protocol.ts:78-85`).

Live browser upserts remove a matching ID and append the new row (`app/src/store/reducer.ts:10-14`). The selector documents insertion order as oldest-first (`app/src/store/selectors.ts:14-18`), and `CanvasSurface` reverses that array to render newest-first (`app/src/components/views/CanvasSurface.tsx:32-44`). Host updates replace the same BTreeMap key without changing key order (`views.rs:169-178`).

Concrete sequence: show z, then a, in one Thread. Live state is `[z,a]` and Canvas renders `[a,z]`, matching newest-first. After reconnect, the host snapshot is `[a,z]` (BTreeMap key order), so the same client renders `[z,a]`; older z is now first. Updating z live also moves it to the browser array's end while the host map remains lexicographically ordered. The current reducer test only asserts a hand-built order (`app/src/store/reducer.views.test.ts:50-60`), and the host lifecycle test has only one view (`crates/hirsel-host/src/templates.rs:238-255`).

### Duplicate truth and target

No durable second writer exists, but two independent representations act as ordering truth: host map-key iteration for snapshots and browser array position for incremental events. Their rules disagree, and no field lets a consumer recover intended order.

Preferred target: assign a monotonic `created_seq`/`ordinal` once in `ViewManager` when an identity first appears, retain it for same-ID updates, and carry it through `hirsel-proto::ViewInstance`, `HostToClient::ViewUpsert`, the TypeScript `ViewInstance`/upsert, and hello snapshots. Sort the Canvas selector by this explicit value and remove the implicit reverse-order contract; the reducer should replace an existing ID in place. Clearing and recreating an ID receives a new sequence. This gives incremental and snapshot paths one ordering owner while retaining map-by-identity lookup.

Regression risk is additive wire-shape work and an explicit decision about same-ID show; there is no database migration because active views are process-local. Add host multi-view snapshot/update tests and browser live-vs-hello/reconnect tests, then run focused Rust/TypeScript checks and a real reconnect smoke test. None was run here. Confidence: high.

## Explicit skips

Both responsive Canvas shells are mounted concurrently in `ThreadShell`, so a future stateful form could duplicate local component state across hidden breakpoints. That is a responsive UX concern without a separate protocol or schema contradiction and is deferred under the cap. `ThreadInstrument.viewSlot` and malformed node shapes were likewise inspected but remain producer-guarded or unused at this snapshot.
