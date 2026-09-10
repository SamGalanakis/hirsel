use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use futures_util::{SinkExt, StreamExt};
use hirsel_client_core::{
    AgentActivityState, ChatAuthor, ChatMessage, Client, ClientConfig, ClientObserver,
    ClientSnapshot, ConnectionState, LifecycleEvent, ProcessInfo, ProcessKind, ProcessState,
    ReconnectPolicy, SendThreadMessageRequest, Thread, ThreadAttention,
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
        icon: None,
        showcased_artifact_id: None,
        parent_thread_id: None,
        pinned_at: None,
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
        thread_id: 5,
        id: id.into(),
        kind: ProcessKind::Monitor,
        label: "Research".into(),
        agent: Some("worker".into()),
        model: Some("test".into()),
        state,
        started_ts: Utc.timestamp_opt(1, 0).unwrap(),
        last_event_ts: Utc.timestamp_opt(2, 0).unwrap(),
        summary: None,
    }
}

async fn send_hello(
    socket: &mut ServerSocket,
    messages: Vec<ChatMessage>,
    threads: Vec<Thread>,
    processes: Vec<ProcessInfo>,
) {
    send_server(
        socket,
        &HostToClient::HelloOk {
            history_id: "test-store-a".into(),
            threads,
            processes,
            host_version: "0.1.0 (test)".into(),
            model: None,
            subagent_models: None,
            prompts: None,
            providers: None,
            views: vec![],
        },
    )
    .await;
    for message in messages {
        send_server(socket, &HostToClient::Msg { message }).await;
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
            }
        );
        send_hello(
            &mut socket,
            vec![
                chat(1, ChatAuthor::Owner, "hello"),
                chat(2, ChatAuthor::Agent, "hi"),
            ],
            vec![thread(3, false, false)],
            vec![process("process-1", ProcessState::Running)],
        )
        .await;
        let _ = release_rx.await;
    });

    let client = Client::new(test_config(address)).unwrap();
    let observer = Arc::new(RecordingObserver::default());
    client.set_observer(Some(observer.clone()));
    client.connect().await.unwrap();

    let snapshot = wait_for_snapshot(&client, |state| {
        state.connection == ConnectionState::Online && state.messages.len() == 2
    })
    .await;
    assert_eq!(snapshot.messages.len(), 2);
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.processes.len(), 1);
    assert_eq!(snapshot.history_id.as_deref(), Some("test-store-a"));
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
        send_hello(
            &mut socket,
            vec![],
            vec![thread(9, false, false)],
            vec![process("p", ProcessState::Running)],
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
                thread_id: 9,
                turn_id: 1,
                state: AgentActivityState::Thinking,
                text: Some("working".into()),
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
        send_hello(&mut socket, vec![], vec![], vec![]).await;
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
            },
        )
        .await;
        sleep(Duration::from_millis(100)).await;
    });

    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |state| state.connection == ConnectionState::Online).await;
    let receipt = client
        .send_message(SendThreadMessageRequest::new(
            "test-store-a".into(),
            0,
            "queued thought".into(),
        ))
        .unwrap();
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
    assert!(reconciled.messages.iter().any(|m| m.id() == Some(42)));

    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn offline_queue_flushes_in_order_on_same_store_reconnect() {
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
            }
        );
        send_hello(
            &mut first,
            vec![chat(7, ChatAuthor::Agent, "checkpoint")],
            vec![],
            vec![],
        )
        .await;
        first.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second = accept_async(second_stream).await.unwrap();
        let resume = receive_client(&mut second).await;
        send_hello(&mut second, vec![], vec![], vec![]).await;
        let first_send = receive_client(&mut second).await;
        let second_send = receive_client(&mut second).await;
        result_tx.send((resume, first_send, second_send)).unwrap();
    });

    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |state| {
        state.messages.iter().any(|m| m.id() == Some(7))
    })
    .await;
    wait_for_snapshot(&client, |state| {
        state.connection == ConnectionState::Offline
    })
    .await;
    let first_receipt = client
        .send_message(SendThreadMessageRequest::new(
            "test-store-a".into(),
            0,
            "first offline".into(),
        ))
        .unwrap();
    let second_receipt = client
        .send_message(SendThreadMessageRequest::new(
            "test-store-a".into(),
            0,
            "second offline".into(),
        ))
        .unwrap();
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
        send_hello(&mut socket, vec![], vec![], vec![]).await;
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
        send_hello(&mut socket, vec![], vec![], vec![]).await;
        let ClientToHost::CreateThread {
            client_id,
            history_id,
            title,
            parent_thread_id,
        } = receive_client(&mut socket).await
        else {
            panic!("create");
        };
        assert_eq!(title, "Groceries");
        assert_eq!(history_id, "test-store-a");
        assert_eq!(parent_thread_id, None);
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
                    related_items: vec![],
                    brief: hirsel_proto::ThreadBrief {
                        text: "Current assignment".into(),
                        artifact_ids: vec![44],
                    },
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
        let first_action_id = match action {
            ClientToHost::ThreadAction {
                client_id,
                history_id,
                thread_id: 5,
                action,
                data,
                expected_revision: Some(1),
            } if history_id == "test-store-a"
                && action == "choose"
                && data == serde_json::json!({"choice":"milk","label":"Milk"}) =>
            {
                client_id
            }
            other => panic!("unexpected first action: {other:?}"),
        };
        send_server(
            &mut socket,
            &HostToClient::ThreadActionApplied {
                client_id: first_action_id.clone(),
                history_id: "test-store-a".into(),
                thread_id: 5,
            },
        )
        .await;
        let second_action_id = match receive_client(&mut socket).await {
            ClientToHost::ThreadAction {
                client_id,
                history_id,
                thread_id: 5,
                action,
                data,
                expected_revision: None,
            } if history_id == "test-store-a"
                && action == "read"
                && data == serde_json::json!({}) =>
            {
                client_id
            }
            other => panic!("unexpected second action: {other:?}"),
        };
        send_server(
            &mut socket,
            &HostToClient::Error {
                detail: "Read rejected".into(),
                client_id: Some(second_action_id.clone()),
            },
        )
        .await;
        assert_eq!(
            receive_client(&mut socket).await,
            ClientToHost::CancelTurn {
                history_id: "test-store-a".into(),
                thread_id: 5,
            }
        );
        ready_tx.send((first_action_id, second_action_id)).unwrap();
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    let observer = Arc::new(RecordingObserver::default());
    client.set_observer(Some(observer.clone()));
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    let created = client
        .create_thread("test-store-a".into(), "Groceries".into(), None)
        .unwrap();
    let snapshot = wait_for_snapshot(&client, |s| {
        s.created_threads
            .iter()
            .any(|t| t.client_id == created.client_id)
    })
    .await;
    assert_eq!(snapshot.threads[0].id, 5);
    let opened = client.open_thread(5, None);
    wait_for_snapshot(&client, |s| s.opened_threads.contains(&5)).await;
    let first_action = client
        .thread_action(
            "test-store-a".into(),
            5,
            "choose".into(),
            serde_json::json!({"choice":"milk","label":"Milk"}),
            Some(1),
        )
        .unwrap();
    let second_action = client
        .thread_action(
            "test-store-a".into(),
            5,
            "read".into(),
            serde_json::json!({}),
            None,
        )
        .unwrap();
    client.cancel_turn("test-store-a".into(), 5);
    let (first_action_id, second_action_id) = timeout(Duration::from_secs(3), ready_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_action.client_id, first_action_id);
    assert_eq!(second_action.client_id, second_action_id);
    timeout(Duration::from_secs(3), async {
        loop {
            let events = observer.lifecycle.lock().unwrap().clone();
            let applied = events.iter().any(|event| {
                matches!(event,
                    LifecycleEvent::ThreadActionApplied { client_id, history_id, thread_id: 5 }
                        if client_id == &first_action.client_id && history_id == "test-store-a"
                )
            });
            let opened_event = events.iter().any(|event| {
                matches!(event,
                    LifecycleEvent::ThreadOpened { client_id, thread_id: 5 }
                        if client_id == &opened.client_id
                )
            });
            let failed = events.iter().any(|event| {
                matches!(event,
                    LifecycleEvent::ProtocolError { client_id: Some(client_id), detail }
                        if client_id == &second_action.client_id && detail == "Read rejected"
                )
            });
            if opened_event && applied && failed {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
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
        send_hello(&mut first, vec![], vec![], vec![]).await;
        let create = receive_client(&mut first).await;
        first.close(None).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut second = accept_async(stream).await.unwrap();
        receive_client(&mut second).await;
        send_hello(&mut second, vec![], vec![], vec![]).await;
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
    let receipt = client
        .create_thread("test-store-a".into(), "Groceries".into(), None)
        .unwrap();
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

#[tokio::test]
async fn lost_open_ack_retries_same_identity_after_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut first = accept_async(stream).await.unwrap();
        receive_client(&mut first).await;
        send_hello(&mut first, vec![], vec![thread(5, false, false)], vec![]).await;
        let ClientToHost::OpenThread {
            client_id: original_id,
            thread_id: 5,
            before_id: None,
        } = receive_client(&mut first).await
        else {
            panic!("initial open")
        };
        first.close(None).await.unwrap();

        let (stream, _) = listener.accept().await.unwrap();
        let mut second = accept_async(stream).await.unwrap();
        receive_client(&mut second).await;
        send_hello(&mut second, vec![], vec![thread(5, false, false)], vec![]).await;
        let ClientToHost::OpenThread {
            client_id: retry_id,
            thread_id: 5,
            before_id: None,
        } = receive_client(&mut second).await
        else {
            panic!("open retry")
        };
        assert_eq!(retry_id, original_id);
        send_server(
            &mut second,
            &HostToClient::ThreadOpened {
                client_id: retry_id,
                detail: hirsel_proto::ThreadDetail {
                    related_items: vec![],
                    brief: hirsel_proto::ThreadBrief {
                        text: "Retried open".into(),
                        artifact_ids: vec![],
                    },
                    thread: thread(5, false, false),
                    messages: vec![],
                    turns: vec![],
                    activities: vec![],
                    has_more: false,
                },
            },
        )
        .await;
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    let observer = Arc::new(RecordingObserver::default());
    client.set_observer(Some(observer.clone()));
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    let receipt = client.open_thread(5, None);
    wait_for_snapshot(&client, |s| {
        s.briefs.iter().any(|brief| brief.text == "Retried open")
    })
    .await;
    assert!(observer.lifecycle.lock().unwrap().iter().any(|event| {
        matches!(event, LifecycleEvent::ThreadOpened { client_id, thread_id: 5 } if client_id == &receipt.client_id)
    }));
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn lost_paginated_open_error_retries_same_identity_without_background_duplicate() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut first = accept_async(stream).await.unwrap();
        receive_client(&mut first).await;
        send_hello(&mut first, vec![], vec![thread(5, false, false)], vec![]).await;
        let ClientToHost::OpenThread {
            client_id,
            thread_id: 5,
            before_id: None,
        } = receive_client(&mut first).await
        else {
            panic!("initial open")
        };
        send_server(
            &mut first,
            &HostToClient::ThreadOpened {
                client_id,
                detail: hirsel_proto::ThreadDetail {
                    related_items: vec![],
                    brief: hirsel_proto::ThreadBrief {
                        text: "Initial detail".into(),
                        artifact_ids: vec![],
                    },
                    thread: thread(5, false, false),
                    messages: vec![],
                    turns: vec![],
                    activities: vec![],
                    has_more: true,
                },
            },
        )
        .await;
        let ClientToHost::OpenThread {
            client_id: pagination_id,
            thread_id: 5,
            before_id: Some(10),
        } = receive_client(&mut first).await
        else {
            panic!("pagination open")
        };
        first.close(None).await.unwrap();

        let (stream, _) = listener.accept().await.unwrap();
        let mut second = accept_async(stream).await.unwrap();
        receive_client(&mut second).await;
        send_hello(&mut second, vec![], vec![thread(5, false, false)], vec![]).await;
        let ClientToHost::OpenThread {
            client_id: retry_id,
            thread_id: 5,
            before_id: None,
        } = receive_client(&mut second).await
        else {
            panic!("pagination retry")
        };
        assert_eq!(retry_id, pagination_id);
        assert!(
            timeout(Duration::from_millis(150), second.next())
                .await
                .is_err(),
            "pending pagination generated a second background reopen"
        );
        send_server(
            &mut second,
            &HostToClient::Error {
                detail: "Pagination rejected".into(),
                client_id: Some(retry_id),
            },
        )
        .await;
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    let observer = Arc::new(RecordingObserver::default());
    client.set_observer(Some(observer.clone()));
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    client.open_thread(5, None);
    wait_for_snapshot(&client, |s| s.opened_threads.contains(&5)).await;
    let receipt = client.open_thread(5, Some(10));
    timeout(Duration::from_secs(5), async {
        loop {
            if observer.lifecycle.lock().unwrap().iter().any(|event| {
                matches!(event, LifecycleEvent::ProtocolError { detail, client_id: Some(client_id) }
                    if detail == "Pagination rejected" && client_id == &receipt.client_id)
            }) {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn new_history_discards_pending_transport_actions_and_recovers_unsent_text() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let (proved_tx, proved_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut first = accept_async(stream).await.unwrap();
        receive_client(&mut first).await;
        send_hello(&mut first, vec![], vec![thread(5, false, false)], vec![]).await;
        let _ = first.next().await;
        let (stream, _) = listener.accept().await.unwrap();
        let mut second = accept_async(stream).await.unwrap();
        receive_client(&mut second).await;
        send_server(
            &mut second,
            &HostToClient::HelloOk {
                history_id: "test-store-b".into(),
                threads: vec![thread(5, false, false)],
                processes: vec![],
                host_version: "test".into(),
                model: None,
                subagent_models: None,
                prompts: None,
                providers: None,
                views: vec![],
            },
        )
        .await;
        assert!(
            timeout(Duration::from_millis(250), second.next())
                .await
                .is_err(),
            "old-store operation replayed into reused Thread ID"
        );
        proved_tx.send(()).unwrap();
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.history_id.as_deref() == Some("test-store-a")).await;
    client.disconnect().await;
    client.send_message(SendThreadMessageRequest::new(
        "test-store-a".into(),
        5,
        "recover this draft".into(),
    ));
    client.create_thread("test-store-a".into(), "old create".into(), None);
    client.open_thread(5, None);
    client.thread_action(
        "test-store-a".into(),
        5,
        "archive".into(),
        serde_json::json!({}),
        Some(1),
    );
    client.cancel_turn("test-store-a".into(), 5);
    client.add_thread_related(
        "test-store-a".into(),
        5,
        hirsel_proto::ThreadRelatedTarget::Url {
            url: "https://example.com".into(),
        },
        None,
    );
    client.remove_thread_related("test-store-a".into(), 5, 7);
    client.connect().await.unwrap();
    let snapshot =
        wait_for_snapshot(&client, |s| s.history_id.as_deref() == Some("test-store-b")).await;
    assert_eq!(snapshot.recovered_drafts, vec!["recover this draft"]);
    assert!(snapshot.messages.is_empty());
    timeout(Duration::from_secs(3), proved_rx)
        .await
        .unwrap()
        .unwrap();
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn live_assignment_refreshes_current_brief_without_user_reopen() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        receive_client(&mut socket).await;
        send_hello(&mut socket, vec![], vec![thread(5, false, false)], vec![]).await;
        for text in ["First assignment", "Changed assignment"] {
            let ClientToHost::OpenThread {
                client_id,
                thread_id,
                before_id,
            } = receive_client(&mut socket).await
            else {
                panic!("detail request")
            };
            assert_eq!((thread_id, before_id), (5, None));
            send_server(
                &mut socket,
                &HostToClient::ThreadOpened {
                    client_id,
                    detail: hirsel_proto::ThreadDetail {
                        related_items: vec![],
                        thread: thread(5, false, false),
                        brief: hirsel_proto::ThreadBrief {
                            text: text.into(),
                            artifact_ids: vec![44],
                        },
                        messages: vec![],
                        turns: vec![],
                        activities: vec![],
                        has_more: false,
                    },
                },
            )
            .await;
            if text == "First assignment" {
                send_server(&mut socket, &HostToClient::ThreadActivity {
                    activity: hirsel_proto::ThreadActivity {
                        id: 8, thread_id: 5, turn_id: None,
                        kind: "delegation_received".into(),
                        data: serde_json::json!({"brief":"Event text is not authoritative detail"}),
                        artifact_ids: vec![], ts: Utc::now(),
                    },
                }).await;
            }
        }
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    client.open_thread(5, None);
    let state = wait_for_snapshot(&client, |s| {
        s.briefs.iter().any(|b| b.text == "Changed assignment")
    })
    .await;
    assert_eq!(state.briefs[0].artifact_ids, vec![44]);
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn saved_link_commands_snapshots_and_correlated_results_cross_native_transport() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel();
    let (ids_tx, ids_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        receive_client(&mut socket).await;
        send_hello(&mut socket, vec![], vec![thread(5, false, false)], vec![]).await;
        let ClientToHost::OpenThread { client_id, .. } = receive_client(&mut socket).await else {
            panic!("open")
        };
        let saved = hirsel_proto::ThreadRelatedItem {
            id: 7,
            thread_id: 5,
            target: hirsel_proto::ThreadRelatedTarget::Url {
                url: "https://example.com/?q=1#part".into(),
            },
            title: Some("Reference".into()),
            created_at: Utc.timestamp_opt(1, 0).unwrap(),
        };
        send_server(
            &mut socket,
            &HostToClient::ThreadOpened {
                client_id,
                detail: hirsel_proto::ThreadDetail {
                    thread: thread(5, false, false),
                    brief: hirsel_proto::ThreadBrief {
                        text: String::new(),
                        artifact_ids: vec![44],
                    },
                    related_items: vec![saved.clone()],
                    messages: vec![],
                    turns: vec![],
                    activities: vec![],
                    has_more: true,
                },
            },
        )
        .await;
        let add = receive_client(&mut socket).await;
        let ClientToHost::AddThreadRelated { ref client_id, .. } = add else {
            panic!("add")
        };
        let add_id = client_id.clone();
        assert_eq!(
            serde_json::to_value(&add).unwrap(),
            serde_json::json!({
                "type":"add_thread_related", "client_id":add_id, "history_id":"test-store-a", "thread_id":5,
                "target":{"kind":"url","url":"https://example.com/?q=1#part"}, "title":null
            })
        );
        let mut updated = thread(5, false, false);
        updated.revision = 2;
        send_server(&mut socket, &HostToClient::ThreadUpsert { thread: updated }).await;
        send_server(
            &mut socket,
            &HostToClient::ThreadRelatedChanged {
                client_id: Some(add_id.clone()),
                history_id: "test-store-a".into(),
                thread_id: 5,
                revision: 2,
                items: vec![saved],
            },
        )
        .await;
        let remove = receive_client(&mut socket).await;
        let ClientToHost::RemoveThreadRelated { ref client_id, .. } = remove else {
            panic!("remove")
        };
        let remove_id = client_id.clone();
        assert_eq!(
            serde_json::to_value(&remove).unwrap(),
            serde_json::json!({
                "type":"remove_thread_related", "client_id":remove_id, "history_id":"test-store-a", "thread_id":5, "item_id":7
            })
        );
        send_server(
            &mut socket,
            &HostToClient::ThreadRelatedChanged {
                client_id: Some(remove_id.clone()),
                history_id: "test-store-a".into(),
                thread_id: 5,
                revision: 3,
                items: vec![],
            },
        )
        .await;
        // A delayed callback after reset retains its original history on the wire.
        send_server(
            &mut socket,
            &HostToClient::HelloOk {
                history_id: "test-store-b".into(),
                threads: vec![thread(5, false, false)],
                processes: vec![],
                host_version: "test".into(),
                model: None,
                subagent_models: None,
                prompts: None,
                providers: None,
                views: vec![],
            },
        )
        .await;
        let delayed = receive_client(&mut socket).await;
        let ClientToHost::RemoveThreadRelated {
            client_id,
            history_id,
            thread_id,
            item_id,
        } = delayed
        else {
            panic!("delayed remove")
        };
        assert_eq!(
            (history_id.as_str(), thread_id, item_id),
            ("test-store-a", 5, 7)
        );
        send_server(
            &mut socket,
            &HostToClient::Error {
                client_id: Some(client_id.clone()),
                detail: "History changed".into(),
            },
        )
        .await;
        ids_tx.send((add_id, remove_id, client_id)).unwrap();
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    let observer = Arc::new(RecordingObserver::default());
    client.set_observer(Some(observer.clone()));
    client.connect().await.unwrap();
    wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    client.open_thread(5, None);
    let opened = wait_for_snapshot(&client, |s| !s.related_items.is_empty()).await;
    assert_eq!(opened.related_items[0].id, 7);
    assert_eq!(opened.briefs[0].artifact_ids, vec![44]);
    let added = client.add_thread_related(
        "test-store-a".into(),
        5,
        hirsel_proto::ThreadRelatedTarget::Url {
            url: "https://example.com/?q=1#part".into(),
        },
        None,
    );
    wait_for_snapshot(&client, |s| s.threads[0].revision == 2).await;
    let removed = client.remove_thread_related("test-store-a".into(), 5, 7);
    let reset =
        wait_for_snapshot(&client, |s| s.history_id.as_deref() == Some("test-store-b")).await;
    assert!(reset.related_items.is_empty());
    let delayed = client.remove_thread_related("test-store-a".into(), 5, 7);
    let ids = timeout(Duration::from_secs(3), ids_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ids,
        (
            added.client_id.clone(),
            removed.client_id.clone(),
            delayed.client_id.clone()
        )
    );
    timeout(Duration::from_secs(3), async {
        loop {
            if observer.lifecycle.lock().unwrap().iter().any(|e| matches!(e,
                LifecycleEvent::ProtocolError { client_id:Some(id), detail } if id == &delayed.client_id && detail == "History changed"
            )) { break; }
            sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    let events = observer.lifecycle.lock().unwrap().clone();
    for id in [added.client_id, removed.client_id] {
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LifecycleEvent::ThreadRelatedChanged {
            client_id:Some(actual), history_id, thread_id:5,
        } if actual == &id && history_id == "test-store-a"))
        );
    }
    assert!(
        observer
            .snapshots
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.history_id.as_deref() == Some("test-store-a")
                && s.opened_threads.contains(&5)
                && s.related_items.is_empty())
    );
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}

#[tokio::test]
async fn typed_thread_target_preserves_both_origin_and_target_identity_on_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        receive_client(&mut socket).await;
        send_hello(&mut socket, vec![], vec![thread(5, false, false)], vec![]).await;
        receive_client(&mut socket).await
    });
    let client = Client::new(test_config(address)).unwrap();
    let receipt = client.add_thread_related(
        "original-origin-history".into(),
        5,
        hirsel_proto::ThreadRelatedTarget::Thread {
            history_id: "original-target-history".into(),
            thread_id: 0,
        },
        None,
    );
    client.connect().await.unwrap();
    let sent = timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(sent).unwrap(),
        serde_json::json!({
            "type":"add_thread_related", "client_id":receipt.client_id,
            "history_id":"original-origin-history", "thread_id":5,
            "target":{"kind":"thread","history_id":"original-target-history","thread_id":0}, "title":null,
        })
    );
    client.disconnect().await;
}

#[tokio::test]
async fn typed_navigation_rechecks_native_history_when_displayed_snapshot_lags_reset() {
    use hirsel_proto::ThreadRelatedTarget;
    let target = || ThreadRelatedTarget::Thread {
        history_id: "test-store-a".into(),
        thread_id: 0,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (reset_tx, reset_rx) = oneshot::channel();
    let (checked_tx, checked_rx) = oneshot::channel();
    let (proof_tx, proof_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        receive_client(&mut socket).await;
        send_hello(&mut socket, vec![], vec![thread(0, false, false)], vec![]).await;
        assert!(matches!(
            receive_client(&mut socket).await,
            ClientToHost::OpenThread { thread_id: 0, .. }
        ));
        reset_rx.await.unwrap();
        send_server(
            &mut socket,
            &HostToClient::HelloOk {
                history_id: "test-store-b".into(),
                threads: vec![thread(0, false, false)],
                processes: vec![],
                host_version: "test".into(),
                model: None,
                subagent_models: None,
                prompts: None,
                providers: None,
                views: vec![],
            },
        )
        .await;
        checked_rx.await.unwrap();
        assert!(
            timeout(Duration::from_millis(150), socket.next())
                .await
                .is_err(),
            "stale typed target queued into reused Thread ID"
        );
        proof_tx.send(()).unwrap();
        let _ = release_rx.await;
    });
    let client = Client::new(test_config(address)).unwrap();
    assert!(client.open_related_thread(target()).is_none());
    client.connect().await.unwrap();
    let displayed = wait_for_snapshot(&client, |s| s.connection == ConnectionState::Online).await;
    assert!(
        client
            .open_related_thread(ThreadRelatedTarget::Thread {
                history_id: "test-store-a".into(),
                thread_id: 99
            })
            .is_none()
    );
    assert!(
        client
            .open_related_thread(ThreadRelatedTarget::Url {
                url: "https://example.com/t/0".into()
            })
            .is_none()
    );
    assert!(client.open_related_thread(target()).is_some());
    reset_tx.send(()).unwrap();
    wait_for_snapshot(&client, |s| s.history_id.as_deref() == Some("test-store-b")).await;
    // Android still holds displayed A/0, but the FFI call preserves that tuple.
    assert_eq!(displayed.history_id.as_deref(), Some("test-store-a"));
    assert!(client.open_related_thread(target()).is_none());
    checked_tx.send(()).unwrap();
    timeout(Duration::from_secs(3), proof_rx)
        .await
        .unwrap()
        .unwrap();
    release_tx.send(()).unwrap();
    client.disconnect().await;
    server.await.unwrap();
}
