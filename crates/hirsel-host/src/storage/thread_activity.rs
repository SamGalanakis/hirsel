use super::{Storage, common::parse_ts, threads};
use hirsel_proto::{ThreadActivity, ThreadTurn, ThreadTurnState};
use rusqlite::{Connection, OptionalExtension, params};
fn turn_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadTurn> {
    let state: String = r.get(4)?;
    Ok(ThreadTurn {
        id: r.get(0)?,
        thread_id: r.get(1)?,
        requester_thread_id: r.get(7)?,
        requester_turn_id: r.get(8)?,
        owner_message_id: r.get(2)?,
        agent_message_id: r.get(3)?,
        state: serde_json::from_value(serde_json::Value::String(state)).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        started_at: parse_ts(&r.get::<_, String>(5)?)?,
        finished_at: r
            .get::<_, Option<String>>(6)?
            .map(|s| parse_ts(&s))
            .transpose()?,
    })
}
pub(super) fn get(c: &Connection, id: u64) -> anyhow::Result<ThreadTurn> {
    Ok(c.query_row("SELECT id,thread_id,owner_message_id,agent_message_id,state,started_at,finished_at,requester_thread_id,requester_turn_id FROM thread_turns WHERE id=?1",[id],turn_row)?)
}
pub(super) fn turns(c: &Connection, id: u64) -> anyhow::Result<Vec<ThreadTurn>> {
    Ok(c.prepare("SELECT id,thread_id,owner_message_id,agent_message_id,state,started_at,finished_at,requester_thread_id,requester_turn_id FROM thread_turns WHERE thread_id=?1 ORDER BY id")?.query_map([id],turn_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
}
fn activity_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadActivity> {
    Ok(ThreadActivity {
        id: r.get(0)?,
        thread_id: r.get(1)?,
        turn_id: r.get(2)?,
        artifact_ids: serde_json::from_str(&r.get::<_, String>(6)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?,
        kind: r.get(3)?,
        data: serde_json::from_str(&r.get::<_, String>(4)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        ts: parse_ts(&r.get::<_, String>(5)?)?,
    })
}
pub(super) fn activities(c: &Connection, id: u64) -> anyhow::Result<Vec<ThreadActivity>> {
    Ok(c.prepare("SELECT id,thread_id,turn_id,kind,data,ts,(SELECT json_group_array(artifact_id) FROM activity_artifacts WHERE activity_id=thread_activities.id) FROM thread_activities WHERE thread_id=?1 ORDER BY id")?.query_map([id],activity_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
}
impl Storage {
    /// Accept a background wake and its owning turn together. The activity key
    /// remains after delivery, so a retried wake cannot create another turn.
    pub async fn queue_background_thread_request(
        &self,
        client_id: &str,
        thread_id: u64,
        payload: &serde_json::Value,
    ) -> anyhow::Result<ThreadTurn> {
        anyhow::ensure!(payload.is_object(), "background request must be an object");
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(
            &tx,
            payload["history_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("background history is required"))?,
        )?;
        threads::get(&tx, thread_id)?;
        let key = format!("background-request:{client_id}");
        if let Some(id) = tx.query_row(
            "SELECT a.turn_id FROM thread_activity_keys k JOIN thread_activities a ON a.id=k.activity_id WHERE k.key=?1",
            [&key], |r| r.get::<_,u64>(0),
        ).optional()? {
            let turn = get(&tx, id)?;
            anyhow::ensure!(turn.thread_id == thread_id, "background request belongs to another thread");
            tx.commit()?;
            return Ok(turn);
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO thread_turns(thread_id,state,started_at,requester_thread_id) VALUES(?1,'queued',?2,(SELECT parent_thread_id FROM threads WHERE id=?1))",
            params![thread_id, now],
        )?;
        let turn_id = tx.last_insert_rowid() as u64;
        super::thread_execution::capture(&tx, thread_id, turn_id, None)?;
        let mut payload = payload.clone();
        payload["turn_id"] = serde_json::json!(turn_id);
        tx.execute(
            "INSERT INTO thread_requests(client_id,payload) VALUES(?1,?2)",
            params![client_id, serde_json::to_string(&payload)?],
        )?;
        tx.execute("INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,'background_queued','{}',?3)",params![thread_id,turn_id,now])?;
        tx.execute(
            "INSERT INTO thread_activity_keys(key,activity_id) VALUES(?1,?2)",
            params![key, tx.last_insert_rowid()],
        )?;
        let turn = get(&tx, turn_id)?;
        tx.commit()?;
        Ok(turn)
    }

    pub async fn queue_thread_turn(
        &self,
        thread_id: u64,
        owner_message_id: Option<u64>,
    ) -> anyhow::Result<ThreadTurn> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        threads::get(&tx, thread_id)?;
        if let Some(mid) = owner_message_id {
            anyhow::ensure!(
                super::chat::get_chat_message(&tx, mid)?.thread_id == thread_id,
                "turn message belongs to another thread"
            );
            if let Some(id) = tx
                .query_row(
                    "SELECT id FROM thread_turns WHERE owner_message_id=?1",
                    [mid],
                    |r| r.get::<_, u64>(0),
                )
                .optional()?
            {
                let t = get(&tx, id)?;
                tx.commit()?;
                return Ok(t);
            }
        }
        tx.execute("INSERT INTO thread_turns(thread_id,owner_message_id,state,started_at,requester_thread_id) VALUES(?1,?2,'queued',?3,(SELECT parent_thread_id FROM threads WHERE id=?1))",params![thread_id,owner_message_id,chrono::Utc::now().to_rfc3339()])?;
        let id = tx.last_insert_rowid() as u64;
        super::thread_execution::capture(&tx, thread_id, id, None)?;
        let t = get(&tx, id)?;
        tx.commit()?;
        Ok(t)
    }
    pub async fn run_thread_turn(&self, id: u64) -> anyhow::Result<ThreadTurn> {
        let c = self.conn.lock().await;
        let t = get(&c, id)?;
        if matches!(t.state, ThreadTurnState::Queued) {
            c.execute(
                "UPDATE thread_turns SET state='running',started_at=?2 WHERE id=?1",
                params![id, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        get(&c, id)
    }
    pub async fn start_thread_turn(
        &self,
        thread_id: u64,
        owner_message_id: Option<u64>,
    ) -> anyhow::Result<ThreadTurn> {
        let c = self.conn.lock().await;
        threads::get(&c, thread_id)?;
        if let Some(mid) = owner_message_id {
            anyhow::ensure!(
                super::chat::get_chat_message(&c, mid)?.thread_id == thread_id,
                "turn message belongs to another thread"
            );
            if let Some(id) = c
                .query_row(
                    "SELECT id FROM thread_turns WHERE owner_message_id=?1",
                    [mid],
                    |r| r.get::<_, u64>(0),
                )
                .optional()?
            {
                return get(&c, id);
            }
        }
        c.execute("INSERT INTO thread_turns(thread_id,owner_message_id,state,started_at,requester_thread_id) VALUES(?1,?2,'running',?3,(SELECT parent_thread_id FROM threads WHERE id=?1))",params![thread_id,owner_message_id,chrono::Utc::now().to_rfc3339()])?;
        let id = c.last_insert_rowid() as u64;
        super::thread_execution::capture(&c, thread_id, id, None)?;
        get(&c, id)
    }
    pub async fn finish_thread_turn(
        &self,
        id: u64,
        state: ThreadTurnState,
        agent_message_id: Option<u64>,
    ) -> anyhow::Result<ThreadTurn> {
        anyhow::ensure!(
            !matches!(state, ThreadTurnState::Queued | ThreadTurnState::Running),
            "finish requires terminal state"
        );
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let turn = finish(&tx, id, state, agent_message_id)?;
        tx.commit()?;
        Ok(turn)
    }
    pub async fn interrupt_unfinished_thread_turns(&self) -> anyhow::Result<Vec<ThreadTurn>> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let ids = tx
            .prepare("SELECT id FROM thread_turns WHERE state='running'")?
            .query_map([], |r| r.get::<_, u64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut turns = Vec::with_capacity(ids.len());
        for id in ids {
            let (thread_id, config): (u64, Option<String>) = tx.query_row(
                "SELECT t.thread_id,e.config FROM thread_turns t LEFT JOIN thread_turn_execution e ON e.turn_id=t.id WHERE t.id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if config
                .as_deref()
                .map(serde_json::from_str::<super::ThreadExecution>)
                .transpose()?
                .is_some_and(|execution| {
                    matches!(execution, super::ThreadExecution::LashWorker { .. })
                })
            {
                // A direct durable Lash turn may have accepted input or begun a
                // shell effect before the Host stopped. Abandon this session
                // generation so a later follow-up cannot drive that uncertain
                // pending input as if it were new work.
                let key = format!("thread:{thread_id}:native_worker_fingerprint");
                let value = format!("interrupted-turn:{id}");
                tx.execute(
                    "INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, value],
                )?;
            }
            turns.push(finish(&tx, id, ThreadTurnState::Interrupted, None)?);
        }
        tx.commit()?;
        Ok(turns)
    }
    pub async fn append_thread_activity(
        &self,
        thread_id: u64,
        turn_id: Option<u64>,
        kind: &str,
        data: &serde_json::Value,
    ) -> anyhow::Result<ThreadActivity> {
        let c = self.conn.lock().await;
        threads::get(&c, thread_id)?;
        if let Some(id) = turn_id {
            anyhow::ensure!(
                get(&c, id)?.thread_id == thread_id,
                "activity turn belongs to another thread"
            );
        }
        c.execute(
            "INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,?3,?4,?5)",
            params![
                thread_id,
                turn_id,
                kind,
                serde_json::to_string(data)?,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(c.query_row(
            "SELECT id,thread_id,turn_id,kind,data,ts,(SELECT json_group_array(artifact_id) FROM activity_artifacts WHERE activity_id=thread_activities.id) FROM thread_activities WHERE id=?1",
            [c.last_insert_rowid()],
            activity_row,
        )?)
    }
}
impl Storage {
    pub async fn append_thread_activity_once(
        &self,
        key: &str,
        thread_id: u64,
        turn_id: Option<u64>,
        kind: &str,
        data: &serde_json::Value,
    ) -> anyhow::Result<ThreadActivity> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        if let Some(id) = tx
            .query_row(
                "SELECT activity_id FROM thread_activity_keys WHERE key=?1",
                [key],
                |r| r.get::<_, u64>(0),
            )
            .optional()?
        {
            let activity = tx.query_row(
                "SELECT id,thread_id,turn_id,kind,data,ts,(SELECT json_group_array(artifact_id) FROM activity_artifacts WHERE activity_id=thread_activities.id) FROM thread_activities WHERE id=?1",
                [id],
                activity_row,
            )?;
            anyhow::ensure!(
                activity.thread_id == thread_id && activity.turn_id == turn_id,
                "activity replay key belongs to another turn"
            );
            tx.commit()?;
            return Ok(activity);
        }
        threads::get(&tx, thread_id)?;
        if let Some(id) = turn_id {
            anyhow::ensure!(
                get(&tx, id)?.thread_id == thread_id,
                "activity turn belongs to another thread"
            );
        }
        tx.execute(
            "INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,?3,?4,?5)",
            params![
                thread_id,
                turn_id,
                kind,
                serde_json::to_string(data)?,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO thread_activity_keys(key,activity_id) VALUES(?1,?2)",
            params![key, id],
        )?;
        let activity = tx.query_row(
            "SELECT id,thread_id,turn_id,kind,data,ts,(SELECT json_group_array(artifact_id) FROM activity_artifacts WHERE activity_id=thread_activities.id) FROM thread_activities WHERE id=?1",
            [id],
            activity_row,
        )?;
        tx.commit()?;
        Ok(activity)
    }
}

pub(super) fn finish(
    c: &Connection,
    id: u64,
    state: ThreadTurnState,
    agent_message_id: Option<u64>,
) -> anyhow::Result<ThreadTurn> {
    let previous = get(c, id)?;
    if previous.finished_at.is_some() {
        return Ok(previous);
    }
    if let Some(mid) = agent_message_id {
        anyhow::ensure!(
            super::chat::get_chat_message(c, mid)?.thread_id == previous.thread_id,
            "turn reply belongs to another thread"
        );
    }
    let status = serde_json::to_value(state)?;
    c.execute("UPDATE thread_turns SET state=?2,agent_message_id=COALESCE(?3,agent_message_id),finished_at=?4 WHERE id=?1",params![id,status.as_str(),agent_message_id,chrono::Utc::now().to_rfc3339()])?;
    let turn = get(c, id)?;
    if turn.requester_thread_id.is_some() {
        let message = turn
            .agent_message_id
            .map(|id| super::chat::get_chat_message(c, id))
            .transpose()?;
        let summary = message
            .as_ref()
            .map(|m| m.body.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(8000).collect::<String>())
            .unwrap_or_else(|| {
                format!(
                    "Child execution {} without a final assistant message.",
                    status.as_str().unwrap_or("ended")
                )
            });
        let refs=c.prepare("SELECT artifact_id FROM turn_output_artifacts WHERE turn_id=?1 ORDER BY artifact_id")?.query_map([id],|r|r.get::<_,u64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        super::thread_delegation::report(
            c,
            &turn,
            "terminal",
            status.as_str().expect("turn state string"),
            &summary,
            &refs,
        )?;
    }
    Ok(turn)
}

impl Storage {
    pub async fn thread_turn(&self, id: u64) -> anyhow::Result<ThreadTurn> {
        let c = self.conn.lock().await;
        get(&c, id)
    }
}

pub(super) fn activity(c: &Connection, id: u64) -> anyhow::Result<ThreadActivity> {
    Ok(c.query_row("SELECT id,thread_id,turn_id,kind,data,ts,(SELECT json_group_array(artifact_id) FROM activity_artifacts WHERE activity_id=thread_activities.id) FROM thread_activities WHERE id=?1",[id],activity_row)?)
}
