use hirsel_proto::{HostToClient, Thread, ThreadAttention, ThreadTurnState};
use serde_json::json;

fn summary(state: &crate::AppState, thread_id: u64) -> Thread {
    state
        .broadcast_log
        .recent()
        .into_iter()
        .rev()
        .find_map(|frame| match frame {
            HostToClient::ThreadUpsert { thread } if thread.id == thread_id => Some(thread),
            _ => None,
        })
        .expect("durable activity publishes an inventory summary without opening history")
}

#[tokio::test]
async fn unopened_thread_receives_each_execution_transition_at_the_same_revision() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let (thread, _) = state
        .storage
        .create_thread(
            "inventory",
            "Background work",
            "",
            &json!({}),
            ThreadAttention::NeedsOwner,
            None,
        )
        .await
        .unwrap();
    let queued = state
        .storage
        .queue_thread_turn(thread.id, None)
        .await
        .unwrap();
    state.tools.publish_thread_turn(queued.clone()).await;
    let pending = summary(&state, thread.id);
    assert_eq!(pending.queued_turn_count, 1);
    assert!(pending.running_turn.is_none());
    assert_eq!(pending.revision, thread.revision);

    let running = state.storage.run_thread_turn(queued.id).await.unwrap();
    state.tools.publish_thread_turn(running.clone()).await;
    let active = summary(&state, thread.id);
    assert_eq!(active.running_turn.as_ref(), Some(&running));
    assert_eq!(active.queued_turn_count, 0);
    assert_eq!(active.revision, thread.revision);

    let second = state
        .storage
        .queue_thread_turn(thread.id, None)
        .await
        .unwrap();
    state.tools.publish_thread_turn(second).await;
    let active = summary(&state, thread.id);
    assert_eq!(active.running_turn.as_ref(), Some(&running));
    assert_eq!(active.queued_turn_count, 1);

    let finished = state
        .storage
        .finish_thread_turn(running.id, ThreadTurnState::Completed, None)
        .await
        .unwrap();
    state.tools.publish_thread_turn(finished.clone()).await;
    let done = summary(&state, thread.id);
    assert!(done.running_turn.is_none());
    assert_eq!(done.queued_turn_count, 1);
    assert_eq!(done.last_finished_turn.as_ref(), Some(&finished));
    assert_eq!(done.last_activity_at, finished.finished_at.unwrap());
    assert_eq!(done.revision, thread.revision);
    assert_eq!(done.attention, ThreadAttention::NeedsOwner);
    assert_eq!(done.read, thread.read);
    assert!(done.settled_at.is_none());
    let frames = state.broadcast_log.recent();
    assert!(
        matches!(&frames[frames.len() - 2], HostToClient::ThreadTurn { turn } if turn == &finished)
    );
    assert!(
        matches!(&frames[frames.len() - 1], HostToClient::ThreadUpsert { thread } if thread == &done)
    );
}

#[tokio::test]
async fn message_and_activity_publish_recency_without_changing_thread_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let (thread, _) = state
        .storage
        .create_thread(
            "closed",
            "Closed work",
            "",
            &json!({}),
            ThreadAttention::NeedsOwner,
            None,
        )
        .await
        .unwrap();
    state.storage.mark_thread_read(thread.id).await.unwrap();
    state.storage.settle_thread(thread.id, true).await.unwrap();
    let before = state.storage.archive_thread(thread.id, true).await.unwrap();
    let message = state
        .tools
        .thread_chat_send(thread.id, "Late result".into(), None, Vec::new())
        .await
        .unwrap();
    let after_message = summary(&state, thread.id);
    assert_eq!(after_message.last_activity_at, message.ts);
    let activity = state
        .storage
        .append_thread_activity(
            thread.id,
            None,
            "background_result",
            &json!({"result":"ready"}),
        )
        .await
        .unwrap();
    state.tools.publish_thread_activity(activity.clone()).await;
    let after = summary(&state, thread.id);
    assert_eq!(after.last_activity_at, activity.ts);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after.settled_at, before.settled_at);
    assert_eq!(after.archived_at, before.archived_at);
    assert_eq!(after.attention, before.attention);
    assert_eq!(after.read, before.read);
}

#[tokio::test]
async fn coordinator_chat_and_scheduled_digest_refresh_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let thread = state
        .storage
        .create_thread(
            "digest-origin",
            "Digest origin",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let message = state
        .tools
        .thread_chat_send(thread.id, "Coordinator result".into(), None, Vec::new())
        .await
        .unwrap();
    assert_eq!(summary(&state, thread.id).last_activity_at, message.ts);
    state.broadcast_log.clear();
    let event = state
        .tools
        .emit_scheduled_digest(
            &state.storage.history_id().await.unwrap(),
            thread.id,
            "daily",
            "Digest",
            "ready",
        )
        .await
        .unwrap();
    assert_eq!(summary(&state, thread.id).last_activity_at, event.ts);
}
