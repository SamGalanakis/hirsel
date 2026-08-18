use super::super::Storage;
use hirsel_proto::ChatAuthor;

#[tokio::test]
async fn side_chat_transcripts_use_a_separate_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let owner = storage
        .append_side_chat_message("side:test", ChatAuthor::Owner, "question")
        .await
        .unwrap();
    let agent = storage
        .append_side_chat_message("side:test", ChatAuthor::Agent, "answer")
        .await
        .unwrap();
    assert_eq!(owner.id + 1, agent.id);
    assert!(storage.all_chat().await.unwrap().is_empty());

    let transcript = storage.side_chat_transcript("side:test").await.unwrap();
    assert_eq!(transcript, vec![owner, agent]);

    storage
        .delete_side_chat_transcript("side:test")
        .await
        .unwrap();
    assert!(
        storage
            .side_chat_transcript("side:test")
            .await
            .unwrap()
            .is_empty()
    );
}
