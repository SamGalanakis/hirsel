use super::*;
#[tokio::test]
async fn fresh_store_is_current_and_reopen_keeps_identity() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let id = storage.history_id().await.unwrap();
    let conn = storage.conn.lock().await;
    assert!(
        conn.pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0))
            .unwrap()
    );
    let names = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for obsolete in [
        "pings",
        "inbox_items",
        "side_chat_messages",
        "taste_decisions",
        "monitors",
    ] {
        assert!(!names.iter().any(|n| n == obsolete));
    }
    for required in [
        "process_deliveries",
        "thread_process_sessions",
        "thread_process_authorities",
    ] {
        assert!(names.iter().any(|name| name == required));
    }
    let icon_foreign_key: (String, String) = conn.query_row(
        r#"SELECT "table", "to" FROM pragma_foreign_key_list('threads') WHERE "from"='icon_blob_id'"#,
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(icon_foreign_key, ("blobs".into(), "id".into()));
    assert_eq!(
        conn.query_row("SELECT count(*) FROM threads", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    let thread_columns = conn
        .prepare("SELECT name,type,\"notnull\" FROM pragma_table_xinfo('threads') ORDER BY cid")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        thread_columns,
        vec![
            ("id".into(), "INTEGER".into(), false),
            ("client_id".into(), "TEXT".into(), false),
            ("kind".into(), "TEXT".into(), true),
            ("parent_thread_id".into(), "INTEGER".into(), false),
            ("pinned_at".into(), "TEXT".into(), false),
            ("title".into(), "TEXT".into(), true),
            ("icon".into(), "TEXT".into(), false),
            ("icon_blob_id".into(), "TEXT".into(), false),
            ("showcased_artifact_id".into(), "INTEGER".into(), false),
            ("description".into(), "TEXT".into(), true),
            ("instrument".into(), "TEXT".into(), true),
            ("attention".into(), "TEXT".into(), true),
            ("settled_at".into(), "TEXT".into(), false),
            ("archived_at".into(), "TEXT".into(), false),
            ("snoozed_until".into(), "TEXT".into(), false),
            ("read".into(), "INTEGER".into(), true),
            ("created_at".into(), "TEXT".into(), true),
            ("updated_at".into(), "TEXT".into(), true),
            ("revision".into(), "INTEGER".into(), true),
        ]
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u64>(
            0
        ))
        .unwrap(),
        0
    );
    drop(conn);
    drop(storage);
    let reopened = Storage::open(dir.path()).await.unwrap();
    assert_eq!(reopened.history_id().await.unwrap(), id);
}
#[tokio::test]
async fn unsupported_store_is_rejected_without_modifying_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hirsel.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE pings(id INTEGER PRIMARY KEY, content TEXT); INSERT INTO pings VALUES(4,'keep me');").unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(
        Storage::open(dir.path())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("fresh current store")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("hirsel.sqlite-wal").exists());
}

#[tokio::test]
async fn view_only_store_is_not_fresh_and_remains_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hirsel.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE VIEW unexpected AS SELECT 42 AS retained")
        .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(
        Storage::open(dir.path())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("fresh current store")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("hirsel.sqlite-wal").exists());
}

#[tokio::test]
async fn explicit_debug_history_reset_changes_store_identity() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let old = storage.history_id().await.unwrap();
    storage.reset().await.unwrap();
    assert_ne!(storage.history_id().await.unwrap(), old);
    assert_eq!(storage.thread_snapshot().await.unwrap().len(), 0);
}

#[tokio::test]
async fn previous_schema_version_is_refused_without_in_place_evolution() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    assert_eq!(
        storage
            .conn
            .lock()
            .await
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        7
    );
    drop(storage);
    let path = dir.path().join("hirsel.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 6).unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(Storage::open(dir.path()).await.is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
}
#[tokio::test]
async fn unknown_current_layouts_and_bad_identity_are_untouched() {
    for change in [
        "CREATE TABLE unexpected(id INTEGER PRIMARY KEY)",
        "UPDATE meta SET value='not-a-uuid' WHERE key='history_id'",
        "DROP TABLE thread_related_items",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.sqlite");
        drop(Storage::open(dir.path()).await.unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(change).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(Storage::open(dir.path()).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("hirsel.sqlite-wal").exists());
    }
}

#[tokio::test]
async fn branch_specific_schema_seven_layouts_are_refused_without_modification() {
    for layout in [
        include_str!("icons-only-v7.sql"),
        include_str!("processes-only-v7.sql"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(layout).unwrap();
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('history_id',?1)",
            [uuid::Uuid::new_v4().to_string()],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let error = Storage::open(dir.path()).await.err().unwrap();
        assert!(
            error
                .to_string()
                .contains("unsupported Hirsel history layout")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("hirsel.sqlite-wal").exists());
    }
}
