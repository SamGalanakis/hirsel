use super::*;

#[cfg(unix)]
#[tokio::test]
async fn history_reset_reaps_an_owned_native_shell_command() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let AgentBackend::Threaded(registry) = state.agent.backend.as_ref() else {
        panic!("Thread registry")
    };
    let tools = Arc::new(
        crate::native_coding_tools::NativeCodingTools::new(dir.path().to_path_buf()).unwrap(),
    );
    let work = super::native_worker::NativeWorkerTurn::new(1);
    work.install_active_tools_for_test(tools.clone()).await;
    registry.install_native_turn_for_test(1, work).await;

    let prepared = lash_core::PreparedToolCall::from_parts(
        "history-reset-call",
        "hirsel:native-coding:exec-command:v1",
        "exec_command",
        serde_json::json!({
            "cmd": "sh -c 'echo $$ > history-reset.pid; sleep 0.5; : > history-reset-late; exec sleep 30' >/dev/null 2>&1 & wait",
            "timeout_ms": 30000
        }),
        None,
        Value::Null,
    );
    let effect_controller = lash_core::ScopedEffectController::shared(
        Arc::new(
            lash::runtime::NativeRuntimeEffectController::default()
                .allow_process_lifetime_completion_keys(),
        ),
        lash_core::ExecutionScope::runtime_operation("native-tools-history-reset-test"),
    )
    .unwrap();
    let provider: Arc<dyn lash::tools::ToolProvider> = tools;
    let running = tokio::spawn(async move {
        lash_core::testing::coordinate_tool_provider_with_services(
            effect_controller,
            Arc::new(lash_core::testing::MockSessionManager::default()),
            "native-tools-history-reset-session",
            crate::native_coding_tools::exec_definition_for_test(),
            provider,
            prepared,
        )
        .await
        .unwrap()
        .output
    });

    let pid_path = dir.path().join("history-reset.pid");
    for _ in 0..200 {
        if pid_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(pid_path.exists(), "native command did not publish its pid");
    let pid = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();

    state.agent.reset_history().await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("history reset must join the native shell")
        .expect("tool caller");
    assert!(matches!(
        output.outcome,
        lash_core::ToolCallOutcome::Cancelled(_)
    ));
    assert!(
        native_test_process_is_terminated(pid),
        "native shell descendant survived reset"
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !dir.path().join("history-reset-late").exists(),
        "native shell descendant wrote after reset returned"
    );
}

#[cfg(unix)]
fn native_test_process_is_terminated(pid: i32) -> bool {
    // SAFETY: signal zero only probes the test-owned PID read from the fixture.
    if unsafe { libc::kill(pid, 0) } == -1 {
        return true;
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    matches!(
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().next()),
        Some("Z" | "X")
    )
}

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
            hirsel_proto::ThreadKind::Task,
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
            hirsel_proto::ThreadKind::Task,
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
