//! `hello` — the hirsel plugin template.
//!
//! This crate exists to be copied. It exercises every surface of the plugin
//! contract exactly once, with enough commentary that a new plugin can be
//! written by deleting the parts you do not need:
//!
//! * a **setting** (`greeting`) the owner edits in Settings → Plugins,
//! * an **agent tool** (`ping`) the agent can call,
//! * an **HTTP route** (`POST /greet`) the plugin's UI module calls,
//! * **KV** state that survives restarts (the greet counter),
//! * a **daemon** (`run`) that pushes a `tick` to connected clients.
//!
//! Folder layout of a plugin (see `docs/plugins.md`):
//!
//! ```text
//! plugins/hello/
//!   Cargo.toml     # this crate; `version` is what the app shows
//!   src/lib.rs     # the Plugin impl + `pub fn plugin()`
//!   ui/index.tsx   # optional Solid module, glob-loaded by the app
//!   skills/*.md    # optional prompt packs appended to the agent prompt
//! ```
//!
//! Sharing a plugin is copying the folder into another hirsel checkout and
//! running `scripts/sync-plugins.sh`.

use std::time::Duration;

use axum::{Json, Router, extract::State, routing::post};
use hirsel_plugin_api::{
    Plugin, PluginCtx, PluginRouterState, PluginTool, SettingDescriptor, async_trait,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How often the daemon pushes a tick.
const TICK_INTERVAL: Duration = Duration::from_secs(30);

/// The KV key holding the greet counter. KV keys live in a namespace private
/// to this plugin, so a short name is fine.
const GREET_COUNT_KEY: &str = "greet_count";

/// The push topic the hello UI module subscribes to (`api.onPush("tick", …)`).
const UI_PUSH_TOPIC: &str = "tick";

pub struct HelloPlugin;

/// The one symbol the generated registry looks for. Every plugin crate must
/// export it with exactly this name and signature.
pub fn plugin() -> Box<dyn Plugin> {
    Box::new(HelloPlugin)
}

#[async_trait]
impl Plugin for HelloPlugin {
    /// Must equal the folder name (`plugins/hello`), lowercase kebab-case.
    fn id(&self) -> &'static str {
        "hello"
    }

    fn label(&self) -> &'static str {
        "Hello"
    }

    /// One string setting with a default. Use `SettingDescriptor::secret` for
    /// credentials — the host masks those as `"<set>"` everywhere and never
    /// logs them.
    fn settings(&self) -> Vec<SettingDescriptor> {
        vec![SettingDescriptor::string("greeting", "Greeting").with_default("Hello")]
    }

    /// Agent tools. The host namespaces this one into the catalog as
    /// `plugin__hello__ping` and gives the handler 120 seconds.
    fn tools(&self) -> Vec<PluginTool> {
        vec![PluginTool::new(
            "ping",
            "Reply with pong, echoing an optional message. A hello-world tool for the plugin template.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "message": { "type": "string", "description": "Optional text echoed back." }
                }
            }),
            // A handler is any `Fn(PluginCtx, Value) -> impl Future`, so an
            // `async move` block is the whole shape — no boxing needed here.
            |ctx: PluginCtx, args: Value| async move {
                let message = args
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                ctx.log().info("hello.ping called");
                Ok(json!({ "pong": true, "message": message }))
            },
        )]
    }

    /// Prompt packs under `plugins/hello/skills/*.md` are appended to the
    /// agent prompt while this plugin is enabled. Return `None` when the
    /// plugin ships no skills.
    fn skills_dir(&self) -> Option<&'static str> {
        Some("skills")
    }

    /// A router the host nests at `/api/plugins/hello/`, behind the same
    /// owner-token gate as the rest of the host API and reachable only while
    /// the plugin is enabled. Its state is this plugin's `PluginCtx`.
    fn routes(&self) -> Option<Router<PluginRouterState>> {
        Some(Router::new().route("/greet", post(greet)))
    }

    /// The optional daemon. The host supervises it: a panic is caught and
    /// restarted with exponential backoff, and a crash loop parks the plugin
    /// in the `errored` state. Returning normally means "finished" — the host
    /// does not restart it. Loop forever if the plugin is meant to stay live.
    async fn run(&self, ctx: PluginCtx) {
        let mut settings = ctx.watch_settings();
        loop {
            // Waking on the settings watch as well as the timer means a
            // settings save is observed immediately rather than up to a tick
            // later. `changed()` resolves on the *next* save.
            tokio::select! {
                _ = tokio::time::sleep(TICK_INTERVAL) => {}
                changed = settings.changed() => {
                    if changed.is_err() {
                        // The host dropped the sender: the plugin is being
                        // disabled or the host is shutting down.
                        return;
                    }
                    ctx.log().info("hello settings changed");
                    continue;
                }
            }
            let count = greet_count(&ctx).await;
            ctx.push(UI_PUSH_TOPIC, json!({ "count": count }));
        }
    }
}

#[derive(Debug, Deserialize)]
struct GreetRequest {
    name: String,
}

#[derive(Debug, Serialize)]
struct GreetResponse {
    text: String,
    count: u64,
}

/// `POST /api/plugins/hello/greet` — `{"name": "Sam"}` →
/// `{"text": "Hello, Sam!", "count": 3}`.
///
/// The greeting comes from settings and the count from KV, so the response
/// shows both persistence surfaces at once.
async fn greet(
    State(ctx): State<PluginCtx>,
    Json(request): Json<GreetRequest>,
) -> Result<Json<GreetResponse>, (axum::http::StatusCode, String)> {
    let greeting = ctx
        .setting_str("greeting")
        .unwrap_or_else(|| "Hello".to_string());
    let count = greet_count(&ctx).await + 1;
    ctx.kv()
        .set(GREET_COUNT_KEY, json!(count))
        .await
        .map_err(|error| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(GreetResponse {
        text: format!("{greeting}, {}!", request.name),
        count,
    }))
}

async fn greet_count(ctx: &PluginCtx) -> u64 {
    ctx.kv()
        .get(GREET_COUNT_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
}
