use crate::Storage;
use crate::lash_runtime::ScopedThreadTools;
use crate::storage::ThreadCaller;
use hirsel_proto::{ThreadAttention, ThreadKind};
use serde_json::json;

async fn thread(s: &Storage, key: &str, kind: ThreadKind, parent: Option<u64>) -> u64 {
    s.create_thread(
        key,
        key,
        "",
        None,
        ThreadAttention::NeedsOwner,
        kind,
        parent,
    )
    .await
    .unwrap()
    .0
    .id
}
async fn caller(s: &Storage, id: u64) -> ThreadCaller {
    let turn = s.start_thread_turn(id, None).await.unwrap();
    let history = s.history_id().await.unwrap();
    let launch = uuid::Uuid::new_v4().to_string();
    s.bind_thread_execution(&history, &launch, &launch, turn.id)
        .await
        .unwrap()
}
async fn archived_at(s: &Storage, id: u64) -> bool {
    s.thread(id).await.unwrap().unwrap().archived_at.is_some()
}

#[tokio::test]
async fn archiving_takes_the_subtree_out_of_the_tree_and_cancels_its_work() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let lead = thread(s, "lead", ThreadKind::Space, None).await;
    let scratch = thread(s, "scratch", ThreadKind::Task, Some(lead)).await;
    let nested = thread(s, "nested", ThreadKind::Task, Some(scratch)).await;
    let running = s.start_thread_turn(nested, None).await.unwrap();
    let actor = caller(s, lead).await;
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: actor.clone(),
        operation_id: "archive-1".into(),
    };

    let result = tools
        .execute("threads_archive", &json!({"thread": scratch}))
        .await
        .unwrap();
    assert_eq!(result["thread_id"], json!(scratch));
    assert_eq!(result["archived"], json!(true));
    assert_eq!(result["cancelled_turn_ids"], json!([running.id]));
    // Root first, then the rest of the subtree; the untouched parent stays out.
    assert_eq!(
        result["threads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|thread| thread["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![scratch, nested]
    );
    assert!(archived_at(s, scratch).await && archived_at(s, nested).await);
    assert!(!archived_at(s, lead).await);
    // Attention drops to quiet across the archived subtree.
    for id in [scratch, nested] {
        assert_eq!(
            s.thread(id).await.unwrap().unwrap().attention,
            ThreadAttention::Quiet
        );
    }
    assert_eq!(
        s.requested_thread_cancellations()
            .await
            .unwrap()
            .into_iter()
            .map(|turn| turn.id)
            .collect::<Vec<_>>(),
        vec![running.id]
    );

    // Exactly one activity, on the Thread that ran the turn, so it renders on
    // that run card.
    let entries = s
        .thread_detail(lead, None, 100)
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| activity.kind == "archived")
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].turn_id, Some(actor.turn_id));
    assert_eq!(entries[0].data["actor"], json!("agent"));
    assert_eq!(entries[0].data["title"], json!("scratch"));
    assert_eq!(entries[0].data["thread_kind"], json!("task"));
    assert_eq!(entries[0].data["thread_count"], json!(2));
    assert_eq!(entries[0].data["cancelled_turns"], json!(1));

    // Archiving is the only removal: nothing is deleted.
    assert!(s.thread(nested).await.unwrap().is_some());

    tools.operation_id = "unarchive-1".into();
    let restored = tools
        .execute("threads_unarchive", &json!({"thread": scratch}))
        .await
        .unwrap();
    assert_eq!(restored["archived"], json!(false));
    assert_eq!(restored["cancelled_turn_ids"], json!([]));
    assert!(!archived_at(s, scratch).await && !archived_at(s, nested).await);
    assert_eq!(
        s.thread_detail(lead, None, 100)
            .await
            .unwrap()
            .activities
            .into_iter()
            .filter(|activity| activity.kind == "archived")
            .count(),
        2
    );
}

#[tokio::test]
async fn archiving_outside_reach_is_refused_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let worker = thread(s, "worker", ThreadKind::Task, None).await;
    let stranger = thread(s, "stranger", ThreadKind::Task, None).await;
    let actor = caller(s, worker).await;
    let tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: actor.clone(),
        operation_id: "archive-stranger".into(),
    };

    let refused = tools
        .execute("threads_archive", &json!({"thread": stranger}))
        .await
        .unwrap();
    assert_eq!(refused["refused"], json!(true));
    assert_eq!(refused["reason"], json!("outside_grant"));
    assert_eq!(refused["tool"], json!("threads_archive"));
    assert_eq!(
        refused["target"],
        json!({"kind":"thread","thread_id":stranger})
    );
    assert!(!archived_at(s, stranger).await);

    let refusals = s
        .thread_detail(worker, None, 100)
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| activity.kind == "refusal")
        .collect::<Vec<_>>();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].data["tool"], json!("threads_archive"));
    assert_eq!(refusals[0].turn_id, Some(actor.turn_id));
}

#[tokio::test]
async fn a_thread_archiving_itself_finishes_its_turn_and_wakes_no_further() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let worker = thread(s, "worker", ThreadKind::Task, None).await;
    let history = s.history_id().await.unwrap();
    // A wake accepted just before the archive is held, not run.
    s.queue_background_thread_request(
        "wake-0",
        worker,
        &json!({"history_id":history,"thread_id":worker,"body":"ping"}),
    )
    .await
    .unwrap();
    let actor = caller(s, worker).await;
    let tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: actor.clone(),
        operation_id: "archive-self".into(),
    };

    let result = tools
        .execute("threads_archive", &json!({"thread": "."}))
        .await
        .unwrap();
    assert_eq!(result["thread_id"], json!(worker));
    assert!(archived_at(s, worker).await);
    // The turn issuing the archive is never cancelled; it ends cleanly. Its
    // own queued wake was cancelled with the rest of the subtree's work.
    assert_eq!(result["cancelled_turn_ids"].as_array().unwrap().len(), 1);
    assert!(
        !s.requested_thread_cancellations()
            .await
            .unwrap()
            .iter()
            .any(|turn| turn.id == actor.turn_id)
    );

    // Nothing queued for an archived Thread is admitted, and a fresh wake is
    // refused outright, so no turn runs after this one.
    assert!(s.pending_thread_requests().await.unwrap().is_empty());
    assert!(
        s.queue_background_thread_request(
            "wake-1",
            worker,
            &json!({"history_id":history,"thread_id":worker,"body":"ping"}),
        )
        .await
        .is_err()
    );

    // Unarchiving readmits the held request; nothing was destroyed.
    let restored = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: actor.clone(),
        operation_id: "unarchive-self".into(),
    };
    restored
        .execute("threads_unarchive", &json!({"thread": "."}))
        .await
        .unwrap();
    assert_eq!(s.pending_thread_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_owner_action_and_the_agent_tool_archive_identically() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let history = s.history_id().await.unwrap();
    let owned = thread(s, "owned", ThreadKind::Space, None).await;
    let owned_child = thread(s, "owned-child", ThreadKind::Task, Some(owned)).await;
    let running = s.start_thread_turn(owned_child, None).await.unwrap();

    let root = state
        .handle_addressed_thread_action(&history, owned, "archive".into(), json!({}), None)
        .await
        .unwrap();
    assert!(root.archived_at.is_some());
    assert!(archived_at(s, owned_child).await);
    assert_eq!(
        s.thread(owned_child).await.unwrap().unwrap().attention,
        ThreadAttention::Quiet
    );
    assert_eq!(
        s.requested_thread_cancellations()
            .await
            .unwrap()
            .into_iter()
            .map(|turn| turn.id)
            .collect::<Vec<_>>(),
        vec![running.id]
    );
    let entries = s
        .thread_detail(owned, None, 100)
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| activity.kind == "archived")
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].data["actor"], json!("owner"));
    assert_eq!(entries[0].turn_id, None);

    let restored = state
        .handle_addressed_thread_action(&history, owned, "unarchive".into(), json!({}), None)
        .await
        .unwrap();
    assert!(restored.archived_at.is_none());
    assert!(!archived_at(s, owned_child).await);
}
