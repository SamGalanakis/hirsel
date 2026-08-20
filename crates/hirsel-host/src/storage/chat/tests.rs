use super::super::Storage;
use hirsel_proto::ChatAuthor;
use hirsel_proto::ChatMessage;
use hirsel_proto::ToolCallSummary;

#[tokio::test]
async fn owner_messages_are_idempotent_by_client_id() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let (first, inserted) = storage
        .append_owner_message("client-1", "hello", None, &[])
        .await
        .unwrap();
    let (second, duplicate_inserted) = storage
        .append_owner_message("client-1", "hello again", None, &[])
        .await
        .unwrap();

    assert!(inserted);
    assert!(!duplicate_inserted);
    assert_eq!(first, second);
    assert_eq!(storage.all_chat().await.unwrap().len(), 1);
}

#[tokio::test]
async fn chat_tool_summaries_are_persisted_with_chat_messages() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let tool_calls = vec![
        ToolCallSummary {
            name: "shell_run".to_string(),
            ok: true,
        },
        ToolCallSummary {
            name: "subagents_spawn".to_string(),
            ok: false,
        },
    ];

    let message = storage
        .append_chat_with_tool_calls(ChatAuthor::Agent, "used tools", None, tool_calls.clone())
        .await
        .unwrap();
    let replay = storage.replay_messages(None).await.unwrap();

    assert_eq!(message.tool_calls, tool_calls);
    assert_eq!(replay[0].tool_calls, tool_calls);
}

#[tokio::test]
async fn hello_snapshot_derives_latest_id_from_replayed_rows() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    storage
        .append_chat(ChatAuthor::Agent, "one", None)
        .await
        .unwrap();
    storage
        .append_chat(ChatAuthor::Agent, "two", None)
        .await
        .unwrap();

    // `last_seen` is an attention cursor, NOT a history gate: a reload by a
    // fully caught-up client still gets the recent window back, so the
    // conversation survives a refresh instead of rendering empty.
    let snapshot = storage.hello_snapshot(Some(1)).await.unwrap();
    assert_eq!(snapshot.latest_msg_id, 2);
    assert_eq!(
        snapshot
            .messages
            .iter()
            .map(|message| message.body.as_str())
            .collect::<Vec<_>>(),
        vec!["one", "two"]
    );

    let caught_up = storage.hello_snapshot(Some(2)).await.unwrap();
    assert_eq!(caught_up.latest_msg_id, 2);
    assert_eq!(
        caught_up
            .messages
            .iter()
            .map(|message| message.body.as_str())
            .collect::<Vec<_>>(),
        vec!["one", "two"]
    );

    let stale = storage.hello_snapshot(Some(99_999)).await.unwrap();
    assert_eq!(stale.latest_msg_id, 2);
    assert_eq!(
        stale
            .messages
            .iter()
            .map(|message| message.body.as_str())
            .collect::<Vec<_>>(),
        vec!["one", "two"]
    );
}

#[tokio::test]
async fn hello_replay_always_carries_the_recent_window_and_grows_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    // One more than the window, so the floor is genuinely exercised.
    let total = super::HELLO_REPLAY_WINDOW + 50;
    for index in 0..total {
        storage
            .append_chat(ChatAuthor::Agent, &format!("m{index}"), None)
            .await
            .unwrap();
    }

    // A caught-up client (the reload case) gets exactly the newest window back
    // rather than nothing at all.
    let reload = storage.hello_snapshot(Some(total)).await.unwrap();
    assert_eq!(reload.latest_msg_id, total);
    assert_eq!(reload.messages.len() as u64, super::HELLO_REPLAY_WINDOW);
    assert_eq!(
        reload.messages[0].body,
        format!("m{}", total - super::HELLO_REPLAY_WINDOW)
    );
    assert_eq!(
        reload.messages.last().unwrap().body,
        format!("m{}", total - 1)
    );

    // A null cursor keeps its historical meaning: the same window.
    let fresh = storage.hello_snapshot(None).await.unwrap();
    assert_eq!(fresh.messages.len() as u64, super::HELLO_REPLAY_WINDOW);

    // A client further behind than the window gets everything it missed, not a
    // truncated window — the cursor still widens the replay, it just can never
    // narrow it below the window.
    let behind = storage.hello_snapshot(Some(1)).await.unwrap();
    assert_eq!(behind.messages.len() as u64, total - 1);
    assert_eq!(behind.messages[0].body, "m1");
}

#[tokio::test]
async fn fetch_messages_pages_backwards_at_boundaries_and_caps_limits() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let empty = storage.fetch_messages(u64::MAX, 20).await.unwrap();
    assert!(empty.messages.is_empty());
    assert!(!empty.has_more);

    for id in 1..=150 {
        storage
            .append_chat(ChatAuthor::Agent, format!("m{id}"), None)
            .await
            .unwrap();
    }

    let beyond_newest = storage.fetch_messages(10_000, 500).await.unwrap();
    assert_eq!(beyond_newest.messages.len(), 100);
    assert_eq!(beyond_newest.messages.first().unwrap().id, 51);
    assert_eq!(beyond_newest.messages.last().unwrap().id, 150);
    assert!(beyond_newest.has_more);

    let middle = storage.fetch_messages(76, 20).await.unwrap();
    assert_eq!(
        middle
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        (56..=75).collect::<Vec<_>>()
    );
    assert!(middle.has_more);

    let before_oldest = storage.fetch_messages(1, 20).await.unwrap();
    assert!(before_oldest.messages.is_empty());
    assert!(!before_oldest.has_more);

    let beginning = storage.fetch_messages(6, 20).await.unwrap();
    assert_eq!(
        beginning
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert!(!beginning.has_more);
}

#[tokio::test]
async fn hello_snapshot_includes_all_archived_events() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap()
        .id;
    for index in 0..25 {
        let event = storage
            .create_ping(
                format!("event-{index}"),
                format!("Event {index}"),
                format!("Event {index}"),
                anchor,
                false,
                Vec::new(),
            )
            .await
            .unwrap();
        storage.archive_event(event.id).await.unwrap();
    }

    let snapshot = storage.hello_snapshot(None).await.unwrap();
    assert_eq!(snapshot.events.len(), 25);
    assert!(snapshot.events.iter().all(|event| event.archived));
}

#[tokio::test]
async fn delete_chat_message_removes_client_id_and_attachment_joins() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let blob = storage
        .store_blob("text-upload", "note.txt", "text/plain", b"hello".to_vec())
        .await
        .unwrap();
    let attachment_ids = vec![blob.blob.id.clone()];
    let (message, inserted) = storage
        .append_owner_message("client-1", "queued", None, &attachment_ids)
        .await
        .unwrap();

    assert!(inserted);
    assert_eq!(
        storage.message_id_for_client_id("client-1").await.unwrap(),
        Some(message.id)
    );
    assert!(
        !storage
            .blobs_for_message(message.id)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(storage.delete_chat_message(message.id).await.unwrap());
    assert_eq!(storage.all_chat().await.unwrap(), Vec::<ChatMessage>::new());
    assert_eq!(
        storage.message_id_for_client_id("client-1").await.unwrap(),
        None
    );
    assert!(
        storage
            .blobs_for_message(message.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(storage.blob(&blob.blob.id).await.unwrap().is_some());
    assert!(!storage.delete_chat_message(message.id).await.unwrap());
}
