use super::*;
use hirsel_proto::{ChatAuthor, ThreadAttention, ThreadTurnState};

#[tokio::test]
async fn ordinary_thread_tool_creation_is_visible_and_mutable_without_action_wake() {
    let (executor, storage, log, _dir) = super::tests::test_event_executor().await;
    executor.anchors.lock().await.active = None;
    let args = json!({"client_id":"groceries","title":"Buy groceries"});
    let created = executor.threads_create(&args).await.unwrap();
    let id = created["thread_id"].as_u64().unwrap();
    assert_eq!(
        executor.threads_create(&args).await.unwrap()["thread_id"],
        id
    );
    assert!(
        storage
            .thread_snapshot()
            .await
            .unwrap()
            .iter()
            .any(|t| t.id == id)
    );
    assert!(
        log.recent()
            .iter()
            .any(|f| matches!(f,HostToClient::ThreadUpsert{thread} if thread.id==id))
    );
    assert!(storage.all_pings().await.unwrap().is_empty());
    executor.threads_update(&json!({"thread_id":id,"attention":"needs_owner","instrument":{"type":"text","text":"Which store?"}})).await.unwrap();
    storage.mark_thread_read(id).await.unwrap();
    executor
        .threads_update(&json!({"thread_id":id,"attention":"quiet","instrument":null}))
        .await
        .unwrap();
    let result = executor
        .threads_read(&json!({"thread_id":id}))
        .await
        .unwrap();
    assert!(result["thread"]["settled_at"].is_null());
    assert_eq!(result["thread"]["attention"], "quiet");
    let names = hirsel_tool_definitions(&crate::subagent_models::registry_catalog())
        .into_iter()
        .map(|d| d.name().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("events_") || n.starts_with("pings_"))
    );
}

#[tokio::test]
async fn queued_scripted_replies_and_telemetry_keep_their_owning_threads() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (key, title) in [("a", "Alpha"), ("b", "Beta")] {
        let (thread, _) = state
            .storage
            .create_thread(key, title, "", &Value::Null, ThreadAttention::Quiet)
            .await
            .unwrap();
        ids.push(thread.id);
        state
            .submit_thread_message(
                format!("message-{key}"),
                thread.id,
                "pong".into(),
                Vec::new(),
                Vec::new(),
                SendMode::NextTurn,
            )
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let a = state.storage.thread_detail(ids[0], None, 30).await.unwrap();
            let b = state.storage.thread_detail(ids[1], None, 30).await.unwrap();
            if a.turns
                .iter()
                .any(|t| t.state == ThreadTurnState::Completed)
                && b.turns
                    .iter()
                    .any(|t| t.state == ThreadTurnState::Completed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for id in ids {
        let detail = state.storage.thread_detail(id, None, 30).await.unwrap();
        assert_eq!(detail.messages.len(), 2);
        assert_eq!(detail.messages[1].body, "pong");
        assert_eq!(detail.messages[1].thread_id, id);
        let turn = &detail.turns[0];
        assert_eq!(turn.agent_message_id, Some(detail.messages[1].id));
        assert!(state.broadcast_log.recent().iter().any(|f|matches!(f,HostToClient::TurnEvent{thread_id:Some(t),turn_id:Some(r),..} if *t==id&&*r==turn.id)));
        assert!(detail.thread.settled_at.is_none());
    }
    assert!(
        state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn durable_admission_never_exposes_two_thread_inputs_to_lash() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    config.anthropic_api_key = Some("test-key-no-inference".into());
    let state = crate::build_state(config).await.unwrap();
    let AgentBackend::Lash(runtime) = state.agent.backend.as_ref() else {
        panic!("Lash runtime");
    };
    // Hold the pump. The test exercises actual durable admission and cancellation
    // without making provider requests.
    let _pump = runtime.pump_lock.lock().await;
    let mut owners = Vec::new();
    for key in ["first", "second"] {
        let (thread, _) = state
            .storage
            .create_thread(key, key, "", &Value::Null, ThreadAttention::Quiet)
            .await
            .unwrap();
        let owner = state
            .storage
            .append_thread_chat(thread.id, ChatAuthor::Owner, key, None, Vec::new())
            .await
            .unwrap();
        runtime
            .enqueue_inner(OwnerTurn {
                thread_id: thread.id,
                thread_action: None,
                message_id: owner.id,
                client_id: key.into(),
                body: key.into(),
                anchor: None,
                attachments: Vec::new(),
                mentioned_pings: Vec::new(),
                mode: SendMode::Send,
                task_action: None,
            })
            .await
            .unwrap();
        owners.push(owner);
    }
    assert_eq!(
        runtime
            .admit_next_thread_request()
            .await
            .unwrap()
            .as_deref(),
        Some("first")
    );
    let pending = runtime.session.pending_turn_inputs().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_key.as_deref(), Some("host:first"));
    assert_eq!(
        state.storage.pending_thread_requests().await.unwrap().len(),
        2
    );
    assert_eq!(
        runtime.cancel_thread_request("first").await.unwrap(),
        CancelQueuedResult::AlreadyClaimed
    );
    assert_eq!(
        runtime.cancel_thread_request("second").await.unwrap(),
        CancelQueuedResult::Cancelled
    );
    assert_eq!(
        state
            .storage
            .thread_detail(owners[1].thread_id, None, 30)
            .await
            .unwrap()
            .turns[0]
            .state,
        ThreadTurnState::Cancelled
    );
    // Simulate process restart recovery: admitted but unfinished input is
    // cancelled in Lash when its interrupted Thread request is reconciled.
    state
        .storage
        .interrupt_unfinished_thread_turns()
        .await
        .unwrap();
    assert!(runtime.admit_next_thread_request().await.unwrap().is_none());
    assert!(
        runtime
            .session
            .pending_turn_inputs()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn projection_retry_uses_one_thread_reply() {
    let (executor, storage, _, _dir) = super::tests::test_event_executor().await;
    let (thread, _) = storage
        .create_thread("reply", "Reply", "", &Value::Null, ThreadAttention::Quiet)
        .await
        .unwrap();
    let owner = storage
        .append_thread_chat(thread.id, ChatAuthor::Owner, "hello", None, Vec::new())
        .await
        .unwrap();
    let turn = storage
        .start_thread_turn(thread.id, Some(owner.id))
        .await
        .unwrap();
    let output = super::tests::test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "hello back".into(),
        }),
        "hello back",
        Vec::new(),
    );
    let a = materialize_thread_turn_reply(&executor.tools, &output, turn.id, Some(owner.id))
        .await
        .unwrap();
    let b = materialize_thread_turn_reply(&executor.tools, &output, turn.id, Some(owner.id))
        .await
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(
        storage
            .thread_detail(thread.id, None, 30)
            .await
            .unwrap()
            .messages
            .len(),
        2
    );
}
