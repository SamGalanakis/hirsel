use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use hirsel_proto::TurnEventKind;
use lash::{TurnInput, provider::ReasoningSelection, rlm::RlmTurnBuilderExt};
use lash_core::{LlmOutputPart, llm::types::LlmResponse};

use super::*;

#[tokio::test]
async fn native_worker_rlm_persists_code_tool_activity_and_final_message() {
    let (executor, storage, _log, dir) = super::super::tests::test_event_executor().await;
    let route = executor.anchors.lock().await.active.clone().unwrap();
    let thread_id = route.thread_id;
    let turn_id = route.thread_turn_id;
    let fixture_dir = dir.path().join("fixture");
    std::fs::create_dir(&fixture_dir).unwrap();
    std::fs::write(fixture_dir.join("probe.txt"), "opened-session-probe\n").unwrap();
    let requests = Arc::new(AtomicU64::new(0));
    let provider = lash_core::testing::TestProvider::builder()
        .kind("hirsel-native-rlm-timeline")
        .complete({
            let requests = Arc::clone(&requests);
            move |request| {
                requests.fetch_add(1, Ordering::SeqCst);
                let prompt = serde_json::to_string(&request).unwrap();
                assert!(prompt.contains("<typescript>"));
                assert!(prompt.contains("Triggers and processes"));
                async move {
                    Ok(LlmResponse {
                        parts: vec![LlmOutputPart::Text {
                            text: r#"<typescript>
await files.read({ path: "probe.txt" });
finish("native read completed");
</typescript>"#
                                .into(),
                            response_meta: None,
                        }],
                        ..LlmResponse::default()
                    })
                }
            }
        })
        .build()
        .into_handle();
    let model = lash::ModelSpec::builder("native-rlm-timeline-model")
        .variant(ReasoningSelection::ProviderDefault)
        .context_window_tokens(200_000)
        .build()
        .unwrap();
    let coding_tools = Arc::new(NativeCodingTools::new(fixture_dir).unwrap());
    let core = super::super::native_worker_protocol::build_native_worker_core(
        &dir.path().join("native-runtime"),
        provider,
        model,
        Arc::clone(&coding_tools),
        lash_core::LeaseOwnerIdentity::opaque("hirsel-native-rlm-timeline", "one"),
    )
    .await
    .unwrap();
    let guidance = native_worker_guidance(dir.path(), None);
    let session = core
        .session("native-rlm-timeline")
        .plugin_option(
            RLM_PROTOCOL_PLUGIN_ID,
            RlmCreateExtras {
                dialect: Some(AGENT_RLM_DIALECT),
                ..RlmCreateExtras::default()
            },
        )
        .unwrap()
        .prompt_contribution(lash::prompt::PromptContribution::guidance(
            "Hirsel native coding worker",
            guidance,
        ))
        .open()
        .await
        .unwrap();
    let tool_names = session
        .observe()
        .active_tool_manifests()
        .into_iter()
        .map(|manifest| manifest.name)
        .collect::<Vec<_>>();
    ensure_native_rlm_surface(&tool_names).unwrap();
    let history_id = storage.history_id().await.unwrap();
    let sink = NativeTimelineSink::new(
        &history_id,
        thread_id,
        turn_id,
        executor.tools.clone(),
        json!({"agent":"lash"}),
    );
    let report = session
        .turn(TurnInput::text("Read probe.txt and report completion."))
        .require_finish()
        .unwrap()
        .turn_id("native-rlm-timeline-turn")
        .stream_to(&sink)
        .await
        .unwrap();
    sink.finish().await;
    let output = lash::TurnOutput {
        result: report,
        activities: sink.activities().await,
    };
    let (outcome, final_text, tool_calls) = lash_terminal_projection(Some(&output));
    let projection = TurnIngest::complete(
        &executor.tools,
        &history_id,
        turn_id,
        outcome,
        final_text,
        tool_calls,
    )
    .await
    .unwrap();
    assert_eq!(projection.state, ThreadTurnState::Completed);

    let detail = storage.thread_detail(thread_id, None, 100).await.unwrap();
    let events = &detail
        .turn_timelines
        .iter()
        .find(|timeline| timeline.turn_id == turn_id)
        .expect("native worker turn timeline")
        .events;
    let ordered = events
        .iter()
        .filter_map(|event| match &event.event {
            TurnEventKind::CodeStart { .. } => Some("CodeStart"),
            TurnEventKind::ToolStart { .. } => Some("ToolStart"),
            TurnEventKind::ToolDone { .. } => Some("ToolDone"),
            TurnEventKind::CodeDone { .. } => Some("CodeDone"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ordered, ["CodeStart", "ToolStart", "ToolDone", "CodeDone"]);
    assert!(detail.activities.iter().any(|activity| {
        activity.turn_id == Some(turn_id)
            && activity.kind == "tool_completed"
            && activity.data["name"] == "read"
    }));
    assert!(detail.messages.iter().any(|message| {
        message.body == "native read completed"
            && message
                .tool_calls
                .iter()
                .any(|call| call.name == "read" && call.ok)
    }));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    coding_tools.shutdown().await;
}
