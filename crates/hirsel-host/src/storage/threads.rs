use super::{Storage, common::parse_ts};
use chrono::{DateTime, Utc};
use hirsel_proto::{Thread, ThreadAttention};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(super) const COLUMNS: &str = "id,title,description,instrument,attention,settled_at,archived_at,snoozed_until,read,created_at,updated_at,revision,parent_thread_id,pinned_at,icon,showcased_artifact_id";
pub(super) fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    let parent_thread_id = r.get::<_, Option<u64>>(12)?;
    let time = |i| -> rusqlite::Result<Option<DateTime<Utc>>> {
        r.get::<_, Option<String>>(i)?
            .map(|s| parse_ts(&s))
            .transpose()
    };
    Ok(Thread {
        id: r.get(0)?,
        parent_thread_id,
        pinned_at: time(13)?,
        title: r.get(1)?,
        icon: r.get(14)?,
        showcased_artifact_id: r.get(15)?,
        description: r.get(2)?,
        instrument: serde_json::from_str(&r.get::<_, String>(3)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        attention: match r.get::<_, String>(4)?.as_str() {
            "needs_owner" => ThreadAttention::NeedsOwner,
            _ => ThreadAttention::Quiet,
        },
        settled_at: time(5)?,
        archived_at: time(6)?,
        snoozed_until: time(7)?,
        read: r.get(8)?,
        created_at: parse_ts(&r.get::<_, String>(9)?)?,
        updated_at: parse_ts(&r.get::<_, String>(10)?)?,
        revision: r.get(11)?,
        running_turn: None,
        queued_turn_count: 0,
        last_finished_turn: None,
        last_activity_at: parse_ts(&r.get::<_, String>(9)?)?,
    })
}
pub(super) fn get(conn: &Connection, id: u64) -> anyhow::Result<Thread> {
    let mut thread = conn.query_row(
        &format!("SELECT {COLUMNS} FROM threads WHERE id=?1"),
        [id],
        from_row,
    )?;
    super::thread_summary::populate(conn, &mut thread)?;
    Ok(thread)
}
pub(super) fn snapshot(conn: &Connection) -> anyhow::Result<Vec<Thread>> {
    let mut threads = conn
        .prepare(&format!("SELECT {COLUMNS} FROM threads ORDER BY id"))?
        .query_map([], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for thread in &mut threads {
        super::thread_summary::populate(conn, thread)?;
    }
    Ok(threads)
}
pub(super) fn attention(value: ThreadAttention) -> &'static str {
    match value {
        ThreadAttention::Quiet => "quiet",
        ThreadAttention::NeedsOwner => "needs_owner",
    }
}
pub(super) fn validate_instrument(instrument: &serde_json::Value) -> anyhow::Result<()> {
    // An empty instrument is valid when ordinary work has no controls yet.
    if instrument.is_null()
        || instrument
            .as_object()
            .is_some_and(|object| object.is_empty())
    {
        return Ok(());
    }
    crate::thread_instrument::validate(instrument)?;
    let mut pending = vec![instrument];
    while let Some(node) = pending.pop() {
        if let Some(nodes) = node.as_array() {
            pending.extend(nodes);
        }
        if let Some(object) = node.as_object() {
            if let Some(action) = object.get("action").and_then(serde_json::Value::as_str) {
                anyhow::ensure!(
                    !matches!(
                        action,
                        "set_icon"
                            | "set_showcase"
                            | "settle"
                            | "reopen"
                            | "read"
                            | "archive"
                            | "unarchive"
                            | "snooze"
                            | "unsnooze"
                            | "pin"
                            | "unpin"
                    ),
                    "instrument action `{action}` is reserved for Thread lifecycle commands"
                );
            }
            if let Some(children) = object.get("children").and_then(serde_json::Value::as_array) {
                pending.extend(children);
            }
        }
    }
    Ok(())
}

fn create_in_transaction(
    tx: &Transaction<'_>,
    client_id: &str,
    title: &str,
    description: &str,
    instrument: &serde_json::Value,
    needs: ThreadAttention,
    parent_thread_id: Option<u64>,
) -> anyhow::Result<(Thread, bool)> {
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM threads WHERE client_id=?1",
            [client_id],
            |r| r.get::<_, u64>(0),
        )
        .optional()?
    {
        let thread = get(tx, id)?;
        anyhow::ensure!(
            thread.parent_thread_id == parent_thread_id,
            "creation key belongs to another parent"
        );
        return Ok((thread, false));
    }
    if let Some(parent) = parent_thread_id {
        get(tx, parent)?;
    }
    let now = Utc::now().to_rfc3339();
    tx.execute("INSERT INTO threads(client_id,title,description,instrument,attention,read,created_at,updated_at,revision,parent_thread_id) VALUES(?1,?2,?3,?4,?5,0,?6,?6,1,?7)",params![client_id,title.trim(),description,serde_json::to_string(instrument)?,attention(needs),now,parent_thread_id])?;
    Ok((get(tx, tx.last_insert_rowid() as u64)?, true))
}

impl Storage {
    pub async fn thread(&self, id: u64) -> anyhow::Result<Option<Thread>> {
        let c = self.conn.lock().await;
        match get(&c, id) {
            Ok(t) => Ok(Some(t)),
            Err(e)
                if e.downcast_ref::<rusqlite::Error>()
                    .is_some_and(|e| matches!(e, rusqlite::Error::QueryReturnedNoRows)) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
    pub async fn thread_snapshot(&self) -> anyhow::Result<Vec<Thread>> {
        {
            let c = self.conn.lock().await;
            snapshot(&c)
        }
    }
    pub(crate) async fn addressed_thread(
        &self,
        expected_history: &str,
        id: u64,
    ) -> anyhow::Result<Thread> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(&c, expected_history)?;
        get(&c, id)
    }
    pub async fn create_thread(
        &self,
        client_id: &str,
        title: &str,
        description: &str,
        instrument: &serde_json::Value,
        needs: ThreadAttention,
        parent_thread_id: Option<u64>,
    ) -> anyhow::Result<(Thread, bool)> {
        validate_instrument(instrument)?;
        anyhow::ensure!(!client_id.is_empty(), "client_id must not be empty");
        anyhow::ensure!(!title.trim().is_empty(), "thread title must not be empty");
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let result = create_in_transaction(
            &tx,
            client_id,
            title,
            description,
            instrument,
            needs,
            parent_thread_id,
        )?;
        tx.commit()?;
        Ok(result)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_addressed_thread(
        &self,
        expected_history: &str,
        client_id: &str,
        title: &str,
        description: &str,
        instrument: &serde_json::Value,
        needs: ThreadAttention,
        parent_thread_id: Option<u64>,
    ) -> anyhow::Result<(Thread, bool)> {
        validate_instrument(instrument)?;
        anyhow::ensure!(!client_id.is_empty(), "client_id must not be empty");
        anyhow::ensure!(!title.trim().is_empty(), "thread title must not be empty");
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(&tx, expected_history)?;
        let result = create_in_transaction(
            &tx,
            client_id,
            title,
            description,
            instrument,
            needs,
            parent_thread_id,
        )?;
        tx.commit()?;
        Ok(result)
    }
    pub async fn update_thread(
        &self,
        id: u64,
        title: Option<&str>,
        description: Option<&str>,
        instrument: Option<&serde_json::Value>,
        needs: Option<ThreadAttention>,
    ) -> anyhow::Result<Thread> {
        if let Some(instrument) = instrument {
            validate_instrument(instrument)?;
        }
        if let Some(title) = title {
            anyhow::ensure!(!title.trim().is_empty(), "thread title must not be empty");
        }
        let c = self.conn.lock().await;
        get(&c, id)?;
        c.execute("UPDATE threads SET title=COALESCE(?2,title),description=COALESCE(?3,description),instrument=COALESCE(?4,instrument),attention=COALESCE(?5,attention),updated_at=?6,revision=revision+1,read=0 WHERE id=?1",params![id,title,description,instrument.map(serde_json::to_string).transpose()?,needs.map(attention),Utc::now().to_rfc3339()])?;
        get(&c, id)
    }
    async fn set_thread_field(
        &self,
        id: u64,
        column: &str,
        value: Option<String>,
    ) -> anyhow::Result<Thread> {
        let c = self.conn.lock().await;
        get(&c, id)?;
        c.execute(
            &format!(
                "UPDATE threads SET {column}=?2,updated_at=?3,revision=revision+1 WHERE id=?1"
            ),
            params![id, value, Utc::now().to_rfc3339()],
        )?;
        get(&c, id)
    }
    async fn set_addressed_thread_field(
        &self,
        expected_history: &str,
        id: u64,
        column: &str,
        value: Option<String>,
    ) -> anyhow::Result<Thread> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(&c, expected_history)?;
        get(&c, id)?;
        c.execute(
            &format!(
                "UPDATE threads SET {column}=?2,updated_at=?3,revision=revision+1 WHERE id=?1"
            ),
            params![id, value, Utc::now().to_rfc3339()],
        )?;
        get(&c, id)
    }
    pub(crate) async fn settle_addressed_thread(
        &self,
        expected_history: &str,
        id: u64,
        settled: bool,
    ) -> anyhow::Result<Thread> {
        self.set_addressed_thread_field(
            expected_history,
            id,
            "settled_at",
            settled.then(|| Utc::now().to_rfc3339()),
        )
        .await
    }
    pub(crate) async fn archive_addressed_thread(
        &self,
        expected_history: &str,
        id: u64,
        archived: bool,
    ) -> anyhow::Result<Thread> {
        self.set_addressed_thread_field(
            expected_history,
            id,
            "archived_at",
            archived.then(|| Utc::now().to_rfc3339()),
        )
        .await
    }
    pub(crate) async fn snooze_addressed_thread(
        &self,
        expected_history: &str,
        id: u64,
        until: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Thread> {
        self.set_addressed_thread_field(
            expected_history,
            id,
            "snoozed_until",
            until.map(|value| value.to_rfc3339()),
        )
        .await
    }
    pub async fn settle_thread(&self, id: u64, settled: bool) -> anyhow::Result<Thread> {
        self.set_thread_field(id, "settled_at", settled.then(|| Utc::now().to_rfc3339()))
            .await
    }
    pub async fn archive_thread(&self, id: u64, archived: bool) -> anyhow::Result<Thread> {
        self.set_thread_field(id, "archived_at", archived.then(|| Utc::now().to_rfc3339()))
            .await
    }
    pub async fn snooze_thread(
        &self,
        id: u64,
        until: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Thread> {
        self.set_thread_field(id, "snoozed_until", until.map(|v| v.to_rfc3339()))
            .await
    }
    pub async fn pin_thread(&self, id: u64, pinned: bool) -> anyhow::Result<Thread> {
        let c = self.conn.lock().await;
        let thread = get(&c, id)?;
        anyhow::ensure!(
            thread.parent_thread_id.is_none(),
            "only top-level Threads can be pinned or unpinned"
        );
        c.execute(
            "UPDATE threads SET pinned_at=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
            params![
                id,
                pinned.then(|| Utc::now().to_rfc3339()),
                Utc::now().to_rfc3339()
            ],
        )?;
        get(&c, id)
    }
    pub(crate) async fn pin_addressed_thread(
        &self,
        expected_history: &str,
        id: u64,
        pinned: bool,
    ) -> anyhow::Result<Thread> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(&c, expected_history)?;
        let thread = get(&c, id)?;
        anyhow::ensure!(
            thread.parent_thread_id.is_none(),
            "only top-level Threads can be pinned or unpinned"
        );
        c.execute(
            "UPDATE threads SET pinned_at=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
            params![
                id,
                pinned.then(|| Utc::now().to_rfc3339()),
                Utc::now().to_rfc3339()
            ],
        )?;
        get(&c, id)
    }
    pub async fn mark_thread_read(&self, id: u64) -> anyhow::Result<Thread> {
        self.set_thread_field(id, "read", Some("1".into())).await
    }
    pub(crate) async fn mark_addressed_thread_read(
        &self,
        expected_history: &str,
        id: u64,
    ) -> anyhow::Result<Thread> {
        self.set_addressed_thread_field(expected_history, id, "read", Some("1".into()))
            .await
    }
}

#[cfg(test)]
#[path = "thread_pins_tests.rs"]
mod pin_tests;
