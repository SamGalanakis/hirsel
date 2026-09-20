use super::*;
use hirsel_proto::{ThreadAttention, ThreadTurnState};
use serde_json::json;
async fn thread(s: &Storage, key: &str, parent: Option<u64>) -> u64 {
    s.create_thread(
        key,
        key,
        "",
        None,
        ThreadAttention::Quiet,
        hirsel_proto::ThreadKind::Task,
        parent,
    )
    .await
    .unwrap()
    .0
    .id
}
async fn caller(s: &Storage, id: u64) -> ThreadCaller {
    let turn = s.start_thread_turn(id, None).await.unwrap();
    bound_caller(s, turn.id).await
}
async fn bound_caller(s: &Storage, turn_id: u64) -> ThreadCaller {
    let history = s.history_id().await.unwrap();
    let launch = uuid::Uuid::new_v4().to_string();
    s.bind_thread_execution(&history, &launch, &launch, turn_id)
        .await
        .unwrap()
}

#[tokio::test]
async fn task_focus_is_replayed_idempotently_and_never_widens_reach() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let history = storage.history_id().await.unwrap();
    let (project, _) = storage
        .create_thread(
            "focus-project",
            "Project",
            "",
            None,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Space,
            None,
        )
        .await
        .unwrap();
    let focused = thread(&storage, "focused-task", Some(project.id)).await;
    let outside = thread(&storage, "outside-task", None).await;
    let focus = hirsel_proto::TaskFocus {
        task_thread_id: focused,
        snapshot: json!({"title":"Focused task","brief":"Do the bounded work","instrument_summary":"Ready"}),
    };
    let request = json!({"mode":"send","body":"Discuss this","focus":focus});
    let (message, inserted) = storage
        .append_thread_owner_request_with_focus(
            &history,
            project.id,
            "focus-message",
            "Discuss this".into(),
            &[],
            &[],
            &[],
            Some(&focus),
            &request,
        )
        .await
        .unwrap();
    assert!(inserted);
    assert_eq!(message.focus, Some(focus.clone()));
    let (retried, inserted) = storage
        .append_thread_owner_request_with_focus(
            &history,
            project.id,
            "focus-message",
            "Discuss this".into(),
            &[],
            &[],
            &[],
            Some(&focus),
            &request,
        )
        .await
        .unwrap();
    assert!(!inserted);
    assert_eq!(retried, message);

    let changed = hirsel_proto::TaskFocus {
        snapshot: json!({"title":"Changed"}),
        ..focus.clone()
    };
    assert!(
        storage
            .append_thread_owner_request_with_focus(
                &history,
                project.id,
                "focus-message",
                "Discuss this".into(),
                &[],
                &[],
                &[],
                Some(&changed),
                &request,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("client_id focus changed")
    );

    let outside_focus = hirsel_proto::TaskFocus {
        task_thread_id: outside,
        snapshot: json!({"title":"Outside","brief":"Outside reach","instrument_summary":null}),
    };
    let error = storage
        .append_thread_owner_request_with_focus(
            &history,
            project.id,
            "outside-focus",
            "Discuss this".into(),
            &[],
            &[],
            &[],
            Some(&outside_focus),
            &json!({}),
        )
        .await
        .unwrap_err();
    assert!(
        error.downcast_ref::<OutsideGrant>().is_some(),
        "focus refusal remains the typed reach refusal: {error:#}"
    );
    let worker_error = storage
        .append_thread_owner_request_with_focus(
            &history,
            focused,
            "worker-focus",
            "This must stay an ordinary worker message".into(),
            &[],
            &[],
            &[],
            Some(&focus),
            &json!({}),
        )
        .await
        .unwrap_err();
    assert!(
        worker_error
            .to_string()
            .contains("only accepted by a project chat"),
        "worker accepted project-chat focus: {worker_error:#}"
    );
    assert_eq!(
        storage
            .conn
            .lock()
            .await
            .query_row(
                "SELECT count(*) FROM client_messages WHERE client_id='outside-focus'",
                [],
                |row| row.get::<_, u64>(0),
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn durable_process_authority_survives_the_registering_turn() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let id = thread(&s, "process-owner", None).await;
    let session = s
        .reconcile_agent_tool_surface(id, "surface", &["shell_run".into()])
        .await
        .unwrap();
    let turn = s.start_thread_turn(id, None).await.unwrap();
    let history = s.history_id().await.unwrap();
    s.complete_thread_turn(&history, turn.id, ThreadTurnState::Completed, None)
        .await
        .unwrap();

    let first = s
        .process_caller(&session.session_id, "process-1", "scope-1")
        .await
        .unwrap();
    assert_eq!(first.thread_id, id);
    assert_eq!(first.turn_id, turn.id);
    assert!(
        s.process_caller("unknown-session", "process-1", "scope-1")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn process_authority_keeps_the_profile_of_its_own_session() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let history = s.history_id().await.unwrap();
    s.set_native_execution_default(&crate::storage::ThreadExecution::Native {
        tool_profile: crate::storage::ToolProfile::Worker,
        provider_id: "process-profile-test".into(),
        model: lash::ModelSpec::builder("process-profile-model")
            .variant(lash::provider::ReasoningSelection::ProviderDefault)
            .context_window_tokens(8_000)
            .build()
            .unwrap(),
        cwd: dir.path().to_path_buf(),
    })
    .await
    .unwrap();
    let (project, _) = s.ensure_home_project(&history).await.unwrap();
    let project_session = s
        .reconcile_agent_tool_surface(project.id, "project-surface", &["threads_context".into()])
        .await
        .unwrap();
    let project_turn = s.start_thread_turn(project.id, None).await.unwrap();
    s.run_thread_turn(project_turn.id).await.unwrap();
    s.bind_thread_execution(
        &history,
        &project_session.session_id,
        "project-execution",
        project_turn.id,
    )
    .await
    .unwrap();
    s.complete_thread_turn(&history, project_turn.id, ThreadTurnState::Completed, None)
        .await
        .unwrap();

    let worker = s
        .set_addressed_thread_kind(
            &history,
            project.id,
            hirsel_proto::ThreadKind::Task,
            project.revision,
        )
        .await
        .unwrap();
    let worker_session = s
        .reconcile_agent_tool_surface(worker.id, "worker-surface", &["shell_run".into()])
        .await
        .unwrap();
    let worker_turn = s.start_thread_turn(worker.id, None).await.unwrap();
    s.run_thread_turn(worker_turn.id).await.unwrap();
    s.bind_thread_execution(
        &history,
        &worker_session.session_id,
        "worker-execution",
        worker_turn.id,
    )
    .await
    .unwrap();

    let caller = s
        .process_caller(
            &project_session.session_id,
            "project-process",
            "process-scope",
        )
        .await
        .unwrap();
    assert_eq!(caller.turn_id, project_turn.id);
    assert_eq!(
        s.turn_tool_profile(caller.turn_id).await.unwrap(),
        crate::storage::ToolProfile::ProjectChat
    );
}

#[tokio::test]
async fn hierarchy_paging_and_pin_do_not_grant_peer_access() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    assert!(s.thread_snapshot().await.unwrap().is_empty());
    let a = thread(&s, "a", None).await;
    let b = thread(&s, "b", None).await;
    let child = thread(&s, "child", Some(a)).await;
    let grand = thread(&s, "grand", Some(child)).await;
    let actor = caller(&s, a).await;
    let child_actor = caller(&s, child).await;
    assert!(s.thread_in_scope(a, a).await.unwrap());
    assert!(s.thread_in_scope(a, grand).await.unwrap());
    assert!(!s.thread_in_scope(a, b).await.unwrap());
    assert!(!s.thread_in_scope(child, a).await.unwrap());
    assert_eq!(
        s.resolve_thread(&actor, &ThreadRef::Id(grand))
            .await
            .unwrap(),
        grand
    );
    // 0 names the top of the tree and no grant widens to it except the root
    // grant; an unparsable string is just an invalid reference.
    for reference in [
        ThreadRef::Id(b),
        ThreadRef::Id(0),
        ThreadRef::Dot("..".into()),
        ThreadRef::Dot("./+1".into()),
    ] {
        assert!(s.resolve_thread(&actor, &reference).await.is_err());
    }
    assert!(
        s.resolve_thread(&child_actor, &ThreadRef::Id(a))
            .await
            .is_err()
    );
    let context = s.thread_context(&child_actor).await.unwrap();
    assert_eq!(context.ancestors.len(), 1);
    assert_eq!(context.ancestors[0].id, a);
    let page = s
        .scoped_thread_list(&actor, &ThreadRef::default(), 1, None, 1)
        .await
        .unwrap();
    assert_eq!(page.threads[0].id, child);
    assert_eq!(page.next_after_id, None);
    let page = s
        .scoped_thread_list(&actor, &ThreadRef::default(), 2, None, 1)
        .await
        .unwrap();
    assert_eq!(page.next_after_id, Some(child));
    let before = s.thread(b).await.unwrap().unwrap();
    let pinned = s.pin_thread(b, true).await.unwrap();
    assert!(pinned.pinned_at.is_some());
    assert_eq!(pinned.last_activity_at, before.last_activity_at);
    assert_eq!(pinned.attention, before.attention);
    assert_eq!(pinned.read, before.read);
}
#[tokio::test]
async fn delegation_is_atomic_idempotent_and_reports_once_after_hidden_parent_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let root = thread(&s, "root", None).await;
    let actor = caller(&s, root).await;
    let assignment = Delegation {
        title: "Research".into(),
        brief: "Only the assigned question".into(),
        artifact_ids: vec![],
        child_thread_id: None,
        execution: None,
    };
    let accepted = s
        .delegate_thread(
            &actor,
            "op1",
            &assignment,
            &serde_json::to_value(&assignment).unwrap(),
        )
        .await
        .unwrap();
    let replay = s
        .delegate_thread(
            &actor,
            "op1",
            &assignment,
            &serde_json::to_value(&assignment).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.turn_id, replay.turn_id);
    let changed = Delegation {
        brief: "changed".into(),
        ..assignment.clone()
    };
    assert!(
        s.delegate_thread(
            &actor,
            "op1",
            &changed,
            &serde_json::to_value(&changed).unwrap()
        )
        .await
        .is_err()
    );
    let peer = thread(&s, "peer", None).await;
    let bad = Delegation {
        child_thread_id: Some(peer),
        ..assignment.clone()
    };
    assert!(
        s.delegate_thread(&actor, "op2", &bad, &serde_json::to_value(&bad).unwrap())
            .await
            .is_err()
    );
    let detail = s
        .thread_detail(accepted.thread_id, None, 100)
        .await
        .unwrap();
    assert_eq!(detail.brief.text, assignment.brief);
    assert!(detail.messages.is_empty());
    assert_eq!(detail.turns[0].requester_turn_id, Some(actor.turn_id));
    s.run_thread_turn(accepted.turn_id).await.unwrap();
    // Requester completion cannot abandon independent child execution.
    s.finish_thread_turn(actor.turn_id, ThreadTurnState::Completed, None)
        .await
        .unwrap();
    s.archive_thread(root, true).await.unwrap();
    s.finish_thread_turn(accepted.turn_id, ThreadTurnState::Failed, None)
        .await
        .unwrap();
    s.finish_thread_turn(accepted.turn_id, ThreadTurnState::Cancelled, None)
        .await
        .unwrap();
    let parent = s.thread_detail(root, None, 100).await.unwrap();
    let reports: Vec<_> = parent
        .activities
        .iter()
        .filter(|a| a.kind == "child_report")
        .collect();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].turn_id, None);
    assert_eq!(reports[0].data["status"], "failed");
    assert_eq!(parent.thread.settled_at, None);
    assert!(
        !s.pending_thread_requests()
            .await
            .unwrap()
            .iter()
            .any(|(_, p)| p["report_triggered"] == true)
    );
    s.archive_thread(root, false).await.unwrap();
    assert_eq!(
        s.pending_thread_requests()
            .await
            .unwrap()
            .iter()
            .filter(|(_, p)| p["report_triggered"] == true)
            .count(),
        1
    );
    let c = s.conn.lock().await;
    let fk: u64 = c
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
}
#[tokio::test]
async fn human_child_and_crash_report_actual_child_turn_without_invented_parent_turn() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let root = thread(&s, "root", None).await;
    let child = thread(&s, "child", Some(root)).await;
    let t = s.start_thread_turn(child, None).await.unwrap();
    assert_eq!(t.requester_thread_id, Some(root));
    assert_eq!(t.requester_turn_id, None);
    s.interrupt_unfinished_thread_turns().await.unwrap();
    s.interrupt_unfinished_thread_turns().await.unwrap();
    let detail = s.thread_detail(root, None, 100).await.unwrap();
    assert_eq!(detail.activities.len(), 1);
    assert_eq!(detail.activities[0].data["child_turn_id"], t.id);
    assert_eq!(detail.activities[0].data["requester_turn_id"], json!(null));
    assert_eq!(detail.activities[0].turn_id, None);
}

#[tokio::test]
async fn scoped_artifact_receipts_hide_peer_backlinks_and_cancelled_writes_have_no_effect() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let root = thread(&s, "root", None).await;
    let peer = thread(&s, "peer", None).await;
    let actor = caller(&s, root).await;
    let input = json!({"create":"shared"});
    let draft = ArtifactDraft {
        title: "Shared".into(),
        kind: hirsel_proto::ArtifactKind::File {
            mime: "text/plain".into(),
            filename: None,
        },
        content: "original".into(),
        expected_content: None,
    };
    let (artifact, _) = s
        .publish_artifact("create", &input, &actor, None, Some(draft.clone()))
        .await
        .unwrap();
    let peer_message = s
        .append_thread_chat(
            peer,
            hirsel_proto::ChatAuthor::Agent,
            "Shared reference",
            None,
            vec![],
        )
        .await
        .unwrap();
    {
        let c = s.conn.lock().await;
        c.execute(
            "INSERT INTO message_artifacts(message_id,artifact_id) VALUES(?1,?2)",
            rusqlite::params![peer_message.id, artifact.summary.id],
        )
        .unwrap();
    }
    let assignment = Delegation {
        title: "Child".into(),
        brief: "Edit this reference".into(),
        artifact_ids: vec![artifact.summary.id],
        child_thread_id: None,
        execution: None,
    };
    let child = s
        .delegate_thread(
            &actor,
            "delegate",
            &assignment,
            &serde_json::to_value(&assignment).unwrap(),
        )
        .await
        .unwrap();
    s.run_thread_turn(child.turn_id).await.unwrap();
    let child_actor = bound_caller(&s, child.turn_id).await;
    for op in ["show", "edit"] {
        let draft = (op == "edit").then(|| ArtifactDraft {
            content: "edited".into(),
            expected_content: Some("original".into()),
            ..draft.clone()
        });
        let value = json!({"op":op});
        s.publish_artifact(op, &value, &child_actor, Some(artifact.summary.id), draft)
            .await
            .unwrap();
        let replay = s
            .artifact_operation(op, &child_actor, &value)
            .await
            .unwrap()
            .unwrap();
        let scoped = s
            .scoped_artifact(&child_actor, replay.0.summary.id)
            .await
            .unwrap();
        assert_eq!(scoped.summary.thread_ids, vec![child.thread_id]);
    }
    assert_eq!(
        s.artifact(artifact.summary.id)
            .await
            .unwrap()
            .summary
            .thread_ids,
        vec![root, peer, child.thread_id]
    );
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker_storage = s.clone();
    let worker_actor = child_actor.clone();
    let worker_draft = draft.clone();
    let worker = tokio::spawn(async move {
        worker_storage
            .authorize_thread_artifact(&worker_actor, artifact.summary.id)
            .await
            .unwrap();
        worker_barrier.wait().await;
        worker_barrier.wait().await;
        worker_storage
            .publish_artifact(
                "cancel-race",
                &json!({}),
                &worker_actor,
                None,
                Some(worker_draft),
            )
            .await
    });
    barrier.wait().await;
    s.finish_thread_turn(child.turn_id, ThreadTurnState::Cancelled, None)
        .await
        .unwrap();
    barrier.wait().await;
    assert!(worker.await.unwrap().is_err());
    assert!(
        s.artifact_operation("show", &child_actor, &json!({"op":"show"}))
            .await
            .is_err()
    );
    assert!(
        s.mutate_scoped_thread(
            &child_actor,
            "late-create",
            &ThreadMutation::Create {
                icon: None,
                client_id: "late".into(),
                kind: hirsel_proto::ThreadKind::Task,
                title: "late".into(),
                parent: ThreadRef::default(),
                description: String::new(),
                instrument: None,
                attention: ThreadAttention::Quiet
            }
        )
        .await
        .is_err()
    );
    let c = s.conn.lock().await;
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM artifact_operations WHERE operation_id='cancel-race'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM thread_mutation_receipts WHERE operation_id='late-create'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn bounded_agent_history_cursor_reaches_all_collections_once() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let root = thread(&s, "root", None).await;
    let actor = caller(&s, root).await;
    for i in 0..9 {
        s.append_thread_activity(root, None, "progress", &json!({"i":i}))
            .await
            .unwrap();
        if i < 5 {
            s.append_thread_chat(
                root,
                hirsel_proto::ChatAuthor::Agent,
                i.to_string(),
                None,
                vec![],
            )
            .await
            .unwrap();
        }
        if i < 7 {
            let turn = s.queue_thread_turn(root, None).await.unwrap();
            s.finish_thread_turn(turn.id, ThreadTurnState::Completed, None)
                .await
                .unwrap();
        }
    }
    let mut cursor = None;
    let (mut messages, mut turns, mut activities) = (vec![], vec![], vec![]);
    loop {
        let page = s
            .scoped_thread_read(&actor, &ThreadRef::default(), cursor, 2)
            .await
            .unwrap();
        assert!(page.messages.len() <= 2 && page.turns.len() <= 2 && page.activities.len() <= 2);
        messages.extend(page.messages.iter().map(|m| m.id));
        turns.extend(page.turns.iter().map(|t| t.id));
        activities.extend(page.activities.iter().map(|a| a.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!((messages.len(), turns.len(), activities.len()), (5, 8, 9));
    for ids in [messages, turns, activities] {
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            ids.len()
        );
    }
}

#[tokio::test]
async fn active_transport_revocation_fences_preconstructed_thread_writer() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let root = thread(&s, "root", None).await;
    let actor = caller(&s, root).await;
    let before = s.thread(root).await.unwrap().unwrap();
    s.revoke_thread_execution(&actor).await.unwrap();
    let mutation = ThreadMutation::Update {
        icon: None,
        showcased_artifact_id: None,
        thread: ThreadRef::default(),
        title: Some("forbidden".into()),
        description: None,
        instrument: None,
        attention: None,
    };
    assert!(
        s.mutate_scoped_thread(&actor, "late-update", &mutation)
            .await
            .is_err()
    );
    assert_eq!(s.thread(root).await.unwrap().unwrap(), before);
}

#[tokio::test]
async fn history_reset_reused_ids_reject_old_callers_receipts_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let id = thread(&s, "old", None).await;
    let old = caller(&s, id).await;
    let mutation = ThreadMutation::Update {
        icon: None,
        showcased_artifact_id: None,
        thread: ThreadRef::default(),
        title: Some("old edit".into()),
        description: None,
        instrument: None,
        attention: None,
    };
    s.mutate_scoped_thread(&old, "receipt", &mutation)
        .await
        .unwrap();
    let old_session = s
        .reconcile_agent_tool_surface(id, "surface", &["threads_context".into()])
        .await
        .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let release = barrier.clone();
    let store = s.clone();
    let stale = old.clone();
    let worker = tokio::spawn(async move {
        store.thread_context(&stale).await.unwrap();
        release.wait().await;
        release.wait().await;
        store
            .publish_artifact(
                "old-callback",
                &json!({}),
                &stale,
                None,
                Some(ArtifactDraft {
                    title: "must not exist".into(),
                    kind: hirsel_proto::ArtifactKind::File {
                        mime: "text/plain".into(),
                        filename: None,
                    },
                    content: "old".into(),
                    expected_content: None,
                }),
            )
            .await
    });
    barrier.wait().await;
    s.reset().await.unwrap();
    let new_id = thread(&s, "fresh", None).await;
    let fresh = caller(&s, new_id).await;
    assert_eq!(
        (old.thread_id, old.turn_id),
        (fresh.thread_id, fresh.turn_id)
    );
    assert_ne!(old.history_id, fresh.history_id);
    let new_session = s
        .reconcile_agent_tool_surface(new_id, "surface", &["threads_context".into()])
        .await
        .unwrap();
    assert_ne!(old_session.session_id, new_session.session_id);
    assert!(!new_session.rotated);
    let before = s.thread_detail(new_id, None, 100).await.unwrap();
    barrier.wait().await;
    assert!(worker.await.unwrap().is_err());
    assert!(s.thread_context(&old).await.is_err());
    assert!(
        s.scoped_thread_read(&old, &ThreadRef::default(), None, 10)
            .await
            .is_err()
    );
    assert!(
        s.artifact_operation("old-callback", &old, &json!({}))
            .await
            .is_err()
    );
    assert!(
        s.mutate_scoped_thread(&old, "receipt", &mutation)
            .await
            .is_err()
    );
    assert!(
        s.execution_caller(&old.session_id, &old.execution_id)
            .await
            .is_err()
    );
    assert!(
        s.bind_thread_execution(&old.history_id, "late", "late", fresh.turn_id)
            .await
            .is_err()
    );
    s.revoke_thread_execution(&old).await.unwrap(); // old bridge Drop cannot revoke fresh execution
    assert_eq!(
        serde_json::to_value(s.thread_detail(new_id, None, 100).await.unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    s.mutate_scoped_thread(&fresh, "fresh-write", &mutation)
        .await
        .unwrap();
    assert_eq!(
        s.thread_context(&fresh).await.unwrap().thread.title,
        "old edit"
    );
    let c = s.conn.lock().await;
    for table in ["artifacts", "artifact_operations"] {
        assert_eq!(
            c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM thread_mutation_receipts", [], |r| r
            .get::<_, u64>(
            0
        ))
        .unwrap(),
        1
    );
}

/// One Native session means one tool-surface profile per Thread. The session
/// holds while the advertised surface holds, and rotates the moment the merged
/// Thread-plus-coding surface changes under it.
#[tokio::test]
async fn the_single_agent_tool_surface_rotates_only_when_the_surface_changes() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let id = thread(&storage, "native", None).await;
    let surface: Vec<String> = ["threads_context", "read", "edit", "write", "exec_command"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let original = storage
        .reconcile_agent_tool_surface(id, "native-v1", &surface)
        .await
        .unwrap();
    assert!(!original.rotated);
    {
        let conn = storage.conn.lock().await;
        let profile_keys = conn
            .prepare("SELECT key FROM meta WHERE key LIKE 'thread:%session_profile' ORDER BY key")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            profile_keys,
            vec![format!("thread:{id}:agent_session_profile")],
            "a Thread has exactly one session profile"
        );
    }

    let unchanged = storage
        .reconcile_agent_tool_surface(id, "native-v1", &surface)
        .await
        .unwrap();
    assert_eq!(unchanged.session_id, original.session_id);
    assert!(!unchanged.rotated);

    let mut widened = surface.clone();
    widened.push("threads_delegate".into());
    let rotated = storage
        .reconcile_agent_tool_surface(id, "native-v2", &widened)
        .await
        .unwrap();
    assert!(rotated.rotated);
    assert_ne!(rotated.session_id, original.session_id);
    assert_eq!(rotated.added_tools, vec!["threads_delegate".to_string()]);
}

#[tokio::test]
async fn human_artifact_reference_is_atomic_explicit_and_scoped_without_peer_access() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let a = thread(&s, "A", None).await;
    let b = thread(&s, "B", None).await;
    let c = thread(&s, "C", None).await;
    let actor = caller(&s, a).await;
    let plain = caller(&s, c).await;
    {
        let db = s.conn.lock().await;
        let now = chrono::Utc::now().to_rfc3339();
        db.execute("INSERT INTO artifacts(id,title,kind,kind_data,content,created_at,updated_at) VALUES(44,'Shared result','file','{\"mime\":\"text/plain\",\"filename\":null}','original',?1,?1)",[now]).unwrap();
    }
    s.publish_artifact_human("publish-b", &json!({}), b, Some(44), None)
        .await
        .unwrap();
    assert_eq!(s.artifact(44).await.unwrap().summary.thread_ids, vec![b]); // global human preview only
    assert!(s.scoped_artifact(&actor, 44).await.is_err());
    let request = json!({"mode":"send","thread_action":null,"body":"make this simpler"});
    let (message, inserted) = s
        .append_thread_owner_request(
            &s.history_id().await.unwrap(),
            a,
            "share-44",
            "make this simpler".into(),
            &[],
            &[],
            &[44, 44],
            &request,
        )
        .await
        .unwrap();
    assert!(inserted);
    assert_eq!(message.artifact_ids, vec![44]);
    let accepted = s.thread_request("share-44").await.unwrap().unwrap();
    let turn_id = accepted["turn_id"].as_u64().unwrap();
    assert_eq!(
        s.accepted_message_references(&actor.history_id, turn_id)
            .await
            .unwrap(),
        vec![json!({"artifact_id":44,"title":"Shared result"})]
    );
    assert_eq!(
        s.scoped_artifact(&actor, 44)
            .await
            .unwrap()
            .summary
            .thread_ids,
        vec![a]
    );
    assert!(s.resolve_thread(&actor, &ThreadRef::Id(b)).await.is_err());
    s.publish_artifact(
        "edit-a",
        &json!({"edit":44}),
        &actor,
        Some(44),
        Some(ArtifactDraft {
            title: "Shared result".into(),
            kind: hirsel_proto::ArtifactKind::File {
                mime: "text/plain".into(),
                filename: None,
            },
            content: "simpler".into(),
            expected_content: Some("original".into()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(s.artifact(44).await.unwrap().content, "simpler");
    let (replay, inserted) = s
        .append_thread_owner_request(
            &s.history_id().await.unwrap(),
            a,
            "share-44",
            "make this simpler".into(),
            &[],
            &[],
            &[44],
            &request,
        )
        .await
        .unwrap();
    assert!(!inserted);
    assert_eq!(replay.id, message.id);
    for refs in [vec![], vec![44, 45]] {
        assert!(
            s.append_thread_owner_request(
                &s.history_id().await.unwrap(),
                a,
                "share-44",
                "make this simpler".into(),
                &[],
                &[],
                &refs,
                &request
            )
            .await
            .is_err()
        );
    }
    let baseline=s.conn.lock().await.query_row("SELECT (SELECT COUNT(*) FROM chat_messages),(SELECT COUNT(*) FROM thread_turns),(SELECT COUNT(*) FROM thread_requests),(SELECT COUNT(*) FROM message_artifacts)",[],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?,r.get::<_,u64>(3)?))).unwrap();
    assert!(
        s.append_thread_owner_request(
            &s.history_id().await.unwrap(),
            a,
            "invalid",
            "bad ref".into(),
            &[],
            &[],
            &[999],
            &request
        )
        .await
        .is_err()
    );
    assert!(
        s.append_thread_owner_request(
            &s.history_id().await.unwrap(),
            a,
            "too-many",
            "bad ref".into(),
            &[],
            &[],
            &(1..=17).collect::<Vec<_>>(),
            &request
        )
        .await
        .is_err()
    );
    assert_eq!(baseline,s.conn.lock().await.query_row("SELECT (SELECT COUNT(*) FROM chat_messages),(SELECT COUNT(*) FROM thread_turns),(SELECT COUNT(*) FROM thread_requests),(SELECT COUNT(*) FROM message_artifacts)",[],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?,r.get::<_,u64>(3)?))).unwrap());
    let (text_only, _) = s
        .append_thread_owner_request(
            &s.history_id().await.unwrap(),
            c,
            "plain",
            "edit artifact #44".into(),
            &[],
            &[],
            &[],
            &request,
        )
        .await
        .unwrap();
    assert!(text_only.artifact_ids.is_empty());
    assert!(s.scoped_artifact(&plain, 44).await.is_err());
}

/// One Native session carries the full Thread tool set, so the Owner's input
/// to a Native Thread is the ordinary input: artifacts and attachments are
/// accepted, skills still expand before acceptance, and the turn captures the
/// Thread's own Native route.
#[tokio::test]
async fn direct_owner_native_input_accepts_the_full_thread_surface() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let task = thread(&storage, "native-owner-policy", None).await;
    let execution = ThreadExecution::Native {
        tool_profile: ToolProfile::Worker,
        provider_id: "openrouter".into(),
        model: lash::ModelSpec::builder("vendor/owner-policy-model")
            .variant(lash::provider::ReasoningSelection::ProviderDefault)
            .context_window_tokens(200_000)
            .build()
            .unwrap(),
        cwd: std::env::current_dir().unwrap().canonicalize().unwrap(),
    };
    storage
        .conn
        .lock()
        .await
        .execute(
            "INSERT INTO thread_execution_preferences(thread_id,config) VALUES(?1,?2)",
            rusqlite::params![task, serde_json::to_string(&execution).unwrap()],
        )
        .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    storage
        .conn
        .lock()
        .await
        .execute(
            "INSERT INTO artifacts(id,title,kind,kind_data,content,created_at,updated_at) VALUES(44,'Native input','file','{\"mime\":\"text/plain\",\"filename\":null}','content',?1,?1)",
            [&now],
        )
        .unwrap();
    let attachment = storage
        .store_blob(
            "native-owner-blob",
            "input.txt",
            "text/plain",
            b"content".to_vec(),
        )
        .await
        .unwrap();
    let history = storage.history_id().await.unwrap();
    let request = json!({"mode":"send","thread_action":null,"body":"supported input"});
    let (message, inserted) = storage
        .append_thread_owner_request(
            &history,
            task,
            "native-owner-full-surface",
            "supported input".into(),
            &[attachment.blob.id],
            &[],
            &[44],
            &request,
        )
        .await
        .unwrap();
    assert!(inserted);
    assert_eq!(message.artifact_ids, vec![44]);
    assert_eq!(message.attachments.len(), 1);

    let supported = json!({
        "mode":"send",
        "thread_action":null,
        "body":"<skill name=\"review\">Inspect the focused diff.</skill>\nApply it"
    });
    let (message, inserted) = storage
        .append_thread_owner_request(
            &history,
            task,
            "native-owner-text",
            "/skill:review Apply it".into(),
            &[],
            &[],
            &[],
            &supported,
        )
        .await
        .unwrap();
    assert!(inserted);
    assert_eq!(message.body, "/skill:review Apply it");
    let accepted = storage
        .thread_request("native-owner-text")
        .await
        .unwrap()
        .unwrap();
    assert!(
        accepted["body"]
            .as_str()
            .unwrap()
            .contains("Inspect the focused diff.")
    );
    let ThreadExecution::Native { provider_id, .. } = storage
        .turn_execution(accepted["turn_id"].as_u64().unwrap())
        .await
        .unwrap()
    else {
        panic!("a Native Thread must capture its own Native route");
    };
    assert_eq!(provider_id, "openrouter");
}

#[tokio::test]
async fn report_receipt_uses_activity_payload_and_preserves_exact_artifact_order() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let parent = thread(&storage, "report-parent", None).await;
    let child = thread(&storage, "report-child", Some(parent)).await;
    let actor = caller(&storage, child).await;
    let mut ids = Vec::new();
    for title in ["First", "Second"] {
        let draft = ArtifactDraft {
            title: title.into(),
            kind: hirsel_proto::ArtifactKind::File {
                mime: "text/plain".into(),
                filename: None,
            },
            content: title.into(),
            expected_content: None,
        };
        let (artifact, _) = storage
            .publish_artifact(title, &json!({"title":title}), &actor, None, Some(draft))
            .await
            .unwrap();
        ids.push(artifact.summary.id);
    }
    let refs = [ids[1], ids[0], ids[1]];
    let first = storage
        .report_thread_progress(&actor, "report", "Progress", &refs)
        .await
        .unwrap();
    assert_eq!(
        storage
            .report_thread_progress(&actor, "report", "Progress", &refs)
            .await
            .unwrap(),
        first
    );
    assert!(
        storage
            .report_thread_progress(&actor, "report", "Changed", &refs)
            .await
            .is_err()
    );
    assert!(
        storage
            .report_thread_progress(&actor, "report", "Progress", &[ids[0], ids[1]])
            .await
            .is_err()
    );
    let detail = storage.thread_detail(parent, None, 100).await.unwrap();
    let reports: Vec<_> = detail
        .activities
        .iter()
        .filter(|activity| activity.kind == "child_report")
        .collect();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].id, first);
    assert_eq!(reports[0].data["artifact_ids"], json!(refs));
    assert!(reports[0].data.get("report_seq").is_none());
}
