use super::*;
fn draft(content: &str) -> ArtifactDraft {
    ArtifactDraft {
        title: "Architecture".into(),
        kind: ArtifactKind::Html,
        content: content.into(),
        expected_content: None,
    }
}
#[tokio::test]
async fn explicit_artifact_is_atomic_replay_safe_and_globally_referenced() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let caller = storage.test_running_caller().await;
    let input = serde_json::json!({"content":"first"});
    let (first, card) = storage
        .publish_artifact_human(
            "create",
            &input,
            caller.thread_id,
            None,
            Some(draft("first")),
        )
        .await
        .unwrap();
    let card = card.unwrap();
    assert_eq!(card.artifact_ids, vec![first.summary.id]);
    let replay = storage
        .publish_artifact_human(
            "create",
            &input,
            caller.thread_id,
            None,
            Some(draft("first")),
        )
        .await
        .unwrap();
    assert_eq!(replay.1.unwrap().id, card.id);
    assert_eq!(
        storage
            .thread_detail(caller.thread_id, None, 100)
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
            None,
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    storage
        .publish_artifact_human(
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
        .publish_artifact_human(
            "edit",
            &serde_json::json!({"edit":"latest"}),
            caller.thread_id,
            Some(first.summary.id),
            Some(edited),
        )
        .await
        .unwrap();
    let old_replay = storage
        .artifact_operation("create", &caller, &input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_replay.0.content, "latest");
    assert_eq!(old_replay.1.unwrap().id, card.id);
    assert_eq!(
        old_replay.0.summary.thread_ids,
        vec![caller.thread_id, thread.id]
    );
    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened.artifact(first.summary.id).await.unwrap().content,
        "latest"
    );
    assert_eq!(
        reopened
            .thread_detail(caller.thread_id, None, 100)
            .await
            .unwrap()
            .messages[0]
            .artifact_ids,
        vec![first.summary.id]
    );
    assert!(
        reopened
            .artifact_operation("create", &caller, &serde_json::json!({"different":true}))
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
    let caller = s.test_running_caller().await;
    let input = serde_json::json!({});
    assert!(
        s.publish_artifact_human("bad-thread", &input, 999, None, Some(draft("safe")))
            .await
            .is_err()
    );
    assert!(
        s.publish_artifact_human(
            "huge",
            &input,
            caller.thread_id,
            None,
            Some(draft(&"x".repeat(1_048_577)))
        )
        .await
        .is_err()
    );
    let mut invalid = draft("safe");
    invalid.kind = ArtifactKind::File {
        mime: "text/plain".into(),
        filename: Some("../secret".into()),
    };
    assert!(
        s.publish_artifact_human("path", &input, caller.thread_id, None, Some(invalid))
            .await
            .is_err()
    );
    assert!(s.artifacts(None).await.unwrap().is_empty());
    assert!(
        s.thread_detail(caller.thread_id, None, 100)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    let (a, _) = s
        .publish_artifact_human("ok", &input, caller.thread_id, None, Some(draft("first")))
        .await
        .unwrap();
    let mut stale = draft("replacement");
    stale.expected_content = Some("wrong".into());
    assert!(
        s.publish_artifact_human(
            "stale",
            &input,
            caller.thread_id,
            Some(a.summary.id),
            Some(stale)
        )
        .await
        .is_err()
    );
    assert_eq!(s.artifact(a.summary.id).await.unwrap().content, "first");
    assert_eq!(
        s.thread_detail(caller.thread_id, None, 100)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    s.reset().await.unwrap();
    assert!(s.artifacts(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn every_kind_round_trips_and_the_store_rejects_an_unknown_tag() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let caller = s.test_running_caller().await;
    let kinds = [
        ArtifactKind::Solid,
        ArtifactKind::Html,
        ArtifactKind::Markdown,
        ArtifactKind::OpenUi,
        ArtifactKind::Image {
            mime: "image/svg+xml".into(),
        },
        ArtifactKind::File {
            mime: "text/plain".into(),
            filename: Some("notes.txt".into()),
        },
        ArtifactKind::File {
            mime: "text/plain".into(),
            filename: None,
        },
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
        let draft = ArtifactDraft {
            title: format!("Result {index}"),
            kind: kind.clone(),
            content: "body".into(),
            expected_content: None,
        };
        let (published, _) = s
            .publish_artifact_human(
                &format!("create-{index}"),
                &serde_json::json!({ "index": index }),
                caller.thread_id,
                None,
                Some(draft),
            )
            .await
            .unwrap();
        assert_eq!(published.summary.kind, kind);
        let reread = s.artifact(published.summary.id).await.unwrap();
        assert_eq!(reread.summary.kind, kind);
        let stored: (String, String) = s
            .conn
            .lock()
            .await
            .query_row(
                "SELECT kind,kind_data FROM artifacts WHERE id=?1",
                [published.summary.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, kind.tag());
        assert!(!stored.1.contains("\"kind\""));
    }
    let refused = s.conn.lock().await.execute(
        "INSERT INTO artifacts(title,kind,kind_data,content,created_at,updated_at) VALUES('Bad','scroll','{}','body','2026-09-10T00:00:00Z','2026-09-10T00:00:00Z')",
        [],
    );
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("CHECK constraint failed"),
    );
}

#[tokio::test]
async fn one_published_artifact_yields_exactly_one_card_in_the_thread() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let caller = s.test_running_caller().await;
    let (published, _) = s
        .publish_artifact(
            "turn-create",
            &serde_json::json!({"content":"body"}),
            &caller,
            None,
            Some(draft("body")),
        )
        .await
        .unwrap();
    let history = s.history_id().await.unwrap();
    s.complete_thread_turn(
        &history,
        caller.turn_id,
        hirsel_proto::ThreadTurnState::Completed,
        Some(("Published the result.".into(), vec![])),
    )
    .await
    .unwrap();
    let messages = s
        .thread_detail(caller.thread_id, None, 100)
        .await
        .unwrap()
        .messages;
    let cards = messages
        .iter()
        .filter(|message| message.artifact_ids.contains(&published.summary.id))
        .count();
    assert_eq!(cards, 1, "{messages:#?}");
}
