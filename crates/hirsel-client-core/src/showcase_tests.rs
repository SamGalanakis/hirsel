use super::*;
use crate::ConnectionState;
use serde_json::json;

#[test]
fn showcase_edits_reject_reused_thread_identity_after_history_change() {
    let client = Client::new(ClientConfig::new("localhost:3000".into(), "test".into())).unwrap();
    let thread: hirsel_proto::Thread = serde_json::from_value(json!({
        "id":5,"kind":"space","parent_thread_id":null,"pinned_at":null,"title":"Same ID","icon":"🐙",
        "description":"","instrument":null,"attention":"quiet","settled_at":null,
        "archived_at":null,"snoozed_until":null,"read":false,
        "created_at":"2026-09-10T10:00:00Z","updated_at":"2026-09-10T10:00:00Z","revision":7,
        "running_turn":null,"queued_turn_count":0,"last_finished_turn":null,
        "last_activity_at":"2026-09-10T10:00:00Z"
    }))
    .unwrap();
    {
        let mut store = client.inner.write_store();
        store.apply_hello_ok(
            "history-a".into(),
            vec![thread.clone()],
            vec![],
            "test".into(),
        );
        store.connection = ConnectionState::Online;
        // Native has already received a reset while the UI still has history A.
        store.apply_hello_ok("history-b".into(), vec![thread], vec![], "test".into());
    }
    assert!(
        client
            .update_thread_showcase("history-a".into(), 5, Some(42), 7)
            .is_none()
    );
    assert!(
        client
            .update_thread_showcase("history-b".into(), 5, Some(42), 6)
            .is_none()
    );
    assert!(
        client
            .update_thread_showcase("history-b".into(), 99, Some(42), 7)
            .is_none()
    );
    client.inner.write_store().connection = ConnectionState::Offline;
    assert!(
        client
            .update_thread_showcase("history-b".into(), 5, Some(42), 7)
            .is_none()
    );
    assert!(client.inner.pending_frames.lock().unwrap().is_empty());
    client.inner.write_store().connection = ConnectionState::Online;
    let receipt = client
        .update_thread_showcase("history-b".into(), 5, None, 7)
        .unwrap();
    let frames = client.inner.pending_frames.lock().unwrap();
    assert_eq!(frames.len(), 1);
    assert!(
        matches!(&frames[0], ClientToHost::ThreadAction { client_id, history_id, thread_id:5, action, data, expected_revision:Some(7) } if client_id == &receipt.client_id && history_id == "history-b" && action == "set_showcase" && data == &json!({"artifact_id":null}))
    );
}
