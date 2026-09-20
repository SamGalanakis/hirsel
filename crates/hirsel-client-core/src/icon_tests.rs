use super::*;
use crate::ConnectionState;
use serde_json::json;

fn symbol(name: &str) -> hirsel_proto::ThreadIcon {
    hirsel_proto::ThreadIcon::Symbol {
        name: name.to_owned(),
        tint: hirsel_proto::ThreadTint::Teal,
    }
}

#[test]
fn icon_edits_reject_reused_thread_identity_after_history_change() {
    let client = Client::new(ClientConfig::new("localhost:3000".into(), "test".into())).unwrap();
    let thread: hirsel_proto::Thread = serde_json::from_value(json!({
        "id":5,"kind":"space","parent_thread_id":null,"pinned_at":null,"title":"Same ID","icon":{"kind":"symbol","name":"rocket","tint":"blue"},
        "description":"","instrument":null,
        "state":{"revision":1,"headline":"Space ready","own_headline":"Space ready","findings":[],"artifact_ids":[],"checkpoint_at":null,"steering_revision":0},
        "attention":"quiet","settled_at":null,
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
            .update_thread_icon("history-a".into(), 5, Some(symbol("flask")), 7)
            .is_none()
    );
    assert!(
        client
            .update_thread_icon("history-b".into(), 5, Some(symbol("flask")), 6)
            .is_none()
    );
    assert!(
        client
            .update_thread_icon("history-b".into(), 99, Some(symbol("flask")), 7)
            .is_none()
    );
    client.inner.write_store().connection = ConnectionState::Offline;
    assert!(
        client
            .update_thread_icon("history-b".into(), 5, Some(symbol("flask")), 7)
            .is_none()
    );
    assert!(client.inner.pending_frames.lock().unwrap().is_empty());
    client.inner.write_store().connection = ConnectionState::Online;
    let receipt = client
        .update_thread_icon("history-b".into(), 5, None, 7)
        .unwrap();
    let frames = client.inner.pending_frames.lock().unwrap();
    assert_eq!(frames.len(), 1);
    assert!(
        matches!(&frames[0], ClientToHost::ThreadAction { client_id, history_id, thread_id:5, action, data, expected_revision:Some(7) } if client_id == &receipt.client_id && history_id == "history-b" && action == "set_icon" && data == &json!({"icon":null}))
    );
}
