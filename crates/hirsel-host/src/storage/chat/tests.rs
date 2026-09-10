use super::super::Storage;
use hirsel_proto::ChatAuthor;
use hirsel_proto::ChatMessage;
use hirsel_proto::ToolCallSummary;

#[tokio::test]
async fn owner_messages_are_idempotent_by_client_id() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = storage
        .create_thread(
            "conversation",
            "Conversation",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;

    let (first, inserted) = storage
        .append_thread_owner_message(
            &storage.history_id().await.unwrap(),
            thread.id,
            "client-1",
            "hello",
            None,
            &[],
            &[],
            &[],
        )
        .await
        .unwrap();
    let (second, duplicate_inserted) = storage
        .append_thread_owner_message(
            &storage.history_id().await.unwrap(),
            thread.id,
            "client-1",
            "hello again",
            None,
            &[],
            &[],
            &[],
        )
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
    let thread = storage
        .create_thread(
            "conversation",
            "Conversation",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let tool_calls = vec![
        ToolCallSummary {
            id: "call-shell".to_string(),
            name: "shell_run".to_string(),
            ok: true,
        },
        ToolCallSummary {
            id: "call-delegate".to_string(),
            name: "threads_delegate".to_string(),
            ok: false,
        },
    ];

    let message = storage
        .append_thread_chat(
            thread.id,
            ChatAuthor::Agent,
            "used tools",
            None,
            tool_calls.clone(),
        )
        .await
        .unwrap();
    let replay = storage.all_chat().await.unwrap();

    assert_eq!(message.tool_calls, tool_calls);
    assert_eq!(replay[0].tool_calls, tool_calls);
}

#[tokio::test]
async fn delete_chat_message_removes_client_id_and_attachment_joins() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = storage
        .create_thread(
            "conversation",
            "Conversation",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let blob = storage
        .store_blob("text-upload", "note.txt", "text/plain", b"hello".to_vec())
        .await
        .unwrap();
    let attachment_ids = vec![blob.blob.id.clone()];
    let (message, inserted) = storage
        .append_thread_owner_message(
            &storage.history_id().await.unwrap(),
            thread.id,
            "client-1",
            "queued",
            None,
            &attachment_ids,
            &[],
            &[],
        )
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
