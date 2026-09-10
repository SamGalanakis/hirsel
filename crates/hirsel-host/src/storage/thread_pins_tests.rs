use super::*;
use serde_json::json;

#[tokio::test]
async fn root_pin_actions_advance_revision_without_changing_history() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let storage = &state.storage;
    let history = storage.history_id().await.unwrap();
    let root = storage
        .create_thread(
            "root",
            "Root",
            "",
            &json!(null),
            ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let child = storage
        .create_thread(
            "child",
            "Child",
            "",
            &json!(null),
            ThreadAttention::Quiet,
            Some(root.id),
        )
        .await
        .unwrap()
        .0;
    let pinned = state
        .handle_thread_action(root.id, "pin".into(), json!({}), None)
        .await
        .unwrap();
    assert!(pinned.pinned_at.is_some());
    assert_eq!(pinned.revision, root.revision + 1);
    assert_eq!(pinned.last_activity_at, root.last_activity_at);
    assert_eq!(storage.history_id().await.unwrap(), history);
    let before = storage.thread(child.id).await.unwrap().unwrap();
    for action in ["pin", "unpin"] {
        assert!(
            state
                .handle_thread_action(child.id, action.into(), json!({}), None)
                .await
                .unwrap_err()
                .to_string()
                .contains("top-level")
        );
        assert_eq!(storage.thread(child.id).await.unwrap().unwrap(), before);
        assert_eq!(storage.history_id().await.unwrap(), history);
    }
    let unpinned = state
        .handle_thread_action(root.id, "unpin".into(), json!({}), None)
        .await
        .unwrap();
    assert!(unpinned.pinned_at.is_none());
    assert_eq!(unpinned.revision, pinned.revision + 1);
    assert_eq!(storage.history_id().await.unwrap(), history);
    assert_eq!(storage.thread_snapshot().await.unwrap().len(), 2);
    assert_eq!(
        storage
            .thread(child.id)
            .await
            .unwrap()
            .unwrap()
            .parent_thread_id,
        Some(root.id)
    );
}

#[tokio::test]
async fn schema_rejects_child_pins_and_reparenting_pinned_roots() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let root = storage
        .create_thread(
            "root",
            "Root",
            "",
            &json!(null),
            ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let child = storage
        .create_thread(
            "child",
            "Child",
            "",
            &json!(null),
            ThreadAttention::Quiet,
            Some(root.id),
        )
        .await
        .unwrap()
        .0;
    let c = storage.conn.lock().await;
    c.execute(
        "UPDATE threads SET pinned_at='2026-09-10T08:00:00Z' WHERE id=?1",
        [root.id],
    )
    .unwrap();
    assert!(
        c.execute(
            "UPDATE threads SET pinned_at='2026-09-10T08:00:00Z' WHERE id=?1",
            [child.id],
        )
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE threads SET parent_thread_id=?2 WHERE id=?1",
            [root.id, child.id],
        )
        .is_err()
    );
    drop(c);
    assert_eq!(storage.thread(child.id).await.unwrap().unwrap(), child);
    let root = storage.thread(root.id).await.unwrap().unwrap();
    assert_eq!(root.parent_thread_id, None);
    assert!(root.pinned_at.is_some());
}
