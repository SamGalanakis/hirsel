use crate::storage::Storage;
use chrono::{DateTime, Utc};
use hirsel_proto::{ChatAuthor, ThreadAttention, ThreadTurnState};
use serde_json::json;

fn ts(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

async fn work(storage: &Storage) -> hirsel_proto::Thread {
    storage
        .create_thread(
            "work",
            "Work",
            "",
            &json!({}),
            ThreadAttention::NeedsOwner,
            None,
        )
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn ordinary_inventory_has_truthful_creation_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = work(&storage).await;
    let snapshot = storage.hello_snapshot().await.unwrap();
    assert_eq!(snapshot.threads.len(), 1);
    for row in snapshot.threads {
        assert_eq!(row.last_activity_at, row.created_at);
        assert_eq!(row.queued_turn_count, 0);
        assert!(row.running_turn.is_none() && row.last_finished_turn.is_none());
    }
    assert_eq!(
        storage
            .thread_detail(thread.id, None, 1)
            .await
            .unwrap()
            .thread,
        thread
    );
}

#[tokio::test]
async fn queue_wait_is_not_working_duration_and_repeated_running_keeps_actual_start() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = work(&storage).await;
    let queued = storage.queue_thread_turn(thread.id, None).await.unwrap();
    storage
        .conn
        .lock()
        .await
        .execute(
            "UPDATE thread_turns SET started_at='2020-01-01T00:00:00Z' WHERE id=?1",
            [queued.id],
        )
        .unwrap();
    let waiting = storage.thread(thread.id).await.unwrap().unwrap();
    assert!(waiting.running_turn.is_none());
    assert_eq!(waiting.queued_turn_count, 1);
    let running = storage.run_thread_turn(queued.id).await.unwrap();
    assert!(running.started_at > ts("2020-01-01T00:00:00Z"));
    storage.queue_thread_turn(thread.id, None).await.unwrap();
    let row = storage.thread(thread.id).await.unwrap().unwrap();
    assert_eq!(row.running_turn, Some(running.clone()));
    assert_eq!(row.queued_turn_count, 1);
    assert_eq!(storage.run_thread_turn(running.id).await.unwrap(), running);
    assert_eq!(row.revision, thread.revision);
    assert_eq!(row.attention, ThreadAttention::NeedsOwner);
    assert!(!row.read && row.settled_at.is_none());
}

#[tokio::test]
async fn every_terminal_outcome_survives_snapshot_restart_without_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = work(&storage).await;
    for state in [
        ThreadTurnState::Completed,
        ThreadTurnState::Failed,
        ThreadTurnState::Cancelled,
        ThreadTurnState::Interrupted,
    ] {
        let turn = storage.start_thread_turn(thread.id, None).await.unwrap();
        let finished = storage
            .finish_thread_turn(turn.id, state, None)
            .await
            .unwrap();
        let row = storage.thread(thread.id).await.unwrap().unwrap();
        assert_eq!(row.last_finished_turn, Some(finished.clone()));
        assert_eq!(row.last_activity_at, finished.finished_at.unwrap());
        assert!(row.running_turn.is_none() && row.settled_at.is_none());
        assert_eq!(row.attention, ThreadAttention::NeedsOwner);
        assert_eq!(row.revision, thread.revision);
        assert_eq!(
            storage
                .thread_snapshot()
                .await
                .unwrap()
                .into_iter()
                .find(|t| t.id == thread.id),
            Some(row)
        );
    }
    let before = storage.thread(thread.id).await.unwrap().unwrap();
    drop(storage);
    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(reopened.thread(thread.id).await.unwrap(), Some(before));
}

#[tokio::test]
async fn activity_tracks_facts_and_not_read_or_lifecycle_bookkeeping() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = work(&storage).await;
    let message = storage
        .append_thread_chat(thread.id, ChatAuthor::Agent, "Result", None, vec![])
        .await
        .unwrap();
    assert_eq!(
        storage
            .thread(thread.id)
            .await
            .unwrap()
            .unwrap()
            .last_activity_at,
        message.ts
    );
    let activity = storage
        .append_thread_activity(thread.id, None, "background_result", &json!({}))
        .await
        .unwrap();
    storage.mark_thread_read(thread.id).await.unwrap();
    storage.settle_thread(thread.id, true).await.unwrap();
    storage.archive_thread(thread.id, true).await.unwrap();
    storage
        .snooze_thread(thread.id, Some(Utc::now() + chrono::Duration::hours(1)))
        .await
        .unwrap();
    storage
        .update_thread(
            thread.id,
            Some("Renamed"),
            None,
            None,
            Some(ThreadAttention::Quiet),
        )
        .await
        .unwrap();
    let row = storage.thread(thread.id).await.unwrap().unwrap();
    assert_eq!(row.last_activity_at, activity.ts);
    assert!(row.updated_at > row.last_activity_at);
    assert!(row.settled_at.is_some() && row.archived_at.is_some());
    assert_eq!(row.attention, ThreadAttention::Quiet);
    // Activity kinds are extensible facts, not lifecycle command names.
    let fact = storage
        .append_thread_activity(thread.id, None, "read", &json!({"source":"research paper"}))
        .await
        .unwrap();
    assert_eq!(
        storage
            .thread(thread.id)
            .await
            .unwrap()
            .unwrap()
            .last_activity_at,
        fact.ts
    );
}

#[tokio::test]
async fn activity_and_latest_outcome_use_timestamp_chronology_not_ids_or_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = work(&storage).await;
    let early_id = storage.queue_thread_turn(thread.id, None).await.unwrap();
    let later_id = storage.queue_thread_turn(thread.id, None).await.unwrap();
    let conn = storage.conn.lock().await;
    conn.execute(
        "UPDATE threads SET created_at='2020-01-01T00:00:00Z' WHERE id=?1",
        [thread.id],
    )
    .unwrap();
    conn.execute("UPDATE thread_turns SET state='completed',started_at='2020-01-01T00:00:00Z',finished_at='2021-01-01T10:00:00Z' WHERE id=?1", [early_id.id]).unwrap();
    conn.execute("UPDATE thread_turns SET state='failed',started_at='2020-01-01T00:00:00Z',finished_at='2021-01-01T11:00:00+02:00' WHERE id=?1", [later_id.id]).unwrap();
    drop(conn);
    let row = storage.thread(thread.id).await.unwrap().unwrap();
    assert_eq!(row.last_finished_turn.unwrap().id, early_id.id);
    assert_eq!(row.last_activity_at, ts("2021-01-01T10:00:00Z"));
}

#[tokio::test]
async fn deleted_message_recency_recedes_to_remaining_facts_and_stays_thread_local() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = work(&storage).await;
    let (message, _) = storage
        .append_thread_owner_message(
            &storage.history_id().await.unwrap(),
            thread.id,
            "message",
            "Draft",
            None,
            &[],
            &[],
            &[],
        )
        .await
        .unwrap();
    storage
        .append_thread_chat(
            storage
                .create_thread(
                    "other",
                    "Other",
                    "",
                    &json!({}),
                    ThreadAttention::Quiet,
                    None,
                )
                .await
                .unwrap()
                .0
                .id,
            ChatAuthor::Agent,
            "Other conversation",
            None,
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        storage
            .thread(thread.id)
            .await
            .unwrap()
            .unwrap()
            .last_activity_at,
        message.ts
    );
    storage.delete_chat_message(message.id).await.unwrap();
    assert_eq!(
        storage
            .thread(thread.id)
            .await
            .unwrap()
            .unwrap()
            .last_activity_at,
        thread.created_at
    );
}

async fn assert_precise_terminal_projection(
    first_finished_at: &str,
    second_finished_at: &str,
    latest_is_first: bool,
) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread = work(&storage).await;
    let first = storage.queue_thread_turn(thread.id, None).await.unwrap();
    let second = storage.queue_thread_turn(thread.id, None).await.unwrap();
    {
        let conn = storage.conn.lock().await;
        conn.execute(
            "UPDATE threads SET created_at='2020-01-01T00:00:00Z' WHERE id=?1",
            [thread.id],
        )
        .unwrap();
        for (id, finished_at, state) in [
            (first.id, first_finished_at, "completed"),
            (second.id, second_finished_at, "failed"),
        ] {
            conn.execute(
                "UPDATE thread_turns SET state=?2,started_at='2020-01-01T00:00:00Z',finished_at=?3 WHERE id=?1",
                rusqlite::params![id, state, finished_at],
            )
            .unwrap();
        }
    }
    let (expected_id, expected_time, expected_state) = if latest_is_first {
        (first.id, ts(first_finished_at), ThreadTurnState::Completed)
    } else {
        (second.id, ts(second_finished_at), ThreadTurnState::Failed)
    };
    let row = storage.thread(thread.id).await.unwrap().unwrap();
    let finished = row.last_finished_turn.as_ref().unwrap();
    assert_eq!(finished.id, expected_id);
    assert_eq!(finished.state, expected_state);
    assert_eq!(finished.finished_at, Some(expected_time));
    assert_eq!(row.last_activity_at, expected_time);
    assert_eq!(
        storage
            .hello_snapshot()
            .await
            .unwrap()
            .threads
            .into_iter()
            .find(|item| item.id == thread.id),
        Some(row)
    );
}

#[tokio::test]
async fn timestamp_projection_orders_mixed_offsets_within_one_millisecond() {
    assert_precise_terminal_projection(
        "2021-01-01T10:00:00.000000200Z",
        "2021-01-01T12:00:00.000000100+02:00",
        true,
    )
    .await;
}

#[tokio::test]
async fn timestamp_projection_orders_fractional_after_whole_second() {
    assert_precise_terminal_projection(
        "2021-01-01T10:00:00.000000100Z",
        "2021-01-01T10:00:00Z",
        true,
    )
    .await;
}

#[tokio::test]
async fn timestamp_projection_breaks_equal_instants_by_turn_id_despite_offsets() {
    assert_precise_terminal_projection(
        "2021-01-01T12:00:00.123456789+02:00",
        "2021-01-01T10:00:00.123456789Z",
        false,
    )
    .await;
}
