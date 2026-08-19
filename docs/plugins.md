# Writing a hirsel plugin

A plugin is a folder in this repository. Installing one is dropping the folder
in, running `just sync-plugins`, and rebuilding — the compiler is the manifest
parser, the version gate, and the sandbox. There is no runtime install, no wire
protocol, and no sandbox: a plugin is compiled into the host and runs in-process
with full trust. See [ADR-0014](adr/0014-in-tree-plugin-folders.md) for why.

## Folder layout

```text
plugins/<id>/
  Cargo.toml       # package name is free; `version` is what the app shows
  src/lib.rs       # the Plugin impl + `pub fn plugin() -> Box<dyn Plugin>`
  ui/index.tsx     # optional Solid module, glob-loaded by the app
  skills/*.md      # optional prompt packs appended to the agent prompt
```

`<id>` must be lowercase kebab-case and must equal `Plugin::id()`. A plugin
whose id is invalid or duplicated is logged and skipped at boot.

The layout above and the trait below are the whole template: make the folder,
implement the two required methods, and add only the surfaces you need. No
plugin ships in this repository — `plugins/` is empty until you install one.

## The contract

Plugins depend on `hirsel-plugin-api` and on nothing else of hirsel's (the host
depends on plugins through the generated aggregator, so any other edge is a
cycle). The trait:

```rust
#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> &'static str;                                  // required
    fn label(&self) -> &'static str;                               // required
    fn settings(&self) -> Vec<SettingDescriptor> { .. }            // default: none
    fn tools(&self) -> Vec<PluginTool> { .. }                      // default: none
    fn skills_dir(&self) -> Option<&'static str> { .. }            // default: none
    fn routes(&self) -> Option<Router<PluginRouterState>> { .. }   // default: none
    async fn run(&self, ctx: PluginCtx) { .. }                     // default: returns
}
```

Every crate must also export the constructor the generated registry calls:

```rust
pub fn plugin() -> Box<dyn Plugin> { Box::new(MyPlugin) }
```

### Settings

`SettingDescriptor::string / boolean / secret`, optionally `.with_default(..)`.
Values are persisted by the host and shown in Settings → Plugins. Secrets are
write-only from the app's side: they read back as `"<set>"`, and sending
`"<set>"` back means "leave it alone". They are never logged.

### Tools

Each `PluginTool` enters the agent catalog as
`plugin__<id_with_underscores>__<name>` and binds into the lashlang surface as
`plugins.<id>.<name>`, so `name` must be a lowercase identifier. Handlers are
`Fn(PluginCtx, Value) -> impl Future<Output = Result<Value, String>>` and are
bounded at 120 seconds. Return a JSON object.

Enabling or disabling a plugin changes the agent tool surface, which rotates
the agent session through the existing handoff-seed path. That is accepted and
expected — see the ADR.

### Routes

`routes()` returns an `axum::Router<PluginRouterState>` (the state *is* the
plugin's `PluginCtx`). The host nests it at `/api/plugins/<id>/`, behind the
same owner-token gate as the rest of the API, and serves it only while the
plugin is enabled — a disabled plugin's routes 404. **`/enabled` and
`/settings` are reserved** by the management API; defining either will collide.

### Skills

`skills_dir()` names a folder-relative directory of `.md` files. Their contents
are appended to the agent prompt as delimited per-plugin sections while the
plugin is enabled. The prompt is assembled once per session, so a toggle takes
effect on the next host start.

### The daemon

`run(ctx)` is optional and supervised. A panic is caught and restarted with
exponential backoff capped at 30s; five crashes inside 60 seconds park the
plugin as `errored` (visible in Settings with the last error) until it is
re-enabled. Returning normally means "finished" — the host does not restart it,
which is what makes the default no-op free.

## `PluginCtx`

Cheap to clone; hand it around freely.

| Capability | Use |
| --- | --- |
| `ctx.events()` | `notify` / `summary` / `judgment` / `resolve` — typed Events in Sam's feed |
| `ctx.kv()` | `get` / `set` / `delete` / `entries` in a namespace private to the plugin |
| `ctx.setting_str(k)`, `ctx.setting_bool(k)`, `ctx.watch_settings()` | current settings, and a `watch` that fires on save |
| `ctx.push(topic, data)` | a `plugin_push` frame to every connected client |
| `ctx.log()` | `debug` / `info` / `warn` / `error`, tagged `plugin = <id>` |

## The sync script

`scripts/sync-plugins.sh` scans `plugins/*/Cargo.toml` and regenerates
`crates/hirsel-plugins/{Cargo.toml, src/registry.rs}`. Both are checked in:

- `just dev` runs it automatically;
- `just check` and CI run `scripts/check-plugins-synced.sh`, which re-runs the
  generator and fails if the tree differs — so a folder dropped in but never
  synced is caught rather than silently ignored;
- run `just sync-plugins` by hand after adding or removing a folder.

The generator reads `name` and `version` out of the plugin's `Cargo.toml`
literally, so a plugin cannot inherit its version from the workspace.

Adding a folder under `plugins/` also makes it a workspace member (the root
`Cargo.toml` lists `plugins/*`), so `cargo clippy --workspace` and
`cargo test --workspace` cover it with no further wiring.

## Sharing

Copy the folder into another hirsel checkout and run `just sync-plugins`. That
is the whole distribution story, and it is proportionate to a system with one
user.
