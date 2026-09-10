use super::*;
use crate::lash_runtime::ScopedThreadTools;
use crate::storage::ThreadCaller;
use hirsel_proto::ThreadAttention;
use serde_json::json;

async fn thread(s: &Storage, key: &str, parent: Option<u64>) -> u64 {
    s.create_thread(
        key,
        key,
        "",
        &Value::Null,
        ThreadAttention::Quiet,
        hirsel_proto::ThreadKind::Task,
        parent,
    )
    .await
    .unwrap()
    .0
    .id
}
async fn caller(s: &Storage, id: u64) -> ThreadCaller {
    let turn = s.start_thread_turn(id, None).await.unwrap();
    let launch = uuid::Uuid::new_v4().to_string();
    s.bind_thread_execution(&s.history_id().await.unwrap(), &launch, &launch, turn.id)
        .await
        .unwrap()
}

#[test]
fn artifact_touch_advances_a_future_timestamp_once_by_one_nanosecond() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE artifacts(id INTEGER PRIMARY KEY, updated_at TEXT NOT NULL);\
         INSERT INTO artifacts VALUES(42, '2099-01-01T00:00:00Z');",
    )
    .unwrap();

    touch_artifacts(&c, Some(42), Some(42)).unwrap();

    let updated: String = c
        .query_row("SELECT updated_at FROM artifacts WHERE id=42", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(updated, "2099-01-01T00:00:00.000000001+00:00");
}

#[tokio::test]
async fn artifact_edit_remains_newer_than_a_future_showcase_touch() {
    let dir = tempfile::tempdir().unwrap();
    let s = Storage::open(dir.path()).await.unwrap();
    let id = thread(&s, "owner", None).await;
    let draft = super::super::artifacts::ArtifactDraft {
        title: "Result".into(),
        kind: hirsel_proto::ArtifactKind::File,
        mime: "text/plain".into(),
        filename: None,
        content: "first".into(),
        expected_content: None,
    };
    let (artifact, _) = s
        .publish_artifact_human("create", &json!({"content":"first"}), id, None, Some(draft))
        .await
        .unwrap();
    let artifact_id = artifact.summary.id;
    s.conn
        .lock()
        .await
        .execute(
            "UPDATE artifacts SET updated_at='2099-01-01T00:00:00Z' WHERE id=?1",
            [artifact_id],
        )
        .unwrap();
    let revision = s.thread(id).await.unwrap().unwrap().revision;
    let history = s.history_id().await.unwrap();
    s.update_thread_showcase(&history, id, Some(artifact_id), revision)
        .await
        .unwrap();
    assert_eq!(
        s.artifact(artifact_id).await.unwrap().summary.updated_at,
        chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00.000000001+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );

    let edited = super::super::artifacts::ArtifactDraft {
        title: "Result".into(),
        kind: hirsel_proto::ArtifactKind::File,
        mime: "text/plain".into(),
        filename: None,
        content: "second".into(),
        expected_content: Some("first".into()),
    };
    let (artifact, _) = s
        .publish_artifact_human(
            "edit",
            &json!({"content":"second"}),
            id,
            Some(artifact_id),
            Some(edited),
        )
        .await
        .unwrap();
    assert_eq!(
        artifact.summary.updated_at,
        chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00.000000002+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
}

#[tokio::test]
async fn showcase_tool_scope_replay_reference_grant_and_removal() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let root = thread(s, "root", None).await;
    let child = thread(s, "child", Some(root)).await;
    let peer = thread(s, "peer", None).await;
    let actor = caller(s, root).await;
    let child_actor = caller(s, child).await;
    let peer_actor = caller(s, peer).await;
    let mut tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: actor,
        operation_id: "create-artifact".into(),
    };
    let created = tools
        .execute(
            "artifacts_create",
            &json!({"title":"Working result","kind":"file","mime":"text/plain","content":"first"}),
        )
        .await
        .unwrap();
    let artifact_id = created["id"].as_u64().unwrap();
    assert!(s.scoped_artifact(&child_actor, artifact_id).await.is_err());
    tools.operation_id = "showcase-child".into();
    let args = json!({"thread":child,"showcased_artifact_id":artifact_id});
    let result = tools.execute("threads_update", &args).await.unwrap();
    assert_eq!(result["thread"]["showcased_artifact_id"], artifact_id);
    assert_eq!(
        result,
        tools.execute("threads_update", &args).await.unwrap()
    );
    assert!(
        tools
            .execute(
                "threads_update",
                &json!({"thread":child,"showcased_artifact_id":null})
            )
            .await
            .is_err()
    );
    let shared = s.scoped_artifact(&child_actor, artifact_id).await.unwrap();
    assert_eq!(shared.summary.thread_ids, vec![child]);
    assert_eq!(
        s.scoped_artifacts(&child_actor, child).await.unwrap().len(),
        1
    );
    assert_eq!(s.artifacts(Some(child)).await.unwrap().len(), 1);
    assert!(s.scoped_artifact(&peer_actor, artifact_id).await.is_err());
    assert_eq!(
        s.artifact(artifact_id).await.unwrap().summary.thread_ids,
        vec![root, child]
    );
    // Showcase assignment is independent of the original artifact card.
    assert_eq!(
        s.conn
            .lock()
            .await
            .query_row(
                "SELECT count(*) FROM chat_messages WHERE thread_id=?1",
                [child],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    tools.operation_id = "preserve-showcase".into();
    assert_eq!(
        tools
            .execute("threads_update", &json!({"thread":child,"title":"Renamed"}))
            .await
            .unwrap()["thread"]["showcased_artifact_id"],
        artifact_id
    );
    let mut child_tools = ScopedThreadTools {
        tools: state.tools.clone(),
        caller: child_actor.clone(),
        operation_id: "child-self".into(),
    };
    assert!(
        child_tools
            .execute(
                "threads_update",
                &json!({"thread":root,"showcased_artifact_id":null})
            )
            .await
            .is_err()
    );
    assert!(
        child_tools
            .execute(
                "threads_update",
                &json!({"thread":peer,"showcased_artifact_id":artifact_id})
            )
            .await
            .is_err()
    );
    assert!(
        child_tools
            .execute("threads_update", &json!({"showcased_artifact_id":99999}))
            .await
            .is_err()
    );
    let cleared = child_tools
        .execute("threads_update", &json!({"showcased_artifact_id":null}))
        .await
        .unwrap();
    assert!(cleared["thread"]["showcased_artifact_id"].is_null());
    assert!(s.scoped_artifact(&child_actor, artifact_id).await.is_err());
    assert!(
        s.scoped_artifacts(&child_actor, child)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(s.artifacts(Some(child)).await.unwrap().is_empty());
    assert_eq!(
        s.artifact(artifact_id).await.unwrap().summary.thread_ids,
        vec![root]
    );
    // A guessed global ID cannot re-grant access after its only reference clears.
    child_tools.operation_id = "unauthorized-regrant".into();
    assert!(
        child_tools
            .execute(
                "threads_update",
                &json!({"showcased_artifact_id":artifact_id})
            )
            .await
            .is_err()
    );
    tools.operation_id = "revoked".into();
    s.conn
        .lock()
        .await
        .execute(
            "UPDATE thread_execution_bindings SET revoked=1 WHERE turn_id=?1",
            [tools.caller.turn_id],
        )
        .unwrap();
    assert!(tools.execute("threads_update", &args).await.is_err());
}

#[tokio::test]
async fn showcase_owner_checks_history_revision_input_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let s = &state.storage;
    let id = thread(s, "owner-thread", None).await;
    let history = s.history_id().await.unwrap();
    {
        let c = s.conn.lock().await;
        c.execute("INSERT INTO artifacts(id,title,kind,mime,content,created_at,updated_at) VALUES(42,'Result','\"file\"','text/plain','bytes','2026-09-10T00:00:00Z','2026-09-10T00:00:00Z')", []).unwrap();
    }
    let revision = s.thread(id).await.unwrap().unwrap().revision;
    for (data, expected) in [
        (json!({}), Some(revision)),
        (
            json!({"artifact_id":42,"history_id":"old-history"}),
            Some(revision),
        ),
        (json!({"artifact_id":999}), Some(revision)),
        (json!({"artifact_id":42}), None),
        (json!({"artifact_id":42}), Some(revision + 1)),
        (
            json!({"artifact_id":42,"history_id":history,"extra":true}),
            Some(revision),
        ),
    ] {
        assert!(
            state
                .handle_addressed_thread_action(
                    &state.storage.history_id().await.unwrap(),
                    id,
                    "set_showcase".into(),
                    data,
                    expected
                )
                .await
                .is_err()
        );
        assert_eq!(s.thread(id).await.unwrap().unwrap().revision, revision);
    }
    s.conn.lock().await.execute_batch("CREATE TRIGGER block_showcase_metadata BEFORE UPDATE ON artifacts BEGIN SELECT RAISE(FAIL,'metadata unavailable'); END;").unwrap();
    assert!(
        state
            .handle_addressed_thread_action(
                &state.storage.history_id().await.unwrap(),
                id,
                "set_showcase".into(),
                json!({"artifact_id":42}),
                Some(revision)
            )
            .await
            .is_err()
    );
    assert_eq!(s.thread(id).await.unwrap().unwrap().revision, revision);
    assert_eq!(
        s.thread(id).await.unwrap().unwrap().showcased_artifact_id,
        None
    );
    s.conn
        .lock()
        .await
        .execute_batch("DROP TRIGGER block_showcase_metadata")
        .unwrap();
    let set = state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            id,
            "set_showcase".into(),
            json!({"artifact_id":42}),
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(set.showcased_artifact_id, Some(42));
    assert_eq!(set.revision, revision + 1);
    let initial_summary = s.artifact(42).await.unwrap().summary;
    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened
            .thread(id)
            .await
            .unwrap()
            .unwrap()
            .showcased_artifact_id,
        Some(42)
    );
    s.conn.lock().await.execute("INSERT INTO artifacts(id,title,kind,mime,content,created_at,updated_at) SELECT 43,'Replacement',kind,mime,content,created_at,updated_at FROM artifacts WHERE id=42", []).unwrap();
    let replacement = state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            id,
            "set_showcase".into(),
            json!({"artifact_id":43}),
            Some(set.revision),
        )
        .await
        .unwrap();
    assert_eq!(replacement.showcased_artifact_id, Some(43));
    assert!(s.artifact(42).await.unwrap().summary.thread_ids.is_empty());
    let cleared = state
        .handle_addressed_thread_action(
            &state.storage.history_id().await.unwrap(),
            id,
            "set_showcase".into(),
            json!({"artifact_id":null}),
            Some(replacement.revision),
        )
        .await
        .unwrap();
    assert_eq!(cleared.showcased_artifact_id, None);
    assert_eq!(cleared.revision, revision + 3);
    let frames = state.broadcast_log.recent();
    let summaries: Vec<_> = frames
        .iter()
        .filter_map(|frame| match frame {
            hirsel_proto::HostToClient::ArtifactUpsert { artifact } => Some(artifact),
            _ => None,
        })
        .collect();
    assert!(
        summaries
            .iter()
            .any(|a| a.id == 42 && a.thread_ids == vec![id])
    );
    assert!(summaries.iter().any(|a| a.id == 42
        && a.thread_ids.is_empty()
        && a.updated_at > initial_summary.updated_at));
    assert!(
        summaries
            .iter()
            .any(|a| a.id == 43 && a.thread_ids == vec![id])
    );
    assert!(
        summaries
            .last()
            .is_some_and(|a| a.id == 43 && a.thread_ids.is_empty())
    );
}

#[test]
fn showcase_parser_distinguishes_omission_null_and_positive_identity() {
    assert_eq!(parse_showcase(&json!({}), "id").unwrap(), None);
    assert_eq!(
        parse_showcase(&json!({"id":null}), "id").unwrap(),
        Some(None)
    );
    assert_eq!(
        parse_showcase(&json!({"id":5}), "id").unwrap(),
        Some(Some(5))
    );
    for id in [
        json!(0),
        json!(-1),
        json!(1.2),
        json!("1"),
        json!([]),
        json!({}),
        json!(true),
    ] {
        assert!(parse_showcase(&json!({"id":id}), "id").is_err());
    }
}
