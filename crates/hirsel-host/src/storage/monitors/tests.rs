use super::super::Storage;
use super::MonitorCondition;
use hirsel_proto::ProcessKind;
use hirsel_proto::ProcessState;

#[tokio::test]
async fn monitors_are_persisted_and_project_to_process_info() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let monitor = storage
        .create_monitor(
            storage
                .create_thread(
                    "fixture-Monitor",
                    "Monitor",
                    "",
                    &serde_json::Value::Null,
                    hirsel_proto::ThreadAttention::Quiet,
                    None,
                )
                .await
                .unwrap()
                .0
                .id,
            "printf ready",
            5,
            MonitorCondition::Changed,
            "watch ready",
        )
        .await
        .unwrap();
    assert_eq!(monitor.every_secs, 30);
    assert_eq!(storage.active_monitors().await.unwrap().len(), 1);

    let updated = storage
        .record_monitor_tick(
            &monitor.id,
            "ready".to_string(),
            "exit 0: ready".to_string(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.summary.as_deref(), Some("exit 0: ready"));

    let snapshot = storage.monitor_snapshot().await.unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].kind, ProcessKind::Monitor);
    assert_eq!(snapshot[0].state, ProcessState::Running);
    assert_eq!(
        snapshot[0].summary.as_deref(),
        Some("printf ready · every 30s — exit 0: ready")
    );

    let cancelled = storage.cancel_monitor(&monitor.id).await.unwrap().unwrap();
    assert!(cancelled.cancelled_ts.is_some());
    let snapshot = storage.monitor_snapshot().await.unwrap();
    assert_eq!(snapshot[0].state, ProcessState::Cancelled);
}

#[tokio::test]
async fn current_schema_enforces_condition_shape_and_reads_validate_regex_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let thread_id = storage
        .create_thread(
            "monitor-condition-schema",
            "Monitor condition schema",
            "",
            &serde_json::Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0
        .id;
    let regex = storage
        .create_monitor(
            thread_id,
            "printf ready",
            30,
            MonitorCondition::parse("regex", Some("ready".to_string())).unwrap(),
            "regex",
        )
        .await
        .unwrap();
    let changed = storage
        .create_monitor(
            thread_id,
            "printf changed",
            30,
            MonitorCondition::Changed,
            "changed",
        )
        .await
        .unwrap();

    let conn = storage.conn.lock().await;
    for sql in [
        "UPDATE monitors SET pattern = NULL WHERE id = ?1",
        "UPDATE monitors SET pattern = '   ' WHERE id = ?1",
        "UPDATE monitors SET wake_on = 'unknown' WHERE id = ?1",
    ] {
        assert!(conn.execute(sql, [&regex.id]).is_err(), "accepted {sql}");
    }
    assert!(
        conn.execute(
            "UPDATE monitors SET pattern = 'ignored' WHERE id = ?1",
            [&changed.id],
        )
        .is_err()
    );
    conn.execute(
        "UPDATE monitors SET pattern = '[' WHERE id = ?1",
        [&regex.id],
    )
    .unwrap();
    drop(conn);

    let error = storage.monitor(&regex.id).await.unwrap_err().to_string();
    assert!(
        error.contains("invalid monitor regex"),
        "unexpected error: {error}"
    );
}
