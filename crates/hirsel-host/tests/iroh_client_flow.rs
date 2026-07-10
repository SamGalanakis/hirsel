use std::time::Duration;

use hirsel_client_core::{
    Client, ClientConfig, ClientSnapshot, ConnectionState, ReconnectPolicy, SendMessageRequest,
};
use hirsel_host::{
    build_state,
    config::{AgentMode, Config, DriverMode, ProviderMode},
    iroh::IrohServer,
};
use hirsel_proto::{ChatAuthor, PingStatus};

const TOKEN: &str = "iroh-proof-token";
const ROUND_TRIP_BODY: &str = "iroh client-core round-trip";

#[tokio::test]
async fn client_core_round_trips_chat_over_iroh() {
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
    println!("host NodeId: {}", server.endpoint_id());

    let mut client_config = ClientConfig::new_iroh(server.ticket().to_owned(), TOKEN.to_owned());
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
    println!("connection established over iroh");

    client.send_message(SendMessageRequest::new(ROUND_TRIP_BODY.to_owned()));
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

    let round_tripped = wait_for_snapshot(&client, |snapshot| {
        snapshot.messages.iter().any(|message| {
            message.author == ChatAuthor::Owner
                && message.body == ROUND_TRIP_BODY
                && message.id.is_some()
                && !message.pending
        })
    })
    .await;
    assert!(round_tripped.pings.iter().any(|item| item.id == ping.id));
    println!("message round-trip over iroh: {ROUND_TRIP_BODY}");

    client.disconnect().await;
    server.shutdown().await;
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
        driver: DriverMode::Fake,
        fake_fixture: None,
        listen: "127.0.0.1:0".parse().unwrap(),
        debug: true,
        sidechat_ttl_secs: 86_400,
    }
}
