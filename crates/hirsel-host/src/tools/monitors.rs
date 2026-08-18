use super::ToolSuite;
use crate::storage::{MonitorRecord, MonitorWakeOn, monitor_process_info};

impl ToolSuite {
    pub async fn monitors_create(
        &self,
        cmd: String,
        every_secs: u64,
        wake_on: MonitorWakeOn,
        pattern: Option<String>,
        label: String,
    ) -> anyhow::Result<MonitorRecord> {
        let record = self
            .storage
            .create_monitor(cmd, every_secs, wake_on, pattern, label)
            .await?;
        self.broadcast_monitor_upsert(&record);
        Ok(record)
    }

    pub async fn monitors_list(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        self.storage.monitors_list().await
    }

    pub async fn active_monitors(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        self.storage.active_monitors().await
    }

    pub async fn monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        self.storage.monitor(monitor_id).await
    }

    pub async fn monitors_cancel(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let record = self.storage.cancel_monitor(monitor_id).await?;
        if let Some(record) = &record {
            self.broadcast_monitor_upsert(record);
        }
        Ok(record)
    }

    pub async fn record_monitor_tick(
        &self,
        monitor_id: &str,
        last_output: String,
        summary: String,
    ) -> anyhow::Result<Option<MonitorRecord>> {
        let record = self
            .storage
            .record_monitor_tick(monitor_id, last_output, summary)
            .await?;
        if let Some(record) = &record {
            self.broadcast_monitor_upsert(record);
        }
        Ok(record)
    }

    pub fn broadcast_monitor_upsert(&self, record: &MonitorRecord) {
        self.broadcast_process_upsert(monitor_process_info(record));
    }
}
