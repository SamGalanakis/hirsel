use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use axum::{Router, body::Body, http::Request, routing::post};
use hirsel_plugin_api::{
    NewEvent, Plugin, PluginCtx, PluginRegistration, PluginRouterState, PluginTool,
    SettingDescriptor, async_trait,
};
use hirsel_proto::HostToClient;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;

use super::{PluginHost, PluginState, SupervisorConfig, http::masked_values};
use crate::{
    BroadcastLog,
    processes::ProcessStore,
    storage::Storage,
    tools::{ToolSuite, ToolsConfig},
};

/// Fast enough that a crash loop resolves inside a test, same shape as
/// production.
fn test_supervisor_config() -> SupervisorConfig {
    SupervisorConfig {
        base_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
        crash_window: Duration::from_secs(60),
        crash_limit: 3,
    }
}

async fn test_tools(
    path: &std::path::Path,
) -> (ToolSuite, Storage, broadcast::Sender<HostToClient>) {
    let storage = Storage::open(path).await.unwrap();
    let (broadcaster, _keepalive) = broadcast::channel(64);
    let (pushes, _recorded) = crate::push::PushGateway::recording(storage.clone());
    let broadcast_log = BroadcastLog::default();
    let templates =
        crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
            .await
            .unwrap();
    let views =
        crate::templates::ViewManager::new(templates, broadcaster.clone(), broadcast_log.clone());
    let config_store = crate::host_config::ConfigStore::load(
        path.join("hirsel.toml"),
        path,
        std::path::Path::new("/docs/hirsel-config.md"),
    )
    .await
    .unwrap();
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: crate::config::DriverMode::Fake,
            fake_fixture: None,
            subagent_models: crate::subagent_models::SubagentModelState::load(config_store),
        },
        storage.clone(),
        broadcaster.clone(),
        broadcast_log.clone(),
        ProcessStore::default(),
        pushes,
        views,
    );
    (tools, storage, broadcaster)
}

async fn start_host(
    path: &std::path::Path,
    registrations: Vec<PluginRegistration>,
) -> (
    PluginHost,
    ToolSuite,
    Storage,
    broadcast::Sender<HostToClient>,
) {
    let (tools, storage, broadcaster) = test_tools(path).await;
    let host = PluginHost::start(
        registrations,
        storage.clone(),
        tools.clone(),
        broadcaster.clone(),
        BroadcastLog::default(),
        test_supervisor_config(),
    )
    .await
    .unwrap();
    (host, tools, storage, broadcaster)
}

/// A plugin with a tool, a setting, and no daemon.
struct QuietPlugin {
    id: &'static str,
}

#[async_trait]
impl Plugin for QuietPlugin {
    fn id(&self) -> &'static str {
        self.id
    }

    fn label(&self) -> &'static str {
        "Quiet"
    }

    fn settings(&self) -> Vec<SettingDescriptor> {
        vec![
            SettingDescriptor::string("greeting", "Greeting").with_default("Hi"),
            SettingDescriptor::secret("token", "Token"),
        ]
    }

    fn tools(&self) -> Vec<PluginTool> {
        vec![
            PluginTool::new(
                "echo",
                "Echo the argument.",
                json!({}),
                |_ctx, args| async move { Ok(json!({ "echoed": args })) },
            ),
            // Rejected at registration: not a lowercase identifier.
            PluginTool::new(
                "Bad-Name",
                "Never registered.",
                json!({}),
                |_ctx, _args| async move { Ok(Value::Null) },
            ),
        ]
    }
}

/// A plugin whose daemon panics every time it runs.
struct PanicPlugin {
    runs: Arc<AtomicU32>,
}

#[async_trait]
impl Plugin for PanicPlugin {
    fn id(&self) -> &'static str {
        "panicky"
    }

    fn label(&self) -> &'static str {
        "Panicky"
    }

    async fn run(&self, _ctx: PluginCtx) {
        self.runs.fetch_add(1, Ordering::SeqCst);
        panic!("plugin daemon exploded");
    }
}

/// A plugin whose daemon pushes once and then parks.
struct PushPlugin;

#[async_trait]
impl Plugin for PushPlugin {
    fn id(&self) -> &'static str {
        "pusher"
    }

    fn label(&self) -> &'static str {
        "Pusher"
    }

    fn routes(&self) -> Option<Router<PluginRouterState>> {
        Some(Router::new().route(
            "/noop",
            post(|| async { axum::Json(json!({ "ok": true })) }),
        ))
    }

    async fn run(&self, ctx: PluginCtx) {
        ctx.push("ui_push", json!({ "hello": 1 }));
        std::future::pending::<()>().await;
    }
}

#[tokio::test]
async fn invalid_and_duplicate_plugin_ids_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (host, tools, _storage, _broadcaster) = start_host(
        dir.path(),
        vec![
            PluginRegistration::new(
                Box::new(QuietPlugin { id: "quiet" }),
                "1.0.0",
                "plugins/quiet",
            ),
            // Duplicate id: the later registration loses.
            PluginRegistration::new(
                Box::new(QuietPlugin { id: "quiet" }),
                "9.9.9",
                "plugins/quiet",
            ),
            // Not kebab-case: skipped entirely.
            PluginRegistration::new(
                Box::new(QuietPlugin { id: "Not_Kebab" }),
                "1.0.0",
                "plugins/Not_Kebab",
            ),
        ],
    )
    .await;

    assert_eq!(host.inner.plugins.len(), 1);
    assert_eq!(host.inner.plugins[0].id, "quiet");
    assert_eq!(host.inner.plugins[0].version, "1.0.0");
    // The `Bad-Name` tool cannot be bound into the lashlang surface, so only
    // `echo` reaches the catalog.
    assert_eq!(tools.plugin_tools().names(), vec!["plugin__quiet__echo"]);
}

#[tokio::test]
async fn storage_round_trips_enable_state_settings_and_kv() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    assert!(storage.plugin_enabled_flags().await.unwrap().is_empty());
    storage.set_plugin_enabled("hello", false).await.unwrap();
    storage.set_plugin_enabled("other", true).await.unwrap();
    storage.set_plugin_enabled("hello", true).await.unwrap();
    let flags = storage.plugin_enabled_flags().await.unwrap();
    assert_eq!(flags.get("hello"), Some(&true));
    assert_eq!(flags.get("other"), Some(&true));

    let mut values = Map::new();
    values.insert("greeting".into(), json!("Hei"));
    values.insert("token".into(), json!("s3cret"));
    let merged = storage
        .merge_plugin_settings("hello", &values)
        .await
        .unwrap();
    assert_eq!(merged.get("greeting"), Some(&json!("Hei")));

    let mut partial = Map::new();
    partial.insert("greeting".into(), json!("Yo"));
    let merged = storage
        .merge_plugin_settings("hello", &partial)
        .await
        .unwrap();
    assert_eq!(merged.get("greeting"), Some(&json!("Yo")));
    assert_eq!(
        merged.get("token"),
        Some(&json!("s3cret")),
        "a partial merge must not drop untouched keys"
    );

    storage
        .plugin_kv_set("hello", "count", &json!(3))
        .await
        .unwrap();
    assert_eq!(
        storage.plugin_kv_get("hello", "count").await.unwrap(),
        Some(json!(3))
    );
    assert_eq!(
        storage.plugin_kv_get("other", "count").await.unwrap(),
        None,
        "KV namespaces are per plugin"
    );
    assert_eq!(
        storage.plugin_kv_entries("hello").await.unwrap(),
        vec![("count".to_string(), json!(3))]
    );
    storage.plugin_kv_delete("hello", "count").await.unwrap();
    assert!(storage.plugin_kv_entries("hello").await.unwrap().is_empty());
}

#[test]
fn secret_values_are_masked_and_never_returned_in_cleartext() {
    let descriptors = vec![
        SettingDescriptor::string("greeting", "Greeting"),
        SettingDescriptor::secret("token", "Token"),
    ];
    let mut values = Map::new();
    values.insert("greeting".into(), json!("Hi"));
    values.insert("token".into(), json!("s3cret"));

    let masked = masked_values(&descriptors, &values);
    assert_eq!(masked.get("greeting"), Some(&json!("Hi")));
    assert_eq!(masked.get("token"), Some(&json!("<set>")));

    let unset = masked_values(&descriptors, &Map::new());
    assert_eq!(unset.get("token"), Some(&Value::Null));
}

#[tokio::test]
async fn settings_updates_are_persisted_masked_and_delivered_to_the_watch() {
    let dir = tempfile::tempdir().unwrap();
    let (host, _tools, storage, _broadcaster) = start_host(
        dir.path(),
        vec![PluginRegistration::new(
            Box::new(QuietPlugin { id: "quiet" }),
            "1.0.0",
            "plugins/quiet",
        )],
    )
    .await;
    let ctx = host.inner.plugins[0].ctx.clone();
    let mut watch = ctx.watch_settings();
    assert_eq!(ctx.setting_str("greeting").as_deref(), Some("Hi"));

    let mut values = Map::new();
    values.insert("greeting".into(), json!("Hei"));
    values.insert("token".into(), json!("s3cret"));
    host.update_settings("quiet", &values).await.unwrap();

    watch.changed().await.unwrap();
    assert_eq!(ctx.setting_str("greeting").as_deref(), Some("Hei"));
    assert_eq!(ctx.setting_str("token").as_deref(), Some("s3cret"));
    assert_eq!(
        storage.plugin_settings("quiet").await.unwrap().get("token"),
        Some(&json!("s3cret"))
    );
}

#[tokio::test]
async fn a_crash_looping_daemon_is_restarted_then_parked_as_errored() {
    let dir = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicU32::new(0));
    let (host, _tools, _storage, _broadcaster) = start_host(
        dir.path(),
        vec![PluginRegistration::new(
            Box::new(PanicPlugin {
                runs: Arc::clone(&runs),
            }),
            "1.0.0",
            "plugins/panicky",
        )],
    )
    .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if host.status("panicky").state == PluginState::Errored {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "daemon never parked");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Restarted after each panic until the crash limit stopped it.
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    let status = host.status("panicky");
    assert!(status.error.is_some(), "an errored plugin reports why");
    assert!(!host.is_running("panicky"));
}

#[tokio::test]
async fn disabling_a_plugin_removes_its_tools_and_reenabling_restores_them() {
    let dir = tempfile::tempdir().unwrap();
    let (host, tools, storage, _broadcaster) = start_host(
        dir.path(),
        vec![PluginRegistration::new(
            Box::new(QuietPlugin { id: "quiet" }),
            "1.0.0",
            "plugins/quiet",
        )],
    )
    .await;
    assert!(host.is_running("quiet"));
    assert_eq!(tools.plugin_tools().names().len(), 1);

    assert!(host.set_enabled("quiet", false).await.unwrap());
    assert!(
        !host.set_enabled("quiet", false).await.unwrap(),
        "idempotent"
    );
    assert!(tools.plugin_tools().names().is_empty());
    assert_eq!(host.status("quiet").state, PluginState::Disabled);
    assert_eq!(
        storage.plugin_enabled_flags().await.unwrap().get("quiet"),
        Some(&false)
    );

    assert!(host.set_enabled("quiet", true).await.unwrap());
    assert_eq!(tools.plugin_tools().names(), vec!["plugin__quiet__echo"]);
    assert!(host.is_running("quiet"));
}

#[tokio::test]
async fn a_plugin_push_reaches_every_connected_client() {
    let dir = tempfile::tempdir().unwrap();
    let (tools, storage, broadcaster) = test_tools(dir.path()).await;
    let mut client = broadcaster.subscribe();
    let _host = PluginHost::start(
        vec![PluginRegistration::new(
            Box::new(PushPlugin),
            "1.0.0",
            "plugins/pusher",
        )],
        storage,
        tools,
        broadcaster.clone(),
        BroadcastLog::default(),
        test_supervisor_config(),
    )
    .await
    .unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("a push arrives")
        .unwrap();
    assert_eq!(
        frame,
        HostToClient::PluginPush {
            plugin: "pusher".to_string(),
            topic: "ui_push".to_string(),
            data: json!({ "hello": 1 }),
        }
    );
}

#[tokio::test]
async fn plugin_events_land_in_the_feed() {
    let dir = tempfile::tempdir().unwrap();
    let (host, _tools, storage, _broadcaster) = start_host(
        dir.path(),
        vec![PluginRegistration::new(
            Box::new(QuietPlugin { id: "quiet" }),
            "1.0.0",
            "plugins/quiet",
        )],
    )
    .await;
    let ctx = host.inner.plugins[0].ctx.clone();

    let event_id = ctx
        .events()
        .notify(NewEvent::new("build", "The build finished").with_content("All green."))
        .await
        .unwrap();
    let event = storage.ping(event_id).await.unwrap().expect("event exists");
    assert_eq!(event.name, "quiet-build");
    assert_eq!(event.description, "The build finished");

    ctx.events().resolve(event_id).await.unwrap();
    assert_eq!(
        storage.ping(event_id).await.unwrap().unwrap().status,
        hirsel_proto::EventStatus::Done
    );
}

/// The tracked `plugins/hello` template, exercised through the real host:
/// its tool dispatches, its route runs behind the owner gate, and its KV
/// counter survives across calls.
#[tokio::test]
async fn hello_plugin_works_end_to_end_in_the_real_host() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();

    // Registered from the generated aggregator, enabled on first sight.
    assert!(state.plugins.is_running("hello"));
    assert_eq!(state.plugins.tool_names(), vec!["plugin__hello__ping"]);

    // Its skills folder is resolved and folded into the agent prompt.
    let skills = state.plugins.skills_prompt();
    assert!(
        skills.contains("## Plugin skill: Hello / greeting"),
        "hello's skills must reach the agent prompt: {skills:?}"
    );

    // Tool dispatch through the shared plugin tool table.
    let result = state
        .tools
        .plugin_tools()
        .call("plugin__hello__ping", json!({ "message": "hi" }))
        .await
        .expect("hello registers plugin__hello__ping")
        .unwrap();
    assert_eq!(result, json!({ "pong": true, "message": "hi" }));

    let router = crate::router_from_state(state.clone());

    // The plugin's own route, twice: the KV counter increments and the
    // greeting comes from settings.
    for expected in 1..=2u64 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/plugins/hello/greet")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "name": "Sam" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = serde_json::from_slice(&read_body(response).await).unwrap();
        assert_eq!(body, json!({ "text": "Hello, Sam!", "count": expected }));
    }

    // Management listing: descriptors, version, state, values.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/plugins")
                .header("authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_slice(&read_body(response).await).unwrap();
    let hello = &body["plugins"][0];
    assert_eq!(hello["id"], json!("hello"));
    assert_eq!(hello["version"], json!("0.1.0"));
    assert_eq!(hello["state"], json!("running"));
    assert_eq!(hello["values"]["greeting"], json!("Hello"));
    assert_eq!(hello["settings"][0]["kind"], json!("string"));

    // Settings save reaches the plugin, and the route reflects it.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/plugins/hello/settings")
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "values": { "greeting": "Hei" } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/plugins/hello/greet")
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": "Sam" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&read_body(response).await).unwrap();
    assert_eq!(body["text"], json!("Hei, Sam!"));

    // Disabled: tools gone, plugin routes 404, management routes still there.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/plugins/hello/enabled")
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "enabled": false }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(state.plugins.tool_names().is_empty());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/plugins/hello/greet")
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": "Sam" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 404, "a disabled plugin serves no routes");
}

#[tokio::test]
async fn the_plugin_api_requires_the_owner_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config_production_auth(dir.path()))
        .await
        .unwrap();
    let response = crate::router_from_state(state)
        .oneshot(
            Request::builder()
                .uri("/api/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

async fn read_body(response: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap()
        .to_vec()
}
