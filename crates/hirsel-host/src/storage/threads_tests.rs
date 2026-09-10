use super::Storage;
use hirsel_proto::{ChatAuthor, ThreadAttention, ThreadTurnState};
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
        .create_thread("a", "A", "", &json!({}), ThreadAttention::Quiet, None)
        .await
        .unwrap();
    let (b, _) = s
        .create_thread("b", "B", "", &json!({}), ThreadAttention::Quiet, None)
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
        .create_thread("a", "A", "", &json!({}), ThreadAttention::Quiet, None)
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
        .create_thread("a", "A", "", &json!({}), ThreadAttention::Quiet, None)
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
        .create_thread("work", "Work", "", &json!({}), ThreadAttention::Quiet, None)
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
        s.create_thread("a", "A", "", &ui, ThreadAttention::Quiet, None)
            .await
            .is_err()
    );
    let (t, _) = s
        .create_thread("b", "B", "", &json!({}), ThreadAttention::Quiet, None)
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
        .create_thread("t", "T", "", &json!({}), ThreadAttention::Quiet, None)
        .await
        .unwrap();
    let request =
        json!({"mode":"send","thread_action":{"thread":t,"action":"choose","data":{"choice":"A"}}});
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
    conflicting["thread_action"]["data"]["choice"] = json!("B");
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
