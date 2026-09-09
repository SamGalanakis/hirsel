use crate::store::{LocalStore, PendingSend};
use chrono::Utc;
use hirsel_proto::{
    ChatAuthor, ChatMessage, ClientToHost, Thread, ThreadAttention, ThreadDetail, ThreadTurn,
    ThreadTurnState, TurnEventKind,
};

fn thread(revision: u64) -> Thread {
    Thread {
        id: 5,
        title: "Groceries".into(),
        description: "Buy milk".into(),
        instrument: serde_json::json!({"type":"text","text":"Milk"}),
        attention: ThreadAttention::Quiet,
        settled_at: None,
        archived_at: None,
        snoozed_until: None,
        read: false,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        revision,
        running_turn: None,
        queued_turn_count: 0,
        last_finished_turn: None,
        last_activity_at: Utc::now(),
    }
}
fn message(id: u64, thread_id: u64, client_id: Option<&str>) -> ChatMessage {
    ChatMessage {
        artifact_ids: vec![],
        id,
        thread_id,
        client_id: client_id.map(str::to_owned),
        mentions: vec![9],
        author: ChatAuthor::Owner,
        body: "same words".into(),
        r#ref: None,
        ts: Utc::now(),
        attachments: vec![],
        tool_calls: vec![],
    }
}
fn pending(thread_id: u64, client_id: &str) -> PendingSend {
    PendingSend::new(
        thread_id,
        vec!["blob-1".into()],
        client_id.into(),
        "same words".into(),
        None,
        vec![9],
    )
}
#[test]
fn ordinary_thread_survives_snapshot_read_and_stale_upsert() {
    let mut store = LocalStore::default();
    let mut work = thread(2);
    work.read = true;
    store.apply_hello_ok(0, vec![], vec![work.clone()], vec![], "test".into());
    store.upsert_thread(thread(1));
    assert_eq!(store.snapshot().threads, vec![work]);
    assert!(store.threads[0].settled_at.is_none());
}

#[test]
fn unopened_thread_accepts_same_revision_execution_and_activity_projections() {
    let mut store = LocalStore::default();
    let mut work = thread(2);
    work.attention = ThreadAttention::NeedsOwner;
    work.read = true;
    work.settled_at = Some(work.created_at);
    work.queued_turn_count = 2;
    store.apply_hello_ok(0, vec![], vec![work.clone()], vec![], "test".into());

    let started_at = work.last_activity_at + chrono::Duration::seconds(10);
    let mut turn = ThreadTurn {
        id: 10,
        thread_id: work.id,
        owner_message_id: Some(1),
        agent_message_id: None,
        state: ThreadTurnState::Running,
        started_at,
        finished_at: None,
    };
    work.running_turn = Some(turn.clone());
    work.queued_turn_count = 1;
    work.last_activity_at = started_at;
    store.upsert_thread(work.clone());
    assert_eq!(store.snapshot().threads, vec![work.clone()]);

    let finished_at = started_at + chrono::Duration::seconds(30);
    turn.state = ThreadTurnState::Completed;
    turn.agent_message_id = Some(2);
    turn.finished_at = Some(finished_at);
    work.running_turn = None;
    work.last_finished_turn = Some(turn);
    work.last_activity_at = finished_at;
    store.upsert_thread(work.clone());
    assert_eq!(store.snapshot().threads, vec![work.clone()]);
    assert_eq!(work.attention, ThreadAttention::NeedsOwner);
    assert!(work.read && work.settled_at.is_some());
    assert!(store.opened_threads.is_empty());
    assert!(store.turns.is_empty());

    // Activity can move backward when a message is removed; arrival order is authoritative.
    work.last_activity_at = finished_at + chrono::Duration::seconds(60);
    store.upsert_thread(work.clone());
    assert_eq!(store.snapshot().threads, vec![work.clone()]);
    work.last_activity_at = finished_at;
    store.upsert_thread(work.clone());
    let mut stale = thread(1);
    stale.last_activity_at = finished_at;
    store.upsert_thread(stale);
    assert_eq!(store.snapshot().threads, vec![work]);
}
#[test]
fn echo_and_reconnect_reconcile_by_client_and_owner_thread_never_body() {
    let mut store = LocalStore::default();
    store.add_optimistic_send(pending(5, "first"));
    store.add_optimistic_send(pending(6, "second"));
    store.apply_message(message(1, 6, Some("first")));
    assert_eq!(store.pending_sends().count(), 2);
    store.apply_hello_ok(
        2,
        vec![message(2, 5, Some("first"))],
        vec![thread(1)],
        vec![],
        "test".into(),
    );
    assert_eq!(
        store
            .pending_sends()
            .map(|s| s.client_id.as_str())
            .collect::<Vec<_>>(),
        vec!["second"]
    );
    let confirmed = store
        .messages
        .iter()
        .find_map(|m| match m {
            crate::ChatEntry::Confirmed(m) if m.id == 2 => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(confirmed.thread_id, 5);
    assert_eq!(confirmed.mentions, vec![9]);
    assert_eq!(confirmed.client_id.as_deref(), Some("first"));
}
#[test]
fn open_requires_matching_request_and_message_ownership() {
    let mut store = LocalStore::default();
    store.requests.push(("open".into(), 5));
    let detail = ThreadDetail {
        thread: thread(1),
        messages: vec![message(1, 5, None), message(2, 9, None)],
        turns: vec![],
        activities: vec![],
        has_more: false,
    };
    store.apply_detail("wrong", detail.clone());
    assert!(store.messages.is_empty());
    store.apply_detail("open", detail);
    assert_eq!(store.messages.len(), 1);
    assert_eq!(store.opened_threads, vec![5]);
}
#[test]
fn thread_stream_rejects_prior_turn_duplicate_sequence_and_terminal_deltas() {
    let mut store = LocalStore::default();
    let prose = |text: &str| TurnEventKind::Prose { text: text.into() };
    store.apply_delta(5, 10, 0, prose("old"));
    store.apply_delta(5, 11, 0, prose("new"));
    store.apply_delta(5, 10, 1, prose("late"));
    store.apply_delta(5, 11, 0, prose("duplicate"));
    store.apply_delta(6, 12, 0, prose("other thread"));
    let turn = ThreadTurn {
        id: 11,
        thread_id: 5,
        owner_message_id: None,
        agent_message_id: Some(8),
        state: ThreadTurnState::Completed,
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
    };
    store.upsert_turn(turn);
    store.apply_delta(5, 11, 1, prose("after terminal"));
    assert_eq!(store.streams[0].events, vec![prose("new")]);
    assert!(store.streams[0].finished);
    assert_eq!(store.streams[1].events, vec![prose("other thread")]);
}
#[test]
fn native_outbound_preserves_ownership_attachments_and_citations() {
    assert!(
        matches!(crate::client::pending_to_wire(&pending(5,"send")),ClientToHost::SendThreadMessage{thread_id:5,attachments,mentions,client_id,..} if attachments==vec!["blob-1"] && mentions==vec![9] && client_id=="send")
    );
}

#[test]
fn removed_message_stays_removed_across_late_echo_snapshot_and_open_history() {
    let mut store = LocalStore::default();
    let removed = message(1, 5, Some("cancelled"));
    store.apply_message(removed.clone());
    store.apply_message(message(2, 9, None));
    store.remove_message(1);
    store.add_optimistic_send(pending(5, "cancelled"));
    store.apply_message(removed.clone());
    assert_eq!(store.pending_sends().count(), 0);
    store.apply_hello_ok(
        2,
        vec![removed.clone()],
        vec![thread(1)],
        vec![],
        "test".into(),
    );
    store.requests.push(("history".into(), 5));
    store.apply_detail(
        "history",
        ThreadDetail {
            thread: thread(1),
            messages: vec![removed],
            turns: vec![],
            activities: vec![],
            has_more: false,
        },
    );
    assert_eq!(store.messages.len(), 1);
    assert_eq!(store.messages[0].id(), Some(2));
}
