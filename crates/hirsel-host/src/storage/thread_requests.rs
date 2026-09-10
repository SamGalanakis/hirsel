use super::Storage;
use rusqlite::{OptionalExtension, params};
impl Storage {
    pub(crate) async fn thread_request(
        &self,
        client_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let c = self.conn.lock().await;
        let row: Option<String> = c
            .query_row(
                "SELECT payload FROM thread_requests WHERE client_id=?1",
                [client_id],
                |r| r.get(0),
            )
            .optional()?;
        row.map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub async fn save_thread_request(
        &self,
        client_id: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(
            &c,
            payload["history_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("request history is required"))?,
        )?;
        c.execute("INSERT INTO thread_requests(client_id,payload) VALUES(?1,?2) ON CONFLICT(client_id) DO NOTHING",params![client_id,serde_json::to_string(payload)?])?;
        Ok(())
    }
    pub async fn pending_thread_requests(
        &self,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let c = self.conn.lock().await;
        let rows = c
            .prepare("SELECT r.client_id,r.payload FROM thread_requests r JOIN threads t ON t.id=r.thread_id WHERE r.report_triggered=0 OR (t.archived_at IS NULL AND (t.snoozed_until IS NULL OR hirsel_utc_timestamp(t.snoozed_until)<=hirsel_utc_timestamp(?1))) ORDER BY r.id")?
            .query_map([chrono::Utc::now().to_rfc3339()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, payload)| Ok((id, serde_json::from_str(&payload)?)))
            .collect()
    }
    pub async fn remove_thread_request(&self, client_id: &str) -> anyhow::Result<bool> {
        Ok(self.conn.lock().await.execute(
            "DELETE FROM thread_requests WHERE client_id=?1",
            [client_id],
        )? > 0)
    }
}

impl Storage {
    pub(crate) async fn requested_thread_cancellations(
        &self,
    ) -> anyhow::Result<Vec<hirsel_proto::ThreadTurn>> {
        let c = self.conn.lock().await;
        let ids=c.prepare("SELECT t.id FROM thread_cancellations r JOIN thread_turns t ON t.id=r.turn_id WHERE t.state IN ('queued','running')")?.query_map([],|r|r.get::<_,u64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| super::thread_activity::get(&c, id))
            .collect()
    }
}
