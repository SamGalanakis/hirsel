use super::*;

#[tokio::test]
async fn monitor_create_accepts_only_valid_condition_variants() {
    let (executor, storage, _log, _dir) = super::tests::test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let mut tools = ScopedThreadTools {
        tools: executor.tools,
        caller,
        operation_id: "invalid-regex".into(),
    };

    for (name, condition) in [
        ("missing", json!({"wake_on": "regex"})),
        ("empty", json!({"wake_on": "regex", "pattern": ""})),
        ("malformed", json!({"wake_on": "regex", "pattern": "["})),
        (
            "irrelevant",
            json!({"wake_on": "changed", "pattern": "ignored"}),
        ),
    ] {
        tools.operation_id = format!("invalid-{name}");
        let mut args = condition;
        args["cmd"] = json!("printf ready");
        args["label"] = json!(format!("invalid {name}"));
        args["every_secs"] = json!(30);
        let error = tools.execute("monitors_create", &args).await.unwrap_err();
        assert!(!error.is_empty());
        assert!(storage.active_monitors().await.unwrap().is_empty());
        assert!(storage.monitor_snapshot().await.unwrap().is_empty());
    }

    for (name, condition) in [
        ("changed", json!({"wake_on": "changed"})),
        ("exit-zero", json!({"wake_on": "exit_zero"})),
        ("exit-nonzero", json!({"wake_on": "exit_nonzero"})),
        ("regex", json!({"wake_on": "regex", "pattern": "ready"})),
        ("space-regex", json!({"wake_on": "regex", "pattern": " "})),
        ("nul-regex", json!({"wake_on": "regex", "pattern": "\u{0}"})),
    ] {
        tools.operation_id = format!("valid-{name}");
        let mut args = condition;
        args["cmd"] = json!("printf ready");
        args["label"] = json!(format!("valid {name}"));
        args["every_secs"] = json!(30);
        tools.execute("monitors_create", &args).await.unwrap();
    }
    assert_eq!(storage.active_monitors().await.unwrap().len(), 6);
}

#[tokio::test]
async fn scoped_views_monitors_and_shell_reject_foreign_or_cancelled_execution() {
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
    let view = json!({"instance_id":"same-name","spec":{"type":"text","text":"A private view"},"placement":"canvas"});
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
    assert!(
        peer.execute("views_clear", &json!({"instance_id":"same-name"}))
            .await
            .is_err()
    );
    let monitor = own
        .execute(
            "monitors_create",
            &json!({"cmd":"printf safe","label":"A monitor","every_secs":30,"wake_on":"changed"}),
        )
        .await
        .unwrap();
    let id = monitor["monitor_id"].as_str().unwrap();
    assert!(
        peer.execute("monitors_cancel", &json!({"monitor_id":id}))
            .await
            .is_err()
    );
    assert!(
        storage
            .background_monitor(&a.history_id, b.thread_id, id)
            .await
            .is_err()
    );
    assert_eq!(storage.scoped_monitors(&a).await.unwrap().len(), 1);
    assert!(storage.scoped_monitors(&b).await.unwrap().is_empty());
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
    assert!(
        own.execute(
            "monitors_create",
            &json!({"cmd":"printf forbidden","label":"late","every_secs":30,"wake_on":"changed"})
        )
        .await
        .is_err()
    );
    assert_eq!(storage.active_monitors().await.unwrap().len(), 1);
    let old_history = a.history_id;
    storage.reset().await.unwrap();
    assert!(
        storage
            .background_monitor(&old_history, a.thread_id, id)
            .await
            .is_err()
    );
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
