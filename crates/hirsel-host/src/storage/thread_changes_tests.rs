use super::*;
use crate::storage::{ArtifactDraft, ThreadMutation, ThreadRef};
use hirsel_proto::{ArtifactKind, ReachTarget, ThreadAttention, ThreadKind, ThreadTurnState};

async fn create(
    storage: &Storage,
    key: &str,
    kind: ThreadKind,
    parent: Option<u64>,
) -> hirsel_proto::Thread {
    storage
        .create_thread(key, key, "", None, ThreadAttention::Quiet, kind, parent)
        .await
        .unwrap()
        .0
}

async fn caller(storage: &Storage, thread_id: u64, suffix: &str) -> super::super::ThreadCaller {
    let turn = storage.start_thread_turn(thread_id, None).await.unwrap();
    let history = storage.history_id().await.unwrap();
    storage
        .bind_thread_execution(
            &history,
            &format!("change-session-{suffix}"),
            &format!("change-execution-{suffix}"),
            turn.id,
        )
        .await
        .unwrap()
}

async fn checkpoint(
    storage: &Storage,
    caller: &super::super::ThreadCaller,
    target: u64,
    operation: &str,
    headline: &str,
    artifact_ids: Option<Vec<u64>>,
) {
    let revision = storage
        .thread(target)
        .await
        .unwrap()
        .unwrap()
        .state
        .revision;
    storage
        .mutate_scoped_thread(
            caller,
            operation,
            &ThreadMutation::State {
                thread: ThreadRef::Id(target),
                expected_state_revision: revision,
                headline: headline.into(),
                findings: None,
                artifact_ids,
            },
        )
        .await
        .unwrap();
}

async fn accepted_space_context(
    storage: &Storage,
    space_id: u64,
    key: &str,
) -> (u64, TurnAdmissionContext) {
    let history = storage.history_id().await.unwrap();
    storage
        .append_thread_owner_request(
            &history,
            space_id,
            key,
            format!("context {key}"),
            &[],
            &[],
            &[],
            &json!({"mode":"send","thread_action":null}),
        )
        .await
        .unwrap();
    let request = storage.thread_request(key).await.unwrap().unwrap();
    let turn_id = request["turn_id"].as_u64().unwrap();
    let context = storage
        .accepted_turn_context(&history, turn_id)
        .await
        .unwrap();
    (turn_id, context)
}

#[tokio::test]
async fn outside_changes_coalesce_and_only_completed_context_advances_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = create(&storage, "source-space", ThreadKind::Space, None).await;
    let source_task = create(&storage, "source-task", ThreadKind::Task, Some(source.id)).await;
    let target = create(&storage, "target-space", ThreadKind::Space, None).await;
    let target_task = create(&storage, "target-task", ThreadKind::Task, Some(target.id)).await;
    let history = storage.history_id().await.unwrap();
    storage
        .set_thread_reach(
            "source-reach",
            &history,
            source_task.id,
            ReachTarget::Thread {
                thread_id: target_task.id,
            },
            None,
            true,
        )
        .await
        .unwrap();
    let actor = caller(&storage, source_task.id, "coalesce").await;
    checkpoint(
        &storage,
        &actor,
        target_task.id,
        "change-1",
        "First outside update",
        None,
    )
    .await;
    checkpoint(
        &storage,
        &actor,
        target_task.id,
        "change-2",
        "Second outside update",
        None,
    )
    .await;

    let detail = storage.thread_detail(target.id, None, 100).await.unwrap();
    let activity = detail
        .activities
        .iter()
        .find(|activity| activity.kind == "outside_change")
        .unwrap();
    assert!(
        activity.data["text"]
            .as_str()
            .unwrap()
            .starts_with("Changed by source-space")
    );
    assert_eq!(
        detail
            .activities
            .iter()
            .filter(|activity| activity.kind == "outside_change")
            .count(),
        1
    );

    let (failed_turn, first) = accepted_space_context(&storage, target.id, "failed-context").await;
    assert_eq!(first.changes.changes.len(), 1);
    assert_eq!(first.changes.changes[0].before_headline, "Task ready");
    assert_eq!(
        first.changes.changes[0].after_headline,
        "Second outside update"
    );
    storage
        .finish_thread_turn(failed_turn, ThreadTurnState::Failed, None)
        .await
        .unwrap();
    let cursor: u64 = storage
        .conn
        .lock()
        .await
        .query_row(
            "SELECT consumed_change_id FROM thread_change_cursors WHERE chat_thread_id=?1",
            [target.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor, 0);

    let (completed_turn, redelivered) =
        accepted_space_context(&storage, target.id, "completed-context").await;
    assert_eq!(redelivered.changes, first.changes);
    storage
        .finish_thread_turn(completed_turn, ThreadTurnState::Completed, None)
        .await
        .unwrap();
    let detail = storage.thread_detail(target.id, None, 100).await.unwrap();
    let accepted = detail.accepted_context.unwrap();
    assert_eq!(accepted.turn_id, completed_turn);
    assert!(accepted.consumed_at.is_some());
    assert_eq!(accepted.through_change_id, first.changes.through_change_id);
}

#[tokio::test]
async fn digest_stops_after_thirty_two_raw_changes_without_advancing_over_more() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = create(&storage, "overflow-source", ThreadKind::Space, None).await;
    let source_task = create(
        &storage,
        "overflow-actor",
        ThreadKind::Task,
        Some(source.id),
    )
    .await;
    let target = create(&storage, "overflow-target", ThreadKind::Space, None).await;
    let target_task = create(&storage, "overflow-task", ThreadKind::Task, Some(target.id)).await;
    let history = storage.history_id().await.unwrap();
    storage
        .set_thread_reach(
            "overflow-reach",
            &history,
            source_task.id,
            ReachTarget::Thread {
                thread_id: target_task.id,
            },
            None,
            true,
        )
        .await
        .unwrap();
    let actor = caller(&storage, source_task.id, "overflow").await;
    for index in 0..40 {
        checkpoint(
            &storage,
            &actor,
            target_task.id,
            &format!("overflow-{index}"),
            &format!("Outside update number {index}"),
            None,
        )
        .await;
    }
    let (_, first) = accepted_space_context(&storage, target.id, "overflow-page-1").await;
    assert_eq!(first.changes.changes.len(), 1, "one Thread is coalesced");
    assert!(first.changes.has_more);
    assert!(serde_json::to_vec(&first.changes).unwrap().len() <= MAX_DIGEST_BYTES);
    let total_through: u64 = storage
        .conn
        .lock()
        .await
        .query_row(
            "SELECT max(change_id) FROM thread_change_deliveries WHERE chat_thread_id=?1",
            [target.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(first.changes.through_change_id < total_through);
}

#[tokio::test]
async fn shared_artifact_delivery_rechecks_revoked_thread_reach_at_admission() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = create(&storage, "artifact-source", ThreadKind::Space, None).await;
    let source_task = create(
        &storage,
        "artifact-owner",
        ThreadKind::Task,
        Some(source.id),
    )
    .await;
    let target = create(&storage, "artifact-target", ThreadKind::Space, None).await;
    let target_task = create(
        &storage,
        "artifact-reader",
        ThreadKind::Task,
        Some(target.id),
    )
    .await;
    let artifact = storage
        .publish_artifact_human(
            "shared-create",
            &json!({"content":"shared"}),
            source_task.id,
            None,
            Some(ArtifactDraft {
                title: "Shared evidence".into(),
                kind: ArtifactKind::Markdown,
                content: "shared".into(),
                expected_content: None,
            }),
        )
        .await
        .unwrap()
        .0;
    let history = storage.history_id().await.unwrap();
    storage
        .append_thread_owner_message(
            &history,
            target_task.id,
            "shared-reference",
            "Keep this evidence",
            None,
            &[],
            &[],
            &[artifact.summary.id],
        )
        .await
        .unwrap();
    storage
        .set_thread_reach(
            "artifact-reader-reach",
            &history,
            target.id,
            ReachTarget::Thread {
                thread_id: source.id,
            },
            None,
            true,
        )
        .await
        .unwrap();
    let actor = caller(&storage, source_task.id, "artifact").await;
    checkpoint(
        &storage,
        &actor,
        source_task.id,
        "artifact-change-visible",
        "Shared evidence changed",
        Some(vec![artifact.summary.id]),
    )
    .await;
    let (visible_turn, visible) =
        accepted_space_context(&storage, target.id, "artifact-visible").await;
    assert_eq!(visible.changes.changes.len(), 1);
    storage
        .finish_thread_turn(visible_turn, ThreadTurnState::Completed, None)
        .await
        .unwrap();

    checkpoint(
        &storage,
        &actor,
        source_task.id,
        "artifact-change-revoked",
        "Shared evidence changed again",
        Some(vec![artifact.summary.id]),
    )
    .await;
    storage
        .set_thread_reach(
            "artifact-reader-revoke",
            &history,
            target.id,
            ReachTarget::Thread {
                thread_id: source.id,
            },
            None,
            false,
        )
        .await
        .unwrap();
    let (_, hidden) = accepted_space_context(&storage, target.id, "artifact-hidden").await;
    assert!(hidden.changes.changes.is_empty());
}

#[tokio::test]
async fn changes_pages_from_the_exact_last_returned_change_without_skipping() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = create(&storage, "page-source", ThreadKind::Space, None).await;
    let source_task = create(&storage, "page-actor", ThreadKind::Task, Some(source.id)).await;
    let target = create(&storage, "page-target", ThreadKind::Space, None).await;
    let first_task = create(&storage, "page-first", ThreadKind::Task, Some(target.id)).await;
    let second_task = create(&storage, "page-second", ThreadKind::Task, Some(target.id)).await;
    let history = storage.history_id().await.unwrap();
    storage
        .set_thread_reach(
            "page-reach",
            &history,
            source_task.id,
            ReachTarget::Thread {
                thread_id: target.id,
            },
            None,
            true,
        )
        .await
        .unwrap();
    let actor = caller(&storage, source_task.id, "page-source").await;
    checkpoint(
        &storage,
        &actor,
        first_task.id,
        "page-1",
        "First page change",
        None,
    )
    .await;
    checkpoint(
        &storage,
        &actor,
        second_task.id,
        "page-2",
        "Second page change",
        None,
    )
    .await;

    let reader = caller(&storage, target.id, "page-reader").await;
    let first = storage.thread_changes(&reader, 0, 1).await.unwrap();
    assert_eq!(first.changes.len(), 1);
    assert!(first.has_more);
    let second = storage
        .thread_changes(&reader, first.through_change_id, 1)
        .await
        .unwrap();
    assert_eq!(second.changes.len(), 1);
    assert!(second.through_change_id > first.through_change_id);
}

#[tokio::test]
async fn concurrent_outside_writes_keep_every_delivery_and_one_visible_line() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = create(&storage, "concurrent-source", ThreadKind::Space, None).await;
    let source_task = create(
        &storage,
        "concurrent-actor",
        ThreadKind::Task,
        Some(source.id),
    )
    .await;
    let target = create(&storage, "concurrent-target", ThreadKind::Space, None).await;
    let left = create(
        &storage,
        "concurrent-left",
        ThreadKind::Task,
        Some(target.id),
    )
    .await;
    let right = create(
        &storage,
        "concurrent-right",
        ThreadKind::Task,
        Some(target.id),
    )
    .await;
    let history = storage.history_id().await.unwrap();
    storage
        .set_thread_reach(
            "concurrent-reach",
            &history,
            source_task.id,
            ReachTarget::Thread {
                thread_id: target.id,
            },
            None,
            true,
        )
        .await
        .unwrap();
    let actor = caller(&storage, source_task.id, "concurrent").await;
    tokio::join!(
        checkpoint(
            &storage,
            &actor,
            left.id,
            "concurrent-left-change",
            "Left changed outside",
            None,
        ),
        checkpoint(
            &storage,
            &actor,
            right.id,
            "concurrent-right-change",
            "Right changed outside",
            None,
        )
    );
    let delivery_count: u64 = storage
        .conn
        .lock()
        .await
        .query_row(
            "SELECT count(*) FROM thread_change_deliveries WHERE chat_thread_id=?1",
            [target.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(delivery_count >= 2, "an outside commit lost its delivery");
    let activities = storage
        .thread_detail(target.id, None, 100)
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| activity.kind == "outside_change")
        .collect::<Vec<_>>();
    assert_eq!(activities.len(), 1);
    assert_eq!(activities[0].data["change_count"], delivery_count);
}

#[tokio::test]
async fn history_reset_deletes_delivery_cursor_and_admission_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let source = create(&storage, "reset-source", ThreadKind::Space, None).await;
    let source_task = create(&storage, "reset-actor", ThreadKind::Task, Some(source.id)).await;
    let target = create(&storage, "reset-target", ThreadKind::Space, None).await;
    let target_task = create(&storage, "reset-task", ThreadKind::Task, Some(target.id)).await;
    let history = storage.history_id().await.unwrap();
    storage
        .set_thread_reach(
            "reset-reach",
            &history,
            source_task.id,
            ReachTarget::Thread {
                thread_id: target_task.id,
            },
            None,
            true,
        )
        .await
        .unwrap();
    let actor = caller(&storage, source_task.id, "reset").await;
    checkpoint(
        &storage,
        &actor,
        target_task.id,
        "reset-change",
        "Reset delivery exists",
        None,
    )
    .await;
    accepted_space_context(&storage, target.id, "reset-context").await;
    storage.reset().await.unwrap();
    let conn = storage.conn.lock().await;
    for table in [
        "thread_change_deliveries",
        "thread_change_cursors",
        "thread_turn_contexts",
    ] {
        let count: u64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table} survived history reset");
    }
}
