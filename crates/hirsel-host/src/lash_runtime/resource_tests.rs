use super::*;

#[tokio::test]
async fn scoped_views_and_shell_reject_foreign_or_cancelled_execution() {
    let (executor, storage, _log, dir) = super::tests::test_event_executor().await;
    let a = storage.test_running_caller().await;
    let b = storage.test_running_caller().await;
    let own = ScopedThreadTools {
        tools: executor.tools.clone(),
        caller: a.clone(),
        operation_id: "own".into(),
    };
    let peer = ScopedThreadTools {
        tools: executor.tools.clone(),
        caller: b.clone(),
        operation_id: "peer".into(),
    };
    let view = json!({"instance_id":"same-name","spec":{"type":"text","text":"A private view"}});
    own.execute("views_show", &view).await.unwrap();
    assert!(peer.execute("views_show", &view).await.is_err());
    assert!(
        peer.execute(
            "views_update",
            &json!({"instance_id":"same-name","spec":{"type":"text","text":"forbidden"}})
        )
        .await
        .is_err()
    );
    // A peer's view is addressable and refused, never silently reachable.
    let refused = peer
        .execute("views_clear", &json!({"instance_id":"same-name"}))
        .await
        .unwrap();
    assert_eq!(refused["refused"], json!(true));
    assert_eq!(refused["reason"], json!("outside_grant"));
    storage
        .request_thread_cancellation(&a.history_id, a.thread_id)
        .await
        .unwrap();
    let marker = dir.path().join("must-not-exist");
    assert!(
        own.execute(
            "shell_run",
            &json!({"cmd":format!("touch {}",marker.display())})
        )
        .await
        .is_err()
    );
    assert!(!marker.exists());
    assert!(
        own.execute("views_clear", &json!({"instance_id":"same-name"}))
            .await
            .is_err()
    );
    storage.reset().await.unwrap();
}

#[tokio::test]
async fn durable_child_report_emits_the_typed_report_trigger() {
    let (executor, storage, _log, _dir) = super::tests::test_event_executor().await;
    let (parent, _) = storage
        .create_thread(
            "report-parent",
            "Parent",
            "",
            None,
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Space,
            None,
        )
        .await
        .unwrap();
    let (child, _) = storage
        .create_thread(
            "report-child",
            "Child",
            "",
            None,
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            Some(parent.id),
        )
        .await
        .unwrap();
    let turn = storage.start_thread_turn(child.id, None).await.unwrap();
    let history = storage.history_id().await.unwrap();
    let caller = storage
        .bind_thread_execution(&history, "report-session", "report-execution", turn.id)
        .await
        .unwrap();
    ScopedThreadTools {
        tools: executor.tools.clone(),
        caller,
        operation_id: "report-trigger".into(),
    }
    .execute(
        "threads_report",
        &json!({"summary":"ready for review", "artifact_ids":[]}),
    )
    .await
    .unwrap();

    let events = executor.tools.recorded_thread_triggers().await;
    assert!(events.iter().any(|event| {
        event.source_type == THREAD_REPORTED_SOURCE_TYPE
            && event.event_type == THREAD_REPORTED_EVENT_TYPE
            && event.thread_id == child.id
            && event.payload == "ready for review"
    }));
}

#[tokio::test]
async fn related_tools_filter_agent_results_but_publish_current_complete_human_snapshots() {
    let (executor, storage, log, _dir) = super::tests::test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let peer = storage.test_running_caller().await;
    let foreign = storage
        .add_thread_related(
            "foreign-ref",
            &caller.history_id,
            caller.thread_id,
            &hirsel_proto::ThreadRelatedTarget::Thread {
                history_id: caller.history_id.clone(),
                thread_id: peer.thread_id,
            },
            None,
        )
        .await
        .unwrap();
    let foreign_id = foreign.related_items[0].id;
    let mut tools = ScopedThreadTools {
        tools: executor.tools.clone(),
        caller: caller.clone(),
        operation_id: "add".into(),
    };
    let result = tools
        .execute(
            "threads_add_related",
            &json!({"target":{"kind":"url","url":"https://example.com/repo#readme"},"title":""}),
        )
        .await
        .unwrap();
    assert_eq!(result["related_items"].as_array().unwrap().len(), 1);
    let item = result["related_items"][0]["id"].as_u64().unwrap();
    assert!(result["related_items"][0]["title"].is_null());
    assert!(log.recent().iter().any(|frame|matches!(frame,HostToClient::ThreadRelatedChanged {client_id:None,history_id,thread_id,items,..} if history_id==&caller.history_id && *thread_id==caller.thread_id && items.len()==2)));
    tools.operation_id = "remove".into();
    assert!(
        tools
            .execute(
                "threads_remove_related",
                &json!({"item_id":item,"caller":99})
            )
            .await
            .is_err()
    );
    assert_eq!(
        tools
            .execute("threads_remove_related", &json!({"item_id":item}))
            .await
            .unwrap()["related_items"],
        json!([])
    );
    storage
        .remove_thread_related(
            "remove-foreign",
            &caller.history_id,
            caller.thread_id,
            foreign_id,
        )
        .await
        .unwrap();
    let target = hirsel_proto::ThreadRelatedTarget::Url {
        url: "https://example.com/new".into(),
    };
    let human = storage
        .add_thread_related(
            "human-add",
            &caller.history_id,
            caller.thread_id,
            &target,
            None,
        )
        .await
        .unwrap();
    storage
        .remove_thread_related(
            "human-remove",
            &caller.history_id,
            caller.thread_id,
            human.related_items[0].id,
        )
        .await
        .unwrap();
    let replay = storage
        .add_thread_related(
            "human-add",
            &caller.history_id,
            caller.thread_id,
            &target,
            None,
        )
        .await
        .unwrap();
    executor
        .tools
        .publish_thread_related(Some("human-add".into()), replay)
        .await
        .unwrap();
    assert!(
        matches!(log.recent().last(),Some(HostToClient::ThreadRelatedChanged {client_id:Some(id),items,..}) if id=="human-add" && items.is_empty())
    );
    // Even a queued publication of the original add cannot resurrect removed UI rows.
    executor
        .tools
        .publish_thread_related(Some("human-add".into()), human)
        .await
        .unwrap();
    assert!(
        matches!(log.recent().last(),Some(HostToClient::ThreadRelatedChanged {items,..}) if items.is_empty())
    );
    let delayed = storage
        .add_thread_related(
            "delayed",
            &caller.history_id,
            caller.thread_id,
            &target,
            None,
        )
        .await
        .unwrap();
    storage.reset().await.unwrap();
    let before = log.recent().len();
    assert!(
        executor
            .tools
            .publish_thread_related(Some("old-request".into()), delayed)
            .await
            .is_err()
    );
    assert_eq!(log.recent().len(), before);
}
