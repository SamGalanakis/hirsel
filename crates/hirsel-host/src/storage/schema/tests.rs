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
        "message_task_focus",
    ] {
        assert!(names.iter().any(|name| name == required));
    }
    let icon_foreign_key: (String, String) = conn.query_row(
        r#"SELECT "table", "to" FROM pragma_foreign_key_list('threads') WHERE "from"='icon_blob_id'"#,
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(icon_foreign_key, ("blobs".into(), "id".into()));
    let focus_columns = conn
        .prepare("SELECT name,type,\"notnull\" FROM pragma_table_xinfo('message_task_focus') ORDER BY cid")
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        focus_columns,
        vec![
            ("message_id".into(), "INTEGER".into(), false),
            ("task_thread_id".into(), "INTEGER".into(), true),
            ("snapshot_json".into(), "TEXT".into(), true),
        ]
    );
    let focus_foreign_keys = conn
        .prepare(r#"SELECT "from","table","to" FROM pragma_foreign_key_list('message_task_focus') ORDER BY "from""#)
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        focus_foreign_keys,
        vec![
            ("message_id".into(), "chat_messages".into(), "id".into()),
            ("task_thread_id".into(), "threads".into(), "id".into()),
        ]
    );
    let push_columns = conn
        .prepare("SELECT name FROM pragma_table_xinfo('push_tokens') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        push_columns,
        vec![
            "token",
            "device_token",
            "platform",
            "created_ts",
            "last_seen_ts",
        ]
    );
    let push_foreign_key: (String, String) = conn
        .query_row(
            r#"SELECT "table", "to" FROM pragma_foreign_key_list('push_tokens') WHERE "from"='device_token'"#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(push_foreign_key, ("device_tokens".into(), "token".into()));
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
            ("icon_symbol".into(), "TEXT".into(), false),
            ("icon_tint".into(), "TEXT".into(), false),
            ("icon_blob_id".into(), "TEXT".into(), false),
            ("showcased_artifact_id".into(), "INTEGER".into(), false),
            ("description".into(), "TEXT".into(), true),
            ("instrument".into(), "TEXT".into(), false),
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
        14
    );
    drop(storage);
    let path = dir.path().join("hirsel.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 13).unwrap();
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
    // 10 is a stale version number; 14 is the current one carrying a layout
    // that is not the current one.
    for version in [10, 14] {
        for layout in [
            include_str!("icons-only-v7.sql"),
            include_str!("processes-only-v7.sql"),
            include_str!("runtime-only-v8.sql"),
            include_str!("push-only-v8.sql"),
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
            conn.pragma_update(None, "user_version", version).unwrap();
            drop(conn);
            let before = std::fs::read(&path).unwrap();
            let error = Storage::open(dir.path()).await.err().unwrap();
            assert!(error.to_string().contains(if version == 10 {
                "unsupported Hirsel history schema"
            } else {
                "unsupported Hirsel history layout"
            }));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!dir.path().join("hirsel.sqlite-wal").exists());
        }
    }
}

#[tokio::test]
async fn turn_state_and_completion_timestamp_must_agree() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let (thread, _) = storage
        .create_thread(
            "constraints",
            "Constraints",
            "",
            None,
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    let turn = storage.queue_thread_turn(thread.id, None).await.unwrap();
    let conn = storage.conn.lock().await;
    for (state, finished, valid) in [
        ("queued", None, true),
        ("running", None, true),
        ("completed", Some("2026-09-13T00:00:00Z"), true),
        ("failed", Some("2026-09-13T00:00:00Z"), true),
        ("cancelled", Some("2026-09-13T00:00:00Z"), true),
        ("interrupted", Some("2026-09-13T00:00:00Z"), true),
        ("completed", None, false),
        ("running", Some("2026-09-13T00:00:00Z"), false),
        ("typo", None, false),
        ("typo", Some("2026-09-13T00:00:00Z"), false),
    ] {
        assert_eq!(
            conn.execute(
                "UPDATE thread_turns SET state=?2,finished_at=?3,started_at=CASE WHEN ?2='running' THEN accepted_at ELSE NULL END WHERE id=?1",
                rusqlite::params![turn.id, state, finished]
            )
            .is_ok(),
            valid,
            "{state}, {finished:?}"
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='thread_cancellations'",
            [],
            |row| row.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    for (state, started, finished, valid) in [
        ("queued", None, None, true),
        ("queued", Some("2026-09-13T00:00:00Z"), None, false),
        ("running", None, None, false),
        ("running", Some("2026-09-13T00:00:00Z"), None, true),
        ("cancelled", None, Some("2026-09-13T00:01:00Z"), true),
        (
            "cancelled",
            Some("2026-09-13T00:00:00Z"),
            Some("2026-09-13T00:01:00Z"),
            true,
        ),
    ] {
        assert_eq!(
            conn.execute(
                "UPDATE thread_turns SET state=?2,started_at=?3,finished_at=?4 WHERE id=?1",
                rusqlite::params![turn.id, state, started, finished]
            )
            .is_ok(),
            valid,
            "{state}, {started:?}"
        );
    }
    assert!(
        conn.execute(
            "UPDATE thread_turns SET accepted_at=NULL WHERE id=?1",
            [turn.id]
        )
        .is_err()
    );
    let report_columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('thread_reports') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        report_columns,
        ["child_turn_id", "operation_id", "activity_id"]
    );
}
