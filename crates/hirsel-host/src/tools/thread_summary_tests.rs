use hirsel_proto::{HostToClient, Thread, ThreadAttention, ThreadTurnState, ToolCallSummary};
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
            None,
            ThreadAttention::NeedsOwner,
            hirsel_proto::ThreadKind::Task,
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
async fn terminal_child_turn_emits_typed_turn_and_completion_triggers() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let (parent, _) = state
        .storage
        .create_thread(
            "trigger-parent",
            "Parent",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Space,
            None,
        )
        .await
        .unwrap();
    let (child, _) = state
        .storage
        .create_thread(
            "trigger-child",
            "Child",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            Some(parent.id),
        )
        .await
        .unwrap();
    let queued = state
        .storage
        .queue_thread_turn(child.id, None)
        .await
        .unwrap();
    let running = state.storage.run_thread_turn(queued.id).await.unwrap();
    let finished = state
        .storage
        .finish_thread_turn(running.id, ThreadTurnState::Completed, None)
        .await
        .unwrap();
    state.tools.publish_thread_turn(finished).await;

    let events = state.tools.recorded_thread_triggers().await;
    assert!(events.iter().any(|event| event.source_type
        == crate::lash_runtime::THREAD_TURN_SOURCE_TYPE
        && event.event_type == crate::lash_runtime::THREAD_TURN_EVENT_TYPE
        && event.thread_id == child.id));
    assert!(events.iter().any(|event| event.source_type
        == crate::lash_runtime::THREAD_COMPLETED_SOURCE_TYPE
        && event.event_type == crate::lash_runtime::THREAD_COMPLETED_EVENT_TYPE
        && event.thread_id == child.id));
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
            None,
            ThreadAttention::NeedsOwner,
            hirsel_proto::ThreadKind::Task,
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
async fn agent_chat_and_scheduled_digest_refresh_inventory() {
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
            None,
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let message = state
        .tools
        .thread_chat_send(thread.id, "Agent result".into(), None, Vec::new())
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

#[tokio::test]
async fn one_new_effect_publishes_only_that_receipt_after_a_large_history() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let thread = state
        .storage
        .create_thread(
            "effect-delta",
            "Effect delta",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let turn = state
        .storage
        .start_thread_turn(thread.id, None)
        .await
        .unwrap();
    let history = state.storage.history_id().await.unwrap();
    let caller = state
        .storage
        .bind_thread_execution(&history, "effect-delta", "effect-delta", turn.id)
        .await
        .unwrap();
    for index in 0..150 {
        state
            .storage
            .scoped_thread_read(
                &caller,
                Some(&format!("old-effect-{index}")),
                &crate::storage::ThreadRef::default(),
                None,
                1,
            )
            .await
            .unwrap();
    }
    state
        .storage
        .scoped_thread_read(
            &caller,
            Some("new-effect"),
            &crate::storage::ThreadRef::default(),
            None,
            1,
        )
        .await
        .unwrap();
    state.broadcast_log.clear();

    crate::lash_runtime::TurnIngest::record_tool_completion(
        &state.tools,
        &history,
        (thread.id, turn.id),
        &ToolCallSummary {
            id: "new-effect".into(),
            name: "threads_read".into(),
            ok: true,
        },
    )
    .await
    .unwrap();

    let publications = state
        .broadcast_log
        .recent()
        .into_iter()
        .filter_map(|frame| match frame {
            HostToClient::ThreadEffectsChanged {
                turn_id, effects, ..
            } if turn_id == turn.id => Some(effects),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(publications.len(), 1);
    assert_eq!(publications[0].len(), 1);
    assert_eq!(publications[0][0].receipt.operation_id, "new-effect");

    state.tools.reset_runtime_projections().await;
    crate::lash_runtime::TurnIngest::record_tool_completion(
        &state.tools,
        &history,
        (thread.id, turn.id),
        &ToolCallSummary {
            id: "new-effect".into(),
            name: "threads_read".into(),
            ok: true,
        },
    )
    .await
    .unwrap();
    assert!(state.broadcast_log.recent().iter().all(|frame| {
        !matches!(frame, HostToClient::ThreadEffectsChanged { turn_id, .. } if *turn_id == turn.id)
    }));
}

#[tokio::test]
async fn action_refresh_publishes_only_the_receipt_whose_projection_changed() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let source = state
        .storage
        .create_thread(
            "effect-source",
            "Effect source",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let source_turn = state
        .storage
        .start_thread_turn(source.id, None)
        .await
        .unwrap();
    let history = state.storage.history_id().await.unwrap();
    let caller = state
        .storage
        .bind_thread_execution(&history, "effect-action", "effect-action", source_turn.id)
        .await
        .unwrap();
    let assignment = crate::storage::Delegation {
        title: "Child".into(),
        brief: "Do the work".into(),
        artifact_ids: vec![],
        child_thread_id: None,
        execution: None,
    };
    let invocation = serde_json::to_value(&assignment).unwrap();
    let accepted = state
        .storage
        .delegate_thread(
            &caller,
            "delegate-effect",
            "threads_delegate",
            &assignment,
            &invocation,
        )
        .await
        .unwrap();
    crate::lash_runtime::TurnIngest::record_tool_completion(
        &state.tools,
        &history,
        (source.id, source_turn.id),
        &ToolCallSummary {
            id: "delegate-effect".into(),
            name: "threads_delegate".into(),
            ok: true,
        },
    )
    .await
    .unwrap();
    state.broadcast_log.clear();

    let running = state
        .storage
        .run_thread_turn(accepted.turn_id)
        .await
        .unwrap();
    state.tools.publish_thread_turn(running.clone()).await;
    let changed = state
        .broadcast_log
        .recent()
        .into_iter()
        .filter_map(|frame| match frame {
            HostToClient::ThreadEffectsChanged { effects, .. } => Some(effects),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].receipt.target_turn_id, Some(accepted.turn_id));
    assert!(
        changed[0]
            .actions
            .contains(&hirsel_proto::EffectAction::Stop {
                thread_id: accepted.thread_id,
                turn_id: accepted.turn_id,
            })
    );

    state.broadcast_log.clear();
    state.tools.publish_thread_turn(running).await;
    assert!(
        state
            .broadcast_log
            .recent()
            .iter()
            .all(|frame| { !matches!(frame, HostToClient::ThreadEffectsChanged { .. }) })
    );
}

#[tokio::test]
async fn stale_pre_reset_thread_cannot_be_published_as_the_reused_current_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let device_token = state
        .storage
        .issue_device_token("Owner phone", "node-a")
        .await
        .unwrap();
    state
        .storage
        .register_push_token(
            &device_token,
            hirsel_proto::PushPlatform::Android,
            "durable-token",
        )
        .await
        .unwrap();
    let old_history = state.storage.history_id().await.unwrap();
    let old = state
        .storage
        .create_thread(
            "old",
            "Old private title",
            "Old private body",
            None,
            ThreadAttention::NeedsOwner,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let database = dir.path().join("hirsel.sqlite");
    let persisted_thread_fields = {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .query_row(
                "SELECT parent_thread_id,pinned_at,title,description,instrument,attention,settled_at,archived_at,snoozed_until,read,created_at,updated_at,revision,icon_symbol,showcased_artifact_id FROM threads WHERE id=?1",
                [old.id],
                |row| {
                    (0..15)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                },
            )
            .unwrap()
    };

    state.storage.reset().await.unwrap();
    let current_history = state.storage.history_id().await.unwrap();
    let recreated = state
        .storage
        .create_thread(
            "current",
            "Current title",
            "Current body",
            None,
            ThreadAttention::NeedsOwner,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    assert_ne!(current_history, old_history);
    assert_eq!(
        recreated.id, old.id,
        "reset fixture must reuse the numeric ID"
    );
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        let values = persisted_thread_fields
            .into_iter()
            .chain([rusqlite::types::Value::Integer(recreated.id as i64)]);
        connection
            .execute(
                "UPDATE threads SET parent_thread_id=?1,pinned_at=?2,title=?3,description=?4,instrument=?5,attention=?6,settled_at=?7,archived_at=?8,snoozed_until=?9,read=?10,created_at=?11,updated_at=?12,revision=?13,icon_symbol=?14,showcased_artifact_id=?15 WHERE id=?16",
                rusqlite::params_from_iter(values),
            )
            .unwrap();
    }
    let identical_reused = state.storage.thread(recreated.id).await.unwrap().unwrap();
    assert_eq!(
        identical_reused, old,
        "fixture must preserve every Thread field"
    );

    state.tools.publish_thread(&old_history, old).await;
    tokio::task::yield_now().await;
    assert!(state.pushes.recorded_pushes().is_empty());
    assert!(state.broadcast_log.recent().iter().all(|frame| {
        !matches!(frame, HostToClient::ThreadUpsert { thread } if thread.title == "Old private title")
    }));

    let current = state
        .storage
        .update_thread(
            recreated.id,
            Some("Current title"),
            Some("Current body"),
            None,
            None,
        )
        .await
        .unwrap();
    state
        .tools
        .publish_thread(&current_history, current.clone())
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while state.pushes.recorded_pushes().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let pushes = state.pushes.recorded_pushes();
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].payload.data.history_id, current_history);
    assert_eq!(pushes[0].payload.data.title, "Current title");
    assert_eq!(pushes[0].payload.body, "Current body");
    assert_eq!(summary(&state, current.id).title, "Current title");
}
