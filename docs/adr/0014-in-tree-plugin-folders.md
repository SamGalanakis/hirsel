# In-tree plugin folders

Settles how hirsel is extended: a plugin is a folder in this repository, compiled into the host.
Supersedes the TypeScript-subprocess plugin design drafted earlier in the same cycle.

## Context

hirsel needed an extension point: a way to add a capability — an integration, a tool, a Settings
panel, a background watcher — without threading it through the host's own domains every time.

The first design took the shape most systems reach for. Plugins would be **TypeScript packages run
as subprocesses**, speaking a JSON wire protocol to the host, with their UI **served as bundles**
the app fetched at runtime (a synthesis of the bb and DeepSeek-Harness plugin models). It had the
familiar properties: install without rebuilding, isolate failures in a process boundary, iterate on
a plugin without touching the host.

The owner's ruling replaced it: **convention over installation.** hirsel is a single-owner system
running on the owner's own machine, from the owner's own checkout. There is no marketplace, no
third-party author, and no untrusted code. Runtime installation buys nothing that `git pull &&
cargo build` does not already give, and it costs a wire protocol, a process supervisor, a bundle
server, a manifest parser, and a version negotiation — all of which are, in a compiled system, jobs
the compiler already does for free.

Zed was the closest comparison worth taking seriously: it solved the same problem with **WASM**.
That is the right answer *for a marketplace* — a sandbox for code you did not write, distributed to
people you have never met. hirsel has no marketplace. Adopting WASM here would be buying the
sandbox without the distribution it exists to make safe, and paying for it in a host-boundary ABI,
a component model, and an async story that no plugin here needs.

"Flip the host to TypeScript so plugins are native" was considered and declined: it does not remove
the boundary, it moves it — from host↔plugin to host↔lash, which is a far more load-bearing seam
than the one being simplified.

## Decision

**A plugin is a folder at `plugins/<id>/` containing a Rust crate.** Installing a plugin is
dropping the folder in, running `scripts/sync-plugins.sh`, and rebuilding. The compiler is the
manifest parser, the version gate, and the sandbox.

- **The contract is `hirsel-plugin-api`** — a narrow crate holding the `Plugin` trait and
  `PluginCtx`. Plugins depend on it and on nothing else of hirsel's; the host depends on plugins
  through a generated aggregator, so any other edge would be a cycle. It is deliberately small and
  versioned in spirit like a WIT interface.
- **Discovery is generated, not scanned.** `scripts/sync-plugins.sh` reads `plugins/*/Cargo.toml`
  and writes `crates/hirsel-plugins/{Cargo.toml, src/registry.rs}`, both checked in. CI re-runs the
  script and fails if the tree differs, so a folder dropped in but never synced is caught at review
  time rather than silently ignored.
- **Plugins run in-process with full trust.** No subprocess, no wire protocol, no WASM, no served
  bundles. A plugin can do anything the host can do, which is the correct amount of trust for code
  the owner put in the owner's own repository.
- **Five surfaces, all optional:** settings (persisted, secret-aware), agent tools (namespaced
  `plugin__<id>__<name>`), an axum router nested at `/api/plugins/<id>/`, `.md` skills appended to
  the agent prompt, and a supervised `run()` daemon.
- **Sharing is copying the folder.** Into another checkout, and syncing. That is the whole
  distribution story, and it is proportionate to a system with one user.

## Consequences

- **Rebuild to install.** Adding, removing, or updating a plugin requires a rebuild. Accepted: this
  is the same loop every other change to hirsel already takes.
- **Tool-surface fingerprint rotation on toggle.** Enabling or disabling a plugin changes the agent
  tool catalog, which rotates the agent session through the existing handoff-seed path
  (`agent_tool_surface`). Accepted: toggling a plugin is a deliberate, rare owner action, and
  routing it through the mechanism that already exists is strictly better than inventing a second
  one that does not rotate.
- **Skills are assembled at boot.** The agent prompt is built once per session, so a plugin toggled
  at runtime contributes (or stops contributing) skills from the next host start. Its tools and
  routes change immediately.
- **A panicking plugin is contained, a wedged one is not.** The supervisor restarts a panicking
  daemon with exponential backoff and parks it as `errored` after five crashes in sixty seconds; a
  tool handler is bounded at 120s. Nothing contains a plugin that corrupts host state — full trust
  means full trust.
- **The escape hatch is mechanical, if distribution ever materializes.** The `Plugin` trait was
  kept narrow precisely so it ports to a WIT boundary: settings, tools with JSON schemas, routes,
  and a run loop are all expressible there. If hirsel ever grows third-party authors, WASM becomes
  the right answer and this trait is the thing to translate — not a rewrite of the extension model.
