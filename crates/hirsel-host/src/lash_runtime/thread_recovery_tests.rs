use super::*;
use hirsel_proto::{ChatAuthor, ThreadAttention, ThreadTurnState};

async fn runtime_fixture() -> (crate::AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    config.anthropic_api_key = Some("test-key-no-inference".into());
    let state = crate::build_state(config).await.unwrap();
    let AgentBackend::Threaded(registry) = state.agent.backend.as_ref() else {
        panic!("registry")
    };
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
            &Value::Null,
            ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap();
    let accepted = state
        .submit_thread_message(
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
        .submit_thread_message(
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
        .submit_thread_message(
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
            .submit_thread_message(
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
            .submit_thread_message(
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
            .submit_thread_message(
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
            &instrument,
            ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap();
    state
        .handle_thread_action(
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
        .create_thread(key, key, "", &Value::Null, ThreadAttention::Quiet, None)
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
        .enqueue(owner_turn_input(&turn).await.unwrap())
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
        .enqueue(owner_turn_input(&turn).await.unwrap())
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
            .cancel_thread_turn(next.thread_id)
            .await
            .is_err()
    );
    state
        .agent
        .cancel_thread_turn(turn.thread_id)
        .await
        .unwrap();
    let id = runtime.active_turn_id.lock().await.clone().unwrap();
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

#[test]
fn delayed_observations_route_without_retaining_completed_turns() {
    assert_eq!(
        super::bridges::observation_thread_route("host-queue-drain:100:3:thread:8:turn:9"),
        Some((8, 9))
    );
    assert_eq!(
        super::bridges::observation_thread_route("host-queue-drain:100:4:thread:2:turn:11"),
        Some((2, 11))
    );
    assert_eq!(
        super::bridges::observation_thread_route("host-queue-drain:100:3:thread:8:turn:9"),
        Some((8, 9))
    );
    assert_eq!(super::bridges::observation_thread_route("unowned"), None);
    assert_eq!(
        super::bridges::observation_thread_route("host-queue-drain:100:3:thread:8:turn:9:extra"),
        None
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
    let id = runtime.active_turn_id.lock().await.clone().unwrap();
    assert_eq!(
        super::bridges::observation_thread_route(&id),
        Some((runtime.thread_id, route.thread_turn_id.unwrap()))
    );
    runtime.cancel_turn().await.unwrap();
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
        .enqueue(owner_turn_input(&turn).await.unwrap())
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
            &Value::Null,
            ThreadAttention::Quiet,
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
                mime: "text/html".into(),
                filename: None,
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
    let AgentBackend::Threaded(registry) = state.agent.backend.as_ref() else {
        panic!("registry")
    };
    let id = match id {
        Some(id) => id,
        None => {
            state
                .storage
                .create_thread(
                    "fixture-runtime",
                    "Fixture",
                    "",
                    &Value::Null,
                    ThreadAttention::Quiet,
                    None,
                )
                .await
                .unwrap()
                .0
                .id
        }
    };
    let lane = registry.lane(id).await.unwrap();
    let AgentBackend::Lash(runtime) = lane.as_ref() else {
        panic!("Lash lane")
    };
    runtime.clone()
}
