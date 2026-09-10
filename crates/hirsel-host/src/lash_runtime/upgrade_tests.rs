use super::*;
#[tokio::test]
async fn lash_sessions_are_lazy_thread_local_and_current_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    config.anthropic_api_key = Some("test-key-no-inference".into());
    let state = crate::build_state(config).await.unwrap();
    let AgentBackend::Threaded(registry) = state.agent.backend.as_ref() else {
        panic!("Thread registry")
    };
    registry.capacity.close();
    assert!(registry.opened().await.is_empty());
    assert!(state.storage.thread_snapshot().await.unwrap().is_empty());
    let thread = state
        .storage
        .create_thread(
            "fixture-Ordinary root",
            "Ordinary root",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let lane = registry.lane(thread.id).await.unwrap();
    let AgentBackend::Lash(runtime) = lane.as_ref() else {
        panic!("Lash lane")
    };
    let manifests = runtime
        .session
        .admin()
        .tools()
        .active_manifests()
        .await
        .unwrap();
    assert!(manifests.iter().any(|m| m.name == "threads_delegate"));
    assert!(!manifests.iter().any(|m| m.name.starts_with("subagents_")));
    let snapshot = runtime.session.admin().state().export().await;
    assert_eq!(
        lash_protocol_rlm::rlm_session_dialect(&snapshot.protocol_turn_options).unwrap(),
        RlmDialect::Typescript
    );
    assert!(!dir.path().join("lash/sessions/durable-core.db").exists());
    state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "before-reset".into(),
            thread.id,
            "private old history".into(),
            vec![],
            vec![],
            SendMode::NextTurn,
            vec![],
        )
        .await
        .unwrap();
    runtime.admit_next_thread_request().await.unwrap();
    assert_eq!(
        runtime.session.pending_turn_inputs().await.unwrap().len(),
        1
    );
    state.agent.reset_history().await.unwrap();
    assert!(registry.opened().await.is_empty());
    let fresh = state
        .storage
        .create_thread(
            "fresh",
            "Fresh",
            "",
            &Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    assert_eq!(fresh.id, thread.id);
    let new_lane = registry.lane(fresh.id).await.unwrap();
    let AgentBackend::Lash(new_runtime) = new_lane.as_ref() else {
        panic!("fresh Lash lane")
    };
    assert_ne!(runtime.session_id, new_runtime.session_id);
    assert_ne!(runtime.history_id, new_runtime.history_id);
    assert!(
        new_runtime
            .session
            .pending_turn_inputs()
            .await
            .unwrap()
            .is_empty()
    );
    state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "after-reset".into(),
            fresh.id,
            "new history input".into(),
            vec![],
            vec![],
            SendMode::NextTurn,
            vec![],
        )
        .await
        .unwrap();
    new_runtime.admit_next_thread_request().await.unwrap();
    let pending =
        serde_json::to_string(&new_runtime.session.pending_turn_inputs().await.unwrap()).unwrap();
    assert!(pending.contains("new history input"));
    assert!(!pending.contains("private old history"));
}
