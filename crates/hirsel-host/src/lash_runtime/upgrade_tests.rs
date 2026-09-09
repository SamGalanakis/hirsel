use super::*;

#[tokio::test]
async fn lash_runtime_boots_with_fresh_sqlite_stores_and_typescript_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::tests::test_config(dir.path());
    config.agent = AgentMode::Lash;
    // Boot constructs the provider but does not make an inference request.
    config.anthropic_api_key = Some("test-key-no-inference".to_string());
    let state = crate::build_state(config).await.unwrap();
    let AgentBackend::Lash(runtime) = state.agent.backend.as_ref() else {
        panic!("expected the real Lash runtime");
    };
    state.agent.readiness().unwrap();
    let manifests = runtime
        .session
        .admin()
        .tools()
        .active_manifests()
        .await
        .unwrap();
    assert!(
        manifests
            .iter()
            .any(|manifest| manifest.name == "subagents_wait")
    );
    assert!(dir.path().join("lash/sessions/durable-core.db").is_file());
    let snapshot = runtime.session.admin().state().export().await;
    assert_eq!(
        lash_protocol_rlm::rlm_session_dialect(&snapshot.protocol_turn_options).unwrap(),
        RlmDialect::Typescript,
    );

    // Exercise the executor's new facade-based wait against a real SQLite
    // terminal event, without starting a driver or making a provider call.
    let process_id = "upgrade-wait";
    let mut terminal = terminal_event_type(SUBAGENT_COMPLETED, ProcessStatus::Completed);
    terminal.semantics.wake = None;
    runtime
        .core
        .processes()
        .start(
            ProcessStartRequest::external(process_id, ProcessOriginator::host(), json!({}))
                .with_event_types(vec![terminal]),
            inline_trigger_scope("upgrade-test-start"),
        )
        .await
        .unwrap();
    let (event_type, payload) = terminal_event_payload(&TerminalOutcome::Done {
        summary: "persisted completion".into(),
    });
    runtime
        .core
        .process_registry()
        .unwrap()
        .append_event(
            process_id,
            ProcessEventAppendRequest::new(event_type, payload)
                .with_replay_key("upgrade-wait-completion"),
        )
        .await
        .unwrap();
    let executor = HirselToolExecutor {
        tools: runtime.tools.clone(),
        anchors: Arc::clone(&runtime.anchors),
        runtime: Arc::new(std::sync::OnceLock::from(Arc::downgrade(runtime))),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        executor.subagents_wait(&json!({ "process_id": process_id })),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        result["outcome"],
        json!({ "type": "success", "value": { "summary": "persisted completion" } })
    );
}

#[test]
fn driver_terminal_events_roundtrip_to_plain_tool_outcomes() {
    for (terminal, expected) in [
        (
            TerminalOutcome::Done {
                summary: "complete output".into(),
            },
            json!({"type":"success","value":{"summary":"complete output"}}),
        ),
        (
            TerminalOutcome::Failed {
                reason: "failed output".into(),
            },
            json!({"type":"failure","class":"execution","code":"subagent_failed","message":"failed output","raw":{"reason":"failed output"}}),
        ),
        (
            TerminalOutcome::Interrupted,
            json!({"type":"cancelled","message":"Sub-agent was interrupted.","raw":null}),
        ),
    ] {
        let (_, payload) = terminal_event_payload(&terminal);
        let outcome: ProcessAwaitOutput =
            serde_json::from_value(payload["await_output"].clone()).unwrap();
        let projected = subagents_wait_result("process-1", &outcome).unwrap();
        assert_eq!(projected["outcome"], expected);
        let schema = subagents_wait_output_schema();
        let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
        assert!(validator.is_valid(&projected), "{projected}");
    }
}
