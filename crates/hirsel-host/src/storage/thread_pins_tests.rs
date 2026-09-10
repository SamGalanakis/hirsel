use super::*;
use crate::lash_runtime::ScopedThreadTools;
use serde_json::json;

#[tokio::test]
async fn root_pins_preserve_hierarchy_and_child_pin_rejection_preserves_rows() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let storage = &state.storage;
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
    let before = storage.thread(child.id).await.unwrap().unwrap();
    assert!(
        state
            .handle_thread_action(child.id, "pin".into(), json!({}), None)
            .await
            .unwrap_err()
            .to_string()
            .contains("top-level")
    );
    assert_eq!(storage.thread(child.id).await.unwrap().unwrap(), before);
    let unpinned = storage.pin_thread(root.id, false).await.unwrap();
    assert!(unpinned.pinned_at.is_none());
    assert_eq!(unpinned.revision, pinned.revision + 1);
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

    let turn = storage.start_thread_turn(root.id, None).await.unwrap();
    let caller = storage
        .bind_thread_execution(
            &storage.history_id().await.unwrap(),
            "pin-test",
            "pin-test",
            turn.id,
        )
        .await
        .unwrap();
    let tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller,
        operation_id: "child-pin".into(),
    };
    assert!(
        tools
            .execute(
                "threads_update",
                &json!({"thread":child.id,"pinned_at":"2026-09-10T08:00:00Z"})
            )
            .await
            .is_err()
    );
    assert_eq!(storage.thread(child.id).await.unwrap().unwrap(), before);
}

#[tokio::test]
async fn legacy_child_pins_are_inert_on_every_read_and_can_be_cleared() {
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
    storage
        .conn
        .lock()
        .await
        .execute(
            "UPDATE threads SET pinned_at='2026-09-10T08:00:00Z' WHERE id=?1",
            [child.id],
        )
        .unwrap();
    let history = storage.history_id().await.unwrap();
    drop(storage);
    let storage = Storage::open(dir.path()).await.unwrap();
    assert_eq!(storage.history_id().await.unwrap(), history);
    assert_eq!(storage.thread(child.id).await.unwrap().unwrap(), child);
    assert!(
        storage
            .thread_snapshot()
            .await
            .unwrap()
            .iter()
            .all(|thread| thread.pinned_at.is_none())
    );
    assert!(
        storage
            .thread_detail(child.id, None, 100)
            .await
            .unwrap()
            .thread
            .pinned_at
            .is_none()
    );
    let c = storage.conn.lock().await;
    assert_eq!(
        c.query_row(
            "SELECT pinned_at FROM threads WHERE id=?1",
            [child.id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "2026-09-10T08:00:00Z"
    );
    drop(c);
    assert!(storage.pin_thread(child.id, true).await.is_err());
    let cleared = storage.pin_thread(child.id, false).await.unwrap();
    assert!(cleared.pinned_at.is_none());
    assert_eq!(cleared.revision, child.revision + 1);
    assert!(
        storage
            .conn
            .lock()
            .await
            .query_row(
                "SELECT pinned_at FROM threads WHERE id=?1",
                [child.id],
                |r| r.get::<_, Option<String>>(0)
            )
            .unwrap()
            .is_none()
    );
}
