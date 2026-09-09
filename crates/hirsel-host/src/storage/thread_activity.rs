use super::{Storage, common::parse_ts, threads};
use hirsel_proto::{ThreadActivity, ThreadTurn, ThreadTurnState};
use rusqlite::{Connection, OptionalExtension, params};
fn turn_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadTurn> {
    let state: String = r.get(4)?;
    Ok(ThreadTurn {
        id: r.get(0)?,
        thread_id: r.get(1)?,
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
    Ok(c.query_row("SELECT id,thread_id,owner_message_id,agent_message_id,state,started_at,finished_at FROM thread_turns WHERE id=?1",[id],turn_row)?)
}
pub(super) fn turns(c: &Connection, id: u64) -> anyhow::Result<Vec<ThreadTurn>> {
    Ok(c.prepare("SELECT id,thread_id,owner_message_id,agent_message_id,state,started_at,finished_at FROM thread_turns WHERE thread_id=?1 ORDER BY id")?.query_map([id],turn_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
}
fn activity_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadActivity> {
    Ok(ThreadActivity {
        id: r.get(0)?,
        thread_id: r.get(1)?,
        turn_id: r.get(2)?,
        kind: r.get(3)?,
        data: serde_json::from_str(&r.get::<_, String>(4)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        ts: parse_ts(&r.get::<_, String>(5)?)?,
    })
}
pub(super) fn activities(c: &Connection, id: u64) -> anyhow::Result<Vec<ThreadActivity>> {
    Ok(c.prepare("SELECT id,thread_id,turn_id,kind,data,ts FROM thread_activities WHERE thread_id=?1 ORDER BY id")?.query_map([id],activity_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
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
            "INSERT INTO thread_turns(thread_id,state,started_at) VALUES(?1,'queued',?2)",
            params![thread_id, now],
        )?;
        let turn_id = tx.last_insert_rowid() as u64;
        let mut payload = payload.clone();
        payload["_thread_turn_id"] = serde_json::json!(turn_id);
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
        tx.execute("INSERT INTO thread_turns(thread_id,owner_message_id,state,started_at) VALUES(?1,?2,'queued',?3)",params![thread_id,owner_message_id,chrono::Utc::now().to_rfc3339()])?;
        let t = get(&tx, tx.last_insert_rowid() as u64)?;
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
        c.execute("INSERT INTO thread_turns(thread_id,owner_message_id,state,started_at) VALUES(?1,?2,'running',?3)",params![thread_id,owner_message_id,chrono::Utc::now().to_rfc3339()])?;
        get(&c, c.last_insert_rowid() as u64)
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
        let c = self.conn.lock().await;
        let previous = get(&c, id)?;
        if previous.finished_at.is_some() {
            return Ok(previous);
        }
        if let Some(mid) = agent_message_id {
            anyhow::ensure!(
                super::chat::get_chat_message(&c, mid)?.thread_id == previous.thread_id,
                "turn reply belongs to another thread"
            );
        }
        let state = serde_json::to_value(state)?;
        c.execute("UPDATE thread_turns SET state=?2,agent_message_id=COALESCE(?3,agent_message_id),finished_at=?4 WHERE id=?1",params![id,state.as_str(),agent_message_id,chrono::Utc::now().to_rfc3339()])?;
        get(&c, id)
    }
    pub async fn interrupt_unfinished_thread_turns(&self) -> anyhow::Result<Vec<ThreadTurn>> {
        let c = self.conn.lock().await;
        let ids = c
            .prepare("SELECT id FROM thread_turns WHERE state='running'")?
            .query_map([], |r| r.get::<_, u64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        c.execute(
            "UPDATE thread_turns SET state='interrupted',finished_at=?1 WHERE state='running'",
            [chrono::Utc::now().to_rfc3339()],
        )?;
        ids.into_iter().map(|id| get(&c, id)).collect()
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
            "SELECT id,thread_id,turn_id,kind,data,ts FROM thread_activities WHERE id=?1",
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
                "SELECT id,thread_id,turn_id,kind,data,ts FROM thread_activities WHERE id=?1",
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
            "SELECT id,thread_id,turn_id,kind,data,ts FROM thread_activities WHERE id=?1",
            [id],
            activity_row,
        )?;
        tx.commit()?;
        Ok(activity)
    }
}
