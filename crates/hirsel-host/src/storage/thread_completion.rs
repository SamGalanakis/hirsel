//! One terminal commit shared by host and CLI executions.
use super::{Storage, chat, thread_activity, thread_scope};
use hirsel_proto::{ChatMessage, ThreadActivity, ThreadTurn, ThreadTurnState, ToolCallSummary};
use rusqlite::{OptionalExtension, params};

pub(crate) struct ThreadCompletion {
    pub(crate) turn: ThreadTurn,
    pub(crate) message: Option<ChatMessage>,
    pub(crate) failure_activity: Option<ThreadActivity>,
}

impl Storage {
    pub(crate) async fn accepted_thread_turn(
        &self,
        history: &str,
        id: u64,
        thread_id: u64,
    ) -> anyhow::Result<ThreadTurn> {
        let c = self.conn.lock().await;
        thread_scope::validate_history(&c, history)?;
        let turn = thread_activity::get(&c, id)?;
        anyhow::ensure!(
            turn.thread_id == thread_id,
            "accepted turn belongs to another Thread"
        );
        Ok(turn)
    }
    pub(crate) async fn complete_thread_turn(
        &self,
        history: &str,
        id: u64,
        state: ThreadTurnState,
        output: Option<(String, Vec<ToolCallSummary>)>,
    ) -> anyhow::Result<(ThreadTurn, Option<ChatMessage>)> {
        let completion = self
            .complete_thread_turn_with_failure(history, id, state, output, None)
            .await?;
        Ok((completion.turn, completion.message))
    }
    /// Commit the terminal output and failure activity together. Re-entry after
    /// an uncertain receipt returns the original rows without repeating writes.
    pub(crate) async fn complete_thread_turn_with_failure(
        &self,
        history: &str,
        id: u64,
        state: ThreadTurnState,
        output: Option<(String, Vec<ToolCallSummary>)>,
        failure: Option<&str>,
    ) -> anyhow::Result<ThreadCompletion> {
        anyhow::ensure!(
            matches!(
                state,
                ThreadTurnState::Completed
                    | ThreadTurnState::Failed
                    | ThreadTurnState::Cancelled
                    | ThreadTurnState::Interrupted
            ),
            "completion must be terminal"
        );
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_history(&tx, history)?;
        let before = thread_activity::get(&tx, id)?;
        let failure_key = format!("cli-terminal-failure:{history}:{id}");
        if before.finished_at.is_some() {
            let message = before
                .agent_message_id
                .map(|id| chat::get_chat_message(&tx, id))
                .transpose()?;
            let failure_activity = tx
                .query_row(
                    "SELECT activity_id FROM thread_activity_keys WHERE key=?1",
                    [&failure_key],
                    |r| r.get::<_, u64>(0),
                )
                .optional()?
                .map(|id| thread_activity::activity(&tx, id))
                .transpose()?;
            return Ok(ThreadCompletion {
                turn: before,
                message,
                failure_activity,
            });
        }
        let failure_activity = if let Some(reason) = failure {
            tx.execute("INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,'execution_failed',?3,?4)",
                params![before.thread_id,id,serde_json::to_string(&serde_json::json!({"reason":reason}))?,chrono::Utc::now().to_rfc3339()])?;
            let activity_id = tx.last_insert_rowid() as u64;
            tx.execute(
                "INSERT INTO thread_activity_keys(key,activity_id) VALUES(?1,?2)",
                params![failure_key, activity_id],
            )?;
            Some(thread_activity::activity(&tx, activity_id)?)
        } else {
            None
        };
        let message_id = if let Some((text, calls)) =
            output.filter(|(text, calls)| !text.trim().is_empty() || !calls.is_empty())
        {
            tx.execute("INSERT INTO chat_messages(author,body,ref,ts,thread_id,tool_calls) VALUES('agent',?1,?2,?3,?4,?5)",params![text,before.owner_message_id,chrono::Utc::now().to_rfc3339(),before.thread_id,serde_json::to_string(&calls)?])?;
            let mid = tx.last_insert_rowid() as u64;
            tx.execute("INSERT INTO message_artifacts(message_id,artifact_id) SELECT ?1,artifact_id FROM turn_output_artifacts WHERE turn_id=?2",params![mid,id])?;
            Some(mid)
        } else {
            before.agent_message_id
        };
        let turn = thread_activity::finish_with_failure(&tx, id, state, message_id, failure)?;
        tx.execute(
            "DELETE FROM thread_requests WHERE json_extract(payload,'$.turn_id')=?1",
            [id],
        )?;
        tx.execute(
            "UPDATE thread_execution_bindings SET revoked=1 WHERE history_id=?1 AND turn_id=?2",
            params![history, id],
        )?;
        let message = message_id
            .map(|id| chat::get_chat_message(&tx, id))
            .transpose()?;
        tx.commit()?;
        Ok(ThreadCompletion {
            turn,
            message,
            failure_activity,
        })
    }
}

impl Storage {
    pub(crate) async fn background_context(
        &self,
        history: &str,
        thread_id: u64,
    ) -> anyhow::Result<crate::fork_wake::PackContext> {
        let c = self.conn.lock().await;
        thread_scope::validate_history(&c, history)?;
        let thread = super::threads::get(&c, thread_id)?;
        let ids = c
            .prepare("SELECT id FROM chat_messages WHERE thread_id=?1 ORDER BY id DESC LIMIT 30")?
            .query_map([thread_id], |r| r.get::<_, u64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut messages = ids
            .into_iter()
            .map(|id| chat::get_chat_message(&c, id))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(crate::fork_wake::PackContext {
            threads: vec![thread],
            recent_chat: messages,
        })
    }
    pub(crate) async fn record_background_activity(
        &self,
        history: &str,
        thread_id: u64,
        kind: &str,
        data: &serde_json::Value,
    ) -> anyhow::Result<hirsel_proto::ThreadActivity> {
        let c = self.conn.lock().await;
        thread_scope::validate_history(&c, history)?;
        super::threads::get(&c, thread_id)?;
        c.execute("INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,NULL,?2,?3,?4)",params![thread_id,kind,serde_json::to_string(data)?,chrono::Utc::now().to_rfc3339()])?;
        thread_activity::activity(&c, c.last_insert_rowid() as u64)
    }
}

impl Storage {
    pub(crate) async fn accepted_message_references(
        &self,
        history: &str,
        turn_id: u64,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let c = self.conn.lock().await;
        thread_scope::validate_history(&c, history)?;
        let turn = thread_activity::get(&c, turn_id)?;
        let Some(message_id) = turn.owner_message_id else {
            return Ok(vec![]);
        };
        let mut query=c.prepare("SELECT a.id,a.title FROM message_artifacts r JOIN artifacts a ON a.id=r.artifact_id WHERE r.message_id=?1 ORDER BY a.id")?;
        Ok(query.query_map([message_id],|r|Ok(serde_json::json!({"artifact_id":r.get::<_,u64>(0)?,"title":r.get::<_,String>(1)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
impl Storage {
    /// A nontransactional counter proves the real connection hit a failing
    /// trigger even though SQLite correctly rolls back every affected row.
    pub(crate) async fn track_completion_failures(
        &self,
    ) -> anyhow::Result<std::sync::Arc<std::sync::atomic::AtomicUsize>> {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = attempts.clone();
        self.conn.lock().await.create_scalar_function(
            "terminal_delivery_probe",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(0_i64)
            },
        )?;
        Ok(attempts)
    }
}
