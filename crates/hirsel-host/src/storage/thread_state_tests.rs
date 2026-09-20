use crate::storage::{ArtifactDraft, Storage, ThreadCaller, ThreadMutation, ThreadRef};
use hirsel_proto::{ArtifactKind, ThreadAttention, ThreadEffectKind, ThreadKind};

async fn create(storage: &Storage, key: &str, parent: Option<u64>) -> hirsel_proto::Thread {
    storage
        .create_thread(
            key,
            key,
            "",
            None,
            ThreadAttention::Quiet,
            ThreadKind::Task,
            parent,
        )
        .await
        .unwrap()
        .0
}

async fn caller(storage: &Storage, thread_id: u64) -> ThreadCaller {
    let turn = storage.start_thread_turn(thread_id, None).await.unwrap();
    let history = storage.history_id().await.unwrap();
    storage
        .bind_thread_execution(
            &history,
            &format!("state-session-{thread_id}"),
            &format!("state-execution-{thread_id}"),
            turn.id,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn state_patch_normalizes_cas_and_records_one_edited_effect() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let parent = create(&storage, "parent", None).await;
    let child = create(&storage, "child", Some(parent.id)).await;
    let caller = caller(&storage, parent.id).await;
    let current = storage.thread(child.id).await.unwrap().unwrap().state;
    let mutation = ThreadMutation::State {
        thread: ThreadRef::Id(child.id),
        expected_state_revision: current.revision,
        headline: "  Evidence\n  is   ready ".into(),
        findings: Some(vec!["Tests passed".into()]),
        artifact_ids: Some(vec![]),
    };
    let result = storage
        .mutate_scoped_thread(&caller, "state-1", &mutation)
        .await
        .unwrap();
    assert_eq!(result["state"]["own_headline"], "Evidence is ready");
    let updated = storage.thread(child.id).await.unwrap().unwrap();
    assert_eq!(updated.state.headline, "Evidence is ready");
    assert_eq!(updated.state.findings, ["Tests passed"]);
    let parent = storage.thread(parent.id).await.unwrap().unwrap();
    assert_eq!(
        parent.state.headline,
        format!("1 child · #{} idle", child.id)
    );
    let effects = storage.thread_effects(caller.turn_id).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].receipt.effect, ThreadEffectKind::Edited);
    assert_eq!(effects[0].receipt.tool, "threads_state");

    let conflict = storage
        .mutate_scoped_thread(&caller, "state-stale", &mutation)
        .await
        .unwrap();
    assert_eq!(conflict["conflict"], true);
    assert_eq!(conflict["actual_state_revision"], updated.state.revision);
    assert_eq!(
        storage.thread_effects(caller.turn_id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn artifact_edits_increment_artifact_and_every_referencing_state() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let parent = create(&storage, "artifact-parent", None).await;
    let child = create(&storage, "artifact-child", Some(parent.id)).await;
    let caller = caller(&storage, parent.id).await;
    let draft = |content: &str| ArtifactDraft {
        title: "Evidence".into(),
        kind: ArtifactKind::Markdown,
        content: content.into(),
        expected_content: None,
    };
    let (artifact, _) = storage
        .publish_artifact(
            "artifact-create",
            &serde_json::json!({"tool":"artifacts_create"}),
            &caller,
            None,
            Some(draft("first")),
        )
        .await
        .unwrap();
    let child_before = storage.thread(child.id).await.unwrap().unwrap().state;
    storage
        .mutate_scoped_thread(
            &caller,
            "state-artifact",
            &ThreadMutation::State {
                thread: ThreadRef::Id(child.id),
                expected_state_revision: child_before.revision,
                headline: "Evidence linked".into(),
                findings: None,
                artifact_ids: Some(vec![artifact.summary.id]),
            },
        )
        .await
        .unwrap();
    let linked_revision = storage
        .thread(child.id)
        .await
        .unwrap()
        .unwrap()
        .state
        .revision;
    let (edited, _) = storage
        .publish_artifact(
            "artifact-edit",
            &serde_json::json!({"tool":"artifacts_edit"}),
            &caller,
            Some(artifact.summary.id),
            Some(draft("second")),
        )
        .await
        .unwrap();
    assert_eq!(edited.summary.revision, artifact.summary.revision + 1);
    assert!(
        storage
            .thread(child.id)
            .await
            .unwrap()
            .unwrap()
            .state
            .revision
            > linked_revision
    );
}

#[tokio::test]
async fn sql_checks_reject_null_denormalized_and_thirteen_word_headlines() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = create(&storage, "constraints", None).await;
    let conn = storage.conn.lock().await;
    for headline in [
        None,
        Some(" double  space"),
        Some("vertical\u{b}tab"),
        Some("form\u{c}feed"),
        Some("one two three four five six seven eight nine ten eleven twelve thirteen"),
    ] {
        assert!(
            conn.execute(
                "UPDATE thread_state SET headline=?2 WHERE thread_id=?1",
                rusqlite::params![thread.id, headline]
            )
            .is_err()
        );
    }
    assert!(
        conn.execute(
            "UPDATE thread_state SET headline=?2 WHERE thread_id=?1",
            rusqlite::params![thread.id, "é".repeat(121)],
        )
        .is_err()
    );
    assert_eq!(
        conn.execute(
            "UPDATE thread_state SET headline=?2 WHERE thread_id=?1",
            rusqlite::params![thread.id, "one\u{a0}two"],
        )
        .unwrap(),
        1
    );
    assert!(
        conn.execute(
            "UPDATE thread_state SET findings_json=NULL WHERE thread_id=?1",
            [thread.id]
        )
        .is_err()
    );
}

#[test]
fn headline_normalization_matches_the_sql_contract() {
    for (input, expected) in [
        ("  one\ttwo\nthree\r\u{b}four\u{c}  ", "one two three four"),
        ("one\u{a0}two", "one\u{a0}two"),
        (
            "one two three four five six seven eight nine ten eleven twelve",
            "one two three four five six seven eight nine ten eleven twelve",
        ),
    ] {
        assert_eq!(super::validate_headline(input).unwrap(), expected);
    }
    assert!(
        super::validate_headline(
            "one two three four five six seven eight nine ten eleven twelve thirteen"
        )
        .is_err()
    );
    assert!(super::validate_headline(&"é".repeat(121)).is_err());
}

#[tokio::test]
async fn rollups_cascade_through_descendants_and_break_ties_by_numeric_id() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let root = create(&storage, "root", None).await;
    let parent = create(&storage, "parent", Some(root.id)).await;
    let first = create(&storage, "first", Some(parent.id)).await;
    let second = create(&storage, "second", Some(parent.id)).await;

    let root_before = storage
        .thread(root.id)
        .await
        .unwrap()
        .unwrap()
        .state
        .revision;
    storage.start_thread_turn(second.id, None).await.unwrap();
    let parent_running = storage.thread(parent.id).await.unwrap().unwrap();
    let root_running = storage.thread(root.id).await.unwrap().unwrap();
    assert_eq!(
        parent_running.state.headline,
        format!("2 children · #{} running", second.id)
    );
    assert_eq!(
        root_running.state.headline,
        format!("1 child · #{} running", parent.id)
    );
    assert!(root_running.state.revision > root_before);

    storage.start_thread_turn(first.id, None).await.unwrap();
    let tied = storage.thread(parent.id).await.unwrap().unwrap();
    assert_eq!(
        tied.state.headline,
        format!("2 children · #{} running", first.id)
    );
    assert!(tied.settled_at.is_none());
}

#[tokio::test]
async fn reads_and_activity_receipts_do_not_advance_material_state() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = create(&storage, "unchanged", None).await;
    let revision = thread.state.revision;
    storage.mark_thread_read(thread.id).await.unwrap();
    let caller = caller(&storage, thread.id).await;
    let after_start = storage
        .thread(thread.id)
        .await
        .unwrap()
        .unwrap()
        .state
        .revision;
    storage
        .mutate_scoped_thread(
            &caller,
            "activity",
            &ThreadMutation::Activity {
                thread: ThreadRef::Id(thread.id),
                kind: "checkpoint_evidence".into(),
                data: serde_json::json!({"ok":true}),
            },
        )
        .await
        .unwrap();
    assert!(after_start > revision, "starting work is material");
    assert_eq!(
        storage
            .thread(thread.id)
            .await
            .unwrap()
            .unwrap()
            .state
            .revision,
        after_start
    );
}
