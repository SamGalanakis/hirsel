//! Durable cross-store receipts for projecting Lash process events into chat.

use super::{Storage, chat};
use hirsel_proto::{ChatAuthor, ChatMessage};
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone)]
pub(crate) struct ProcessDelivery {
    pub(crate) key: String,
    pub(crate) thread_id: u64,
    pub(crate) process_id: String,
    pub(crate) process_name: String,
    pub(crate) trigger: String,
    pub(crate) outcome: String,
    pub(crate) result: String,
}

pub(crate) struct ProcessMessageDelivery {
    pub(crate) delivery: ProcessDelivery,
    pub(crate) message: ChatMessage,
    pub(crate) newly_appended: bool,
}

impl ProcessDelivery {
    pub(crate) fn body(&self) -> String {
        format!(
            "[process] `{}` via {}: {}\n\n{}",
            self.process_name, self.trigger, self.outcome, self.result
        )
    }
}

impl Storage {
    /// Stage before consuming the Lash queue row. Re-entry keeps the first
    /// payload so a retry cannot change the conversation fact it represents.
    pub(crate) async fn stage_process_delivery(
        &self,
        delivery: &ProcessDelivery,
    ) -> anyhow::Result<()> {
        let c = self.conn.lock().await;
        super::threads::get(&c, delivery.thread_id)?;
        c.execute(
            "INSERT OR IGNORE INTO process_deliveries(delivery_key,thread_id,process_id,process_name,trigger_label,outcome,result) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                delivery.key,
                delivery.thread_id,
                delivery.process_id,
                delivery.process_name,
                delivery.trigger,
                delivery.outcome,
                delivery.result,
            ],
        )?;
        Ok(())
    }

    /// Append the conversation row and receipt atomically. An existing message
    /// is returned without being republished when triage still needs retrying.
    pub(crate) async fn deliver_process_message(
        &self,
        delivery_key: &str,
    ) -> anyhow::Result<ProcessMessageDelivery> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let row = tx
            .query_row(
                "SELECT thread_id,process_id,process_name,trigger_label,outcome,result,message_id,triage_dispatched FROM process_deliveries WHERE delivery_key=?1",
                [delivery_key],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<u64>>(6)?,
                        row.get::<_, bool>(7)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("process delivery was not staged"))?;
        let (thread_id, process_id, process_name, trigger, outcome, result, message_id, dispatched) =
            row;
        anyhow::ensure!(!dispatched, "process delivery was already triaged");
        let delivery = ProcessDelivery {
            key: delivery_key.to_string(),
            thread_id,
            process_id,
            process_name,
            trigger,
            outcome,
            result,
        };
        let newly_appended = message_id.is_none();
        let message_id = match message_id {
            Some(message_id) => message_id,
            None => {
                tx.execute(
                    "INSERT INTO chat_messages(author,body,ref,ts,thread_id,tool_calls) VALUES('agent',?1,NULL,?2,?3,'[]')",
                    params![delivery.body(), chrono::Utc::now().to_rfc3339(), thread_id],
                )?;
                let message_id = tx.last_insert_rowid() as u64;
                tx.execute(
                    "UPDATE process_deliveries SET message_id=?2 WHERE delivery_key=?1 AND message_id IS NULL",
                    params![delivery_key, message_id],
                )?;
                message_id
            }
        };
        let message = chat::get_chat_message(&tx, message_id)?;
        tx.commit()?;
        debug_assert_eq!(message.author, ChatAuthor::Agent);
        Ok(ProcessMessageDelivery {
            delivery,
            message,
            newly_appended,
        })
    }

    pub(crate) async fn mark_process_triage_dispatched(
        &self,
        delivery_key: &str,
    ) -> anyhow::Result<()> {
        let c = self.conn.lock().await;
        anyhow::ensure!(
            c.execute(
                "UPDATE process_deliveries SET triage_dispatched=1 WHERE delivery_key=?1 AND message_id IS NOT NULL",
                [delivery_key],
            )? == 1,
            "process delivery is not ready for triage"
        );
        Ok(())
    }

    pub(crate) async fn pending_process_deliveries(
        &self,
        thread_id: u64,
    ) -> anyhow::Result<Vec<String>> {
        let c = self.conn.lock().await;
        let mut query = c.prepare(
            "SELECT delivery_key FROM process_deliveries WHERE thread_id=?1 AND (message_id IS NULL OR triage_dispatched=0) ORDER BY rowid",
        )?;
        Ok(query
            .query_map([thread_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminal_outcomes_append_once_before_triage() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let caller = storage.test_running_caller().await;

        for outcome in ["completed", "failed", "cancelled"] {
            let key = format!("process-terminal:proc:{outcome}");
            storage
                .stage_process_delivery(&ProcessDelivery {
                    key: key.clone(),
                    thread_id: caller.thread_id,
                    process_id: "proc".into(),
                    process_name: "nightly check".into(),
                    trigger: "cron 0 2 * * *".into(),
                    outcome: outcome.into(),
                    result: format!("{outcome} result"),
                })
                .await
                .unwrap();
            let first = storage.deliver_process_message(&key).await.unwrap();
            assert!(first.newly_appended);
            assert!(first.message.body.contains("nightly check"));
            assert!(first.message.body.contains(outcome));
            let retry = storage.deliver_process_message(&key).await.unwrap();
            assert!(!retry.newly_appended);
            assert_eq!(retry.message.id, first.message.id);
            storage.mark_process_triage_dispatched(&key).await.unwrap();
        }

        let detail = storage
            .thread_detail(caller.thread_id, None, 20)
            .await
            .unwrap();
        assert_eq!(detail.messages.len(), 3);
        assert!(
            storage
                .pending_process_deliveries(caller.thread_id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
