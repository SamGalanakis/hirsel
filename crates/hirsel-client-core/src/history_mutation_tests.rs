use super::*;
use crate::ConnectionState;

fn thread() -> hirsel_proto::Thread {
    serde_json::from_value(serde_json::json!({
        "id":5,"kind":"space","parent_thread_id":null,"pinned_at":null,"title":"Same ID","icon":null,
        "showcased_artifact_id":null,"description":"","instrument":null,"attention":"quiet",
        "settled_at":null,"archived_at":null,"snoozed_until":null,"read":false,
        "created_at":"2026-09-10T10:00:00Z","updated_at":"2026-09-10T10:00:00Z","revision":1,
        "running_turn":null,"queued_turn_count":0,"last_finished_turn":null,
        "last_activity_at":"2026-09-10T10:00:00Z"
    }))
    .unwrap()
}

#[test]
fn delayed_mutation_callbacks_keep_the_history_the_ui_displayed() {
    let client = Client::new(ClientConfig::new("localhost:3000".into(), "test".into())).unwrap();
    {
        let mut store = client.inner.write_store();
        store.apply_hello_ok("history-a".into(), vec![thread()], vec![], "test".into());
        store.connection = ConnectionState::Online;
        store.apply_hello_ok("history-b".into(), vec![thread()], vec![], "test".into());
    }

    assert!(
        client
            .send_message(SendThreadMessageRequest::new(
                "history-a".into(),
                5,
                "stale send".into(),
            ))
            .is_none()
    );
    assert!(
        client
            .create_thread(
                "history-a".into(),
                "Stale child".into(),
                hirsel_proto::ThreadKind::Task,
                Some(5),
            )
            .is_none()
    );
    assert!(
        client
            .thread_action(
                "history-a".into(),
                5,
                "archive".into(),
                serde_json::json!({}),
                None,
            )
            .is_none()
    );
    assert!(!client.cancel_turn("history-a".into(), 5));
    assert!(client.inner.read_store().messages.is_empty());
    assert!(client.inner.read_store().pending_creates.is_empty());
    assert!(client.inner.pending_frames.lock().unwrap().is_empty());

    assert!(
        client
            .send_message(SendThreadMessageRequest::new(
                "history-b".into(),
                5,
                "current send".into(),
            ))
            .is_some()
    );
    assert!(
        client
            .create_thread(
                "history-b".into(),
                "Current child".into(),
                hirsel_proto::ThreadKind::Task,
                Some(5),
            )
            .is_some()
    );
    assert!(
        client
            .thread_action(
                "history-b".into(),
                5,
                "archive".into(),
                serde_json::json!({}),
                None,
            )
            .is_some()
    );
    assert!(client.cancel_turn("history-b".into(), 5));
    assert_eq!(client.inner.read_store().pending_sends().count(), 1);
    assert_eq!(client.inner.read_store().pending_creates.len(), 1);
    let frames = client.inner.pending_frames.lock().unwrap();
    assert!(matches!(
        &frames[0],
        ClientToHost::ThreadAction { history_id, thread_id: 5, .. } if history_id == "history-b"
    ));
    assert!(matches!(
        &frames[1],
        ClientToHost::CancelTurn { history_id, thread_id: 5 } if history_id == "history-b"
    ));
}
