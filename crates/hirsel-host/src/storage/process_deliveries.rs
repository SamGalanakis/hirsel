//! Durable cross-store receipts for projecting Lash process events into chat.

use super::{Storage, chat};
use hirsel_proto::{ChatMessage, MessageOrigin, ProcessOutcome, TriggerLabel};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct ProcessDelivery {
    pub(crate) key: String,
    pub(crate) thread_id: u64,
    pub(crate) origin: MessageOrigin,
}

pub(crate) struct ProcessMessageDelivery {
    pub(crate) message: ChatMessage,
    pub(crate) newly_appended: bool,
}

pub(crate) fn result_body(result: &Value) -> String {
    match result {
        Value::String(text) => text.clone(),
        Value::Object(_) | Value::Array(_) => {
            let json = serde_json::to_string_pretty(result).expect("JSON value serializes");
            // A string inside JSON may contain a Markdown fence itself.
            let fence = "`".repeat(
                json.split(|c| c != '`')
                    .map(str::len)
                    .max()
                    .unwrap_or(0)
                    .max(2)
                    + 1,
            );
            format!("{fence}json\n{json}\n{fence}")
        }
        other => other.to_string(),
    }
}

impl ProcessDelivery {
    pub(crate) fn body(&self) -> String {
        let MessageOrigin::Process { result, error, .. } = &self.origin;
        error.clone().unwrap_or_else(|| result_body(result))
    }
}

// Keep schema 7 intact: result holds the complete structured origin for new
// receipts. Existing scalar receipts are decoded below, without rewriting any
// historical message or importing another store.
pub(super) fn message_origin(
    c: &rusqlite::Connection,
    message_id: u64,
) -> rusqlite::Result<Option<MessageOrigin>> {
    c.query_row(
        "SELECT process_id,process_name,outcome,result FROM process_deliveries WHERE message_id=?1",
        [message_id],
        origin_from_row,
    )
    .optional()
}

fn origin_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageOrigin> {
    let encoded: String = row.get(3)?;
    if let Ok(origin) = serde_json::from_str::<MessageOrigin>(&encoded) {
        return Ok(origin);
    }
    // Old receipts have no registered source metadata. Do not pass their hash
    // labels off as human-facing trigger descriptions.
    let outcome: String = row.get(2)?;
    let result = serde_json::from_str(&encoded).unwrap_or(Value::String(encoded));
    Ok(MessageOrigin::Process {
        process_id: row.get(0)?,
        name: row.get(1)?,
        trigger: TriggerLabel::Other {
            key: "previous process delivery".into(),
        },
        subscription_key: None,
        outcome: match outcome.as_str() {
            "completed" => ProcessOutcome::Completed,
            "failed" => ProcessOutcome::Failed,
            "cancelled" => ProcessOutcome::Cancelled,
            _ => ProcessOutcome::Woke,
        },
        error: (outcome == "failed").then(|| result_body(&result)),
        result,
    })
}

impl Storage {
    /// Stage before consuming the Lash queue row. Re-entry keeps the first payload.
    pub(crate) async fn stage_process_delivery(
        &self,
        delivery: &ProcessDelivery,
    ) -> anyhow::Result<()> {
        let c = self.conn.lock().await;
        super::threads::get(&c, delivery.thread_id)?;
        let MessageOrigin::Process {
            process_id,
            name,
            trigger,
            outcome,
            ..
        } = &delivery.origin;
        c.execute(
            "INSERT OR IGNORE INTO process_deliveries(delivery_key,thread_id,process_id,process_name,trigger_label,outcome,result) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![delivery.key, delivery.thread_id, process_id, name, serde_json::to_string(trigger)?, serde_json::to_value(outcome)?.as_str(), serde_json::to_string(&delivery.origin)?],
        )?;
        Ok(())
    }

    /// Append the conversation row and receipt atomically; queue acceptance is
    /// independently durable and idempotent by the delivery key.
    pub(crate) async fn deliver_process_message(
        &self,
        delivery_key: &str,
    ) -> anyhow::Result<ProcessMessageDelivery> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let (thread_id, message_id) = tx.query_row(
            "SELECT thread_id,message_id FROM process_deliveries WHERE delivery_key=?1",
            [delivery_key],
            |r| Ok((r.get::<_, u64>(0)?, r.get::<_, Option<u64>>(1)?)),
        )?;
        let origin = tx.query_row("SELECT process_id,process_name,outcome,result FROM process_deliveries WHERE delivery_key=?1", [delivery_key], origin_from_row)?;
        let delivery = ProcessDelivery {
            key: delivery_key.into(),
            thread_id,
            origin,
        };
        let newly_appended = message_id.is_none();
        let message_id = match message_id {
            Some(id) => id,
            None => {
                tx.execute("INSERT INTO chat_messages(author,body,ref,ts,thread_id,tool_calls) VALUES('agent',?1,NULL,?2,?3,'[]')", params![delivery.body(),chrono::Utc::now().to_rfc3339(),thread_id])?;
                let id = tx.last_insert_rowid() as u64;
                tx.execute(
                    "UPDATE process_deliveries SET message_id=?2 WHERE delivery_key=?1",
                    params![delivery_key, id],
                )?;
                id
            }
        };
        let message = chat::get_chat_message(&tx, message_id)?;
        tx.commit()?;
        Ok(ProcessMessageDelivery {
            message,
            newly_appended,
        })
    }

    pub(crate) async fn mark_process_enqueued(&self, delivery_key: &str) -> anyhow::Result<()> {
        let c = self.conn.lock().await;
        // The schema-7 receipt column retains its historical name; it now marks
        // durable normal-turn acceptance, never asynchronous fork dispatch.
        anyhow::ensure!(c.execute("UPDATE process_deliveries SET triage_dispatched=1 WHERE delivery_key=?1 AND message_id IS NOT NULL", [delivery_key])? == 1, "process delivery is not ready for enqueue");
        Ok(())
    }

    pub(crate) async fn pending_process_deliveries(
        &self,
        thread_id: u64,
    ) -> anyhow::Result<Vec<String>> {
        let c = self.conn.lock().await;
        let mut query = c.prepare("SELECT delivery_key FROM process_deliveries WHERE thread_id=?1 AND (message_id IS NULL OR triage_dispatched=0) ORDER BY rowid")?;
        Ok(query
            .query_map([thread_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn terminal_outcomes_append_once_before_enqueue() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let caller = storage.test_running_caller().await;

        for outcome in [
            ProcessOutcome::Completed,
            ProcessOutcome::Failed,
            ProcessOutcome::Cancelled,
        ] {
            let key = format!("process-terminal:proc:{outcome:?}");
            storage
                .stage_process_delivery(&ProcessDelivery {
                    key: key.clone(),
                    thread_id: caller.thread_id,
                    origin: MessageOrigin::Process {
                        process_id: "proc".into(),
                        name: "nightly check".into(),
                        trigger: TriggerLabel::Cron {
                            expr: "0 2 * * *".into(),
                            tz: None,
                        },
                        subscription_key: None,
                        outcome,
                        result: Value::String("plain result".into()),
                        error: (outcome == ProcessOutcome::Failed).then(|| "failure reason".into()),
                    },
                })
                .await
                .unwrap();
            let first = storage.deliver_process_message(&key).await.unwrap();
            assert!(first.newly_appended);
            assert_eq!(
                first.message.body,
                if outcome == ProcessOutcome::Failed {
                    "failure reason"
                } else {
                    "plain result"
                }
            );
            assert!(
                matches!(first.message.origin, Some(MessageOrigin::Process { outcome: actual, .. }) if actual == outcome)
            );
            let retry = storage.deliver_process_message(&key).await.unwrap();
            assert!(!retry.newly_appended);
            assert_eq!(retry.message.id, first.message.id);
            storage.mark_process_enqueued(&key).await.unwrap();
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

#[cfg(test)]
mod rendering_tests {
    use super::*;
    #[test]
    fn scalar_and_structured_bodies_preserve_the_result() {
        for (value, expected) in [
            (serde_json::json!("bare \"string\""), "bare \"string\""),
            (serde_json::json!(42), "42"),
            (serde_json::json!(true), "true"),
            (Value::Null, "null"),
        ] {
            assert_eq!(result_body(&value), expected);
        }
        assert_eq!(
            result_body(&serde_json::json!({"ok":true})),
            "```json\n{\n  \"ok\": true\n}\n```"
        );
        assert_eq!(
            result_body(&serde_json::json!([1, null])),
            "```json\n[\n  1,\n  null\n]\n```"
        );
        assert!(result_body(&serde_json::json!(["```"])).starts_with("````json\n"));
    }
}
