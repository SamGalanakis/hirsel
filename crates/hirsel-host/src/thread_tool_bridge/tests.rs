use super::*;

async fn rpc(
    bridge: &ThreadToolBridge,
    instance: &str,
    invocation_id: &str,
    request: Value,
) -> anyhow::Result<Value> {
    let stream = UnixStream::connect(&bridge.socket_path).await?;
    let mut frames = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME));
    frames
        .send(serde_json::to_string(&Invocation {
            capability: tokio::fs::read_to_string(&bridge.capability_file).await?,
            bridge_instance: instance.into(),
            invocation_id: invocation_id.into(),
            request,
        })?)
        .await?;
    let frame = tokio::time::timeout(std::time::Duration::from_secs(3), frames.next())
        .await?
        .ok_or_else(|| anyhow::anyhow!("closed"))??;
    Ok(serde_json::from_str(&frame)?)
}
fn call(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}
async fn bridge(state: &crate::AppState) -> ThreadToolBridge {
    let thread = state
        .storage
        .create_thread(
            &uuid::Uuid::new_v4().to_string(),
            "Bridge root",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
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
    ThreadToolBridge::start(
        state.tools.clone(),
        &state.storage.history_id().await.unwrap(),
        turn.id,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn discovery_eof_does_not_revoke_actual_provider_and_receipts_do_not_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let bridge = bridge(&state).await;
    for method in ["initialize", "tools/list"] {
        assert!(
            rpc(
                &bridge,
                "discovery",
                method,
                json!({"jsonrpc":"2.0","id":1,"method":method})
            )
            .await
            .unwrap()
            .get("result")
            .is_some()
        );
    }
    // Each rpc closes the discovery IPC normally, as Claude's preflight does.
    let request = call(
        "threads_create",
        json!({"client_id":"child","title":"Child"}),
    );
    let first = rpc(&bridge, "actual", "create", request.clone())
        .await
        .unwrap();
    assert_eq!(first["result"]["isError"], false, "{first}");
    assert_eq!(
        rpc(&bridge, "actual", "create", request).await.unwrap(),
        first
    );
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 2);
    assert!(
        rpc(
            &bridge,
            "actual",
            "create",
            call(
                "threads_create",
                json!({"client_id":"child","title":"Changed"})
            )
        )
        .await
        .is_err()
    );
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 2);
    let other = state
        .storage
        .create_thread(
            "peer",
            "Private peer",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let denied = rpc(
        &bridge,
        "actual",
        "denied",
        call("threads_read", json!({"thread":other.id,"limit":1})),
    )
    .await
    .unwrap();
    assert_eq!(denied["result"]["isError"], true);
    let calls = bridge.tool_calls().await;
    assert_eq!(
        calls,
        vec![
            hirsel_proto::ToolCallSummary {
                name: "threads_create".into(),
                ok: true
            },
            hirsel_proto::ToolCallSummary {
                name: "threads_read".into(),
                ok: false
            }
        ]
    );
    let events = state
        .broadcast_log
        .recent()
        .into_iter()
        .filter_map(|frame| match frame {
            hirsel_proto::HostToClient::TurnEvent {
                thread_id,
                turn_id,
                seq,
                event,
            } if thread_id == bridge.caller.thread_id && turn_id == bridge.caller.turn_id => {
                Some((seq, event))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        events.len(),
        4,
        "replay must not emit another tool start/done pair"
    );
    assert_eq!(
        events.iter().map(|e| e.0).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    for pair in events.chunks(2) {
        let (
            hirsel_proto::TurnEventKind::ToolStart { id: start, .. },
            hirsel_proto::TurnEventKind::ToolDone { id: done, .. },
        ) = (&pair[0].1, &pair[1].1)
        else {
            panic!("paired tool events")
        };
        assert_eq!(start, done);
    }
    assert!(!bridge.invalidated.is_cancelled());
    assert!(
        rpc(
            &bridge,
            "restarted-provider",
            "new-call",
            call("threads_context", json!({}))
        )
        .await
        .is_err()
    );
    assert!(bridge.invalidated.is_cancelled());
    assert!(state.storage.thread_context(&bridge.caller).await.is_err());
}

#[tokio::test]
async fn runtime_reset_rejects_old_bridge_and_replay_with_reused_ids() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let old = bridge(&state).await;
    let request = call("threads_context", json!({}));
    assert_eq!(
        rpc(&old, "provider", "context", request.clone())
            .await
            .unwrap()["result"]["isError"],
        false
    );
    state.agent.reset_history().await.unwrap();
    let fresh = bridge(&state).await;
    assert_eq!(old.caller.thread_id, fresh.caller.thread_id);
    assert_eq!(old.caller.turn_id, fresh.caller.turn_id);
    assert_ne!(old.caller.history_id, fresh.caller.history_id);
    assert!(
        rpc(&old, "provider", "context", request.clone())
            .await
            .is_err()
    );
    assert!(
        rpc(
            &old,
            "provider",
            "write",
            call(
                "threads_create",
                json!({"client_id":"old","title":"Forbidden"})
            )
        )
        .await
        .is_err()
    );
    let current = rpc(&fresh, "provider", "context", request).await.unwrap();
    assert_eq!(current["result"]["isError"], false, "{current}");
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 1);
    drop(old);
    tokio::task::yield_now().await;
    assert!(state.storage.thread_context(&fresh.caller).await.is_ok());
}

#[tokio::test]
async fn accepted_stop_immediately_revokes_reads_writes_and_cached_replies() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let active = bridge(&state).await;
    let request = call("threads_context", json!({}));
    assert_eq!(
        rpc(&active, "provider", "context", request.clone())
            .await
            .unwrap()["result"]["isError"],
        false
    );
    state
        .agent
        .cancel_thread_turn(active.caller.thread_id)
        .await
        .unwrap();
    assert!(rpc(&active, "provider", "context", request).await.is_err());
    assert!(
        rpc(
            &active,
            "provider",
            "late",
            call("threads_create", json!({"client_id":"late","title":"Late"}))
        )
        .await
        .is_err()
    );
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 1);
}

fn turn_events(
    state: &crate::AppState,
    caller: &crate::storage::ThreadCaller,
) -> Vec<hirsel_proto::TurnEventKind> {
    state
        .broadcast_log
        .recent()
        .into_iter()
        .filter_map(|f| match f {
            hirsel_proto::HostToClient::TurnEvent {
                thread_id,
                turn_id,
                event,
                ..
            } if thread_id == caller.thread_id && turn_id == caller.turn_id => Some(event),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn self_cancel_pairs_completion_without_restoring_capability_or_replaying_events() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let mut active = bridge(&state).await;
    let request = call("threads_cancel", json!({}));
    let result = rpc(&active, "provider", "cancel", request.clone())
        .await
        .unwrap();
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert!(rpc(&active, "provider", "cancel", request).await.is_err());
    assert!(
        rpc(
            &active,
            "provider",
            "read",
            call("threads_context", json!({}))
        )
        .await
        .is_err()
    );
    assert!(
        rpc(
            &active,
            "provider",
            "write",
            call("threads_create", json!({"client_id":"late","title":"Late"}))
        )
        .await
        .is_err()
    );
    active.finish().await;
    active.finish().await;
    let events = turn_events(&state, &active.caller);
    assert!(
        matches!(&events[..], [hirsel_proto::TurnEventKind::ToolStart{id:a,..}, hirsel_proto::TurnEventKind::ToolDone{id:b,ok:true,..}] if a==b)
    );
    assert_eq!(
        active.tool_calls().await,
        vec![hirsel_proto::ToolCallSummary {
            name: "threads_cancel".into(),
            ok: true
        }]
    );
    assert!(state.storage.thread_context(&active.caller).await.is_err());
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 1);
}

async fn start_held_call(
    active: &ThreadToolBridge,
    state: &crate::AppState,
) -> Framed<UnixStream, LinesCodec> {
    let mut frames = Framed::new(
        UnixStream::connect(&active.socket_path).await.unwrap(),
        LinesCodec::new_with_max_length(MAX_FRAME),
    );
    let marker = active.capability_file.with_file_name("held-started");
    let cmd = format!("printf started > '{}'; sleep 30", marker.display());
    frames
        .send(
            serde_json::to_string(&Invocation {
                capability: tokio::fs::read_to_string(&active.capability_file)
                    .await
                    .unwrap(),
                bridge_instance: "provider".into(),
                invocation_id: "held".into(),
                request: call("shell_run", json!({"cmd":cmd,"timeout_secs":60})),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while turn_events(state, &active.caller).is_empty() || !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    frames
}

#[tokio::test]
async fn interrupted_inflight_call_finishes_once_and_keeps_authority_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let mut active = bridge(&state).await;
    let frames = start_held_call(&active, &state).await;
    state
        .agent
        .cancel_thread_turn(active.caller.thread_id)
        .await
        .unwrap();
    // Provider bridge closes before a result; terminal owner reconciliation
    // must abort the actual callback and persist a truthful failed tool summary.
    drop(frames);
    active.finish().await;
    active.finish().await;
    let events = turn_events(&state, &active.caller);
    assert!(
        matches!(&events[..], [hirsel_proto::TurnEventKind::ToolStart{id:a,..}, hirsel_proto::TurnEventKind::ToolDone{id:b,ok:false,summary:Some(_),..}] if a==b)
    );
    assert_eq!(
        active.tool_calls().await,
        vec![hirsel_proto::ToolCallSummary {
            name: "shell_run".into(),
            ok: false
        }]
    );
    assert!(state.storage.thread_context(&active.caller).await.is_err());
}

#[tokio::test]
async fn reset_discards_inflight_completion_even_when_fresh_ids_match() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let mut old = bridge(&state).await;
    let _frames = start_held_call(&old, &state).await;
    state.agent.reset_history().await.unwrap();
    let fresh = bridge(&state).await;
    assert_eq!(
        (old.caller.thread_id, old.caller.turn_id),
        (fresh.caller.thread_id, fresh.caller.turn_id)
    );
    let before = turn_events(&state, &fresh.caller);
    old.finish().await;
    assert_eq!(
        turn_events(&state, &fresh.caller),
        before,
        "old terminal reconciliation cannot broadcast into reset history"
    );
    assert!(old.tool_calls().await.is_empty());
    assert!(state.storage.thread_context(&old.caller).await.is_err());
    assert!(state.storage.thread_context(&fresh.caller).await.is_ok());
}

#[path = "artifact_tests.rs"]
mod artifact_tests;
