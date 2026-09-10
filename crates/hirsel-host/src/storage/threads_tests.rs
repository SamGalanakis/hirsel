use super::Storage;
use hirsel_proto::{ChatAuthor, Thread, ThreadAttention, ThreadKind, ThreadTurnState};
use serde_json::json;
#[tokio::test]
async fn ordinary_work_snapshot_and_lifecycle_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let (t, inserted) = s
        .create_thread(
            "groceries",
            "Buy groceries",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    assert!(inserted);
    assert_eq!(
        s.create_thread(
            "groceries",
            "ignored",
            "",
            &json!({}),
            ThreadAttention::NeedsOwner,
            hirsel_proto::ThreadKind::Task,
            None
        )
        .await
        .unwrap(),
        (t.clone(), false)
    );
    assert!(s.hello_snapshot().await.unwrap().threads.contains(&t));
    let read = s.mark_thread_read(t.id).await.unwrap();
    assert!(read.settled_at.is_none());
    assert_eq!(read.attention, ThreadAttention::Quiet);
    let attended = s
        .update_thread(t.id, None, None, None, Some(ThreadAttention::NeedsOwner))
        .await
        .unwrap();
    assert!(attended.settled_at.is_none());
    assert!(attended.revision > read.revision);
    let settled = s.settle_thread(t.id, true).await.unwrap();
    assert!(settled.settled_at.is_some());
    assert_eq!(settled.attention, ThreadAttention::NeedsOwner);
    let reopened = s.settle_thread(t.id, false).await.unwrap();
    assert!(reopened.settled_at.is_none());
}
#[tokio::test]
async fn message_ownership_citations_and_pagination_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let (a, _) = s
        .create_thread(
            "a",
            "A",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let (b, _) = s
        .create_thread(
            "b",
            "B",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let (m, _) = s
        .append_thread_owner_message(
            &s.history_id().await.unwrap(),
            a.id,
            "m",
            "look at B",
            None,
            &[],
            &[b.id],
            &[],
        )
        .await
        .unwrap();
    assert_eq!(m.mentions, vec![b.id]);
    assert_eq!(m.client_id.as_deref(), Some("m"));
    assert!(
        s.append_thread_owner_message(
            &s.history_id().await.unwrap(),
            b.id,
            "m",
            "wrong reuse",
            None,
            &[],
            &[],
            &[]
        )
        .await
        .is_err()
    );
    assert!(
        s.append_thread_chat(b.id, ChatAuthor::Agent, "wrong reply", Some(m.id), vec![])
            .await
            .is_err()
    );
    let reply = s
        .append_thread_chat(a.id, ChatAuthor::Agent, "answer", Some(m.id), vec![])
        .await
        .unwrap();
    let page = s.thread_detail(a.id, None, 1).await.unwrap();
    assert_eq!(page.messages, vec![reply]);
    assert!(page.has_more);
    assert!(
        s.thread_detail(b.id, None, 100)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    drop(s);
    let s = Storage::open(dir.path()).await.unwrap();
    let detail = s.thread_detail(a.id, None, 100).await.unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(detail.messages[0], m);
}
#[tokio::test]
async fn durable_turns_queue_and_activity_preserve_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let (t, _) = s
        .create_thread(
            "a",
            "A",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let (m, _) = s
        .append_thread_owner_message(
            &s.history_id().await.unwrap(),
            t.id,
            "message",
            "hello",
            None,
            &[],
            &[],
            &[],
        )
        .await
        .unwrap();
    let queued = s.queue_thread_turn(t.id, Some(m.id)).await.unwrap();
    assert_eq!(queued.state, ThreadTurnState::Queued);
    s.save_thread_request("message", &json!({"thread_id":t.id,"history_id":s.history_id().await.unwrap(),"turn_id":queued.id,"report_triggered":false}))
        .await
        .unwrap();
    assert_eq!(s.pending_thread_requests().await.unwrap().len(), 1);
    assert!(
        s.interrupt_unfinished_thread_turns()
            .await
            .unwrap()
            .is_empty()
    );
    let running = s.run_thread_turn(queued.id).await.unwrap();
    assert_eq!(running.state, ThreadTurnState::Running);
    assert!(
        s.append_thread_activity(0, Some(queued.id), "progress", &json!({}))
            .await
            .is_err()
    );
    s.append_thread_activity(
        t.id,
        Some(queued.id),
        "progress",
        &json!({"text":"working"}),
    )
    .await
    .unwrap();
    let done = s
        .finish_thread_turn(queued.id, ThreadTurnState::Completed, None)
        .await
        .unwrap();
    assert_eq!(
        s.finish_thread_turn(queued.id, ThreadTurnState::Failed, None)
            .await
            .unwrap(),
        done
    );
    assert!(s.remove_thread_request("message").await.unwrap());
    assert!(s.pending_thread_requests().await.unwrap().is_empty());
    assert_eq!(
        s.thread_detail(t.id, None, 100)
            .await
            .unwrap()
            .activities
            .len(),
        1
    );
}

#[tokio::test]
async fn accepted_message_and_durable_request_commit_together() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let (t, _) = s
        .create_thread(
            "a",
            "A",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let request = json!({"mode":"send","thread_action":null});
    let (m, _) = s
        .append_thread_owner_request(
            &s.history_id().await.unwrap(),
            t.id,
            "cmd",
            "hello".into(),
            &[],
            &[t.id],
            &[],
            &request,
        )
        .await
        .unwrap();
    drop(s);
    let s = Storage::open(dir.path()).await.unwrap();
    let pending = s.pending_thread_requests().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].1["message_id"], m.id);
    assert_eq!(pending[0].1["thread_id"], t.id);
    let owner: crate::lash_runtime::OwnerTurn =
        serde_json::from_value(pending[0].1.clone()).unwrap();
    assert_eq!(owner.client_id, "cmd");
    let turn = s.start_thread_turn(t.id, Some(m.id)).await.unwrap();
    let first = s
        .materialize_thread_reply(turn.id, "reply", Some(m.id), vec![])
        .await
        .unwrap();
    let replay = s
        .materialize_thread_reply(turn.id, "different replay", Some(m.id), vec![])
        .await
        .unwrap();
    assert_eq!(first, replay);
    assert_eq!(
        s.thread_detail(t.id, None, 100)
            .await
            .unwrap()
            .messages
            .len(),
        2
    );
}

#[tokio::test]
async fn persisted_turn_activity_replay_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let thread = s
        .create_thread(
            "work",
            "Work",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let t = s.start_thread_turn(thread.id, None).await.unwrap();
    let a = s
        .append_thread_activity_once(
            "turn:1:tool:0",
            thread.id,
            Some(t.id),
            "tool_completed",
            &json!({"name":"shell.run"}),
        )
        .await
        .unwrap();
    let again = s
        .append_thread_activity_once(
            "turn:1:tool:0",
            thread.id,
            Some(t.id),
            "tool_completed",
            &json!({"name":"different replay"}),
        )
        .await
        .unwrap();
    assert_eq!(a, again);
    assert_eq!(
        s.thread_detail(thread.id, None, 100)
            .await
            .unwrap()
            .activities
            .len(),
        1
    );
}
#[tokio::test]
async fn generated_controls_cannot_shadow_lifecycle_verbs() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let ui = json!({"type":"card","children":[{"type":"submit","action":"settle","label":"Continue","settles":false}]});
    assert!(
        s.create_thread(
            "a",
            "A",
            "",
            &ui,
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None
        )
        .await
        .is_err()
    );
    let (t, _) = s
        .create_thread(
            "b",
            "B",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    assert!(
        s.update_thread(t.id, None, None, Some(&ui), None)
            .await
            .is_err()
    );
    assert_eq!(s.thread(t.id).await.unwrap().unwrap(), t);
}
#[tokio::test]
async fn accepted_instrument_revision_is_consumed_and_conflicting_payload_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let (t, _) = s
        .create_thread(
            "t",
            "T",
            "",
            &json!({"type":"submit","action":"choose","label":"Choose","settles":false}),
            ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let action = crate::lash_runtime::ThreadActionContext {
        thread: t.clone().into(),
        action: "choose".into(),
        data: json!({}),
    };
    let request = json!({"mode":"send","thread_action":action});
    let (message, _) = s
        .append_thread_owner_request(
            &s.history_id().await.unwrap(),
            t.id,
            "action",
            "A".into(),
            &[],
            &[],
            &[],
            &request,
        )
        .await
        .unwrap();
    assert_eq!(
        s.thread(t.id).await.unwrap().unwrap().revision,
        t.revision + 1
    );
    assert_eq!(
        s.append_thread_owner_request(
            &s.history_id().await.unwrap(),
            t.id,
            "action",
            "A".into(),
            &[],
            &[],
            &[],
            &request
        )
        .await
        .unwrap(),
        (message, false)
    );
    let mut conflicting = request.clone();
    conflicting["thread_action"]["data"]["unexpected"] = json!(true);
    assert!(
        s.append_thread_owner_request(
            &s.history_id().await.unwrap(),
            t.id,
            "action",
            "B".into(),
            &[],
            &[],
            &[],
            &conflicting
        )
        .await
        .is_err()
    );
    assert!(
        s.append_thread_owner_request(
            &s.history_id().await.unwrap(),
            t.id,
            "another-action",
            "A".into(),
            &[],
            &[],
            &[],
            &request
        )
        .await
        .is_err()
    );
    assert_eq!(
        s.thread_detail(t.id, None, 100)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
}

async fn kinded_thread(
    storage: &Storage,
    client_id: &str,
    kind: ThreadKind,
    parent_thread_id: Option<u64>,
) -> Thread {
    storage
        .create_thread(
            client_id,
            client_id,
            "",
            &json!({}),
            ThreadAttention::Quiet,
            kind,
            parent_thread_id,
        )
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn kinds_enforce_every_parent_child_pair_and_survive_reload() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let space = kinded_thread(&storage, "space", ThreadKind::Space, None).await;
    let task = kinded_thread(&storage, "task", ThreadKind::Task, None).await;
    let nested_space =
        kinded_thread(&storage, "nested-space", ThreadKind::Space, Some(space.id)).await;
    let nested_task =
        kinded_thread(&storage, "nested-task", ThreadKind::Task, Some(space.id)).await;
    let task_child = kinded_thread(&storage, "task-child", ThreadKind::Task, Some(task.id)).await;
    assert!(
        storage
            .create_thread(
                "invalid-space-child",
                "invalid",
                "",
                &json!({}),
                ThreadAttention::Quiet,
                ThreadKind::Space,
                Some(task.id),
            )
            .await
            .is_err()
    );
    assert!(
        storage
            .create_thread(
                "task-child",
                "replay",
                "",
                &json!({}),
                ThreadAttention::Quiet,
                ThreadKind::Space,
                Some(task.id),
            )
            .await
            .is_err(),
        "an idempotency key cannot silently change kind"
    );
    drop(storage);
    let reopened = Storage::open(dir.path()).await.unwrap();
    for (id, kind) in [
        (space.id, ThreadKind::Space),
        (task.id, ThreadKind::Task),
        (nested_space.id, ThreadKind::Space),
        (nested_task.id, ThreadKind::Task),
        (task_child.id, ThreadKind::Task),
    ] {
        assert_eq!(reopened.thread(id).await.unwrap().unwrap().kind, kind);
    }
}

#[tokio::test]
async fn kind_conversion_is_revision_guarded_atomic_and_preserves_identity() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let history = storage.history_id().await.unwrap();

    let convertible = kinded_thread(&storage, "convertible", ThreadKind::Space, None).await;
    let child = kinded_thread(
        &storage,
        "convertible-task-child",
        ThreadKind::Task,
        Some(convertible.id),
    )
    .await;
    let converted = storage
        .set_addressed_thread_kind(
            &history,
            convertible.id,
            ThreadKind::Task,
            convertible.revision,
        )
        .await
        .unwrap();
    assert_eq!(converted.kind, ThreadKind::Task);
    assert_eq!(converted.id, convertible.id);
    assert_eq!(converted.revision, convertible.revision + 1);
    let no_op = storage
        .set_addressed_thread_kind(&history, converted.id, ThreadKind::Task, converted.revision)
        .await
        .unwrap();
    assert_eq!(no_op, converted);
    assert!(
        storage
            .set_addressed_thread_kind(&history, child.id, ThreadKind::Space, child.revision)
            .await
            .is_err(),
        "a child under a Task cannot become a Space"
    );
    assert!(
        storage
            .set_addressed_thread_kind(
                &history,
                converted.id,
                ThreadKind::Space,
                convertible.revision,
            )
            .await
            .is_err(),
        "stale revisions cannot convert"
    );
    assert!(
        storage
            .set_addressed_thread_kind(
                "stale-history",
                converted.id,
                ThreadKind::Space,
                converted.revision,
            )
            .await
            .is_err()
    );

    let blocked = kinded_thread(&storage, "blocked", ThreadKind::Space, None).await;
    let blocked_child = kinded_thread(
        &storage,
        "blocked-space-child",
        ThreadKind::Space,
        Some(blocked.id),
    )
    .await;
    assert!(
        storage
            .set_addressed_thread_kind(&history, blocked.id, ThreadKind::Task, blocked.revision,)
            .await
            .is_err()
    );
    assert_eq!(storage.thread(blocked.id).await.unwrap().unwrap(), blocked);
    assert_eq!(
        storage
            .thread(blocked_child.id)
            .await
            .unwrap()
            .unwrap()
            .parent_thread_id,
        Some(blocked.id)
    );

    let settled = kinded_thread(&storage, "settled", ThreadKind::Task, None).await;
    let settled = storage.settle_thread(settled.id, true).await.unwrap();
    assert!(
        storage
            .set_addressed_thread_kind(&history, settled.id, ThreadKind::Space, settled.revision,)
            .await
            .is_err()
    );
    let reopened = storage.settle_thread(settled.id, false).await.unwrap();
    let space = storage
        .set_addressed_thread_kind(&history, reopened.id, ThreadKind::Space, reopened.revision)
        .await
        .unwrap();
    assert_eq!(space.kind, ThreadKind::Space);
    assert!(space.settled_at.is_none());
    assert!(storage.settle_thread(space.id, true).await.is_err());
}

#[tokio::test]
async fn concurrent_space_child_creation_and_space_to_task_conversion_cannot_both_commit() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let history = storage.history_id().await.unwrap();
    let parent = kinded_thread(&storage, "parent", ThreadKind::Space, None).await;
    let conversion_storage = storage.clone();
    let creation_storage = storage.clone();
    let empty_instrument = json!({});
    let (conversion, creation) = tokio::join!(
        conversion_storage.set_addressed_thread_kind(
            &history,
            parent.id,
            ThreadKind::Task,
            parent.revision,
        ),
        creation_storage.create_thread(
            "racing-space-child",
            "child",
            "",
            &empty_instrument,
            ThreadAttention::Quiet,
            ThreadKind::Space,
            Some(parent.id),
        ),
    );
    assert_ne!(conversion.is_ok(), creation.is_ok());
    let parent = storage.thread(parent.id).await.unwrap().unwrap();
    let children: Vec<_> = storage
        .thread_snapshot()
        .await
        .unwrap()
        .into_iter()
        .filter(|thread| thread.parent_thread_id == Some(parent.id))
        .collect();
    assert!(
        parent.kind == ThreadKind::Space
            || children
                .iter()
                .all(|thread| thread.kind == ThreadKind::Task)
    );
}

#[tokio::test]
async fn generated_completion_and_task_to_space_conversion_commit_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let history = storage.history_id().await.unwrap();
    let instrument = json!({"type":"submit","action":"complete","label":"Complete","settles":true});

    for conversion_first in [false, true] {
        let suffix = if conversion_first {
            "convert"
        } else {
            "complete"
        };
        let client_id = format!("racing-completion-{suffix}");
        let task = storage
            .create_thread(
                &format!("racing-task-{suffix}"),
                "Racing task",
                "",
                &instrument,
                ThreadAttention::Quiet,
                ThreadKind::Task,
                None,
            )
            .await
            .unwrap()
            .0;
        let thread_action = crate::lash_runtime::ThreadActionContext {
            thread: task.clone().into(),
            action: "complete".into(),
            data: json!({}),
        };
        let request = json!({
            "mode": "send",
            "thread_action": thread_action,
        });

        let (acceptance, conversion) = if conversion_first {
            let (conversion, acceptance) = tokio::join!(
                storage.set_addressed_thread_kind(
                    &history,
                    task.id,
                    ThreadKind::Space,
                    task.revision,
                ),
                storage.append_thread_owner_request(
                    &history,
                    task.id,
                    &client_id,
                    "Complete".into(),
                    &[],
                    &[],
                    &[],
                    &request,
                ),
            );
            (acceptance, conversion)
        } else {
            tokio::join!(
                storage.append_thread_owner_request(
                    &history,
                    task.id,
                    &client_id,
                    "Complete".into(),
                    &[],
                    &[],
                    &[],
                    &request,
                ),
                storage.set_addressed_thread_kind(
                    &history,
                    task.id,
                    ThreadKind::Space,
                    task.revision,
                ),
            )
        };

        assert_ne!(acceptance.is_ok(), conversion.is_ok());
        let current = storage.thread(task.id).await.unwrap().unwrap();
        let detail = storage.thread_detail(task.id, None, 100).await.unwrap();
        if acceptance.is_ok() {
            assert_eq!(current.kind, ThreadKind::Task);
            assert!(current.settled_at.is_some());
            assert_eq!(detail.messages.len(), 1);
            assert!(storage.thread_request(&client_id).await.unwrap().is_some());
        } else {
            assert_eq!(current.kind, ThreadKind::Space);
            assert!(current.settled_at.is_none());
            assert!(detail.messages.is_empty());
            assert!(storage.thread_request(&client_id).await.unwrap().is_none());
        }
    }
}
