use super::super::Storage;
use crate::processes::ProcessRecord;
use crate::processes::ProcessStatus;
use chrono::Utc;
use hirsel_drivers::AgentKind;
use hirsel_drivers::SessionHandle;
use hirsel_drivers::SubagentEvent;

#[tokio::test]
async fn running_subagent_processes_restore_as_abandoned_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let now = Utc::now();
    let record = ProcessRecord::restored(
        "proc-running".to_string(),
        AgentKind::Codex,
        Some("gpt-test".to_string()),
        SessionHandle {
            id: "handle-1".to_string(),
            agent: AgentKind::Codex,
        },
        "fix it".to_string(),
        "/tmp".to_string(),
        Some("external-1".to_string()),
        ProcessStatus::Running,
        vec![SubagentEvent::Started {
            external_id: "external-1".to_string(),
        }],
        now,
        now,
    );
    storage.upsert_subagent_process(&record).await.unwrap();

    let restored = storage
        .restore_subagent_processes_after_restart()
        .await
        .unwrap();
    assert_eq!(restored.abandoned, vec!["proc-running".to_string()]);
    assert_eq!(restored.records.len(), 1);
    assert_eq!(restored.records[0].status, ProcessStatus::Abandoned);

    drop(storage);
    let reopened = Storage::open(dir.path()).await.unwrap();
    let restored_again = reopened
        .restore_subagent_processes_after_restart()
        .await
        .unwrap();
    assert!(restored_again.abandoned.is_empty());
    assert_eq!(restored_again.records[0].status, ProcessStatus::Abandoned);
}
