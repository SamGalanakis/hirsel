use super::ToolSuite;
use hirsel_proto::ThreadActivity;
impl ToolSuite {
    pub async fn emit_scheduled_digest(
        &self,
        history_id: &str,
        thread_id: u64,
        job_id: impl Into<String>,
        text: impl Into<String>,
        status: impl Into<String>,
    ) -> anyhow::Result<ThreadActivity> {
        let activity = self.storage.record_background_activity(history_id,thread_id,"scheduled_digest", &serde_json::json!({"job_id":job_id.into(),"text":text.into(),"status":status.into()})).await?;
        self.publish_thread_activity(activity.clone()).await;
        Ok(activity)
    }
    pub(crate) async fn return_expired_snoozes(&self) -> anyhow::Result<()> {
        for thread in self.storage.thread_snapshot().await? {
            if thread
                .snoozed_until
                .is_some_and(|until| until <= chrono::Utc::now())
            {
                let thread = self.storage.snooze_thread(thread.id, None).await?;
                self.publish_thread_summary(thread.id).await;
            }
        }
        Ok(())
    }
}
