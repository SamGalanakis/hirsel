use super::super::Storage;
use super::MonitorWakeOn;
use hirsel_proto::ProcessKind;
use hirsel_proto::ProcessState;

#[tokio::test]
async fn monitors_are_persisted_and_project_to_process_info() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();

    let monitor = storage
        .create_monitor(
            "printf ready",
            5,
            MonitorWakeOn::Changed,
            None,
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
