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

use super::{PluginHost, PluginStatus, SupervisorConfig, http::masked_values};
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

    fn tools(&self) -> Vec<PluginTool> {
        vec![PluginTool::new(
            "echo",
            "Echo the argument.",
            json!({}),
            |_ctx, args| async move { Ok(json!({ "echoed": args })) },
        )]
    }

    async fn run(&self, _ctx: PluginCtx) {
        self.runs.fetch_add(1, Ordering::SeqCst);
        panic!("plugin daemon exploded");
    }
}

struct BlockingPlugin {
    starts: Arc<AtomicU32>,
    active: Arc<AtomicU32>,
}

struct ActiveRun(Arc<AtomicU32>);

impl Drop for ActiveRun {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Plugin for BlockingPlugin {
    fn id(&self) -> &'static str {
        "blocking"
    }

    fn label(&self) -> &'static str {
        "Blocking"
    }

    async fn run(&self, _ctx: PluginCtx) {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveRun(Arc::clone(&self.active));
        std::future::pending::<()>().await;
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

/// A plugin exercising the surfaces an installed plugin uses at once: a
/// setting, a tool, and a route that reads settings and writes KV.
struct GreeterPlugin;

#[async_trait]
impl Plugin for GreeterPlugin {
    fn id(&self) -> &'static str {
        "greeter"
    }

    fn label(&self) -> &'static str {
        "Greeter"
    }

    fn settings(&self) -> Vec<SettingDescriptor> {
        vec![SettingDescriptor::string("greeting", "Greeting").with_default("Hello")]
    }

    fn tools(&self) -> Vec<PluginTool> {
        vec![PluginTool::new(
            "ping",
            "Echo a message back.",
            json!({}),
            |_ctx, args| async move { Ok(json!({ "pong": true, "message": args["message"] })) },
        )]
    }

    fn routes(&self) -> Option<Router<PluginRouterState>> {
        Some(Router::new().route("/greet", post(greet)))
    }
}

/// `POST /api/plugins/greeter/greet` — the greeting comes from settings and
/// the count from KV, so one response covers both persistence surfaces.
async fn greet(
    axum::extract::State(ctx): axum::extract::State<PluginCtx>,
    axum::Json(request): axum::Json<Value>,
) -> axum::Json<Value> {
    let greeting = ctx
        .setting_str("greeting")
        .unwrap_or_else(|| "Hello".to_string());
    let count = ctx
        .kv()
        .get("greet_count")
        .await
        .ok()
        .flatten()
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
        + 1;
    ctx.kv().set("greet_count", json!(count)).await.unwrap();
    axum::Json(json!({
        "text": format!("{greeting}, {}!", request["name"].as_str().unwrap_or("")),
        "count": count,
    }))
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
    let (host, tools, _storage, _broadcaster) = start_host(
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
        if matches!(host.status("panicky").await, PluginStatus::Errored { .. }) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "daemon never parked");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Restarted after each panic until the crash limit stopped it.
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    let status = host.status("panicky").await;
    assert!(
        matches!(status, PluginStatus::Errored { ref detail } if !detail.is_empty()),
        "an errored plugin reports why"
    );
    assert!(!host.is_running("panicky").await);
    assert!(
        tools.plugin_tools().names().is_empty(),
        "an errored plugin has no dispatchable tools"
    );
    assert!(
        tools
            .plugin_tools()
            .call("plugin__panicky__echo", json!({}))
            .await
            .is_none(),
        "an errored plugin tool is absent from dispatch"
    );
}

#[tokio::test]
async fn concurrent_double_enable_starts_one_supervisor_and_disable_aborts_it() {
    let dir = tempfile::tempdir().unwrap();
    let starts = Arc::new(AtomicU32::new(0));
    let active = Arc::new(AtomicU32::new(0));
    let (tools, storage, broadcaster) = test_tools(dir.path()).await;
    storage.set_plugin_enabled("blocking", false).await.unwrap();
    let host = PluginHost::start(
        vec![PluginRegistration::new(
            Box::new(BlockingPlugin {
                starts: Arc::clone(&starts),
                active: Arc::clone(&active),
            }),
            "1.0.0",
            "plugins/blocking",
        )],
        storage,
        tools,
        broadcaster,
        BroadcastLog::default(),
        test_supervisor_config(),
    )
    .await
    .unwrap();

    let (first, second) = tokio::join!(
        host.set_enabled("blocking", true),
        host.set_enabled("blocking", true)
    );
    assert_ne!(
        first.unwrap(),
        second.unwrap(),
        "exactly one enable changes state"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while active.load(Ordering::SeqCst) != 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "the supervisor did not start exactly one daemon"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(host.status("blocking").await, PluginStatus::Running);

    assert!(host.set_enabled("blocking", false).await.unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while active.load(Ordering::SeqCst) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "disabling did not abort the supervised daemon"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1, "no daemon leaked");
    assert_eq!(host.status("blocking").await, PluginStatus::Disabled);
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
    assert!(host.is_running("quiet").await);
    assert_eq!(tools.plugin_tools().names().len(), 1);

    assert!(host.set_enabled("quiet", false).await.unwrap());
    assert!(
        !host.set_enabled("quiet", false).await.unwrap(),
        "idempotent"
    );
    assert!(tools.plugin_tools().names().is_empty());
    assert_eq!(host.status("quiet").await, PluginStatus::Disabled);
    assert_eq!(
        storage.plugin_enabled_flags().await.unwrap().get("quiet"),
        Some(&false)
    );

    assert!(host.set_enabled("quiet", true).await.unwrap());
    assert_eq!(tools.plugin_tools().names(), vec!["plugin__quiet__echo"]);
    assert!(host.is_running("quiet").await);
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

/// No plugins are installed in this repository, so the generated registry is
/// empty: the host must still boot, expose no plugin surface, and serve the
/// management API with an empty list.
#[tokio::test]
async fn an_empty_generated_registry_boots_a_clean_host() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();

    assert!(state.plugins.tool_names().is_empty());
    let skills = state.plugins.skills_prompt();
    assert!(
        !skills.contains("## Plugin skill:"),
        "no installed plugin can contribute skills: {skills:?}"
    );

    let response = crate::router_from_state(state)
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
    assert_eq!(body["plugins"], json!([]));
}

#[tokio::test]
async fn plugin_list_reads_the_cached_settings_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    state.plugins = PluginHost::start(
        vec![PluginRegistration::new(
            Box::new(QuietPlugin { id: "quiet" }),
            "1.0.0",
            "plugins/quiet",
        )],
        state.storage.clone(),
        state.tools.clone(),
        state.broadcaster.clone(),
        state.broadcast_log.clone(),
        test_supervisor_config(),
    )
    .await
    .unwrap();

    let mut out_of_band = Map::new();
    out_of_band.insert("greeting".to_string(), json!("stale sqlite value"));
    state
        .storage
        .merge_plugin_settings("quiet", &out_of_band)
        .await
        .unwrap();

    let response = crate::router_from_state(state)
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
    assert_eq!(body["plugins"][0]["values"]["greeting"], json!("Hi"));
}

/// An installed plugin, exercised through the real router: its tool
/// dispatches, its route runs behind the owner gate, its KV counter survives
/// across calls, and disabling it takes both surfaces away.
#[tokio::test]
async fn an_installed_plugin_works_end_to_end_in_the_real_host() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    // The generated registry is empty in this repository, so the host under
    // test is restarted over a fixture registration — the same code path
    // `build_state` uses, with a plugin to exercise.
    state.plugins = PluginHost::start(
        vec![PluginRegistration::new(
            Box::new(GreeterPlugin),
            "1.0.0",
            "plugins/greeter",
        )],
        state.storage.clone(),
        state.tools.clone(),
        state.broadcaster.clone(),
        state.broadcast_log.clone(),
        test_supervisor_config(),
    )
    .await
    .unwrap();

    // Registered from the aggregator, enabled on first sight.
    assert!(state.plugins.is_running("greeter").await);
    assert_eq!(state.plugins.tool_names(), vec!["plugin__greeter__ping"]);

    // Tool dispatch through the shared plugin tool table.
    let result = state
        .tools
        .plugin_tools()
        .call("plugin__greeter__ping", json!({ "message": "hi" }))
        .await
        .expect("greeter registers plugin__greeter__ping")
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
                    .uri("/api/plugins/greeter/greet")
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
    let entry = &body["plugins"][0];
    assert_eq!(entry["id"], json!("greeter"));
    assert_eq!(entry["version"], json!("1.0.0"));
    assert_eq!(entry["state"], json!("running"));
    assert_eq!(entry["values"]["greeting"], json!("Hello"));
    assert_eq!(entry["settings"][0]["kind"], json!("string"));

    // Settings save reaches the plugin, and the route reflects it.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/plugins/greeter/settings")
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
                .uri("/api/plugins/greeter/greet")
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
                .uri("/api/plugins/greeter/enabled")
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
                .uri("/api/plugins/greeter/greet")
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
