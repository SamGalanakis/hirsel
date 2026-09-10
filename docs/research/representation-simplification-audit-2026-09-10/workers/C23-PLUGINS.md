# C23-PLUGINS audit

## Findings

1. **High — the browser plugin loader does not reconcile its live UI with the Host roster.** The live set can retain a mounted component and push subscriptions after the Owner disables the plugin, because the settings toggle only changes Host state and refreshes the list. The same one-shot start latch permanently loses a transient roster failure. Target one idempotent roster reconciliation owner that tears down disabled/absent IDs and retries an unsuccessful roster load.

2. **High — plugin identity has two independent build-time authorities.** The Rust plugin implementation supplies Plugin::id(), while the generated registration and Vite glob use the folder basename; no type or boot check enforces equality. A mismatch splits API/storage/tool/route identity from skills and browser UI identity. Target a generated canonical folder ID in PluginRegistration, validate the implementation self-report, and use the canonical ID across Host surfaces.

## Finding 1: stale browser UI after Host state changes

Verdict: **recommend**. Confidence: **high**.

### Evidence and state analysis

The loader keeps a process-global live set and only writes it after a successful factory call:

~~~ts
app/src/plugins/loader.ts:60-61
const loaded = new Map<string, () => void>();

app/src/plugins/loader.ts:103-107
const dispose = (factory as PluginFactory)(makeApi(info, disposers));
loaded.set(info.id, () => {
  if (typeof dispose === "function") dispose();
  for (const off of disposers.splice(0).reverse()) off();
});
~~~

Cleanup exists, but only a caller of teardownPlugin reaches it:

~~~ts
app/src/plugins/loader.ts:119-122
export function teardownPlugin(id: string): void {
  const dispose = loaded.get(id);
  loaded.delete(id);

app/src/plugins/loader.ts:129-130
  unregisterPlugin(id);
}
~~~

The cleanup path also clears the registry-owned contributions:

~~~ts
app/src/plugins/registry.ts:137-146
export function unregisterPlugin(id: string): void {
  for (const name of SLOT_NAMES) {
    setSlots(draft => { draft[name] = ((current) => current.filter((entry) => entry.pluginId !== id))(draft[name]); });
  }
  pushHandlers.delete(id);
  clearLoadFailure(id);
}
~~~

Roster loading gates only the current loop. It does not remove IDs absent from the roster, and it skips a disabled entry without teardown; IDs still present are handled by loadOne:

~~~ts
app/src/plugins/loader.ts:138-145
  let plugins: PluginInfo[];
  try {
    plugins = await list();
  } catch (error) {
    // A roster fetch failure leaves plugin loading unavailable for this attempt.
    // eslint-disable-next-line no-console
    console.warn("hirsel plugins: roster unavailable", error);
    return;
  }

app/src/plugins/loader.ts:153-156
    if (info.state === "disabled") continue;
    const importModule = modules[info.id];
    if (!importModule) continue; // A plugin with no UI half — nothing to mount.
    await loadOne(info, importModule);
~~~

The page-level call is permanently latched before that asynchronous load has succeeded:

~~~ts
app/src/App.tsx:74-76
  createEffect(() => state.connection, (connection) => {
    if (connection === "connected") startPlugins();
  });

app/src/plugins/loader.ts:160-169
let started = false;
export function startPlugins(deps: LoaderDeps = {}): void {
  if (started) return;
  started = true;
  void loadPlugins(deps);
}
~~~

The only production toggle path updates the Host and re-reads the roster; it never calls the loader or teardown:

~~~ts
app/src/components/settings/PluginsSection.tsx:156-160
async function toggle(enabled: boolean) {
  setBusy(true);
  try {
    await setPluginEnabled(props.plugin.id, enabled);
    props.onChanged();
  } catch (error) {
    toast(error instanceof Error ? error.message : "Couldn't change plugin");
  }
}

app/src/components/settings/PluginsSection.tsx:205-212
  async function refresh() {
    try {
      setPlugins(await fetchPlugins());
    } catch {
      // No plugin surface (older Host) or the Host is unreachable: the section
      // stays hidden rather than shouting about an absent optional feature.
      setPlugins(null);
    }
  }

app/src/plugins/host.ts:18-20
  method: "POST",
  body: { enabled },
});
~~~

The Host deliberately makes a disabled plugin's own routes unavailable:

~~~rust
crates/hirsel-host/src/plugins/http.rs:92-96
    if gate.host.is_running(&gate.plugin_id).await {
        next.run(request).await
    } else {
        (
            StatusCode::NOT_FOUND,
~~~

The reachable invalid combinations are:

- loaded.has(id) == true plus the freshly authoritative Host roster reporting state == "disabled". The slot registry and push map remain live, while the plugin's own authenticated fetch can now receive 404. The intended initial gate is explicit in loader.ts:149-155, but no runtime path enforces the same invariant.
- started == true plus the first list() rejecting. loadPlugins returns without loading; a later authenticated connection invokes startPlugins again, but the latch returns immediately. This leaves an enabled UI absent for the rest of the page.

The Host toggle is the write path that updates one copy of the state without the other. There is no concurrent writer that updates the live set from that toggle, and no duplicate write path was found for the loader map itself. Current source has no installed plugin UI (plugins/.gitkeep only), so no live entry is already in the invalid combination; the path is reachable as soon as an in-tree plugin with UI is installed.

### Consumer blast radius

Reproducible production consumer/owner query:

~~~sh
rg -n 'startPlugins|loadPlugins|setPluginEnabled|teardownPlugin|fetchPlugins|loaded|started|unregisterPlugin' app/src/App.tsx app/src/components/settings/PluginsSection.tsx app/src/plugins/loader.ts app/src/plugins/host.ts app/src/plugins/registry.ts | wc -l
~~~

Result: **30 matching lines**. The affected behavior crosses the authenticated App boot effect, the settings toggle, the Host HTTP roster, the loader live map, the slot/push registry, and every plugin UI component. It does not require a database or wire-schema change.

Existing coverage demonstrates only adjacent behavior:

- app/src/plugins/loader.test.ts:44-56 verifies that a plugin already reported disabled at initial load is not imported.
- app/src/plugins/loader.test.ts:153-163 verifies the once-per-page latch with an empty successful roster.
- app/src/components/settings/settings-plugins.test.tsx:101-118 verifies POST plus settings-section refresh, but its mocked fetch is not connected to the loader.
- No owned test loads a plugin, changes its roster state to disabled/absent, and asserts disposer, slot, and push cleanup; no test makes the first roster request fail and then retries.

### Target representation and scope

Make the successful Host roster the desired-state owner for browser plugins. A reconciliation operation should:

- derive the desired live IDs from roster entries whose state is not disabled and whose module exists;
- call the existing teardown path for every loaded ID not in that desired set;
- load each desired missing/replaced module exactly once;
- keep an in-flight/generation guard so a stale response cannot mount after a newer disable;
- leave the page-start latch as an idempotence guard only after an unsuccessful roster fetch, rather than treating the failed attempt as success.

The smallest credible scope is app/src/plugins/loader.ts plus the production caller that has the new roster after a toggle, app/src/components/settings/PluginsSection.tsx (with app/src/App.tsx retaining authenticated boot), and loader/settings tests. The existing registry teardown is sufficient. Risk is a visible unmount while a plugin is being disabled, disposer exceptions, and a disable racing an import; the current disposer catch and an explicit generation check cover those boundaries. Required validation, not run here: add load-then-disable/omit, failed-roster-then-retry, disable-during-import, and disable/re-enable no-duplicate tests; then run the bounded web test/lint/type checks.

## Finding 2: folder ID and Plugin::id() can diverge

Verdict: **recommend**. Confidence: **high**.

### Evidence and state analysis

The contract states the equality invariant, but the API type stores no canonical folder ID and accepts independently supplied plugin and directory values:

~~~rust
crates/hirsel-plugin-api/src/lib.rs:60
fn id(&self) -> &'static str;
~~~

The exact preceding contract comment at lines 56-59 says the ID must equal the
folder name under plugins/, must be lowercase kebab-case, and is skipped when
invalid or duplicated.

~~~rust

crates/hirsel-plugin-api/src/lib.rs:101-120
pub struct PluginRegistration {
    pub plugin: Box<dyn Plugin>,
    pub version: &'static str,
    pub dir: &'static str,
}

impl PluginRegistration {
    pub fn new(plugin: Box<dyn Plugin>, version: &'static str, dir: &'static str) -> Self {
        Self {
            plugin,
            version,
            dir,
        }
    }
}
~~~

The generator derives the folder ID for the path and writes that path into the registration, but never checks the plugin's implementation:

~~~bash
scripts/sync-plugins.sh:35-39
while IFS= read -r manifest; do
  dir="$(dirname "$manifest")"
  id="$(basename "$dir")"
  name="$(manifest_field "$manifest" name)"
  version="$(manifest_field "$manifest" version)"
~~~

At scripts/sync-plugins.sh:93-94, the generated call passes the plugin constructor, manifest version, and plugins/<folder-id> as three independent registration arguments. No generated argument contains the plugin implementation's id() result.

At boot the Host makes the implementation's self-reported ID authoritative, while retaining the generated directory separately:

~~~rust
crates/hirsel-host/src/plugins.rs:127-145
for registration in registrations {
    let id = registration.plugin.id().to_string();
    if !is_valid_plugin_id(&id) {
        tracing::error!(
            plugin = %id,
            "plugin id must be lowercase kebab-case; skipping the plugin"
        );
        continue;
    }
    if seen.insert(id.clone(), ()).is_some() {
        tracing::error!(plugin = %id, "duplicate plugin id; skipping the later plugin");
        continue;
    }
    let plugin: Arc<dyn Plugin> = Arc::from(registration.plugin);
    let label = plugin.label().to_string();
    let descriptors = plugin.settings();
    let stored = storage.plugin_settings(&id).await?;
    let (settings_tx, settings_rx) =
        watch::channel(ctx::effective_settings(&descriptors, &stored));
~~~

~~~rust
crates/hirsel-host/src/plugins.rs:169-175
plugins.push(Arc::new(LoadedPlugin {
    plugin,
    id,
    label,
    version: registration.version.to_string(),
    dir: resolve_plugin_dir(registration.dir),
    descriptors,
    ctx: plugin_ctx,
    settings_tx,
}));
~~~

The separate values feed different surfaces:

~~~rust
crates/hirsel-host/src/plugins/http.rs:69
        router = router.nest(&format!("/api/plugins/{}", loaded.id), nested);

crates/hirsel-host/src/plugins/tools.rs:65-69
plugin_id: plugin_id.to_string(),
catalog_name: catalog_name(plugin_id, &tool.name),
module_segment: plugin_id.replace('-', "_"),
operation: tool.name.clone(),

crates/hirsel-host/src/plugins.rs:334-336
let skills_dir = loaded.dir.join(loaded.plugin.skills_dir()?);
~~~

The browser independently takes the folder basename and matches it to the API roster:

~~~ts
app/src/plugins/loader.ts:33
const UI_MODULES = import.meta.glob("../../../plugins/*/ui/index.tsx");
~~~

~~~ts
app/src/plugins/loader.ts:37-38
export function pluginIdFromPath(path: string): string | null {
  return path.match(/\/plugins\/([^/]+)\/ui\/index\.tsx$/)?.[1] ?? null;
}
~~~

~~~ts
app/src/plugins/loader.ts:44-46
  for (const [path, importer] of Object.entries(UI_MODULES)) {
    const id = pluginIdFromPath(path);
    if (id) modules[id] = importer;
  }
~~~

~~~ts
app/src/plugins/loader.ts:153-156
    if (info.state === "disabled") continue;
    const importModule = modules[info.id];
    if (!importModule) continue; // A plugin with no UI half — nothing to mount.
    await loadOne(info, importModule);
~~~

For the concrete combination plugins/github-notifier/ plus an implementation returning Plugin::id() == "github":

- the generated registration carries dir plugins/github-notifier, while the Host API, settings/storage key, route and tool namespace use github;
- skills resolve under the generated directory, plugins/github-notifier;
- the browser module is keyed github-notifier, so modules["github"] is absent and the UI is silently skipped as if the plugin had no UI.

Only the equality case has one coherent identity. The mismatch is reachable when a plugin is added or a test constructs the public PluginRegistration::new with inconsistent values. No mutable write path updates one copy after boot; the duplicate truth is latent at registration/build time. The current checkout has no installed plugin (crates/hirsel-plugins/src/registry.rs:9-11 returns an empty Vec and plugins/.gitkeep is the only file), so no live mismatch or persisted row exists.

### Consumer blast radius

Reproducible production identity query:

~~~sh
rg -n 'plugin\.id\(\)|registration\.dir|loaded\.id|pluginIdFromPath|modules\[info\.id\]|nest\(&format!\("/api/plugins|catalog_name\(' scripts/sync-plugins.sh crates/hirsel-plugin-api/src/lib.rs crates/hirsel-host/src/plugins.rs crates/hirsel-host/src/plugins/http.rs crates/hirsel-host/src/plugins/tools.rs app/src/plugins/loader.ts crates/hirsel-plugins/src/registry.rs | wc -l
~~~

Result: **18 matching lines**. A mismatch can affect the management API, persisted settings and KV namespaces, route gate, tool catalog/fingerprint and dispatch, skills, and browser UI. It is a build/registration contract issue, not a database migration.

Existing fixtures all intentionally match their directory and implementation IDs:

- crates/hirsel-host/src/plugins/tests.rs:285-317 tests invalid and duplicate implementation IDs, but each valid fixture uses the same basename in PluginRegistration::new.
- app/src/plugins/loader.test.ts:166-184 tests folder extraction and an empty discovered set, not an API/loader ID mismatch.
- crates/hirsel-host/src/lash_runtime/tests.rs:978-987 likewise passes catalog-test for both the plugin ID and registration directory.
- No test asserts that a mismatched registration is rejected or that all surfaces share the same canonical ID.

### Target representation and scope

Have the generator place the folder basename in a canonical id field on PluginRegistration, for example:

~~~rust
pub struct PluginRegistration {
    pub id: &'static str,
    pub plugin: Box<dyn Plugin>,
    pub version: &'static str,
    pub dir: &'static str,
}
~~~

The Host should validate plugin.id() == registration.id and that the directory basename is the same canonical ID before constructing LoadedPlugin. After validation, registration.id should be the only authority used for storage keys, ctx attribution, routes, tools and status; the skills path remains a repository location derived from the validated directory/ID, and the browser's existing folder-keyed glob maps to the same generated ID. Keeping Plugin::id as a checked self-report preserves the plugin-facing API while deleting the unvalidated duplicate authority.

The smallest credible scope is crates/hirsel-plugin-api/src/lib.rs, scripts/sync-plugins.sh, generated crates/hirsel-plugins/src/registry.rs, crates/hirsel-host/src/plugins.rs, and the existing PluginRegistration fixtures/tests (including the catalog test consumer). No DB or wire migration is needed. Regression risk is a public constructor/signature change for in-tree plugin crates and the possibility of rejecting a previously bootable misregistered plugin; validation should cover matching, invalid, duplicate, and mismatched IDs, plus the generated sync output. Required validation, not run here: the relevant Rust workspace tests, the plugin-sync check, and the web loader tests.

## Cross-cutting patterns

None promoted. Finding 1 is a runtime desired-state reconciliation defect; Finding 2 is a build-time identity ownership defect. They are distinct and do not supersede one another.

## Coverage contract and skip log

Conversion path reviewed: folder/Cargo manifest -> generated aggregator -> PluginRegistration -> LoadedPlugin -> settings/KV, routes, tools, skills and supervision; independently, Vite folder glob -> module map -> loader -> slot/push registry; HostToClient plugin_push -> WebSocket client -> push registry; settings descriptors -> stored JSON -> effective/watch snapshot -> masked HTTP response -> TypeScript settings form.

Major read-only consumers outside whole-file ownership: app/src/App.tsx, app/src/main.tsx, app/src/ws/client.ts, app/src/components/settings/PluginsSection.tsx, app/src/components/settings/SettingsSheet.tsx, app/vite.config.ts, app/tsconfig.json, app/PROTOCOL.md, crates/hirsel-host/src/lib.rs, crates/hirsel-host/src/tools.rs, crates/hirsel-host/src/lash_runtime/executor.rs, crates/hirsel-host/src/lash_runtime/lifecycle.rs, and crates/hirsel-host/src/lash_runtime/scoped_tools.rs. They were read as consumers only; no adjacent definition was treated as co-owned.

| Owned file or exact shared definition | Reviewed definitions / boundary | Disposition |
| --- | --- | --- |
| app/src/plugins/PluginSlot.tsx | PluginSlot, failureDetail, per-contribution error boundary | Skip: per-entry isolation is structurally clear and covered by registry.test.tsx. |
| app/src/plugins/host.ts | fetchPlugins, setPluginEnabled, savePluginSettings, pluginFetch | Covered by finding 1 as the Host-state write path; no separate defect. |
| app/src/plugins/loader.ts | UI_MODULES, folder parser, LoaderDeps, live map, load/teardown/start lifecycle | Finding 1. |
| app/src/plugins/registry.ts | SlotTable/store, pushHandlers, PluginLoadFailure, teardown/reset | Skip: slot order, nested push ownership and idempotent cleanup have one owner; no additional duplicate truth. |
| app/src/plugins/types.ts | SlotName/SLOT_NAMES, setting shapes, PluginState/PluginInfo, PluginApi | Skip: wire shapes are projections; state/error optionality and setting maps were lower-ranked with no malformed current producer. |
| crates/hirsel-host/src/lash_runtime/plugin.rs | HirselProcessPluginFactory, EmptyHirselSessionPlugin, lashlang surface contribution | Skip: the repeated plugin ID is required by two distinct Lash extension traits; no independent mutable state. |
| crates/hirsel-host/src/plugins.rs | LoadedPlugin, PluginRuntime/PluginStatus, PluginHost start/status/toggle/settings, path resolution and skills | Finding 2 for registration identity; runtime state otherwise skip as intentional enabled/errored supervision state. |
| crates/hirsel-host/src/plugins/ctx.rs | HostThreads, HostKv, HostSettings, HostPush, effective_settings | Skip: capability owners and watch snapshot are explicit; settings lead was lower-ranked and is recorded below. |
| crates/hirsel-host/src/plugins/http.rs | management/router gates, list, settings request validation/masking | Covered by findings 1-2 as consumers; no additional route or mask defect accepted. |
| crates/hirsel-host/src/plugins/scoped_ctx.rs | invocation-scoped threads and thread KV | Skip: plugin/thread scope is host-bound and the table key carries both identities. |
| crates/hirsel-host/src/plugins/supervisor.rs | SupervisorConfig, spawned task, crash window/backoff, set_errored | Skip: crash-loop parking and restart ownership match the settled daemon contract; no extra state transition accepted. |
| crates/hirsel-host/src/plugins/tools.rs | RegisteredTool, PluginToolRegistry, catalog/dispatch | Covered by finding 2 for ID consumption; invalid tool names are rejected and the map owns one catalog key. |
| crates/hirsel-host/src/storage/plugins.rs | enable flags, setting merge, global KV persistence/decoding | Skip: separate setting and global KV namespaces are intentional; primary keys enforce one row per plugin/key. |
| crates/hirsel-plugin-api/src/ctx.rs | SettingsSnapshot, NewThread/NewActivity/receipt, capability traits, PluginCtx | Skip: thread creation, activity and settlement are intentionally distinct; resource capabilities are scoped at construction. |
| crates/hirsel-plugin-api/src/lib.rs | Plugin trait, PluginRegistration, ID/tool validators | Finding 2. |
| crates/hirsel-plugin-api/src/settings.rs | SettingKind, SettingDescriptor and builders | Skip: kind/default coupling is a latent authoring hazard, but the current registry has no plugin declarations and HTTP writes validate kind; not selected over the two higher-impact findings. |
| crates/hirsel-plugin-api/src/tools.rs | PluginToolFuture, handler, PluginTool constructors | Skip: free-form JSON and object-safe handler are deliberate plugin contract choices. |
| crates/hirsel-plugins/src/lib.rs | generated aggregator export | Covered by finding 2; no plugin is currently installed. |
| crates/hirsel-plugins/src/registry.rs | generated all() registry | Covered by finding 2; current output is an empty Vec. |
| scripts/check-plugins-synced.sh | generated-registry freshness check | Skip: the check closes folder/aggregator drift; it cannot validate Plugin::id, which is the distinct finding 2 gap. |
| scripts/sync-plugins.sh | folder discovery, manifest fields, generated registration | Finding 2. |
| app/src/plugins/loader.test.ts | loader gating, isolation, discovery and latch tests | Finding 1 coverage gap; no separate production behavior. |
| app/src/plugins/registry.test.tsx | slot rendering, error isolation, push fan-out and cleanup | Skip: existing tests cover the owned registry invariants. |
| crates/hirsel-host/src/plugins/tests.rs | host boot, settings/KV, supervision, toggles, routes, tools, push and scoped resources | Finding 2 coverage gap; existing tests cover adjacent behavior but no mismatch. No separate finding. |
| crates/hirsel-host/src/storage/current.sql:97 plugin_state | persisted plugin enable table | Skip: one primary key and one explicit enabled intent; no catalog FK is required for compiled in-tree plugins. |
| crates/hirsel-host/src/storage/current.sql:101 plugin_settings | persisted per-plugin setting values | Skip: opaque JSON is deliberate storage for code-owned descriptors; wrong-kind defaults/legacy keys were lower-ranked and not current live values. |
| crates/hirsel-host/src/storage/current.sql:107 plugin_kv | persisted per-plugin global KV | Skip: namespace and primary key are explicit; separate from settings by contract. |
| crates/hirsel-host/src/storage/current.sql:150 plugin_thread_kv | persisted per-plugin/per-thread KV | Skip: thread scope is explicit in the composite primary key and intentionally differs from global KV. |

No existing exclusions item #2-14 names either accepted plugin finding. No duplicate or superseded plugin outcome was found in exclusions.md.

## Ranking and dependencies

Finding 1 is the best first slice: it is a reachable user-visible state contradiction with no storage/protocol migration and a bounded browser/test change. Finding 2 should follow before the first real plugin is installed, because its target changes the public generated registration constructor and all in-tree fixture call sites.

## Audit log

- Read the assigned specification and exclusions.md before inspection.
- Verified before inspection: git rev-parse HEAD HEAD^{tree} = 3ee0621a603659ab0168f565b99012b642415419 / a4aac830c45398a66591f2c44b707aaf3cef281b; git status --porcelain was empty.
- Inspected all 24 exact whole-file owners and the four named current.sql definitions. Generated plugin output is empty and plugins/ contains only .gitkeep.
- Re-opened every quoted definition for both findings and independently reran the two consumer queries: loader 30 lines, identity 18 lines.
- No application code, tests, builds, installs, migrations, live data/config, provider calls, commits or pushes were run. The only written file is this report outside the repository source tree.
- Verified after report: git rev-parse HEAD HEAD^{tree} remained 3ee0621a603659ab0168f565b99012b642415419 / a4aac830c45398a66591f2c44b707aaf3cef281b, and git status --porcelain remained empty.

First fix: reconcile the browser loader with the authoritative Host roster, because it is the only accepted finding reachable through the current Owner toggle path.
