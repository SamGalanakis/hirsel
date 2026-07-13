use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use hirsel_client_core::{
    Client, ClientConfig, ClientObserver, ClientSnapshot, ConnectionState, LifecycleEvent,
    ReconnectPolicy, SendMessageRequest, generate_iroh_identity,
};
use hirsel_host::{
    build_state,
    config::{AgentMode, Config, DriverMode, ProviderMode},
    iroh::IrohServer,
    router_from_state,
};
use hirsel_proto::{ChatAuthor, PingStatus};
use tokio::net::TcpListener;

const TOKEN: &str = "iroh-proof-token";
const ROUND_TRIP_BODY: &str = "iroh client-core round-trip";

#[tokio::test]
// Requires the public n0 relay: cargo test -p hirsel-host --test iroh_client_flow -- --ignored
#[ignore = "requires public n0 relay network access"]
async fn persisted_identity_reconnects_and_rejects_invalid_reuse_or_identity() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_host_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "iroh proof anchor", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "iroh-proof",
            "Iroh proof",
            "Visible over the shared protocol",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    let server = IrohServer::start(state.clone(), dir.path()).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = listener.local_addr().unwrap();
    let http_state = state.clone();
    let http_task = tokio::spawn(async move {
        axum::serve(listener, router_from_state(http_state))
            .await
            .unwrap();
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    let http = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let pair: serde_json::Value = http
        .post(format!("http://{http_addr}/debug/pair"))
        .json(&serde_json::json!({ "device_label": "Owner phone" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let pairing_code = pair["code"].as_str().unwrap().to_owned();
    assert_eq!(pair["ticket"], server.ticket());

    let persisted_identity = generate_iroh_identity();
    let mut client_config = ClientConfig::new_iroh_pairing(
        server.ticket().to_owned(),
        pairing_code.clone(),
        "Owner phone".to_owned(),
        persisted_identity.clone(),
    );
    client_config.reconnect = ReconnectPolicy {
        initial_delay_ms: 50,
        max_delay_ms: 100,
        jitter_ratio: 0.0,
    };
    let client = Client::new(client_config).unwrap();
    client.connect().await.unwrap();

    let online = wait_for_snapshot(&client, |snapshot| {
        snapshot.connection == ConnectionState::Online
    })
    .await;
    assert!(
        online
            .pings
            .iter()
            .any(|item| { item.id == ping.id && item.status == PingStatus::Open })
    );
    let device_token = client
        .paired_device_token()
        .expect("pairing did not surface the issued device token");
    assert!(device_token.len() >= 32);

    let devices: serde_json::Value = http
        .get(format!("http://{http_addr}/debug/devices"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices["devices"][0]["label"], "Owner phone");
    assert_eq!(
        devices["devices"][0]["node_id_prefix"]
            .as_str()
            .unwrap()
            .len(),
        12
    );
    assert!(!devices.to_string().contains(&device_token));

    client.disconnect().await;
    drop(client);

    let reused = pairing_client(
        server.ticket(),
        pairing_code,
        "Owner phone",
        test_reconnect_policy(),
    );
    assert_protocol_error(&reused, "invalid pairing code").await;
    reused.disconnect().await;

    let reconnected_client = device_client(
        server.ticket(),
        device_token.clone(),
        persisted_identity,
        test_reconnect_policy(),
    );
    reconnected_client.connect().await.unwrap();
    let reconnected = wait_for_snapshot(&reconnected_client, |snapshot| {
        snapshot.connection == ConnectionState::Online
    })
    .await;
    assert!(
        reconnected
            .messages
            .iter()
            .any(|message| message.body == "iroh proof anchor")
    );
    assert!(reconnected.pings.iter().any(|item| item.id == ping.id));

    reconnected_client.send_message(SendMessageRequest::new(ROUND_TRIP_BODY.to_owned()));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if state
                .storage
                .all_chat()
                .await
                .unwrap()
                .iter()
                .any(|message| {
                    message.author == ChatAuthor::Owner && message.body == ROUND_TRIP_BODY
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("host did not persist the iroh owner message");

    let round_tripped = wait_for_snapshot(&reconnected_client, |snapshot| {
        snapshot.messages.iter().any(|message| {
            message.author == ChatAuthor::Owner
                && message.body == ROUND_TRIP_BODY
                && message.id.is_some()
                && !message.pending
        })
    })
    .await;
    assert!(round_tripped.pings.iter().any(|item| item.id == ping.id));
    reconnected_client.disconnect().await;

    let mismatch = device_client(
        server.ticket(),
        device_token.clone(),
        generate_iroh_identity(),
        test_reconnect_policy(),
    );
    assert_protocol_error(&mismatch, "invalid device token").await;
    mismatch.disconnect().await;

    let revoked: serde_json::Value = http
        .post(format!("http://{http_addr}/debug/revoke-device"))
        .json(&serde_json::json!({ "label": "Owner phone" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(revoked["revoked"], 1);
    assert_protocol_error(&reconnected_client, "invalid device token").await;
    reconnected_client.disconnect().await;

    let expired_code = state
        .storage
        .mint_pairing_code("Expired phone", Duration::ZERO)
        .await
        .unwrap();
    let expired = pairing_client(
        server.ticket(),
        expired_code,
        "Expired phone",
        test_reconnect_policy(),
    );
    assert_protocol_error(&expired, "invalid pairing code").await;
    expired.disconnect().await;

    http_task.abort();
    server.shutdown().await;
}

fn pairing_client(ticket: &str, code: String, label: &str, reconnect: ReconnectPolicy) -> Client {
    let mut config = ClientConfig::new_iroh_pairing(
        ticket.to_owned(),
        code,
        label.to_owned(),
        generate_iroh_identity(),
    );
    config.reconnect = reconnect;
    Client::new(config).unwrap()
}

fn device_client(
    ticket: &str,
    token: String,
    iroh_identity: String,
    reconnect: ReconnectPolicy,
) -> Client {
    let mut config = ClientConfig::new_iroh(ticket.to_owned(), token, iroh_identity);
    config.reconnect = reconnect;
    Client::new(config).unwrap()
}

async fn assert_protocol_error(client: &Client, expected: &str) {
    let observer = Arc::new(RecordingObserver::default());
    client.set_observer(Some(observer.clone()));
    client.connect().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if observer
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, LifecycleEvent::ProtocolError { detail } if detail == expected))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "client did not report protocol error: {expected}; events: {:?}",
            observer.events.lock().unwrap()
        )
    });
}

#[derive(Default)]
struct RecordingObserver {
    events: Mutex<Vec<LifecycleEvent>>,
}

impl ClientObserver for RecordingObserver {
    fn on_state_changed(&self, _snapshot: ClientSnapshot) {}

    fn on_lifecycle_event(&self, event: LifecycleEvent) {
        self.events.lock().unwrap().push(event);
    }
}

async fn wait_for_snapshot(
    client: &Client,
    predicate: impl Fn(&ClientSnapshot) -> bool,
) -> ClientSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = client.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client state did not converge")
}

fn test_host_config(data_dir: &std::path::Path) -> Config {
    Config {
        token: TOKEN.to_owned(),
        agent: AgentMode::Scripted,
        provider: ProviderMode::Anthropic,
        anthropic_api_key: None,
        model: "claude-opus-4-7".to_owned(),
        data_dir: data_dir.to_owned(),
        templates_dir: hirsel_host::templates::bundled_templates_dir(),
        driver: DriverMode::Fake,
        fake_fixture: None,
        listen: "127.0.0.1:0".parse().unwrap(),
        debug: true,
        sidechat_ttl_secs: 86_400,
    }
}

fn test_reconnect_policy() -> ReconnectPolicy {
    ReconnectPolicy {
        initial_delay_ms: 50,
        max_delay_ms: 100,
        jitter_ratio: 0.0,
    }
}
