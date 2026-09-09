use super::Storage;
use rusqlite::params;
impl Storage {
    pub async fn save_thread_request(
        &self,
        client_id: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        self.conn.lock().await.execute("INSERT INTO thread_requests(client_id,payload) VALUES(?1,?2) ON CONFLICT(client_id) DO NOTHING",params![client_id,serde_json::to_string(payload)?])?;
        Ok(())
    }
    pub async fn pending_thread_requests(
        &self,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let c = self.conn.lock().await;
        let rows = c
            .prepare("SELECT client_id,payload FROM thread_requests ORDER BY id")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
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
