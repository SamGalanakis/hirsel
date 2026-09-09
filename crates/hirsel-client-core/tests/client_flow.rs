use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use futures_util::{SinkExt, StreamExt};
use hirsel_client_core::{
    AgentActivityState, ChatAuthor, ChatMessage, Client, ClientConfig, ClientObserver,
    ClientSnapshot, ConnectionState, LifecycleEvent, ProcessInfo, ProcessKind, ProcessState,
    ReconnectPolicy, SendMessageRequest, Thread, ThreadAttention,
};
use hirsel_proto::{ClientToHost, HelloAuth, HostToClient};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

type ServerSocket = WebSocketStream<TcpStream>;

fn chat(id: u64, author: ChatAuthor, body: &str) -> ChatMessage {
    ChatMessage {
        artifact_ids: Vec::new(),
        thread_id: 0,
        client_id: None,
        mentions: vec![],
        id,
        author,
        body: body.into(),
        r#ref: None,
        ts: Utc.timestamp_opt(id as i64, 0).unwrap(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
    }
}

fn thread(id: u64, read: bool, settled: bool) -> Thread {
    Thread {
        id,
        title: format!("thread-{id}"),
        description: "Work".into(),
        instrument: serde_json::json!({"type":"card","children":[]}),
        attention: ThreadAttention::Quiet,
        settled_at: settled.then(Utc::now),
        archived_at: None,
        snoozed_until: None,
        read,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        revision: if read { 2 } else { 1 },
        running_turn: None,
        queued_turn_count: 0,
        last_finished_turn: None,
        last_activity_at: Utc::now(),
    }
}

fn process(id: &str, state: ProcessState) -> ProcessInfo {
    ProcessInfo {
        id: id.into(),
        kind: ProcessKind::Subagent,
        label: "Research".into(),
        agent: Some("worker".into()),
        model: Some("test".into()),
        state,
        started_ts: Utc.timestamp_opt(1, 0).unwrap(),
        last_event_ts: Utc.timestamp_opt(2, 0).unwrap(),
        summary: None,
    }
}

fn hello_ok(
    latest_msg_id: u64,
    messages: Vec<ChatMessage>,
    threads: Vec<Thread>,
    processes: Vec<ProcessInfo>,
) -> HostToClient {
    HostToClient::HelloOk {
        latest_msg_id,
        messages,
        events: vec![],
        threads,
        processes,
        side_chats: Vec::new(),
        host_version: "0.1.0 (test)".to_string(),
        model: None,
        subagent_models: None,
        prompts: None,
        providers: None,
        views: Vec::new(),
    }
}

async fn receive_client(socket: &mut ServerSocket) -> ClientToHost {
    let frame = timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("client frame timed out")
        .expect("client socket ended")
        .expect("client frame failed");
    let Message::Text(text) = frame else {
        panic!("expected text frame");
    };
    serde_json::from_str(&text).expect("invalid client JSON")
}

async fn send_server(socket: &mut ServerSocket, message: &HostToClient) {
    socket
        .send(Message::Text(serde_json::to_string(message).unwrap()))
        .await
        .unwrap();
}

async fn wait_for_snapshot(
    client: &Client,
    predicate: impl Fn(&ClientSnapshot) -> bool,
) -> ClientSnapshot {
    timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = client.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("client state did not converge")
}

fn test_config(address: std::net::SocketAddr) -> ClientConfig {
    ClientConfig {
        host: address.to_string(),
        iroh_ticket: None,
        iroh_secret_key: None,
        auth: HelloAuth::StaticToken("secret".into()),
        reconnect: ReconnectPolicy {
            initial_delay_ms: 150,
            max_delay_ms: 150,
            jitter_ratio: 0.0,
        },
    }
}

#[derive(Default)]
struct RecordingObserver {
    snapshots: Mutex<Vec<ClientSnapshot>>,
    lifecycle: Mutex<Vec<LifecycleEvent>>,
}

impl ClientObserver for RecordingObserver {
    fn on_state_changed(&self, snapshot: ClientSnapshot) {
        self.snapshots.lock().unwrap().push(snapshot);
    }

    fn on_lifecycle_event(&self, event: LifecycleEvent) {
        self.lifecycle.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn connect_loads_state_and_observer_sees_online() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        assert_eq!(
            receive_client(&mut socket).await,
            ClientToHost::Hello {
                auth: HelloAuth::StaticToken("secret".into()),
                last_seen_msg_id: None,
            }
        );
        send_server(
            &mut socket,
            &hello_ok(
                2,
                vec![
                    chat(1, ChatAuthor::Owner, "hello"),
                    chat(2, ChatAuthor::Agent, "hi"),
                ],
                vec![thread(3, false, false)],
                vec![process("process-1", ProcessState::Running)],
            ),
        )
        .await;
        let _ = release_rx.await;
    });

    let client = Client::new(test_config(address)).unwrap();
    let observer = Arc::new(RecordingObserver::default());
    client.set_observer(Some(observer.clone()));
    client.connect().await.unwrap();

    let snapshot =
        wait_for_snapshot(&client, |state| state.connection == ConnectionState::Online).await;
    assert_eq!(snapshot.messages.len(), 2);
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.processes.len(), 1);
    assert_eq!(snapshot.last_seen_msg_id, Some(2));
    assert!(
        observer
            .lifecycle
            .lock()
            .unwrap()
            .contains(&LifecycleEvent::Online)
    );
    assert!(
        observer
            .snapshots
            .lock()
            .unwrap()
            .iter()
            .any(|state| state.connection == ConnectionState::Online)
    );

    let _ = release_tx.send(());
    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn thread_and_process_upserts_replace_existing_rows() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (push_tx, push_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let _hello = receive_client(&mut socket).await;
        send_server(
            &mut socket,
            &hello_ok(
                0,
                vec![],
                vec![thread(9, false, false)],
                vec![process("p", ProcessState::Running)],
            ),
        )
        .await;
        let _ = push_rx.await;
        send_server(
            &mut socket,
            &HostToClient::ThreadUpsert {
                thread: thread(9, true, true),
            },
        )
        .await;
        send_server(
            &mut socket,
            &HostToClient::ProcessUpsert {
                process: process("p", ProcessState::Done),
            },
        )
        .await;
        send_server(
            &mut socket,
            &HostToClient::AgentActivity {
                thread_id: Some(9),
                turn_id: Some(1),
                state: AgentActivityState::Thinking,
                text: Some("working".into()),
                sc: None,
            },
        )
        .await;
        sleep(Duration::from_millis(100)).await;
    });

    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |state| state.connection == ConnectionState::Online).await;
    push_tx.send(()).unwrap();
    let snapshot = wait_for_snapshot(&client, |state| {
        state.threads.first().is_some_and(|item| item.read)
            && state
                .processes
                .first()
                .is_some_and(|item| item.state == ProcessState::Done)
            && state
                .streams
                .first()
                .is_some_and(|s| s.activity.state == AgentActivityState::Thinking)
    })
    .await;
    assert_eq!(snapshot.threads.len(), 1);
    assert!(snapshot.threads[0].settled_at.is_some());
    assert_eq!(snapshot.processes.len(), 1);

    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn optimistic_send_reconciles_with_owner_echo() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let _hello = receive_client(&mut socket).await;
        send_server(&mut socket, &hello_ok(0, vec![], vec![], vec![])).await;
        let sent = receive_client(&mut socket).await;
        let ClientToHost::SendThreadMessage {
            body,
            client_id,
            thread_id,
            ..
        } = sent
        else {
            panic!("expected send_message");
        };
        send_server(
            &mut socket,
            &HostToClient::Msg {
                message: ChatMessage {
                    artifact_ids: Vec::new(),
                    client_id: Some(client_id),
                    thread_id,
                    ..chat(42, ChatAuthor::Owner, &body)
                },
                sc: None,
            },
        )
        .await;
        sleep(Duration::from_millis(100)).await;
    });

    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |state| state.connection == ConnectionState::Online).await;
    let receipt = client.send_message(SendMessageRequest::new("queued thought".into()));
    let optimistic = client.snapshot();
    assert_eq!(optimistic.messages.len(), 1);
    assert!(optimistic.messages[0].is_pending());
    assert_eq!(
        optimistic.messages[0].client_id(),
        Some(receipt.client_id.as_str())
    );

    let reconciled = wait_for_snapshot(&client, |state| {
        state
            .messages
            .first()
            .is_some_and(|message| message.id() == Some(42))
    })
    .await;
    assert_eq!(reconciled.messages.len(), 1);
    assert!(!reconciled.messages[0].is_pending());
    assert_eq!(
        reconciled.messages[0].client_id(),
        Some(receipt.client_id.as_str())
    );
    assert_eq!(reconciled.last_seen_msg_id, Some(42));

    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn offline_queue_flushes_in_order_and_reconnect_resumes_last_seen() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (result_tx, result_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first = accept_async(first_stream).await.unwrap();
        assert_eq!(
            receive_client(&mut first).await,
            ClientToHost::Hello {
                auth: HelloAuth::StaticToken("secret".into()),
                last_seen_msg_id: None,
            }
        );
        send_server(
            &mut first,
            &hello_ok(
                7,
                vec![chat(7, ChatAuthor::Agent, "checkpoint")],
                vec![],
                vec![],
            ),
        )
        .await;
        first.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second = accept_async(second_stream).await.unwrap();
        let resume = receive_client(&mut second).await;
        send_server(&mut second, &hello_ok(7, vec![], vec![], vec![])).await;
        let first_send = receive_client(&mut second).await;
        let second_send = receive_client(&mut second).await;
        result_tx.send((resume, first_send, second_send)).unwrap();
    });

    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |state| {
        state.connection == ConnectionState::Online && state.last_seen_msg_id == Some(7)
    })
    .await;
    wait_for_snapshot(&client, |state| {
        state.connection == ConnectionState::Offline
    })
    .await;
    let first_receipt = client.send_message(SendMessageRequest::new("first offline".into()));
    let second_receipt = client.send_message(SendMessageRequest::new("second offline".into()));
    assert_ne!(first_receipt.client_id, second_receipt.client_id);
    assert!(
        client
            .snapshot()
            .messages
            .iter()
            .rev()
            .take(2)
            .all(|row| row.is_pending())
    );

    let (resume, first_send, second_send) = timeout(Duration::from_secs(5), result_rx)
        .await
        .expect("reconnect did not flush")
        .unwrap();
    assert_eq!(
        resume,
        ClientToHost::Hello {
            auth: HelloAuth::StaticToken("secret".into()),
            last_seen_msg_id: Some(7),
        }
    );
    let ClientToHost::SendThreadMessage {
        client_id, body, ..
    } = first_send
    else {
        panic!("expected first queued send");
    };
    assert_eq!(client_id, first_receipt.client_id);
    assert_eq!(body, "first offline");
    let ClientToHost::SendThreadMessage {
        client_id, body, ..
    } = second_send
    else {
        panic!("expected second queued send");
    };
    assert_eq!(client_id, second_receipt.client_id);
    assert_eq!(body, "second offline");

    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn push_token_registration_queues_until_the_client_is_online() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let _hello = receive_client(&mut socket).await;
        send_server(&mut socket, &hello_ok(0, vec![], vec![], vec![])).await;
        receive_client(&mut socket).await
    });

    let client = Client::new(test_config(address)).unwrap();
    client
        .register_push_token("android".into(), "fcm-token".into())
        .unwrap();
    client.connect().await.unwrap();

    assert_eq!(
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap(),
        ClientToHost::RegisterPushToken {
            platform: hirsel_proto::PushPlatform::Android,
            token: "fcm-token".into(),
        }
    );
    client.disconnect().await;
}

#[test]
fn push_token_registration_validates_input() {
    let client = Client::new(ClientConfig::new("localhost:3090".into(), "secret".into())).unwrap();

    assert!(
        client
            .register_push_token("desktop".into(), "token".into())
            .is_err()
    );
    assert!(
        client
            .register_push_token("android".into(), "  ".into())
            .is_err()
    );
}

#[tokio::test]
async fn native_thread_commands_roundtrip_revision_and_ownership() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        receive_client(&mut socket).await;
        send_server(&mut socket, &hello_ok(0, vec![], vec![], vec![])).await;
        let ClientToHost::CreateThread { client_id, title } = receive_client(&mut socket).await
        else {
            panic!("create");
        };
        assert_eq!(title, "Groceries");
        send_server(
            &mut socket,
            &HostToClient::ThreadCreated {
                client_id,
                thread: thread(5, false, false),
            },
        )
        .await;
        let ClientToHost::OpenThread {
            client_id,
            thread_id,
            before_id,
        } = receive_client(&mut socket).await
        else {
            panic!("open");
        };
        assert_eq!(thread_id, 5);
        assert_eq!(before_id, None);
        send_server(
            &mut socket,
            &HostToClient::ThreadOpened {
                client_id,
                detail: hirsel_proto::ThreadDetail {
                    thread: thread(5, false, false),
                    messages: vec![ChatMessage {
                        artifact_ids: Vec::new(),
                        thread_id: 5,
                        ..chat(1, ChatAuthor::Agent, "Milk")
                    }],
                    turns: vec![],
                    activities: vec![],
                    has_more: false,
                },
            },
        )
        .await;
        let action = receive_client(&mut socket).await;
        assert_eq!(
            action,
            ClientToHost::ThreadAction {
                thread_id: 5,
                action: "choose".into(),
                data: serde_json::json!({"choice":"milk","label":"Milk"}),
                expected_revision: Some(1)
            }
        );
        assert_eq!(
            receive_client(&mut socket).await,
            ClientToHost::ThreadAction {
                thread_id: 5,
                action: "read".into(),
                data: serde_json::json!({}),
                expected_revision: None
            }
        );
        assert_eq!(
            receive_client(&mut socket).await,
            ClientToHost::CancelTurn {
                thread_id: Some(5),
                sc: None
            }
        );
        ready_tx.send(()).unwrap();
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    let created = client.create_thread("Groceries".into());
    let snapshot = wait_for_snapshot(&client, |s| {
        s.created_threads
            .iter()
            .any(|t| t.client_id == created.client_id)
    })
    .await;
    assert_eq!(snapshot.threads[0].id, 5);
    client.open_thread(5, None);
    wait_for_snapshot(&client, |s| s.opened_threads.contains(&5)).await;
    client.thread_action(
        5,
        "choose".into(),
        serde_json::json!({"choice":"milk","label":"Milk"}),
        Some(1),
    );
    client.thread_action(5, "read".into(), serde_json::json!({}), None);
    client.cancel_turn(5);
    timeout(Duration::from_secs(3), ready_rx)
        .await
        .unwrap()
        .unwrap();
    let snapshot = client.snapshot();
    assert!(snapshot.threads[0].settled_at.is_none());
    assert!(
        matches!(&snapshot.messages[0],hirsel_client_core::ChatEntry::Confirmed(m) if m.thread_id==5)
    );
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn lost_create_ack_retries_same_identity_after_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut first = accept_async(stream).await.unwrap();
        receive_client(&mut first).await;
        send_server(&mut first, &hello_ok(0, vec![], vec![], vec![])).await;
        let create = receive_client(&mut first).await;
        first.close(None).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut second = accept_async(stream).await.unwrap();
        receive_client(&mut second).await;
        send_server(&mut second, &hello_ok(0, vec![], vec![], vec![])).await;
        let retry = receive_client(&mut second).await;
        assert_eq!(create, retry);
        let ClientToHost::CreateThread { client_id, .. } = retry else {
            panic!("create retry")
        };
        send_server(
            &mut second,
            &HostToClient::ThreadCreated {
                client_id,
                thread: thread(5, false, false),
            },
        )
        .await;
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    let receipt = client.create_thread("Groceries".into());
    let snapshot = wait_for_snapshot(&client, |s| {
        s.created_threads
            .iter()
            .any(|c| c.client_id == receipt.client_id)
    })
    .await;
    assert_eq!(snapshot.threads.len(), 1);
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}
