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

    let snapshot = storage.hello_snapshot(Some(1)).await.unwrap();
    assert_eq!(snapshot.latest_msg_id, 2);
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].body, "two");

    let empty = storage.hello_snapshot(Some(2)).await.unwrap();
    assert_eq!(empty.latest_msg_id, 2);
    assert!(empty.messages.is_empty());

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
