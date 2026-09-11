use super::*;
use hirsel_drivers::AgentKind;
use hirsel_proto::{ChatAuthor, ThreadAttention, ThreadTurnState};

async fn delegated_cli_turn(
    storage: &crate::Storage,
    caller: &crate::storage::ThreadCaller,
    operation_id: &str,
    title: &str,
) -> (u64, u64) {
    let assignment = crate::storage::Delegation {
        title: title.into(),
        brief: format!("Run {title}"),
        artifact_ids: Vec::new(),
        child_thread_id: None,
        execution: Some(crate::storage::ThreadExecution::Cli {
            agent: AgentKind::Claude,
            model: "fake-model".into(),
            variant: "fake-variant".into(),
            cwd: std::env::current_dir().unwrap().canonicalize().unwrap(),
        }),
    };
    let delegated = storage
        .delegate_thread(
            caller,
            operation_id,
            &assignment,
            &serde_json::to_value(&assignment).unwrap(),
        )
        .await
        .unwrap();
    (delegated.thread_id, delegated.turn_id)
}

fn native_execution(cwd: std::path::PathBuf) -> crate::storage::ThreadExecution {
    crate::storage::ThreadExecution::LashWorker {
        provider: crate::providers::NativeWorkerProviderSnapshot {
            id: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            revision: "test-route".into(),
        },
        model: crate::providers::NATIVE_WORKER_DEFAULT_MODEL.into(),
        variant: "default".into(),
        cwd,
        tool_profile: crate::storage::NATIVE_CODING_TOOL_PROFILE.into(),
    }
}

#[tokio::test]
async fn native_worker_rejects_artifact_references_before_acceptance() {
    let (executor, storage, _log, _dir) = super::tests::test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let before = storage.thread_snapshot().await.unwrap().len();
    let tools = ScopedThreadTools {
        tools: executor.tools,
        caller,
        operation_id: "native-artifact-refusal".into(),
    };
    let error = tools
        .execute(
            "threads_delegate",
            &json!({
                "title":"Native child",
                "brief":"Use the referenced artifact",
                "artifact_ids":[1],
                "agent":"lash"
            }),
        )
        .await
        .unwrap_err();
    assert!(
        error.contains("artifact references are not supported"),
        "{error}"
    );
    assert_eq!(storage.thread_snapshot().await.unwrap().len(), before);
}

#[tokio::test]
async fn native_worker_preference_is_captured_for_follow_up_turns() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::Storage::open(dir.path()).await.unwrap();
    let caller = storage.test_running_caller().await;
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let assignment = crate::storage::Delegation {
        title: "Native child".into(),
        brief: "Make the focused repair".into(),
        artifact_ids: Vec::new(),
        child_thread_id: None,
        execution: Some(native_execution(cwd.clone())),
    };
    let initial = storage
        .delegate_thread(
            &caller,
            "native-initial",
            &assignment,
            &serde_json::to_value(&assignment).unwrap(),
        )
        .await
        .unwrap();
    let follow_up = crate::storage::Delegation {
        title: "Follow-up".into(),
        brief: "Now rerun the check".into(),
        artifact_ids: Vec::new(),
        child_thread_id: Some(initial.thread_id),
        execution: None,
    };
    let follow_up = storage
        .delegate_thread(
            &caller,
            "native-follow-up",
            &follow_up,
            &serde_json::to_value(&follow_up).unwrap(),
        )
        .await
        .unwrap();

    for turn_id in [initial.turn_id, follow_up.turn_id] {
        let crate::storage::ThreadExecution::LashWorker {
            provider,
            model,
            variant,
            cwd: captured_cwd,
            tool_profile,
        } = storage.turn_execution(turn_id).await.unwrap()
        else {
            panic!("follow-up lost the native worker preference");
        };
        assert_eq!(provider.id, "openrouter");
        assert_eq!(model, crate::providers::NATIVE_WORKER_DEFAULT_MODEL);
        assert_eq!(variant, "default");
        assert_eq!(captured_cwd, cwd);
        assert_eq!(tool_profile, crate::storage::NATIVE_CODING_TOOL_PROFILE);
    }
}

#[tokio::test]
async fn restart_interrupts_running_native_worker_without_replaying_it() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::Storage::open(dir.path()).await.unwrap();
    let caller = storage.test_running_caller().await;
    let assignment = crate::storage::Delegation {
        title: "Native interrupted".into(),
        brief: "Perform one mutation".into(),
        artifact_ids: Vec::new(),
        child_thread_id: None,
        execution: Some(native_execution(
            std::env::current_dir().unwrap().canonicalize().unwrap(),
        )),
    };
    let delegated = storage
        .delegate_thread(
            &caller,
            "native-interrupted",
            &assignment,
            &serde_json::to_value(&assignment).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        storage
            .run_thread_turn(delegated.turn_id)
            .await
            .unwrap()
            .state,
        ThreadTurnState::Running
    );
    let original_session = storage
        .reconcile_native_worker_profile(
            delegated.thread_id,
            "same-profile",
            &[
                "read".into(),
                "edit".into(),
                "write".into(),
                "exec_command".into(),
            ],
        )
        .await
        .unwrap();
    drop(storage);

    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let turn = state.storage.thread_turn(delegated.turn_id).await.unwrap();
    assert_eq!(turn.state, ThreadTurnState::Interrupted);
    assert_eq!(turn.agent_message_id, None);
    let replacement_session = state
        .storage
        .reconcile_native_worker_profile(
            delegated.thread_id,
            "same-profile",
            &[
                "read".into(),
                "edit".into(),
                "write".into(),
                "exec_command".into(),
            ],
        )
        .await
        .unwrap();
    assert!(replacement_session.rotated);
    assert_ne!(replacement_session.session_id, original_session.session_id);
    assert!(
        state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .iter()
            .all(|(_, payload)| payload["turn_id"].as_u64() != Some(delegated.turn_id))
    );
}

#[tokio::test]
async fn restart_interrupts_native_input_without_blocking_later_thread_work() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::Storage::open(dir.path()).await.unwrap();
    let caller = storage.test_running_caller().await;
    let interrupted_client_id = format!("delegation:{}:interrupted", caller.turn_id);
    let interrupted = delegated_cli_turn(&storage, &caller, "interrupted", "Interrupted").await;
    let peer = delegated_cli_turn(&storage, &caller, "peer", "Peer").await;
    assert_eq!(
        storage.run_thread_turn(interrupted.1).await.unwrap().state,
        ThreadTurnState::Running
    );
    drop(storage);

    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let later = state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "later-after-restart".into(),
            interrupted.0,
            "Run the later request".into(),
            Vec::new(),
            Vec::new(),
            SendMode::NextTurn,
            Vec::new(),
        )
        .await
        .unwrap();
    let later_turn_id = state
        .storage
        .thread_request(&later.client_id)
        .await
        .unwrap()
        .unwrap()["turn_id"]
        .as_u64()
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let interrupted_turn = state.storage.thread_turn(interrupted.1).await.unwrap();
            let later_turn = state.storage.thread_turn(later_turn_id).await.unwrap();
            let peer_turn = state.storage.thread_turn(peer.1).await.unwrap();
            if interrupted_turn.state == ThreadTurnState::Interrupted
                && later_turn.state == ThreadTurnState::Completed
                && peer_turn.state == ThreadTurnState::Completed
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("later same-Thread and peer work must complete after restart");

    let detail = state
        .storage
        .thread_detail(interrupted.0, None, 30)
        .await
        .unwrap();
    let old = detail
        .turns
        .iter()
        .find(|turn| turn.id == interrupted.1)
        .unwrap();
    assert_eq!(old.state, ThreadTurnState::Interrupted);
    assert_eq!(old.agent_message_id, None);
    assert_eq!(
        detail
            .turns
            .iter()
            .filter(|turn| turn.state == ThreadTurnState::Completed)
            .count(),
        1
    );
    assert!(
        state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .iter()
            .all(|(client_id, _)| client_id != &interrupted_client_id
                && client_id != &later.client_id)
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
            .create_thread(
                key,
                title,
                "",
                &Value::Null,
                ThreadAttention::Quiet,
                hirsel_proto::ThreadKind::Task,
                None,
            )
            .await
            .unwrap();
        ids.push(thread.id);
        state
            .submit_addressed_thread_message(
                &state.storage.history_id().await.unwrap(),
                format!("message-{key}"),
                thread.id,
                "pong".into(),
                Vec::new(),
                Vec::new(),
                SendMode::NextTurn,
                Vec::new(),
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
        assert!(state.broadcast_log.recent().iter().any(
            |f| matches!(f,HostToClient::TurnEvent{thread_id:t,turn_id:r,..} if *t==id&&*r==turn.id)
        ));
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
async fn durable_admission_is_fifo_with_independent_thread_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    config.anthropic_api_key = Some("test-key-no-inference".into());
    let state = crate::build_state(config).await.unwrap();
    let AgentBackend::Threaded(registry) = state.agent.backend.as_ref() else {
        panic!("registry")
    };
    registry.capacity.close();
    let mut lanes = Vec::new();
    for key in ["alpha", "beta"] {
        let thread = state
            .storage
            .create_thread(
                key,
                key,
                "",
                &Value::Null,
                ThreadAttention::Quiet,
                hirsel_proto::ThreadKind::Task,
                None,
            )
            .await
            .unwrap()
            .0;
        let runtime = super::thread_recovery_tests::runtime_lane(&state, Some(thread.id)).await;
        for n in 1..=2 {
            state
                .submit_addressed_thread_message(
                    &state.storage.history_id().await.unwrap(),
                    format!("{key}-{n}"),
                    thread.id,
                    format!("{key} secret {n}"),
                    vec![],
                    vec![],
                    SendMode::NextTurn,
                    vec![],
                )
                .await
                .unwrap();
        }
        lanes.push(runtime);
    }
    for (runtime, key, peer) in [(&lanes[0], "alpha", "beta"), (&lanes[1], "beta", "alpha")] {
        assert_eq!(
            runtime.admit_next_thread_request().await.unwrap(),
            Some(format!("{key}-1"))
        );
        runtime.admit_next_thread_request().await.unwrap();
        let pending = runtime.session.pending_turn_inputs().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].source_key.as_deref(),
            Some(format!("host:{key}-1").as_str())
        );
        let encoded = serde_json::to_string(&pending).unwrap();
        assert!(encoded.contains(&format!("{key} secret 1")));
        assert!(!encoded.contains(&format!("{key} secret 2")));
        assert!(!encoded.contains(&format!("{peer} secret")));
        assert_eq!(
            runtime
                .cancel_thread_request(&format!("{key}-1"))
                .await
                .unwrap(),
            CancelQueuedResult::AlreadyClaimed
        );
        assert_eq!(
            runtime
                .cancel_thread_request(&format!("{key}-2"))
                .await
                .unwrap(),
            CancelQueuedResult::Cancelled
        );
    }
    state
        .storage
        .interrupt_unfinished_thread_turns()
        .await
        .unwrap();
    for runtime in lanes {
        assert!(runtime.admit_next_thread_request().await.unwrap().is_none());
        assert!(
            runtime
                .session
                .pending_turn_inputs()
                .await
                .unwrap()
                .is_empty()
        );
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
async fn projection_retry_uses_one_thread_reply() {
    let (executor, storage, _, _dir) = super::tests::test_event_executor().await;
    let (thread, _) = storage
        .create_thread(
            "reply",
            "Reply",
            "",
            &Value::Null,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
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

#[tokio::test]
async fn ordinary_thread_tool_creation_is_visible_and_mutable_without_action_wake() {
    let (executor, storage, log, _dir) = super::tests::test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let mut executor = ScopedThreadTools {
        tools: executor.tools,
        caller,
        operation_id: "create".into(),
    };
    let args = json!({"client_id":"groceries","kind":"task","title":"Buy groceries"});
    let created = executor.execute("threads_create", &args).await.unwrap();
    let id = created["thread_id"].as_u64().unwrap();
    assert_eq!(created["thread"]["kind"], "task");
    assert_eq!(
        executor.execute("threads_create", &args).await.unwrap()["thread_id"],
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

    executor.operation_id = "create-space".into();
    let space = executor
        .execute(
            "threads_create",
            &json!({"client_id":"projects","kind":"space","title":"Projects"}),
        )
        .await
        .unwrap();
    assert_eq!(space["thread"]["kind"], "space");
    for (operation_id, args) in [
        (
            "missing-kind",
            json!({"client_id":"missing","title":"Missing"}),
        ),
        (
            "bad-kind",
            json!({"client_id":"bad","kind":"project","title":"Bad"}),
        ),
    ] {
        executor.operation_id = operation_id.into();
        assert!(executor.execute("threads_create", &args).await.is_err());
    }
    let task_turn = storage.start_thread_turn(id, None).await.unwrap();
    let task_caller = storage
        .bind_thread_execution(
            &storage.history_id().await.unwrap(),
            "task-session",
            "task-execution",
            task_turn.id,
        )
        .await
        .unwrap();
    let task_executor = ScopedThreadTools {
        tools: executor.tools.clone(),
        caller: task_caller,
        operation_id: "invalid-nested-space".into(),
    };
    assert!(
        task_executor
            .execute(
                "threads_create",
                &json!({"client_id":"invalid-space","kind":"space","title":"Invalid"}),
            )
            .await
            .is_err()
    );
    assert_eq!(
        storage
            .thread_snapshot()
            .await
            .unwrap()
            .into_iter()
            .filter(|thread| thread.parent_thread_id == Some(id))
            .count(),
        0
    );

    executor.operation_id = "update-1".into();
    executor.execute("threads_update",&json!({"thread":id,"attention":"needs_owner","instrument":{"type":"text","text":"Which store?"}})).await.unwrap();
    storage.mark_thread_read(id).await.unwrap();
    executor.operation_id = "update-2".into();
    executor
        .execute(
            "threads_update",
            &json!({"thread":id,"attention":"quiet","instrument":null}),
        )
        .await
        .unwrap();
    let result = executor
        .execute("threads_read", &json!({"thread":id}))
        .await
        .unwrap();
    assert!(result["thread"]["settled_at"].is_null());
    assert_eq!(result["thread"]["attention"], "quiet");
    let definitions = hirsel_tool_definitions(&crate::subagent_models::registry_catalog(), &[]);
    let create_schema = definitions
        .iter()
        .find(|definition| definition.name() == "threads_create")
        .unwrap()
        .contract
        .input_schema
        .canonical();
    assert!(
        create_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("kind"))
    );
    assert_eq!(
        create_schema["properties"]["kind"]["enum"],
        json!(["space", "task"])
    );
    let update_schema = definitions
        .iter()
        .find(|definition| definition.name() == "threads_update")
        .unwrap()
        .contract
        .input_schema
        .canonical();
    assert!(update_schema["properties"].get("kind").is_none());
    let names = definitions
        .into_iter()
        .map(|d| d.name().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("events_") || n.starts_with("pings_"))
    );
}

/// Operator proof against an explicitly prepared disposable current-schema copy.
#[tokio::test]
#[ignore = "requires HIRSEL_COPY_PROOF_DIR pointing to a disposable /tmp copy"]
async fn retained_current_store_opens_without_synthesizing_sessions_or_work() {
    let path = std::path::PathBuf::from(std::env::var("HIRSEL_COPY_PROOF_DIR").unwrap())
        .canonicalize()
        .unwrap();
    assert!(
        path.to_string_lossy()
            .starts_with("/tmp/hirsel-nested-retained-copy-")
    );
    let storage = crate::Storage::open(&path).await.unwrap();
    assert!(storage.pending_thread_requests().await.unwrap().is_empty());
    let threads = storage.thread_snapshot().await.unwrap();
    let validation_dir = tempfile::tempdir().unwrap();
    let validation = crate::Storage::open(validation_dir.path()).await.unwrap();
    for thread in &threads {
        validation
            .create_thread(
                &format!("validate-{}", thread.id),
                &thread.title,
                &thread.description,
                &thread.instrument,
                thread.attention,
                thread.kind,
                None,
            )
            .await
            .unwrap();
        let mut before = None;
        loop {
            let detail = storage.thread_detail(thread.id, before, 100).await.unwrap();
            assert!(detail.turns.iter().all(|t| !matches!(
                t.state,
                hirsel_proto::ThreadTurnState::Queued | hirsel_proto::ThreadTurnState::Running
            )));
            before = detail.messages.first().map(|m| m.id);
            for message in detail.messages {
                assert_eq!(message.thread_id, thread.id);
                for artifact_id in message.artifact_ids {
                    storage.artifact(artifact_id).await.unwrap();
                }
            }
            if !detail.has_more {
                break;
            }
            assert!(before.is_some());
        }
    }
    let artifacts = storage.artifacts(None).await.unwrap();
    for artifact in &artifacts {
        storage.artifact(artifact.id).await.unwrap();
    }
    let history_id = storage.history_id().await.unwrap();
    drop(storage);
    let mut config = crate::tests::test_config(&path);
    config.agent = AgentMode::Lash;
    config.anthropic_api_key = Some("copy-proof-no-real-credential".into());
    let state = crate::build_state(config).await.unwrap();
    let AgentBackend::Threaded(registry) = state.agent.backend.as_ref() else {
        panic!("current store requires independent Thread registry");
    };
    registry.capacity.close();
    assert!(registry.opened().await.is_empty());
    assert!(!path.join("thread-runtime").exists());
    state.agent.readiness().unwrap();
    assert_eq!(state.storage.history_id().await.unwrap(), history_id);
    assert!(
        state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(state.storage.artifacts(None).await.unwrap(), artifacts);
    assert!(
        state
            .broadcast_log
            .recent()
            .iter()
            .all(|frame| !matches!(frame, HostToClient::TurnEvent { .. }))
    );
}
