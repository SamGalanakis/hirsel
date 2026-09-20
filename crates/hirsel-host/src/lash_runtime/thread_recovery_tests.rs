use super::*;
use hirsel_proto::{ChatAuthor, ThreadAttention, ThreadTurnState};

pub(super) async fn runtime_fixture() -> (crate::AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    config.anthropic_api_key = Some("test-key-no-inference".into());
    let state = crate::build_state(config).await.unwrap();
    let registry = &state.agent.registry;
    // Closed capacity makes automatic provider dispatch impossible; tests drive
    // durable admission manually and only drain already-cancelled inputs.
    registry.capacity.close();
    (state, dir)
}

fn install_test_skill(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("skills/check/SKILL.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "---\nname: check\ndescription: Check the requested work.\n---\nRead references/checklist.md before checking.\n").unwrap();
    path
}

#[tokio::test]
async fn skill_submission_captures_instructions_and_retries_after_removal() {
    let (state, dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let path = install_test_skill(dir.path());
    let guidance = state.prompts.agent_guidance();
    assert!(guidance.contains("Check the requested work."));
    assert!(!guidance.contains("Read references/checklist.md before checking."));
    let (thread, _) = state
        .storage
        .create_thread(
            "skill-work",
            "Review",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let accepted = state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "skill-request".into(),
            thread.id,
            "/skill:check inspect this diff".into(),
            vec![],
            vec![],
            SendMode::NextTurn,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.message.body, "/skill:check inspect this diff");
    let requests = state.storage.pending_thread_requests().await.unwrap();
    let original = requests
        .iter()
        .find(|(id, _)| id == "skill-request")
        .unwrap()
        .1
        .clone();
    let turn: OwnerTurn = serde_json::from_value(original.clone()).unwrap();
    assert!(
        turn.body
            .contains("Read references/checklist.md before checking.")
    );
    assert!(turn.body.ends_with("inspect this diff"));
    assert_eq!(turn.thread_id, thread.id);
    assert_eq!(turn.mode, SendMode::NextTurn);
    std::fs::remove_file(path).unwrap();
    let retried = state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "skill-request".into(),
            thread.id,
            "/skill:check inspect this diff".into(),
            vec![],
            vec![],
            SendMode::NextTurn,
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(!retried.inserted);
    assert_eq!(retried.message.id, accepted.message.id);
    let requests = state.storage.pending_thread_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .find(|(id, _)| id == "skill-request")
            .unwrap()
            .1,
        original
    );
    // Reopening storage recovers the captured body without access to SKILL.md.
    let reopened = crate::storage::Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened
            .pending_thread_requests()
            .await
            .unwrap()
            .iter()
            .find(|(id, _)| id == "skill-request")
            .unwrap()
            .1,
        original
    );
}

#[tokio::test]
async fn skill_commands_cover_addressed_input_and_reject_unknown_before_acceptance() {
    let (state, dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let path = install_test_skill(dir.path());
    let accepted = state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "owner-skill".into(),
            runtime.thread_id,
            "/skill:check check current input".into(),
            vec![],
            vec![],
            SendMode::Send,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.message.body, "/skill:check check current input");
    let pending = state.storage.pending_thread_requests().await.unwrap();
    assert!(
        pending
            .iter()
            .find(|(id, _)| id == "owner-skill")
            .unwrap()
            .1["body"]
            .as_str()
            .unwrap()
            .contains("Read references/checklist.md")
    );
    std::fs::remove_file(path).unwrap();
    assert!(
        !state
            .submit_addressed_thread_message(
                &state.storage.history_id().await.unwrap(),
                "owner-skill".into(),
                runtime.thread_id,
                "/skill:check check current input".into(),
                vec![],
                vec![],
                SendMode::Send,
                Vec::new()
            )
            .await
            .unwrap()
            .inserted
    );
    assert!(
        state
            .submit_addressed_thread_message(
                &state.storage.history_id().await.unwrap(),
                "unknown-skill".into(),
                runtime.thread_id,
                "/skill:absent".into(),
                vec![],
                vec![],
                SendMode::Send,
                Vec::new()
            )
            .await
            .is_err()
    );
    assert!(
        state
            .storage
            .message_id_for_client_id("unknown-skill")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .submit_addressed_thread_message(
                &state.storage.history_id().await.unwrap(),
                "unknown-owner-skill".into(),
                runtime.thread_id,
                "/skill:absent".into(),
                vec![],
                vec![],
                SendMode::Send,
                Vec::new()
            )
            .await
            .is_err()
    );
    assert!(
        state
            .storage
            .message_id_for_client_id("unknown-owner-skill")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn generated_action_labels_are_not_skill_commands() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let instrument = json!({"type":"optionList","action":"advance","settles":false,"options":[{"key":"go","label":"/skill:absent"}]});
    let (thread, _) = state
        .storage
        .create_thread(
            "skill-label",
            "Work",
            "",
            Some(&instrument),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            thread.id,
            "advance".into(),
            json!({"choice":"go"}),
            Some(thread.revision),
        )
        .await
        .unwrap();
    let pending = state.storage.pending_thread_requests().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].1["body"], "/skill:absent");
}

async fn request(state: &crate::AppState, key: &str) -> OwnerTurn {
    let (thread, _) = state
        .storage
        .create_thread(
            key,
            key,
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let (_message, _) = state
        .storage
        .append_thread_owner_request(
            &state.storage.history_id().await.unwrap(),
            thread.id,
            key,
            format!("message {key}"),
            &[],
            &[],
            &[],
            &json!({"mode":"send","thread_action":null}),
        )
        .await
        .unwrap();
    serde_json::from_value(state.storage.thread_request(key).await.unwrap().unwrap()).unwrap()
}

#[tokio::test]
async fn accepted_lash_input_survives_admission_retry_with_changed_thread_context() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let turn = request(&state, "accepted").await;
    let runtime = runtime_lane(&state, Some(turn.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    // Fault boundary: Lash accepted the input, Hirsel has not marked its turn running.
    runtime
        .session
        .enqueue(owner_turn_input(&turn, &state.storage).await.unwrap())
        .id(turn.client_id.clone())
        .ingress(TurnInputIngress::next_turn())
        .send()
        .await
        .unwrap();
    state
        .storage
        .update_thread(
            turn.thread_id,
            Some("Renamed after acceptance"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime.admit_next_thread_request().await.unwrap(),
        Some(turn.client_id)
    );
    assert_eq!(
        runtime.session.pending_turn_inputs().await.unwrap().len(),
        1
    );
    let detail = state
        .storage
        .thread_detail(turn.thread_id, None, 30)
        .await
        .unwrap();
    assert_eq!(detail.turns.len(), 1);
    assert_eq!(detail.turns[0].state, ThreadTurnState::Running);
}

#[tokio::test]
async fn cancelling_between_lash_acceptance_and_hirsel_admission_removes_both_queues() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let turn = request(&state, "cancel-before-admit").await;
    let runtime = runtime_lane(&state, Some(turn.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    runtime
        .session
        .enqueue(owner_turn_input(&turn, &state.storage).await.unwrap())
        .id(turn.client_id.clone())
        .ingress(TurnInputIngress::next_turn())
        .send()
        .await
        .unwrap();
    assert_eq!(
        runtime
            .cancel_thread_request(&turn.client_id)
            .await
            .unwrap(),
        CancelQueuedResult::Cancelled
    );
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
    assert_eq!(
        state
            .storage
            .thread_detail(turn.thread_id, None, 30)
            .await
            .unwrap()
            .turns[0]
            .state,
        ThreadTurnState::Cancelled
    );
}

#[tokio::test]
async fn stop_after_admission_before_dispatch_cancels_exact_thread_without_provider() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let turn = request(&state, "stop-before-dispatch").await;
    let runtime = runtime_lane(&state, Some(turn.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    let next = request(&state, "must-remain-queued").await;
    runtime.admit_next_thread_request().await.unwrap();
    assert!(
        state
            .agent
            .cancel_thread_turn(&next.history_id, next.thread_id)
            .await
            .is_err()
    );
    state
        .agent
        .cancel_thread_turn(&turn.history_id, turn.thread_id)
        .await
        .unwrap();
    let id = runtime.anchors.lock().await.drain_id.clone().unwrap();
    let output = tokio::time::timeout(Duration::from_secs(5), runtime.run_admitted_drain(&id))
        .await
        .unwrap()
        .unwrap()
        .expect("cancelled drain settles");
    assert!(matches!(
        output.result.outcome,
        lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. })
    ));
    runtime
        .finish_thread_request(&turn.client_id, Some(&output))
        .await
        .unwrap();
    assert!(
        runtime
            .session
            .pending_turn_inputs()
            .await
            .unwrap()
            .is_empty()
    );
    let requests = state.storage.pending_thread_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, next.client_id);
    assert_eq!(
        state
            .storage
            .thread_detail(turn.thread_id, None, 30)
            .await
            .unwrap()
            .turns[0]
            .state,
        ThreadTurnState::Cancelled
    );
}

#[tokio::test]
async fn unowned_inputs_are_removed_before_any_thread_can_drain() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let turn = request(&state, "owned").await;
    let runtime = runtime_lane(&state, Some(turn.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    for id in ["orphan-input", "owned"] {
        runtime
            .session
            .enqueue(TurnInput::text(id))
            .id(id)
            .ingress(TurnInputIngress::next_turn())
            .send()
            .await
            .unwrap();
    }
    runtime.reconcile_unowned_inputs().await.unwrap();
    let pending = runtime.session.pending_turn_inputs().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].source_key.as_deref(),
        Some(owner_turn_source_key(&turn.client_id).as_str())
    );
}

#[tokio::test]
async fn later_queued_message_is_not_in_earlier_thread_history() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let turn = request(&state, "earlier").await;
    let runtime = runtime_lane(&state, Some(turn.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    state
        .storage
        .append_thread_chat(
            turn.thread_id,
            ChatAuthor::Owner,
            "future-private-context",
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    runtime.admit_next_thread_request().await.unwrap();
    let pending = runtime.session.pending_turn_inputs().await.unwrap();
    let input = serde_json::to_string(&pending[0].input).unwrap();
    assert!(!input.contains("future-private-context"));
    assert!(input.contains("message earlier"));
}

#[tokio::test]
async fn background_wake_receipt_survives_delivery_and_rejects_duplicate_turn() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let wake = crate::fork_wake::WakeMessage::new(
        runtime.thread_id,
        crate::fork_wake::WakeSource::External {
            origin: "fixture".into(),
        },
        "finished",
        "fixture:finished",
    );
    runtime
        .enqueue_fork_brief(&wake, "First brief")
        .await
        .unwrap();
    runtime
        .enqueue_fork_brief(&wake, "Retried brief")
        .await
        .unwrap();
    let pending = state.storage.pending_thread_requests().await.unwrap();
    assert_eq!(pending.len(), 1);
    runtime.admit_next_thread_request().await.unwrap();
    let drain_id = runtime.anchors.lock().await.drain_id.clone().unwrap();
    runtime.timeline_commits.record(drain_id).await;
    let output = super::tests::test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "Background result".into(),
        }),
        "Background result",
        Vec::new(),
    );
    runtime
        .finish_thread_request(&pending[0].0, Some(&output))
        .await
        .unwrap();
    runtime
        .finish_thread_request(&pending[0].0, Some(&output))
        .await
        .unwrap();
    runtime
        .enqueue_fork_brief(&wake, "Late duplicate")
        .await
        .unwrap();
    assert!(
        state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .is_empty()
    );
    let detail = state
        .storage
        .thread_detail(runtime.thread_id, None, 30)
        .await
        .unwrap();
    assert_eq!(detail.turns.len(), 1);
    assert_eq!(detail.turns[0].state, ThreadTurnState::Completed);
    assert_eq!(detail.messages.len(), 1);
    assert_eq!(
        detail.turns[0].agent_message_id,
        Some(detail.messages[0].id)
    );
}

#[tokio::test]
async fn late_timeline_failure_wins_before_terminal_reply() {
    let (state, _dir) = runtime_fixture().await;
    let root = runtime_lane(&state, None).await;
    let _root_pump = root.pump_lock.lock().await;
    let request = request(&state, "late-timeline-failure").await;
    let runtime = runtime_lane(&state, Some(request.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    runtime.admit_next_thread_request().await.unwrap();
    let drain_id = runtime.anchors.lock().await.drain_id.clone().unwrap();
    let turn_id = runtime
        .anchors
        .lock()
        .await
        .active
        .as_ref()
        .unwrap()
        .thread_turn_id;
    let output = super::tests::test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "must not be published".into(),
        }),
        "must not be published",
        Vec::new(),
    );
    let worker = runtime.clone();
    let client_id = request.client_id.clone();
    let mut completion = tokio::spawn(async move {
        worker
            .finish_thread_request(&client_id, Some(&output))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut completion)
            .await
            .is_err(),
        "terminal projection must wait for the observation commit"
    );
    state
        .tools
        .fail_turn_timeline_integrity(turn_id, "injected late timeline persistence failure")
        .await;
    runtime.timeline_commits.record(drain_id).await;
    completion.await.unwrap().unwrap();

    let detail = state
        .storage
        .thread_detail(request.thread_id, None, 30)
        .await
        .unwrap();
    let turn = detail.turns.iter().find(|turn| turn.id == turn_id).unwrap();
    assert_eq!(turn.state, ThreadTurnState::Failed);
    assert!(turn.agent_message_id.is_none());
    assert!(
        state
            .tools
            .turn_timeline_integrity_failure(turn_id)
            .is_none()
    );
    assert!(
        detail
            .messages
            .iter()
            .all(|message| message.body != "must not be published")
    );
    assert!(state.broadcast_log.recent().iter().all(|frame| {
        !matches!(frame, HostToClient::ThreadTurn { turn } if turn.id == turn_id && turn.state == ThreadTurnState::Completed)
    }));
}

fn remote_observation_event(
    turn_id: &str,
    event: RemoteSessionObservationEventPayload,
) -> RemoteSessionObservationStreamItem {
    RemoteSessionObservationStreamItem::Event(
        lash::remote::observations::RemoteSessionObservationEvent {
            session_id: "fixture-session".into(),
            replay_incarnation_id: "fixture-replay".into(),
            turn_id: Some(turn_id.into()),
            revision: 1,
            cursor: "fixture-cursor".into(),
            event,
        },
    )
}

fn remote_turn_activity(event: RemoteTurnEvent) -> RemoteSessionObservationEventPayload {
    RemoteSessionObservationEventPayload::TurnActivity {
        activity: Box::new(lash::remote::usage::RemoteTurnActivity {
            sequence: 1,
            id: "fixture-activity".into(),
            correlation_id: "fixture-turn".into(),
            event,
        }),
    }
}

fn remote_observation_gap() -> RemoteSessionObservationStreamItem {
    RemoteSessionObservationStreamItem::Gap {
        observation: lash::remote::observations::RemoteSessionObservation {
            session_id: "fixture-session".into(),
            cursor: "latest-cursor".into(),
            turn_index: 1,
            usage: lash::remote::usage::RemoteUsage::default(),
        },
        gap: lash::remote::observations::RemoteLiveReplayGap {
            session_id: "fixture-session".into(),
            requested_cursor: "stale-cursor".into(),
            latest_cursor: "latest-cursor".into(),
            latest_revision: 1,
            reason: lash::remote::observations::RemoteLiveReplayGapReason::Trimmed,
        },
    }
}

async fn process_observation_fixture(
    runtime: &LashAgentRuntime,
    timeline: &mut TurnIngest,
    item: RemoteSessionObservationStreamItem,
) {
    assert!(
        super::bridges::process_observation_stream_item::<std::convert::Infallible>(
            Some(Ok(item)),
            &runtime.broadcast_log,
            &runtime.broadcaster,
            &runtime.tools,
            timeline,
            &runtime.timeline_commits,
            &runtime.anchors,
        )
        .await
    );
}

#[tokio::test]
async fn observation_gap_midturn_fails_before_a_later_commit_can_publish_success() {
    let (state, _dir) = runtime_fixture().await;
    let root = runtime_lane(&state, None).await;
    let _root_pump = root.pump_lock.lock().await;
    let request = request(&state, "gap-midturn").await;
    let runtime = runtime_lane(&state, Some(request.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    runtime.admit_next_thread_request().await.unwrap();
    let drain_id = runtime.anchors.lock().await.drain_id.clone().unwrap();
    let turn_id = runtime
        .anchors
        .lock()
        .await
        .active
        .as_ref()
        .unwrap()
        .thread_turn_id;
    let mut timeline = TurnIngest::unrouted(&runtime.history_id, json!({"agent":"native"}));
    process_observation_fixture(
        &runtime,
        &mut timeline,
        remote_observation_event(
            &drain_id,
            remote_turn_activity(RemoteTurnEvent::ReasoningDelta {
                text: "persisted before the gap".into(),
            }),
        ),
    )
    .await;
    process_observation_fixture(&runtime, &mut timeline, remote_observation_gap()).await;
    process_observation_fixture(
        &runtime,
        &mut timeline,
        remote_observation_event(&drain_id, RemoteSessionObservationEventPayload::Committed),
    )
    .await;

    let output = super::tests::test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "must not be published".into(),
        }),
        "must not be published",
        Vec::new(),
    );
    assert!(
        runtime
            .finish_thread_request(&request.client_id, Some(&output))
            .await
            .is_err()
    );
    runtime
        .finish_thread_request(&request.client_id, None)
        .await
        .unwrap();

    let detail = state
        .storage
        .thread_detail(request.thread_id, None, 30)
        .await
        .unwrap();
    let turn = detail.turns.iter().find(|turn| turn.id == turn_id).unwrap();
    assert_eq!(turn.state, ThreadTurnState::Failed);
    assert!(turn.agent_message_id.is_none());
    assert_eq!(detail.turn_timelines[0].events.len(), 1);
    assert!(detail.activities.iter().any(|activity| {
        activity.kind == "execution_failed"
            && activity.data["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("observation replay window"))
    }));
}

#[tokio::test]
async fn observation_gap_across_commit_releases_only_a_failed_terminal_projection() {
    let (state, _dir) = runtime_fixture().await;
    let root = runtime_lane(&state, None).await;
    let _root_pump = root.pump_lock.lock().await;
    let request = request(&state, "gap-across-commit").await;
    let runtime = runtime_lane(&state, Some(request.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    runtime.admit_next_thread_request().await.unwrap();
    let turn_id = runtime
        .anchors
        .lock()
        .await
        .active
        .as_ref()
        .unwrap()
        .thread_turn_id;
    let mut timeline = TurnIngest::unrouted(&runtime.history_id, json!({"agent":"native"}));
    process_observation_fixture(&runtime, &mut timeline, remote_observation_gap()).await;

    let output = super::tests::test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "must not be published".into(),
        }),
        "must not be published",
        Vec::new(),
    );
    let completion = tokio::time::timeout(
        Duration::from_millis(250),
        runtime.finish_thread_request(&request.client_id, Some(&output)),
    )
    .await
    .expect("gap failure must release the commit barrier");
    assert!(completion.is_err());
    runtime
        .finish_thread_request(&request.client_id, None)
        .await
        .unwrap();

    let detail = state
        .storage
        .thread_detail(request.thread_id, None, 30)
        .await
        .unwrap();
    let turn = detail.turns.iter().find(|turn| turn.id == turn_id).unwrap();
    assert_eq!(turn.state, ThreadTurnState::Failed);
    assert!(turn.agent_message_id.is_none());
    assert!(detail.turn_timelines[0].events.is_empty());
}

#[tokio::test]
async fn rlm_observer_retains_integrity_failure_until_recovery_and_failed_terminal() {
    let (state, dir) = runtime_fixture().await;
    let root = runtime_lane(&state, None).await;
    let _root_pump = root.pump_lock.lock().await;
    let failed_request = request(&state, "double-timeline-write-failure").await;
    let runtime = runtime_lane(&state, Some(failed_request.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    runtime.admit_next_thread_request().await.unwrap();
    let drain_id = runtime.anchors.lock().await.drain_id.clone().unwrap();
    let turn_id = runtime
        .anchors
        .lock()
        .await
        .active
        .as_ref()
        .unwrap()
        .thread_turn_id;
    let attempts = state.storage.track_completion_failures().await.unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_timeline_event BEFORE INSERT ON thread_turn_events BEGIN SELECT terminal_delivery_probe(); SELECT RAISE(FAIL,'injected timeline event failure'); END;
         CREATE TRIGGER fail_timeline_terminal BEFORE UPDATE OF finished_at ON thread_turns WHEN NEW.finished_at IS NOT NULL BEGIN SELECT terminal_delivery_probe(); SELECT RAISE(FAIL,'injected timeline terminal failure'); END;",
    )
    .unwrap();

    let mut timeline = TurnIngest::unrouted(&runtime.history_id, json!({"agent":"native"}));
    process_observation_fixture(
        &runtime,
        &mut timeline,
        remote_observation_event(
            &drain_id,
            remote_turn_activity(RemoteTurnEvent::ReasoningDelta {
                text: "lost event".into(),
            }),
        ),
    )
    .await;
    process_observation_fixture(
        &runtime,
        &mut timeline,
        remote_observation_event(&drain_id, RemoteSessionObservationEventPayload::Committed),
    )
    .await;
    assert!(
        attempts.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "both the event append and terminal failure write must reach SQLite"
    );
    assert_eq!(
        state.storage.thread_turn(turn_id).await.unwrap().state,
        ThreadTurnState::Running
    );
    assert!(
        state
            .tools
            .turn_timeline_integrity_failure(turn_id)
            .is_some_and(|reason| reason.contains("timeline event failure"))
    );
    conn.execute_batch("DROP TRIGGER fail_timeline_event; DROP TRIGGER fail_timeline_terminal;")
        .unwrap();

    let output = super::tests::test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "must not be published".into(),
        }),
        "must not be published",
        Vec::new(),
    );
    runtime
        .finish_thread_request(&failed_request.client_id, Some(&output))
        .await
        .unwrap();

    let detail = state
        .storage
        .thread_detail(failed_request.thread_id, None, 30)
        .await
        .unwrap();
    let turn = detail.turns.iter().find(|turn| turn.id == turn_id).unwrap();
    assert_eq!(turn.state, ThreadTurnState::Failed);
    assert!(turn.agent_message_id.is_none());
    assert!(
        state
            .tools
            .turn_timeline_integrity_failure(turn_id)
            .is_none()
    );
    assert!(detail.turn_timelines[0].events.is_empty());
    assert!(detail.activities.iter().any(|activity| {
        activity.kind == "execution_failed"
            && activity.data["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("timeline event failure"))
    }));
    assert!(state.broadcast_log.recent().iter().all(|frame| {
        !matches!(frame, HostToClient::ThreadTurn { turn } if turn.id == turn_id && turn.state == ThreadTurnState::Completed)
            && !matches!(frame, HostToClient::Msg { message } if message.body == "must not be published")
    }));

    let unaffected = request(&state, "unaffected-after-timeline-failure").await;
    let unaffected_runtime = runtime_lane(&state, Some(unaffected.thread_id)).await;
    let _unaffected_pump = unaffected_runtime.pump_lock.lock().await;
    unaffected_runtime
        .admit_next_thread_request()
        .await
        .unwrap();
    let unaffected_drain = unaffected_runtime
        .anchors
        .lock()
        .await
        .drain_id
        .clone()
        .unwrap();
    unaffected_runtime
        .timeline_commits
        .record(unaffected_drain)
        .await;
    let unaffected_output = super::tests::test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "unaffected success".into(),
        }),
        "unaffected success",
        Vec::new(),
    );
    unaffected_runtime
        .finish_thread_request(&unaffected.client_id, Some(&unaffected_output))
        .await
        .unwrap();
    assert_eq!(
        state
            .storage
            .thread_turn(unaffected.turn_id.unwrap())
            .await
            .unwrap()
            .state,
        ThreadTurnState::Completed
    );
}

#[tokio::test]
async fn background_runtime_drain_has_durable_turn_and_tagged_identity() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    // Exercise the native queued-work drain without a Hirsel Owner request.
    runtime
        .session
        .enqueue(TurnInput::text("background wake"))
        .id("background-test")
        .send()
        .await
        .unwrap();
    assert!(runtime.activate_background_turn().await.unwrap());
    let route = runtime.anchors.lock().await.active.clone().unwrap();
    assert_eq!(route.thread_id, runtime.thread_id);
    assert!(route.request_id.is_none());
    let id = runtime.anchors.lock().await.drain_id.clone().unwrap();
    assert_eq!(
        super::bridges::observation_thread_route(&id),
        Some((runtime.thread_id, route.thread_turn_id))
    );
    runtime.cancel_owned_turn(None, None).await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(5), runtime.run_admitted_drain(&id))
        .await
        .unwrap()
        .unwrap()
        .expect("cancelled background drain");
    runtime.finish_active_thread(Some(&output)).await.unwrap();
    let detail = state
        .storage
        .thread_detail(runtime.thread_id, None, 30)
        .await
        .unwrap();
    assert_eq!(detail.turns.len(), 1);
    assert_eq!(detail.turns[0].state, ThreadTurnState::Cancelled);
}

#[tokio::test]
async fn cancellation_intent_survives_crash_before_lash_cleanup() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let turn = request(&state, "cancel-intent").await;
    let runtime = runtime_lane(&state, Some(turn.thread_id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    runtime
        .session
        .enqueue(owner_turn_input(&turn, &state.storage).await.unwrap())
        .id(turn.client_id.clone())
        .ingress(TurnInputIngress::next_turn())
        .send()
        .await
        .unwrap();
    let queued = state
        .storage
        .queue_thread_turn(turn.thread_id, turn.message_id)
        .await
        .unwrap();
    // Crash after Hirsel recorded cancellation, before Lash acknowledged it.
    state
        .storage
        .finish_thread_turn(queued.id, ThreadTurnState::Cancelled, None)
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
async fn artifact_reference_identity_reaches_the_next_turn_context() {
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let (thread, _) = state
        .storage
        .create_thread(
            "artifact-context",
            "Result discussion",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let (artifact, _) = state
        .storage
        .publish_artifact_human(
            "context-fixture",
            &json!({"create":"diagram"}),
            thread.id,
            None,
            Some(crate::storage::ArtifactDraft {
                title: "Diagram".into(),
                kind: hirsel_proto::ArtifactKind::Html,
                content: "<p>Result</p>".into(),
                expected_content: None,
            }),
        )
        .await
        .unwrap();
    request(&state, "artifact-context").await;
    let runtime = runtime_lane(&state, Some(thread.id)).await;
    let _turn_pump = runtime.pump_lock.lock().await;
    runtime.admit_next_thread_request().await.unwrap();
    let pending = runtime.session.pending_turn_inputs().await.unwrap();
    let input = serde_json::to_value(&pending[0].input).unwrap();
    let encoded = serde_json::to_string(&input).unwrap();
    assert!(
        encoded.contains(&format!("\\\"artifact_ids\\\":[{}]", artifact.summary.id)),
        "{encoded}"
    );
}

pub(super) async fn runtime_lane(
    state: &crate::AppState,
    id: Option<u64>,
) -> Arc<LashAgentRuntime> {
    let registry = &state.agent.registry;
    let id = match id {
        Some(id) => id,
        None => {
            state
                .storage
                .create_thread(
                    "fixture-runtime",
                    "Fixture",
                    "",
                    None,
                    ThreadAttention::Quiet,
                    hirsel_proto::ThreadKind::Task,
                    None,
                )
                .await
                .unwrap()
                .0
                .id
        }
    };
    let lane = registry.lane(id).await.unwrap();
    let LaneRuntime::Lash(runtime) = lane.as_ref() else {
        panic!("Lash lane")
    };
    runtime.clone()
}

#[tokio::test]
async fn solicited_process_deliveries_enqueue_once_without_triage() {
    use hirsel_proto::{MessageOrigin, ProcessOutcome, TriggerLabel};
    let (state, _dir) = runtime_fixture().await;
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    for outcome in [
        ProcessOutcome::Completed,
        ProcessOutcome::Failed,
        ProcessOutcome::Cancelled,
        ProcessOutcome::Woke,
    ] {
        let key = format!("process-test:{outcome:?}");
        let origin = MessageOrigin::Process {
            process_id: "p1".into(),
            name: "wake".into(),
            trigger: TriggerLabel::Timer {
                label: "reminder".into(),
                in_secs: Some(30),
                every_secs: None,
                at: None,
            },
            subscription_key: Some("debug-key".into()),
            outcome,
            result: json!("awake"),
            error: (outcome == ProcessOutcome::Failed).then(|| "failure reason".into()),
        };
        let delivery = crate::storage::ProcessDelivery {
            key: key.clone(),
            thread_id: runtime.thread_id,
            origin: origin.clone(),
        };
        state
            .storage
            .stage_process_delivery(&delivery)
            .await
            .unwrap();
        // Reproduce a restart boundary after appending but before queue acceptance.
        let first = state.storage.deliver_process_message(&key).await.unwrap();
        assert!(first.newly_appended);
        runtime.deliver_process_event(&key).await.unwrap();
        runtime.deliver_process_event(&key).await.unwrap();
        let pending = state.storage.pending_thread_requests().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, format!("process-delivery:{key}"));
        assert!(
            pending[0].1["body"]
                .as_str()
                .unwrap()
                .contains("Solicited process delivery")
        );
        let detail = state
            .storage
            .thread_detail(runtime.thread_id, None, 30)
            .await
            .unwrap();
        let messages = detail
            .messages
            .iter()
            .filter(|m| m.origin.as_ref() == Some(&origin))
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].body,
            if outcome == ProcessOutcome::Failed {
                "failure reason"
            } else {
                "awake"
            }
        );
        runtime.admit_next_thread_request().await.unwrap();
        let inputs = runtime.session.pending_turn_inputs().await.unwrap();
        assert_eq!(inputs.len(), 1);
        assert!(
            serde_json::to_string(&inputs)
                .unwrap()
                .contains("process_id")
        );
        let drain_id = runtime.anchors.lock().await.drain_id.clone().unwrap();
        runtime.timeline_commits.record(drain_id).await;
        let output = super::tests::test_turn_output(
            lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
                text: String::new(),
            }),
            "",
            Vec::new(),
        );
        runtime
            .finish_thread_request(&pending[0].0, Some(&output))
            .await
            .unwrap();
        // Acceptance survives consumption and cannot create a second turn.
        runtime.deliver_process_event(&key).await.unwrap();
        assert!(
            state
                .storage
                .pending_thread_requests()
                .await
                .unwrap()
                .is_empty()
        );
    }
    let detail = state
        .storage
        .thread_detail(runtime.thread_id, None, 30)
        .await
        .unwrap();
    assert_eq!(detail.messages.len(), 4);
    assert_eq!(detail.turns.len(), 4);
    assert!(
        state
            .storage
            .pending_process_deliveries(runtime.thread_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn stop_during_empty_drain_retry_cancels_owned_input_without_provider() {
    let (state, _dir) = runtime_fixture().await;
    let root = runtime_lane(&state, None).await;
    let _root_pump = root.pump_lock.lock().await;
    let turn = request(&state, "stop-drain-retry").await;
    let runtime = runtime_lane(&state, Some(turn.thread_id)).await;
    let _pump = runtime.pump_lock.lock().await;
    runtime.admit_next_thread_request().await.unwrap();
    let drain = runtime.anchors.lock().await.drain_id.clone().unwrap();
    // The exact ownership transition made after an empty drain, with the pump
    // held so the retry cannot run or call a provider before Stop arrives.
    runtime.clear_active_turn_id(&drain).await;
    assert!(runtime.anchors.lock().await.active.is_some());
    runtime
        .cancel_owned_turn(Some(turn.thread_id), turn.turn_id)
        .await
        .unwrap();
    assert!(runtime.anchors.lock().await.active.is_none());
    assert!(
        runtime
            .session
            .pending_turn_inputs()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !state
            .storage
            .pending_thread_requests()
            .await
            .unwrap()
            .iter()
            .any(|(id, _)| id == &turn.client_id)
    );
    assert_eq!(
        state
            .storage
            .thread_detail(turn.thread_id, None, 30)
            .await
            .unwrap()
            .turns[0]
            .state,
        ThreadTurnState::Cancelled
    );
    assert!(runtime.admit_next_thread_request().await.unwrap().is_none());
}

#[tokio::test]
async fn exact_stop_rejects_a_lane_handoff_without_reaching_the_new_turn() {
    let (state, _dir) = runtime_fixture().await;
    let root = runtime_lane(&state, None).await;
    let _root_pump = root.pump_lock.lock().await;
    let first = request(&state, "handoff-first").await;
    let runtime = runtime_lane(&state, Some(first.thread_id)).await;
    let _pump = runtime.pump_lock.lock().await;
    runtime.admit_next_thread_request().await.unwrap();
    let second = request(&state, "handoff-second").await;
    {
        let mut anchors = runtime.anchors.lock().await;
        let active = anchors.active.as_mut().unwrap();
        active.request_id = Some(second.client_id.clone());
        active.thread_turn_id = second.turn_id.unwrap();
    }
    let error = runtime
        .cancel_owned_turn(Some(first.thread_id), first.turn_id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no longer owns"));
    assert_eq!(
        runtime
            .anchors
            .lock()
            .await
            .active
            .as_ref()
            .unwrap()
            .thread_turn_id,
        second.turn_id.unwrap()
    );
    assert_eq!(
        runtime.session.pending_turn_inputs().await.unwrap().len(),
        1,
        "Stop for the completed turn must not reach the newly owned lane"
    );
}

/// A Thread may name its own Native provider. The session opens on the booted
/// one and is rebound when a turn accepted for another provider is admitted —
/// before the input is enqueued, so the turn runs on what the Owner chose and a
/// turn already running keeps the backend it started on.
#[tokio::test]
async fn an_admitted_turn_rebinds_the_session_to_this_threads_native_provider() {
    let (state, _dir) = runtime_fixture().await;
    state
        .providers_roster
        .add(
            "acme",
            "Acme",
            "https://acme.invalid/v1",
            "test-key-no-inference",
            "acme/fast",
        )
        .await
        .unwrap();
    let runtime = runtime_lane(&state, None).await;
    let _pump = runtime.pump_lock.lock().await;
    let history = state.storage.history_id().await.unwrap();
    let booted = runtime.native_provider_id();
    assert_eq!(booted, "anthropic");
    let thread = state
        .storage
        .thread(runtime.thread_id)
        .await
        .unwrap()
        .unwrap();

    let target = hirsel_proto::ThreadExecutionTarget::Native {
        provider_id: "acme".into(),
        model: "acme/deep".into(),
    };
    let chosen = state
        .handle_addressed_thread_action(
            &history,
            thread.id,
            "set_execution".into(),
            json!({ "execution": target }),
            Some(thread.revision),
        )
        .await
        .unwrap();
    assert_eq!(chosen.execution, Some(target));
    // Stored, not applied: the running session keeps the provider it opened on
    // until a turn accepted for the new one is admitted.
    assert_eq!(runtime.native_provider_id(), booted);

    state
        .submit_addressed_thread_message(
            &history,
            "native-turn".into(),
            thread.id,
            "run somewhere else".into(),
            vec![],
            vec![],
            SendMode::Send,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .admit_next_thread_request()
            .await
            .unwrap()
            .as_deref(),
        Some("native-turn")
    );

    assert_eq!(runtime.native_provider_id(), "acme");
    assert_eq!(runtime.session.policy_snapshot().model.id, "acme/deep");
    // The live session config, not just the host's own bookkeeping, names it.
    let expected_kind = openai_compatible_handle(
        "unused-in-this-assertion".to_string(),
        "https://acme.invalid/v1".to_string(),
    )
    .kind()
    .to_string();
    assert_eq!(runtime.native_provider().kind(), expected_kind);
    assert_eq!(
        runtime.session.policy_snapshot().recorded_provider_id(),
        expected_kind
    );

    // Back to the Settings default. The booted provider is a boot label, not a
    // roster instance (the legacy `anthropic` mode has no roster entry), so a
    // Thread that clears its preference must still be able to return.
    let default = state.storage.native_execution_default().await.unwrap();
    let crate::storage::ThreadExecution::Native {
        provider_id,
        model,
        cwd,
        ..
    } = default
    else {
        panic!("the configured default execution is a Native backend");
    };
    assert_eq!(provider_id, booted);
    runtime
        .bind_native(&provider_id, model.clone(), &cwd)
        .await
        .unwrap();
    assert_eq!(runtime.native_provider_id(), booted);
    assert_eq!(runtime.session.policy_snapshot().model, model);
}

/// The Owner and the Agent name a Native provider the same way and are refused for
/// the same reasons: an unknown instance, a Sub-agents-only one, and selectors
/// the Native session has no use for.
#[tokio::test]
async fn a_native_target_is_judged_by_the_provider_roster() {
    let (state, _dir) = runtime_fixture().await;
    let history = state.storage.history_id().await.unwrap();
    let (thread, _) = state
        .storage
        .create_thread(
            "native-refusals",
            "Refusals",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    for (execution, expected) in [
        (
            json!({"kind":"native","provider_id":"nonesuch","model":"m"}),
            "unknown provider instance",
        ),
        (
            json!({"kind":"native","provider_id":"claude","model":"m"}),
            "Sub-agents only",
        ),
        (
            json!({"kind":"native","provider_id":"codex","model":"not-a-codex-model"}),
            "is not available on this provider",
        ),
    ] {
        let error = state
            .handle_addressed_thread_action(
                &history,
                thread.id,
                "set_execution".into(),
                json!({ "execution": execution }),
                Some(thread.revision),
            )
            .await
            .expect_err("the roster refuses this Native provider");
        assert!(
            error.to_string().contains(expected),
            "{error} does not explain {expected}"
        );
        assert_eq!(
            state.storage.thread(thread.id).await.unwrap().unwrap(),
            thread
        );
    }
}

/// The regression this file's rebind machinery was missing: the Owner points
/// the main Agent at another provider while the host runs. The default Native
/// route is the Owner's current choice, so a Thread opened after the change
/// opens on it — a boot-frozen label stranded every Thread on the provider the
/// host happened to start with, while Settings and Thread Info reported the new
/// one and every turn failed on a transport the Owner had already left.
#[tokio::test]
async fn a_main_provider_change_repoints_the_default_native_route_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    config.provider = crate::config::ProviderMode::OpenRouter;
    config.model = "deepseek/deepseek-v4.1-flash".into();
    config.openrouter_api_key = Some("test-key-no-inference".into());
    let state = crate::build_state(config).await.unwrap();
    state.agent.registry.capacity.close();
    assert_eq!(
        state.providers_roster.booted_provider_id(),
        Some("openrouter")
    );

    state
        .add_provider(
            "acme",
            "Acme",
            "https://acme.invalid/v1",
            "test-key-no-inference",
            "acme/deep",
        )
        .await
        .unwrap();
    state
        .set_agent_provider(hirsel_proto::AgentSlot::Main, "acme")
        .await
        .unwrap();

    // The default every Thread without its own execution resolves through names
    // the chosen provider and its model...
    let crate::storage::ThreadExecution::Native {
        provider_id, model, ..
    } = state.storage.native_execution_default().await.unwrap()
    else {
        panic!("the configured default execution is a Native backend");
    };
    assert_eq!(provider_id, "acme");
    assert_eq!(model.id, "acme/deep");

    // ...and a Thread opened now runs on exactly that, with no restart between.
    let runtime = runtime_lane(&state, None).await;
    assert_eq!(runtime.native_provider_id(), "acme");
    assert_eq!(runtime.session.policy_snapshot().model.id, "acme/deep");
    assert_eq!(
        runtime.native_provider().kind(),
        openai_compatible_handle(
            "unused-in-this-assertion".to_string(),
            "https://acme.invalid/v1".to_string(),
        )
        .kind()
    );
}
