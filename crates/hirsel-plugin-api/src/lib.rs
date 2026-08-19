//! The hirsel plugin contract.
//!
//! A hirsel plugin is a folder at `<repo-root>/plugins/<id>/` holding a Rust
//! crate that depends on this crate and implements [`Plugin`]. Installing a
//! plugin means dropping the folder in, running `scripts/sync-plugins.sh`, and
//! rebuilding: the compiler is the manifest parser, the version gate, and the
//! sandbox. Plugins are compiled into the host and run in-process with full
//! trust — hirsel is a single-owner system, so there is no wire protocol, no
//! subprocess, and no capability sandbox to escape.
//!
//! Treat this crate the way you would a versioned WIT interface: narrow,
//! deliberate, and refactorable, but every item earns its place. Nothing here
//! may depend on `hirsel-host` — the host depends on plugins through the
//! generated `hirsel-plugins` aggregator, so the reverse edge is a cycle.
//!
//! ```ignore
//! use hirsel_plugin_api::{Plugin, PluginCtx, async_trait};
//!
//! pub struct Hello;
//!
//! #[async_trait]
//! impl Plugin for Hello {
//!     fn id(&self) -> &'static str { "hello" }
//!     fn label(&self) -> &'static str { "Hello" }
//! }
//!
//! pub fn plugin() -> Box<dyn Plugin> { Box::new(Hello) }
//! ```

mod ctx;
mod settings;
mod tools;

pub use ctx::{
    EventOption, NewEvent, PluginCtx, PluginEvents, PluginKv, PluginLog, PluginPush,
    PluginSettingsAccess, SettingsSnapshot,
};
pub use settings::{SettingDescriptor, SettingKind};
pub use tools::{PluginTool, PluginToolFuture, PluginToolHandler};

/// Re-exported so a plugin crate does not need its own `async-trait`
/// dependency just to implement [`Plugin`].
pub use async_trait::async_trait;

/// State a plugin's own axum router is built against. The host nests the
/// router under `/api/plugins/<id>/` and supplies the plugin's [`PluginCtx`]
/// as its state, so this is an alias rather than a wrapper type.
pub type PluginRouterState = PluginCtx;

/// One installed plugin.
///
/// Every method except [`Plugin::id`] and [`Plugin::label`] has a default that
/// contributes nothing, so a plugin only implements the surfaces it uses.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Stable identity. It MUST equal the plugin's folder name under
    /// `plugins/`, and MUST be lowercase kebab-case (`^[a-z][a-z0-9-]*$`).
    /// The host logs an error and skips a plugin whose id is invalid or
    /// duplicated.
    fn id(&self) -> &'static str;

    /// Human-readable name shown in Settings → Plugins.
    fn label(&self) -> &'static str;

    /// Settings this plugin exposes in the app. Values are persisted by the
    /// host and read back through [`PluginCtx::settings`].
    fn settings(&self) -> Vec<SettingDescriptor> {
        Vec::new()
    }

    /// Agent tools this plugin contributes. The host namespaces them into the
    /// agent catalog as `plugin__<id_with_underscores>__<name>`; `name` must
    /// be a lowercase identifier (`^[a-z][a-z0-9_]*$`).
    fn tools(&self) -> Vec<PluginTool> {
        Vec::new()
    }

    /// Folder-relative path (e.g. `"skills"`) of a directory of `.md` prompt
    /// packs. The host appends each file's contents to the agent prompt as a
    /// delimited per-plugin section while the plugin is enabled.
    fn skills_dir(&self) -> Option<&'static str> {
        None
    }

    /// A router the host nests at `/api/plugins/<id>/`. It is reachable only
    /// while the plugin is enabled (404 otherwise) and sits behind the same
    /// owner-token gate as the rest of the host API.
    fn routes(&self) -> Option<axum::Router<PluginRouterState>> {
        None
    }

    /// Optional long-running daemon. The host supervises it: a panic is caught
    /// and restarted with exponential backoff, and a crash loop parks the
    /// plugin in the `errored` state. Returning normally means "done" — the
    /// host does not restart a daemon that finished on its own.
    async fn run(&self, ctx: PluginCtx) {
        let _ = ctx;
    }
}

/// One entry in the generated registry.
///
/// `version` and `dir` cannot come from the plugin itself (`env!` would expand
/// in this crate, not in the plugin's), so `scripts/sync-plugins.sh` reads them
/// out of the plugin's `Cargo.toml` and bakes them into `hirsel-plugins`.
pub struct PluginRegistration {
    pub plugin: Box<dyn Plugin>,
    /// The plugin crate's `package.version`.
    pub version: &'static str,
    /// The plugin folder, repository-relative (e.g. `"plugins/github-notifier"`).
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

/// True when `id` is a legal plugin id: lowercase kebab-case, starting with a
/// letter, no trailing or doubled dashes.
pub fn is_valid_plugin_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if id.ends_with('-') || id.contains("--") {
        return false;
    }
    chars.all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    })
}

/// True when `name` is a legal plugin tool name: a lowercase identifier. The
/// agent surface binds tools as `plugins.<id>.<name>`, and lash rejects a
/// module path or operation that is not an identifier.
pub fn is_valid_tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
    })
}

#[cfg(test)]
mod tests {
    use super::{is_valid_plugin_id, is_valid_tool_name};

    #[test]
    fn plugin_ids_are_lowercase_kebab() {
        assert!(is_valid_plugin_id("hello"));
        assert!(is_valid_plugin_id("linear-triage"));
        assert!(is_valid_plugin_id("h2"));
        assert!(!is_valid_plugin_id(""));
        assert!(!is_valid_plugin_id("Hello"));
        assert!(!is_valid_plugin_id("2fast"));
        assert!(!is_valid_plugin_id("hello_world"));
        assert!(!is_valid_plugin_id("hello-"));
        assert!(!is_valid_plugin_id("hello--world"));
    }

    #[test]
    fn tool_names_are_lowercase_identifiers() {
        assert!(is_valid_tool_name("ping"));
        assert!(is_valid_tool_name("send_note2"));
        assert!(!is_valid_tool_name("Ping"));
        assert!(!is_valid_tool_name("send-note"));
        assert!(!is_valid_tool_name(""));
    }
}
