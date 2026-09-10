use super::{
    Storage,
    chat::{author_to_str, chat_message_from_row, get_chat_message, load_attachments_for_messages},
    threads,
};
use hirsel_proto::{ChatAuthor, ChatMessage, ThreadDetail, ToolCallSummary};
use rusqlite::{OptionalExtension, params};
impl Storage {
    #[allow(clippy::too_many_arguments)]
    pub async fn append_thread_owner_message(
        &self,
        expected_history: &str,
        thread_id: u64,
        client_id: &str,
        body: impl Into<String>,
        anchor: Option<u64>,
        attachments: &[String],
        mentions: &[u64],
        artifact_ids: &[u64],
    ) -> anyhow::Result<(ChatMessage, bool)> {
        self.append_thread_owner_record(
            expected_history,
            thread_id,
            client_id,
            body.into(),
            anchor,
            attachments,
            mentions,
            artifact_ids,
            None,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn append_thread_owner_request(
        &self,
        expected_history: &str,
        thread_id: u64,
        client_id: &str,
        body: String,
        attachments: &[String],
        mentions: &[u64],
        artifact_ids: &[u64],
        request: &serde_json::Value,
    ) -> anyhow::Result<(ChatMessage, bool)> {
        self.append_thread_owner_record(
            expected_history,
            thread_id,
            client_id,
            body,
            None,
            attachments,
            mentions,
            artifact_ids,
            Some(request),
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn append_thread_owner_record(
        &self,
        expected_history: &str,
        thread_id: u64,
        client_id: &str,
        body: String,
        anchor: Option<u64>,
        attachments: &[String],
        mentions: &[u64],
        artifact_ids: &[u64],
        request: Option<&serde_json::Value>,
    ) -> anyhow::Result<(ChatMessage, bool)> {
        let artifact_ids = artifact_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            artifact_ids.len() <= 16,
            "a message may reference at most 16 distinct artifacts"
        );
        if let Some(request) = request {
            anyhow::ensure!(request.is_object(), "thread request must be an object");
            anyhow::ensure!(
                request.get("body").is_none_or(serde_json::Value::is_string),
                "thread request body must be text"
            );
        }
        let action = request
            .and_then(|r| r.get("thread_action"))
            .filter(|a| !a.is_null());
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(&tx, expected_history)?;
        threads::get(&tx, thread_id)?;
        if let Some(id) = tx
            .query_row(
                "SELECT msg_id FROM client_messages WHERE client_id=?1",
                [client_id],
                |r| r.get::<_, u64>(0),
            )
            .optional()?
        {
            let message = get_chat_message(&tx, id)?;
            anyhow::ensure!(
                message.artifact_ids == artifact_ids,
                "client_id artifact references changed"
            );
            anyhow::ensure!(
                message.thread_id == thread_id,
                "client_id belongs to another thread"
            );
            if let Some(action) = action {
                let receipt = tx
                    .query_row(
                        "SELECT payload FROM thread_action_receipts WHERE client_id=?1",
                        [client_id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?;
                anyhow::ensure!(
                    receipt
                        .as_deref()
                        .map(serde_json::from_str::<serde_json::Value>)
                        .transpose()?
                        .as_ref()
                        == Some(action),
                    "client action ID was already used with another payload"
                );
            }
            tx.commit()?;
            return Ok((message, false));
        }
        if let Some(anchor) = anchor {
            anyhow::ensure!(
                get_chat_message(&tx, anchor)?.thread_id == thread_id,
                "reply belongs to another thread"
            );
        }
        for mention in mentions {
            threads::get(&tx, *mention)
                .map_err(|error| anyhow::anyhow!("unknown mentioned Thread #{mention}: {error}"))?;
        }
        if let Some(action) = action {
            let expected = action
                .get("thread")
                .and_then(|t| t.get("revision"))
                .and_then(serde_json::Value::as_u64);
            let current = threads::get(&tx, thread_id)?;
            anyhow::ensure!(
                expected == Some(current.revision),
                "instrument changed; reload the thread before submitting this action"
            );
            anyhow::ensure!(
                current.settled_at.is_none() && current.archived_at.is_none(),
                "thread is no longer active"
            );
        }
        if let Some(action) = action {
            tx.execute(
                "INSERT INTO thread_action_receipts(client_id,payload) VALUES(?1,?2)",
                params![client_id, serde_json::to_string(action)?],
            )?;
            tx.execute(
                "UPDATE threads SET revision=revision+1,updated_at=?2 WHERE id=?1",
                params![thread_id, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        for artifact_id in &artifact_ids {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM artifacts WHERE id=?1)",
                [artifact_id],
                |r| r.get(0),
            )?;
            anyhow::ensure!(exists, "referenced artifact does not exist");
        }
        super::blobs::validate_blob_ids(&tx, attachments)?;
        tx.execute("INSERT INTO chat_messages(author,body,ref,ts,thread_id,mentions) VALUES('owner',?1,?2,?3,?4,?5)",params![body,anchor,chrono::Utc::now().to_rfc3339(),thread_id,serde_json::to_string(mentions)?])?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO client_messages(client_id,msg_id) VALUES(?1,?2)",
            params![client_id, id],
        )?;
        for (position, blob) in attachments.iter().enumerate() {
            tx.execute(
                "INSERT INTO message_attachments(message_id,blob_id,position) VALUES(?1,?2,?3)",
                params![id, blob, position as u64],
            )?;
        }
        for artifact_id in &artifact_ids {
            tx.execute(
                "INSERT INTO message_artifacts(message_id,artifact_id) VALUES(?1,?2)",
                params![id, artifact_id],
            )?;
        }
        let message = get_chat_message(&tx, id)?;
        if let Some(request) = request {
            let mut request = request.clone();
            request["history_id"] = serde_json::json!(expected_history);
            request["message_id"] = serde_json::json!(id);
            request["report_triggered"] = serde_json::json!(false);
            request["thread_id"] = serde_json::json!(thread_id);
            request["client_id"] = serde_json::json!(client_id);
            // Explicit skills are captured when accepted. Persist model input
            // atomically with the short visible message, never reread on recovery.
            if request.get("body").is_none() {
                request["body"] = serde_json::json!(message.body);
            }
            request["anchor"] = serde_json::json!(anchor);
            request["attachments"] =
                serde_json::to_value(super::blobs::message_attachments(&tx, id)?)?;

            tx.execute("INSERT INTO thread_turns(thread_id,owner_message_id,requester_thread_id,state,started_at) VALUES(?1,?2,(SELECT parent_thread_id FROM threads WHERE id=?1),'queued',?3)",params![thread_id,id,chrono::Utc::now().to_rfc3339()])?;
            let accepted_turn = tx.last_insert_rowid() as u64;
            super::thread_execution::capture(&tx, thread_id, accepted_turn, None)?;
            request["turn_id"] = serde_json::json!(accepted_turn);
            tx.execute(
                "INSERT INTO thread_requests(client_id,payload) VALUES(?1,?2)",
                params![client_id, serde_json::to_string(&request)?],
            )?;
        }
        tx.commit()?;
        Ok((message, true))
    }
    pub async fn append_thread_chat(
        &self,
        thread_id: u64,
        author: ChatAuthor,
        body: impl Into<String>,
        anchor: Option<u64>,
        tool_calls: Vec<ToolCallSummary>,
    ) -> anyhow::Result<ChatMessage> {
        let c = self.conn.lock().await;
        threads::get(&c, thread_id)?;
        if let Some(anchor) = anchor {
            anyhow::ensure!(
                get_chat_message(&c, anchor)?.thread_id == thread_id,
                "reply belongs to another thread"
            );
        }
        c.execute("INSERT INTO chat_messages(author,body,ref,ts,thread_id,tool_calls) VALUES(?1,?2,?3,?4,?5,?6)",params![author_to_str(author),body.into(),anchor,chrono::Utc::now().to_rfc3339(),thread_id,serde_json::to_string(&tool_calls)?])?;
        Ok(get_chat_message(&c, c.last_insert_rowid() as u64)?)
    }
    pub async fn thread_detail(
        &self,
        id: u64,
        before_id: Option<u64>,
        limit: u64,
    ) -> anyhow::Result<ThreadDetail> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let thread = threads::get(&tx, id)?;
        let mut messages=tx.prepare("SELECT id,author,body,ref,ts,tool_calls,thread_id,mentions FROM chat_messages WHERE thread_id=?1 AND id<?2 ORDER BY id DESC LIMIT ?3")?.query_map(params![id,before_id.unwrap_or(i64::MAX as u64).min(i64::MAX as u64),limit.clamp(1,100)],chat_message_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        load_attachments_for_messages(&tx, &mut messages)?;
        let has_more = match messages.first() {
            Some(m) => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE thread_id=?1 AND id<?2)",
                params![id, m.id],
                |r| r.get(0),
            )?,
            None => false,
        };
        let turns = super::thread_activity::turns(&tx, id)?;
        let activities = super::thread_activity::activities(&tx, id)?;
        let brief = super::thread_read::brief(&tx, id)?;
        let related_items = super::thread_related::list(&tx, id)?;
        tx.commit()?;
        Ok(ThreadDetail {
            related_items,
            brief,
            thread,
            messages,
            turns,
            activities,
            has_more,
        })
    }
}
impl Storage {
    pub async fn materialize_thread_reply(
        &self,
        turn_id: u64,
        body: impl Into<String>,
        anchor: Option<u64>,
        tool_calls: Vec<ToolCallSummary>,
    ) -> anyhow::Result<ChatMessage> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let turn = super::thread_activity::get(&tx, turn_id)?;
        if let Some(id) = turn.agent_message_id {
            let message = get_chat_message(&tx, id)?;
            tx.commit()?;
            return Ok(message);
        }
        if let Some(anchor) = anchor {
            anyhow::ensure!(
                get_chat_message(&tx, anchor)?.thread_id == turn.thread_id,
                "reply belongs to another thread"
            );
        }
        tx.execute("INSERT INTO chat_messages(author,body,ref,ts,thread_id,tool_calls) VALUES('agent',?1,?2,?3,?4,?5)",params![body.into(),anchor,chrono::Utc::now().to_rfc3339(),turn.thread_id,serde_json::to_string(&tool_calls)?])?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute(
            "UPDATE thread_turns SET agent_message_id=?2 WHERE id=?1",
            params![turn_id, id],
        )?;
        let message = get_chat_message(&tx, id)?;
        tx.commit()?;
        Ok(message)
    }
}
