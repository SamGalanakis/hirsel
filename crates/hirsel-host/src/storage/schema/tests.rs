use super::super::Storage;
use hirsel_proto::EventStatus;
use rusqlite::Connection;

#[tokio::test]
async fn fresh_storage_enables_foreign_keys_and_busy_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let conn = storage.conn.lock().await;

    let foreign_keys: u32 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    let busy_timeout: u64 = conn
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    assert_eq!(busy_timeout, 5_000);
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
}

#[tokio::test]
async fn open_migrates_legacy_ping_rows() {
    let dir = tempfile::tempdir().unwrap();
    {
        let conn = Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE chat_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                author TEXT NOT NULL,
                body TEXT NOT NULL,
                ref INTEGER NULL,
                ts TEXT NOT NULL
            );
            CREATE TABLE inbox_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                anchor INTEGER NOT NULL REFERENCES chat_messages(id),
                requires_response INTEGER NOT NULL,
                quick_replies TEXT NOT NULL,
                status TEXT NOT NULL,
                ts TEXT NOT NULL
            );
            INSERT INTO chat_messages (author, body, ref, ts)
            VALUES ('agent', 'anchor', NULL, '2026-07-08T12:00:00Z');
            INSERT INTO inbox_items (
                content,
                anchor,
                requires_response,
                quick_replies,
                status,
                ts
            )
            VALUES ('legacy question', 1, 1, '[]', 'open', '2026-07-08T12:00:00Z');
            INSERT INTO inbox_items (
                content,
                anchor,
                requires_response,
                quick_replies,
                status,
                ts
            )
            VALUES ('legacy archived', 1, 0, '[]', 'archived', '2026-07-08T12:01:00Z');
            ",
        )
        .unwrap();
    }

    let storage = Storage::open(dir.path()).await.unwrap();
    let legacy_chat = storage.all_chat().await.unwrap();
    assert_eq!(legacy_chat.len(), 1);
    assert!(legacy_chat[0].tool_calls.is_empty());

    let legacy = storage.all_pings().await.unwrap();
    assert_eq!(legacy.len(), 2);
    assert!(!legacy[0].read);
    assert!(!legacy[0].archived);
    assert_eq!(legacy[0].name, "legacy-question");
    assert_eq!(legacy[0].description, "legacy question");
    assert_eq!(legacy[1].status, EventStatus::Done);
    assert!(legacy[1].archived);

    let read = storage.mark_ping_read(legacy[0].id).await.unwrap().unwrap();
    assert!(read.read);
    drop(storage);

    let reopened = Storage::open(dir.path()).await.unwrap();
    let persisted = reopened.all_pings().await.unwrap();
    assert_eq!(persisted.len(), 2);
    assert!(persisted[0].read);
    assert!(persisted[1].archived);
}

#[tokio::test]
async fn open_normalizes_contradictory_event_lifecycle_rows_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let anchor = storage
        .append_chat(hirsel_proto::ChatAuthor::Agent, "anchor", None)
        .await
        .unwrap();
    let archived_open = storage
        .create_ping(
            "archived-open",
            "Archived open",
            "body",
            anchor.id,
            true,
            vec![],
        )
        .await
        .unwrap();
    let snoozed_done = storage
        .create_ping(
            "snoozed-done",
            "Snoozed done",
            "body",
            anchor.id,
            true,
            vec![],
        )
        .await
        .unwrap();
    {
        let conn = storage.conn.lock().await;
        conn.execute(
            "UPDATE pings SET archived = 1, status = 'open', archived_at = NULL, snoozed_until = '2026-08-21T12:00:00Z' WHERE id = ?1",
            [archived_open.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE pings SET archived = 0, status = 'done', archived_at = '2026-08-20T12:00:00Z', snoozed_until = '2026-08-21T12:00:00Z' WHERE id = ?1",
            [snoozed_done.id],
        )
        .unwrap();
    }
    drop(storage);

    for _ in 0..2 {
        let storage = Storage::open(dir.path()).await.unwrap();
        let archived = storage.ping(archived_open.id).await.unwrap().unwrap();
        assert_eq!(archived.status, EventStatus::Done);
        assert!(archived.archived);
        assert!(archived.archived_at.is_some());
        assert_eq!(archived.snoozed_until, None);

        let done = storage.ping(snoozed_done.id).await.unwrap().unwrap();
        assert_eq!(done.status, EventStatus::Done);
        assert!(!done.archived);
        assert_eq!(done.archived_at, None);
        assert_eq!(done.snoozed_until, None);
    }
}
