use crate::store::{LocalStore, PendingSend};
use chrono::Utc;
use hirsel_proto::{
    ChatAuthor, ChatMessage, ClientToHost, Thread, ThreadAttention, ThreadDetail, ThreadTurn,
    ThreadTurnState, TurnEventKind,
};

fn thread(revision: u64) -> Thread {
    Thread {
        icon: None,
        showcased_artifact_id: None,
        parent_thread_id: None,
        pinned_at: None,
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
        vec![9],
        vec![44],
    )
}
#[test]
fn ordinary_thread_survives_snapshot_read_and_stale_upsert() {
    let mut store = LocalStore::default();
    let mut work = thread(2);
    work.read = true;
    store.apply_hello_ok(
        "test-store-a".into(),
        vec![work.clone()],
        vec![],
        "test".into(),
    );
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
    store.apply_hello_ok(
        "test-store-a".into(),
        vec![work.clone()],
        vec![],
        "test".into(),
    );

    let started_at = work.last_activity_at + chrono::Duration::seconds(10);
    let mut turn = ThreadTurn {
        requester_thread_id: None,
        requester_turn_id: None,
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
    store.apply_message(message(2, 5, Some("first")));
    store.apply_hello_ok(
        "test-store-a".into(),
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
        related_items: vec![],
        brief: hirsel_proto::ThreadBrief {
            text: String::new(),
            artifact_ids: vec![],
        },
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
        requester_thread_id: None,
        requester_turn_id: None,
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
    store.apply_message(removed.clone());
    store.apply_hello_ok(
        "test-store-a".into(),
        vec![thread(1)],
        vec![],
        "test".into(),
    );
    store.requests.push(("history".into(), 5));
    store.apply_detail(
        "history",
        ThreadDetail {
            related_items: vec![],
            brief: hirsel_proto::ThreadBrief {
                text: String::new(),
                artifact_ids: vec![],
            },
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

#[test]
fn changed_history_clears_owned_state_but_preserves_plain_unsent_text() {
    let mut store = LocalStore::default();
    store.apply_hello_ok("A".into(), vec![thread(1)], vec![], "test".into());
    store.add_optimistic_send(pending(5, "old-send"));
    let text = store.pending_sends().next().unwrap().body.clone();
    store
        .pending_creates
        .push(("old-create".into(), "Title".into(), None));
    store.requests.push(("old-open".into(), 5));
    store.opened_threads.push(5);
    store.apply_delta(
        5,
        7,
        1,
        TurnEventKind::Prose {
            text: "old stream".into(),
        },
    );
    store.remove_message(88);
    assert!(!store.apply_hello_ok("A".into(), vec![thread(1)], vec![], "test".into()));
    assert_eq!(store.pending_sends().count(), 1);
    assert!(store.apply_hello_ok("B".into(), vec![thread(1)], vec![], "test".into()));
    assert!(
        store.messages.is_empty()
            && store.pending_creates.is_empty()
            && store.requests.is_empty()
            && store.opened_threads.is_empty()
            && store.streams.is_empty()
    );
    assert_eq!(store.recovered_drafts, vec![text]);
    store.apply_message(message(88, 5, None));
    assert_eq!(
        store.messages.len(),
        1,
        "old tombstones must not affect a new store"
    );
}
#[test]
fn queued_later_turn_does_not_own_running_stream() {
    let mut store = LocalStore::default();
    let now = Utc::now();
    store.upsert_turn(ThreadTurn {
        requester_thread_id: None,
        requester_turn_id: None,
        id: 7,
        thread_id: 5,
        owner_message_id: Some(1),
        agent_message_id: None,
        state: ThreadTurnState::Running,
        started_at: now,
        finished_at: None,
    });
    store.apply_delta(
        5,
        7,
        1,
        TurnEventKind::Prose {
            text: "first".into(),
        },
    );
    store.upsert_turn(ThreadTurn {
        requester_thread_id: None,
        requester_turn_id: None,
        id: 8,
        thread_id: 5,
        owner_message_id: Some(2),
        agent_message_id: None,
        state: ThreadTurnState::Queued,
        started_at: now,
        finished_at: None,
    });
    store.apply_delta(
        5,
        7,
        2,
        TurnEventKind::Prose {
            text: "second".into(),
        },
    );
    assert_eq!(store.streams[0].turn_id, 7);
    assert_eq!(store.streams[0].events.len(), 2);
}

#[test]
fn references_survive_retry_snapshot_but_never_history_reset() {
    let mut store = LocalStore::default();
    store.apply_hello_ok("old".into(), vec![thread(1)], vec![], "test".into());
    store.add_optimistic_send(pending(5, "with-artifact"));
    let snapshot = store.snapshot();
    let crate::ChatEntry::Pending(saved) = &snapshot.messages[0] else {
        panic!("expected pending")
    };
    assert_eq!(saved.artifact_ids, vec![44]);
    let wire = crate::client::pending_to_wire(saved);
    assert!(
        matches!(wire, ClientToHost::SendThreadMessage { artifact_ids, mentions, .. } if artifact_ids == vec![44] && mentions == vec![9])
    );
    store.apply_hello_ok("new".into(), vec![thread(1)], vec![], "test".into());
    assert!(store.messages.is_empty());
    assert_eq!(store.recovered_drafts, vec!["same words"]);
}

#[test]
fn current_brief_is_per_thread_and_survives_paginated_history() {
    let mut store = LocalStore::default();
    for (id, text) in [(5, "current A"), (6, "current B"), (5, "current A")] {
        let mut t = thread(1);
        t.id = id;
        let request = format!("open-{id}");
        store.requests.push((request.clone(), id));
        store.apply_detail(
            &request,
            ThreadDetail {
                related_items: vec![],
                thread: t,
                brief: hirsel_proto::ThreadBrief {
                    text: text.into(),
                    artifact_ids: vec![44],
                },
                messages: vec![],
                turns: vec![],
                activities: vec![],
                has_more: true,
            },
        );
    }
    let snap = store.snapshot();
    assert_eq!(snap.briefs.len(), 2);
    assert_eq!(
        snap.briefs.iter().find(|b| b.thread_id == 5).unwrap().text,
        "current A"
    );
    assert_eq!(
        snap.briefs
            .iter()
            .find(|b| b.thread_id == 6)
            .unwrap()
            .artifact_ids,
        vec![44]
    );
}

#[test]
fn assignment_refresh_fetches_authoritative_detail_only_for_opened_thread() {
    let mut store = LocalStore::default();
    assert!(store.refresh_open_thread(5).is_none());
    store.opened_threads.push(5);
    let ClientToHost::OpenThread {
        client_id,
        thread_id,
        before_id,
    } = store.refresh_open_thread(5).unwrap()
    else {
        panic!("expected detail request")
    };
    assert_eq!((thread_id, before_id), (5, None));
    assert_eq!(store.requests, vec![(client_id, 5)]);
    assert!(store.refresh_open_thread(6).is_none());
}

fn link(id: u64, thread_id: u64) -> hirsel_proto::ThreadRelatedItem {
    hirsel_proto::ThreadRelatedItem {
        id,
        thread_id,
        target: hirsel_proto::ThreadRelatedTarget::Url {
            url: format!("https://example.com/{id}?q=1#part"),
        },
        title: Some(format!("Reference {id}")),
        created_at: Utc::now(),
    }
}

fn link_detail(revision: u64, links: Vec<hirsel_proto::ThreadRelatedItem>) -> ThreadDetail {
    ThreadDetail {
        thread: thread(revision),
        brief: hirsel_proto::ThreadBrief {
            text: "Assignment".into(),
            artifact_ids: vec![44],
        },
        related_items: links,
        messages: vec![],
        turns: vec![],
        activities: vec![],
        has_more: true,
    }
}

#[test]
fn related_items_are_complete_per_thread_even_in_paginated_detail() {
    let mut store = LocalStore::default();
    store.apply_hello_ok("A".into(), vec![thread(1)], vec![], "test".into());
    let other = link(9, 6);
    assert!(store.apply_thread_related("A", 6, 1, vec![other.clone()]));
    let first = link(1, 5);
    let second = link(2, 5);
    store.requests.push(("open".into(), 5));
    store.apply_detail("open", link_detail(1, vec![first.clone(), second.clone()]));
    assert_eq!(
        store.snapshot().related_items,
        vec![other.clone(), first, second.clone()]
    );
    store.requests.push(("older-messages".into(), 5));
    store.apply_detail("older-messages", link_detail(2, vec![second.clone()]));
    assert_eq!(store.snapshot().related_items, vec![other.clone(), second]);
    assert_eq!(store.briefs[0].artifact_ids, vec![44]);
    assert!(store.history_has_more.contains(&5));
    assert!(store.apply_thread_related("A", 5, 3, vec![]));
    assert_eq!(store.snapshot().related_items, vec![other]);
    assert!(store.messages.is_empty() && store.activities.is_empty());
}

#[test]
fn related_items_accept_equal_revision_after_upsert_and_reject_stale_or_foreign_state() {
    let mut store = LocalStore::default();
    store.apply_hello_ok("A".into(), vec![thread(1)], vec![], "test".into());
    let saved = link(1, 5);
    store.upsert_thread(thread(2));
    let metadata = store.threads[0].clone();
    assert!(store.apply_thread_related("A", 5, 2, vec![saved.clone()]));
    assert_eq!(
        store.threads[0], metadata,
        "links do not synthesize Thread activity"
    );
    assert!(!store.apply_thread_related("B", 5, 100, vec![]));
    assert!(!store.apply_thread_related("A", 5, 1, vec![]));
    assert!(!store.apply_thread_related("A", 5, 3, vec![link(2, 6)]));
    assert_eq!(store.related_items, vec![saved.clone()]);
    // An event may precede its metadata upsert. Its revision still prevents regression.
    assert!(store.apply_thread_related("A", 5, 4, vec![]));
    assert!(!store.apply_thread_related("A", 5, 3, vec![saved.clone()]));
    store.requests.push(("stale-detail".into(), 5));
    store.apply_detail("stale-detail", link_detail(3, vec![saved.clone()]));
    assert!(store.related_items.is_empty());
    // Equal revision no-op retries remain valid, including empty snapshots.
    assert!(store.apply_thread_related("A", 5, 4, vec![]));
    store.upsert_thread(thread(5));
    assert!(store.apply_thread_related("A", 5, 4, vec![saved]));
    assert_eq!(store.threads[0].revision, 5);
}

#[test]
fn related_item_identity_and_revision_are_discarded_on_history_reset() {
    let mut store = LocalStore::default();
    let saved = link(1, 5);
    assert!(!store.apply_thread_related("A", 5, 1, vec![saved.clone()]));
    store.apply_hello_ok("A".into(), vec![thread(10)], vec![], "test".into());
    assert!(store.apply_thread_related("A", 5, 10, vec![saved.clone()]));
    store.requests.push(("old-open".into(), 5));
    store.apply_hello_ok("B".into(), vec![thread(1)], vec![], "test".into());
    assert!(store.snapshot().related_items.is_empty());
    store.apply_detail("old-open", link_detail(10, vec![saved.clone()]));
    assert!(store.related_items.is_empty());
    assert!(!store.apply_thread_related("A", 5, 11, vec![saved.clone()]));
    assert!(store.apply_thread_related("B", 5, 1, vec![saved.clone()]));
    assert_eq!(store.snapshot().related_items, vec![saved]);
}

#[test]
fn thread_detail_requires_explicit_related_items() {
    let mut value = serde_json::to_value(link_detail(1, vec![])).unwrap();
    value.as_object_mut().unwrap().remove("related_items");
    assert!(serde_json::from_value::<ThreadDetail>(value).is_err());
}

#[test]
fn related_revision_orders_independently_of_metadata_and_detail_advances_it() {
    let mut store = LocalStore::default();
    store.apply_hello_ok("A".into(), vec![thread(1)], vec![], "test".into());
    assert!(store.apply_thread_related("A", 5, 1, vec![link(1, 5)]));
    store.upsert_thread(thread(3));
    let metadata = store.threads[0].clone();
    let target = hirsel_proto::ThreadRelatedItem {
        id: 2,
        thread_id: 5,
        target: hirsel_proto::ThreadRelatedTarget::Thread {
            history_id: "A".into(),
            thread_id: 0,
        },
        title: None,
        created_at: Utc::now(),
    };
    assert!(store.apply_thread_related("A", 5, 2, vec![target.clone()]));
    assert_eq!(store.related_items, vec![target.clone()]);
    assert_eq!(store.threads[0], metadata);
    assert!(!store.apply_thread_related("A", 5, 1, vec![link(1, 5)]));
    assert!(store.apply_thread_related("A", 5, 2, vec![target.clone()]));
    store.requests.push(("fresh-detail".into(), 5));
    store.apply_detail("fresh-detail", link_detail(4, vec![link(3, 5), target]));
    let fresh = store.snapshot();
    assert_eq!(fresh.related_items.len(), 2);
    assert_eq!(fresh.threads[0].revision, 4);
    assert!(!store.apply_thread_related("A", 5, 3, vec![]));
    assert_eq!(store.snapshot().related_items, fresh.related_items);
    // A current replay acknowledgement after removal cannot restore the prior add.
    assert!(store.apply_thread_related("A", 5, 5, vec![]));
    assert!(store.apply_thread_related("A", 5, 5, vec![]));
    assert!(store.related_items.is_empty());
}
