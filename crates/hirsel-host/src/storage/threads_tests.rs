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
            ThreadAttention::NeedsOwner
        )
        .await
        .unwrap(),
        (t.clone(), false)
    );
    assert!(s.hello_snapshot(None).await.unwrap().threads.contains(&t));
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
        .create_thread("a", "A", "", &json!({}), ThreadAttention::Quiet)
        .await
        .unwrap();
    let (b, _) = s
        .create_thread("b", "B", "", &json!({}), ThreadAttention::Quiet)
        .await
        .unwrap();
    let (m, _) = s
        .append_thread_owner_message(a.id, "m", "look at B", None, &[], &[b.id])
        .await
        .unwrap();
    assert_eq!(m.mentions, vec![b.id]);
    assert_eq!(m.client_id.as_deref(), Some("m"));
    assert!(
        s.append_thread_owner_message(b.id, "m", "wrong reuse", None, &[], &[])
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
        .create_thread("a", "A", "", &json!({}), ThreadAttention::Quiet)
        .await
        .unwrap();
    let (m, _) = s
        .append_thread_owner_message(t.id, "message", "hello", None, &[], &[])
        .await
        .unwrap();
    let queued = s.queue_thread_turn(t.id, Some(m.id)).await.unwrap();
    assert_eq!(queued.state, ThreadTurnState::Queued);
    s.save_thread_request("message", &json!({"thread_id":t.id}))
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
async fn legacy_import_is_once_and_leaves_ambiguous_history_in_coordinator() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let first = s
        .append_chat(ChatAuthor::Owner, "old work", None)
        .await
        .unwrap();
    let reply = s
        .append_chat(ChatAuthor::Agent, "reply", Some(first.id))
        .await
        .unwrap();
    {
        let c = s.conn.lock().await;
        c.execute("DELETE FROM meta WHERE key='threads_import_v1'", [])
            .unwrap();
        c.execute("INSERT INTO pings(id,kind,source_kind,name,description,content,ui,anchor,requires_response,quick_replies,status,read,archived,ts) VALUES(5,'info','agent','buy-groceries','Buy groceries','Buy groceries','{}',?1,0,'[]','open',0,0,?2)",rusqlite::params![first.id,chrono::Utc::now().to_rfc3339()]).unwrap();
    }
    drop(s);
    let s = Storage::open(dir.path()).await.unwrap();
    assert_eq!(s.thread(5).await.unwrap().unwrap().title, "buy-groceries");
    assert_eq!(
        s.chat_message(reply.id).await.unwrap().unwrap().thread_id,
        5
    );
    {
        let c = s.conn.lock().await;
        c.execute("INSERT INTO pings(id,kind,source_kind,name,description,content,ui,anchor,requires_response,quick_replies,status,read,archived,ts) VALUES(6,'info','agent','runtime-info','later','later','{}',1,0,'[]','open',0,0,?1)",[chrono::Utc::now().to_rfc3339()]).unwrap();
    }
    drop(s);
    let s = Storage::open(dir.path()).await.unwrap();
    assert!(s.thread(6).await.unwrap().is_none());
    assert_eq!(s.thread_snapshot().await.unwrap().len(), 2);
}
#[tokio::test]
async fn accepted_message_and_durable_request_commit_together() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let (t, _) = s
        .create_thread("a", "A", "", &json!({}), ThreadAttention::Quiet)
        .await
        .unwrap();
    let request = json!({"mode":"send","thread_action":null});
    let (m, _) = s
        .append_thread_owner_request(t.id, "cmd", "hello".into(), &[], &[0], &request)
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
async fn ambiguous_legacy_anchor_stays_in_orchestrator_and_housekeeping_becomes_activity() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let m = s
        .append_chat(ChatAuthor::Owner, "two work objects", None)
        .await
        .unwrap();
    let reply = s
        .append_chat(ChatAuthor::Agent, "shared reply", Some(m.id))
        .await
        .unwrap();
    {
        let c = s.conn.lock().await;
        c.execute("DELETE FROM meta WHERE key='threads_import_v1'", [])
            .unwrap();
        for (id, name) in [(1, "session-rotated"), (2, "work-a"), (3, "work-b")] {
            c.execute("INSERT INTO pings(id,kind,source_kind,name,description,content,ui,anchor,requires_response,quick_replies,status,read,archived,ts) VALUES(?1,'info','agent',?2,'legacy','legacy','{}',?3,0,'[]','open',0,0,?4)",rusqlite::params![id,name,m.id,chrono::Utc::now().to_rfc3339()]).unwrap();
        }
    }
    drop(s);
    let s = Storage::open(dir.path()).await.unwrap();
    assert!(s.thread(1).await.unwrap().is_none());
    assert!(s.thread(2).await.unwrap().is_some());
    assert!(s.thread(3).await.unwrap().is_some());
    assert_eq!(
        s.chat_message(reply.id).await.unwrap().unwrap().thread_id,
        0
    );
    assert_eq!(
        s.thread_detail(0, None, 100)
            .await
            .unwrap()
            .activities
            .len(),
        1
    );
}
#[tokio::test]
async fn persisted_turn_activity_replay_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let t = s.start_thread_turn(0, None).await.unwrap();
    let a = s
        .append_thread_activity_once(
            "turn:1:tool:0",
            0,
            Some(t.id),
            "tool_completed",
            &json!({"name":"shell.run"}),
        )
        .await
        .unwrap();
    let again = s
        .append_thread_activity_once(
            "turn:1:tool:0",
            0,
            Some(t.id),
            "tool_completed",
            &json!({"name":"different replay"}),
        )
        .await
        .unwrap();
    assert_eq!(a, again);
    assert_eq!(
        s.thread_detail(0, None, 100)
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
        s.create_thread("a", "A", "", &ui, ThreadAttention::Quiet)
            .await
            .is_err()
    );
    let (t, _) = s
        .create_thread("b", "B", "", &json!({}), ThreadAttention::Quiet)
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
        .create_thread("t", "T", "", &json!({}), ThreadAttention::Quiet)
        .await
        .unwrap();
    let request =
        json!({"mode":"send","thread_action":{"thread":t,"action":"choose","data":{"choice":"A"}}});
    let (message, _) = s
        .append_thread_owner_request(t.id, "action", "A".into(), &[], &[], &request)
        .await
        .unwrap();
    assert_eq!(
        s.thread(t.id).await.unwrap().unwrap().revision,
        t.revision + 1
    );
    assert_eq!(
        s.append_thread_owner_request(t.id, "action", "A".into(), &[], &[], &request)
            .await
            .unwrap(),
        (message, false)
    );
    let mut conflicting = request.clone();
    conflicting["thread_action"]["data"]["choice"] = json!("B");
    assert!(
        s.append_thread_owner_request(t.id, "action", "B".into(), &[], &[], &conflicting)
            .await
            .is_err()
    );
    assert!(
        s.append_thread_owner_request(t.id, "another-action", "A".into(), &[], &[], &request)
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

#[tokio::test]
async fn populated_legacy_database_import_keeps_messages_links_and_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
    conn.execute_batch("CREATE TABLE chat_messages (id INTEGER PRIMARY KEY AUTOINCREMENT, author TEXT NOT NULL, body TEXT NOT NULL, ref INTEGER, ts TEXT NOT NULL, tool_calls TEXT NOT NULL DEFAULT '[]');
        INSERT INTO chat_messages VALUES(114,'owner','add a task for buying groceries',NULL,'2026-09-09T15:45:00Z','[]');
        INSERT INTO chat_messages VALUES(115,'agent','Added groceries',114,'2026-09-09T15:45:01Z','[]');
        CREATE TABLE client_messages (client_id TEXT PRIMARY KEY,msg_id INTEGER NOT NULL REFERENCES chat_messages(id));
        INSERT INTO client_messages VALUES('original-request',114);
        CREATE TABLE blobs (id TEXT PRIMARY KEY,name TEXT NOT NULL,mime TEXT NOT NULL,size INTEGER NOT NULL,path TEXT NOT NULL,created_ts TEXT NOT NULL);
        INSERT INTO blobs VALUES('list','list.txt','text/plain',4,'blobs/list','2026-09-09T15:45:00Z');
        CREATE TABLE message_attachments (message_id INTEGER NOT NULL REFERENCES chat_messages(id),blob_id TEXT NOT NULL REFERENCES blobs(id),position INTEGER NOT NULL,PRIMARY KEY(message_id,position));
        INSERT INTO message_attachments VALUES(114,'list',0);").unwrap();
    drop(conn);
    let s = Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        s.chat_message(114).await.unwrap().unwrap().body,
        "add a task for buying groceries"
    );
    assert_eq!(s.chat_message(115).await.unwrap().unwrap().thread_id, 0);
    {
        let conn = s.conn.lock().await;
        assert_eq!(
            conn.query_row(
                "SELECT msg_id FROM client_messages WHERE client_id='original-request'",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
            114
        );
        assert_eq!(
            conn.query_row(
                "SELECT blob_id FROM message_attachments WHERE message_id=114",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "list"
        );
        assert!(
            conn.pragma_query_value(None, "foreign_keys", |row| row.get::<_, bool>(0))
                .unwrap()
        );
        assert!(
            conn.execute("UPDATE chat_messages SET thread_id=999 WHERE id=114", [])
                .is_err()
        );
    }
    drop(s);
    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened.chat_message(115).await.unwrap().unwrap().body,
        "Added groceries"
    );
}
