use super::super::Storage;
use chrono::Utc;
use hirsel_proto::ChatAuthor;
use hirsel_proto::EventStatus;
use hirsel_proto::PingStatus;
use hirsel_proto::QuickReply;

#[tokio::test]
async fn resolving_a_ping_twice_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap();
    let ping = storage
        .create_ping(
            "needs-reply",
            "Needs reply",
            "Needs reply",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    let first = storage.resolve_ping(ping.id).await.unwrap().unwrap();
    let second = storage.resolve_ping(ping.id).await.unwrap().unwrap();
    assert_eq!(first.status, PingStatus::Done);
    assert_eq!(second.status, PingStatus::Done);
}

#[tokio::test]
async fn reopening_a_ping_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap();
    let ping = storage
        .create_ping(
            "needs-reply",
            "Needs reply",
            "Needs reply",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    let done = storage.resolve_ping(ping.id).await.unwrap().unwrap();
    let open = storage.reopen_ping(ping.id).await.unwrap().unwrap();
    let open_again = storage.reopen_ping(ping.id).await.unwrap().unwrap();

    assert_eq!(done.status, PingStatus::Done);
    assert_eq!(open.status, PingStatus::Open);
    assert_eq!(open_again.status, PingStatus::Open);
    assert!(storage.reopen_ping(99_999).await.unwrap().is_none());
}

#[tokio::test]
async fn archiving_an_open_event_is_idempotent_and_unarchive_does_not_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap();
    let event = storage
        .create_ping(
            "needs-reply",
            "Needs reply",
            "Needs reply",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    let archived = storage.archive_event(event.id).await.unwrap().unwrap();
    let archived_again = storage.archive_event(event.id).await.unwrap().unwrap();
    let unarchived = storage.unarchive_event(event.id).await.unwrap().unwrap();

    assert_eq!(archived.status, EventStatus::Done);
    assert!(archived.archived);
    assert!(archived.archived_at.is_some());
    assert_eq!(archived_again, archived);
    assert_eq!(unarchived.status, EventStatus::Done);
    assert!(!unarchived.archived);
    assert_eq!(unarchived.archived_at, None);
    assert!(storage.archive_event(99_999).await.unwrap().is_none());
    assert!(storage.unarchive_event(99_999).await.unwrap().is_none());
}

#[tokio::test]
async fn bulk_archive_counts_only_newly_finished_events() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap()
        .id;
    let done = storage
        .create_ping("done", "Done", "Done", anchor, true, Vec::new())
        .await
        .unwrap();
    storage.resolve_ping(done.id).await.unwrap();
    let read_info = storage
        .create_ping(
            "read-info",
            "Read info",
            "Read info",
            anchor,
            false,
            Vec::new(),
        )
        .await
        .unwrap();
    storage.mark_ping_read(read_info.id).await.unwrap();
    let open_judgment = storage
        .create_ping(
            "open-judgment",
            "Open judgment",
            "Open judgment",
            anchor,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    storage.mark_ping_read(open_judgment.id).await.unwrap();
    let unread_info = storage
        .create_ping(
            "unread-info",
            "Unread info",
            "Unread info",
            anchor,
            false,
            Vec::new(),
        )
        .await
        .unwrap();
    let already_archived = storage
        .create_ping(
            "already-archived",
            "Already archived",
            "Already archived",
            anchor,
            false,
            Vec::new(),
        )
        .await
        .unwrap();
    storage.archive_event(already_archived.id).await.unwrap();

    assert_eq!(storage.archive_finished_events().await.unwrap(), 2);
    assert_eq!(storage.archive_finished_events().await.unwrap(), 0);

    for event_id in [done.id, read_info.id, already_archived.id] {
        let event = storage.ping(event_id).await.unwrap().unwrap();
        assert!(event.archived);
        assert_eq!(event.status, EventStatus::Done);
        assert!(event.archived_at.is_some());
    }
    for event_id in [open_judgment.id, unread_info.id] {
        assert!(!storage.ping(event_id).await.unwrap().unwrap().archived);
    }
}

#[tokio::test]
async fn snooze_round_trips_and_only_expired_snoozes_are_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap();
    let expired = storage
        .create_ping("expired", "Expired", "Expired", anchor.id, true, Vec::new())
        .await
        .unwrap();
    let future = storage
        .create_ping("future", "Future", "Future", anchor.id, true, Vec::new())
        .await
        .unwrap();
    let now = Utc::now();
    storage
        .snooze_event(expired.id, now - chrono::Duration::seconds(1))
        .await
        .unwrap();
    let future_until = now + chrono::Duration::hours(1);
    storage.snooze_event(future.id, future_until).await.unwrap();

    drop(storage);
    let storage = Storage::open(dir.path()).await.unwrap();
    assert!(
        storage
            .ping(expired.id)
            .await
            .unwrap()
            .unwrap()
            .snoozed_until
            .is_some()
    );
    assert_eq!(
        storage
            .ping(future.id)
            .await
            .unwrap()
            .unwrap()
            .snoozed_until,
        Some(future_until)
    );

    let returned = storage.clear_expired_snoozes(now).await.unwrap();
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].id, expired.id);
    assert_eq!(returned[0].snoozed_until, None);
    assert_eq!(
        storage
            .ping(future.id)
            .await
            .unwrap()
            .unwrap()
            .snoozed_until,
        Some(future_until)
    );
}

#[tokio::test]
async fn ping_snapshot_includes_done_pings() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap()
        .id;
    let ping = storage
        .create_ping(
            "release-decision",
            "Choose whether to release",
            "question",
            anchor,
            true,
            vec![QuickReply {
                value: "yes".to_string(),
                label: "Yes".to_string(),
            }],
        )
        .await
        .unwrap();

    assert!(!ping.read);
    let done = storage.resolve_ping(ping.id).await.unwrap().unwrap();
    let snapshot = storage.ping_snapshot().await.unwrap();

    assert_eq!(done.status, PingStatus::Done);
    assert!(!done.read);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].status, PingStatus::Done);
}

#[tokio::test]
async fn mark_ping_read_is_idempotent_and_preserved_when_done() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap()
        .id;
    let ping = storage
        .create_ping("question", "Question", "question", anchor, true, Vec::new())
        .await
        .unwrap();

    assert!(!ping.read);
    let read = storage.mark_ping_read(ping.id).await.unwrap().unwrap();
    let read_again = storage.mark_ping_read(ping.id).await.unwrap().unwrap();
    let done = storage.resolve_ping(ping.id).await.unwrap().unwrap();

    assert!(read.read);
    assert!(read_again.read);
    assert!(done.read);
    assert_eq!(done.status, PingStatus::Done);
    assert!(storage.mark_ping_read(99_999).await.unwrap().is_none());
}
