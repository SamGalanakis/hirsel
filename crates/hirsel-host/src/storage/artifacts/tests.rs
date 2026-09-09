use super::*;
fn draft(content: &str) -> ArtifactDraft {
    ArtifactDraft {
        title: "Architecture".into(),
        kind: ArtifactKind::Html,
        mime: "text/html".into(),
        filename: None,
        content: content.into(),
        expected_content: None,
    }
}
#[tokio::test]
async fn explicit_artifact_is_atomic_replay_safe_and_globally_referenced() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let input = serde_json::json!({"content":"first"});
    let (first, card) = storage
        .publish_artifact("create", &input, 0, None, Some(draft("first")))
        .await
        .unwrap();
    let card = card.unwrap();
    assert_eq!(card.artifact_ids, vec![first.summary.id]);
    let replay = storage
        .publish_artifact("create", &input, 0, None, Some(draft("first")))
        .await
        .unwrap();
    assert_eq!(replay.1.unwrap().id, card.id);
    assert_eq!(
        storage
            .thread_detail(0, None, 100)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    let thread = storage
        .create_thread(
            "other",
            "Other",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
        )
        .await
        .unwrap()
        .0;
    storage
        .publish_artifact(
            "show",
            &serde_json::json!({"show":first.summary.id}),
            thread.id,
            Some(first.summary.id),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        storage.artifacts(Some(thread.id)).await.unwrap()[0].id,
        first.summary.id
    );
    let mut edited = draft("latest");
    edited.expected_content = Some("first".into());
    storage
        .publish_artifact(
            "edit",
            &serde_json::json!({"edit":"latest"}),
            0,
            Some(first.summary.id),
            Some(edited),
        )
        .await
        .unwrap();
    let old_replay = storage
        .artifact_operation("create", 0, &input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_replay.0.content, "latest");
    assert_eq!(old_replay.1.unwrap().id, card.id);
    assert_eq!(old_replay.0.summary.thread_ids, vec![0, thread.id]);
    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened.artifact(first.summary.id).await.unwrap().content,
        "latest"
    );
    assert_eq!(
        reopened.thread_detail(0, None, 100).await.unwrap().messages[0].artifact_ids,
        vec![first.summary.id]
    );
    assert!(
        reopened
            .artifact_operation("create", 0, &serde_json::json!({"different":true}))
            .await
            .is_err()
    );
    let c = reopened.conn.lock().await;
    let receipt: String = c
        .query_row(
            "SELECT payload FROM artifact_operations WHERE operation_id='create'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipt.len(), 64);
    assert!(!receipt.contains("first"));
}
#[tokio::test]
async fn invalid_content_thread_or_stale_edit_leaves_no_partial_artifact_or_card() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let input = serde_json::json!({});
    assert!(
        s.publish_artifact("bad-thread", &input, 999, None, Some(draft("safe")))
            .await
            .is_err()
    );
    assert!(
        s.publish_artifact("huge", &input, 0, None, Some(draft(&"x".repeat(1_048_577))))
            .await
            .is_err()
    );
    let mut invalid = draft("safe");
    invalid.filename = Some("../secret".into());
    assert!(
        s.publish_artifact("path", &input, 0, None, Some(invalid))
            .await
            .is_err()
    );
    assert!(s.artifacts(None).await.unwrap().is_empty());
    assert!(
        s.thread_detail(0, None, 100)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    let (a, _) = s
        .publish_artifact("ok", &input, 0, None, Some(draft("first")))
        .await
        .unwrap();
    let mut stale = draft("replacement");
    stale.expected_content = Some("wrong".into());
    assert!(
        s.publish_artifact("stale", &input, 0, Some(a.summary.id), Some(stale))
            .await
            .is_err()
    );
    assert_eq!(s.artifact(a.summary.id).await.unwrap().content, "first");
    assert_eq!(
        s.thread_detail(0, None, 100).await.unwrap().messages.len(),
        1
    );
    s.reset().await.unwrap();
    assert!(s.artifacts(None).await.unwrap().is_empty());
}
