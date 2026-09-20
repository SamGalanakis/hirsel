use super::{Storage, common::parse_ts};
use chrono::{DateTime, Utc};
use hirsel_proto::{Thread, ThreadAttention, ThreadIcon, ThreadKind, ThreadTint};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ThreadPublication {
    history_id: String,
    thread: Thread,
    ancestors: Vec<Thread>,
}

impl ThreadPublication {
    pub(crate) fn history_id(&self) -> &str {
        &self.history_id
    }

    pub(crate) fn thread(&self) -> &Thread {
        &self.thread
    }

    pub(crate) fn ancestors(&self) -> &[Thread] {
        &self.ancestors
    }

    #[cfg(test)]
    pub(crate) fn test(history_id: String, thread: Thread) -> Self {
        Self {
            history_id,
            thread,
            ancestors: Vec::new(),
        }
    }
}

pub(super) const COLUMNS: &str = "id,title,description,instrument,attention,settled_at,archived_at,snoozed_until,read,created_at,updated_at,revision,parent_thread_id,pinned_at,icon_symbol,icon_blob_id,showcased_artifact_id,kind,icon_tint,own_headline,headline,previous_headline,headline_revision,last_seen_headline_revision";
pub(super) fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    let parent_thread_id = r.get::<_, Option<u64>>(12)?;
    let time = |i| -> rusqlite::Result<Option<DateTime<Utc>>> {
        r.get::<_, Option<String>>(i)?
            .map(|s| parse_ts(&s))
            .transpose()
    };
    Ok(Thread {
        id: r.get(0)?,
        kind: {
            let kind = r.get::<_, String>(17)?;
            parse_kind(&kind).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    17,
                    rusqlite::types::Type::Text,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid Thread kind `{kind}`"),
                    )
                    .into(),
                )
            })?
        },
        parent_thread_id,
        pinned_at: time(13)?,
        title: r.get(1)?,
        icon: match (
            r.get::<_, Option<String>>(14)?,
            r.get::<_, Option<String>>(15)?,
        ) {
            (Some(name), None) => Some(ThreadIcon::Symbol {
                name,
                tint: r
                    .get::<_, Option<String>>(18)?
                    .as_deref()
                    .and_then(ThreadTint::parse)
                    .unwrap_or_default(),
            }),
            (None, Some(blob_id)) => Some(ThreadIcon::Image { blob_id }),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    14,
                    rusqlite::types::Type::Text,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Thread has both symbol and image icons",
                    )
                    .into(),
                ));
            }
        },
        showcased_artifact_id: r.get(16)?,
        description: r.get(2)?,
        execution: None,
        instrument: r
            .get::<_, Option<String>>(3)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        attention: match r.get::<_, String>(4)?.as_str() {
            "needs_owner" => ThreadAttention::NeedsOwner,
            _ => ThreadAttention::Quiet,
        },
        settled_at: time(5)?,
        archived_at: time(6)?,
        snoozed_until: time(7)?,
        read: r.get(8)?,
        own_headline: r.get(19)?,
        headline: r.get(20)?,
        previous_headline: r.get(21)?,
        headline_revision: r.get(22)?,
        last_seen_headline_revision: r.get(23)?,
        created_at: parse_ts(&r.get::<_, String>(9)?)?,
        updated_at: parse_ts(&r.get::<_, String>(10)?)?,
        revision: r.get(11)?,
        running_turn: None,
        queued_turn_count: 0,
        last_finished_turn: None,
        last_activity_at: parse_ts(&r.get::<_, String>(9)?)?,
        status: hirsel_proto::ThreadStatus {
            kind: hirsel_proto::ThreadStatusKind::Idle,
            reason: "No work is active".into(),
        },
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

fn publication(
    conn: &Connection,
    history_id: String,
    id: u64,
) -> anyhow::Result<ThreadPublication> {
    let thread = get(conn, id)?;
    let mut parent = thread.parent_thread_id;
    let mut ancestors = Vec::new();
    while let Some(id) = parent {
        anyhow::ensure!(ancestors.len() < 64, "Thread ancestry exceeds 64 levels");
        let ancestor = get(conn, id)?;
        parent = ancestor.parent_thread_id;
        ancestors.push(ancestor);
    }
    Ok(ThreadPublication {
        history_id,
        thread,
        ancestors,
    })
}
pub(super) fn attention(value: ThreadAttention) -> &'static str {
    match value {
        ThreadAttention::Quiet => "quiet",
        ThreadAttention::NeedsOwner => "needs_owner",
    }
}
/// The inverse of [`kind_name`]. `None` for a value no release ever wrote.
pub(super) fn parse_kind(value: &str) -> Option<ThreadKind> {
    match value {
        "space" => Some(ThreadKind::Space),
        "task" => Some(ThreadKind::Task),
        _ => None,
    }
}
pub(super) fn kind_name(value: ThreadKind) -> &'static str {
    match value {
        ThreadKind::Space => "space",
        ThreadKind::Task => "task",
    }
}
pub(super) fn validate_instrument(instrument: Option<&serde_json::Value>) -> anyhow::Result<()> {
    let Some(instrument) = instrument else {
        return Ok(());
    };
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
                            | "set_kind"
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

#[allow(clippy::too_many_arguments)]
fn create_in_transaction(
    tx: &Transaction<'_>,
    client_id: &str,
    title: &str,
    description: &str,
    instrument: Option<&serde_json::Value>,
    needs: ThreadAttention,
    kind: ThreadKind,
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
        anyhow::ensure!(thread.kind == kind, "creation key belongs to another kind");
        return Ok((thread, false));
    }
    if let Some(parent) = parent_thread_id {
        get(tx, parent)?;
    }
    let now = Utc::now().to_rfc3339();
    tx.execute("INSERT INTO threads(client_id,kind,title,description,instrument,attention,read,created_at,updated_at,revision,parent_thread_id) VALUES(?1,?2,?3,?4,?5,?6,0,?7,?7,1,?8)",params![client_id,kind_name(kind),title.trim(),description,instrument.map(serde_json::to_string).transpose()?,attention(needs),now,parent_thread_id])?;
    let created = get(tx, tx.last_insert_rowid() as u64)?;
    crate::thread_rollups::refresh_from(tx, created.id)?;
    Ok((created, true))
}

/// The Owner and the Agent share one bound for a Thread's own text, so an
/// Owner edit can never fail on something an Agent was allowed to write.
pub(crate) const MAX_THREAD_TITLE_CHARS: usize = 200;
pub(crate) const MAX_THREAD_DESCRIPTION_CHARS: usize = 20_000;
pub(crate) const MAX_HEADLINE_BYTES: usize = 240;
pub(crate) fn normalize_headline(headline: &str) -> String {
    headline
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub(crate) fn validate_headline(headline: &str) -> anyhow::Result<String> {
    let normalized = normalize_headline(headline);
    anyhow::ensure!(!normalized.is_empty(), "headline must not be empty");
    anyhow::ensure!(
        normalized.split(' ').count() <= 12,
        "headline must contain at most 12 words"
    );
    anyhow::ensure!(
        normalized.len() <= MAX_HEADLINE_BYTES,
        "headline must contain at most {MAX_HEADLINE_BYTES} bytes"
    );
    Ok(normalized)
}
pub(crate) fn set_headline(c: &Connection, id: u64, headline: &str) -> anyhow::Result<Vec<u64>> {
    let headline = validate_headline(headline)?;
    get(c, id)?;
    let has_children: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM threads WHERE parent_thread_id=?1 AND archived_at IS NULL)",
        [id],
        |r| r.get(0),
    )?;
    c.execute("UPDATE threads SET own_headline=?2,previous_headline=headline,headline=CASE WHEN ?3 THEN headline ELSE ?2 END,headline_revision=headline_revision+1,updated_at=?4,revision=revision+1,read=0 WHERE id=?1", params![id, headline, has_children, Utc::now().to_rfc3339()])?;
    let mut changed = vec![id];
    if has_children {
        changed.extend(crate::thread_rollups::refresh_self_and_ancestors(c, id)?);
    } else {
        changed.extend(crate::thread_rollups::refresh_from(c, id)?);
    }
    changed.sort_unstable();
    changed.dedup();
    Ok(changed)
}
pub(crate) fn validate_thread_title(title: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!title.trim().is_empty(), "thread title must not be empty");
    anyhow::ensure!(
        title.chars().count() <= MAX_THREAD_TITLE_CHARS,
        "thread title must be at most {MAX_THREAD_TITLE_CHARS} characters"
    );
    Ok(())
}
pub(crate) fn validate_thread_description(description: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        description.chars().count() <= MAX_THREAD_DESCRIPTION_CHARS,
        "thread description must be at most {MAX_THREAD_DESCRIPTION_CHARS} characters"
    );
    Ok(())
}

impl Storage {
    pub(crate) async fn mark_headlines_seen(
        &self,
        expected_history: &str,
        ids: &[u64],
    ) -> anyhow::Result<Vec<Thread>> {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        anyhow::ensure!(
            ids.len() <= 500,
            "at most 500 displayed Threads may be marked seen"
        );
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(&tx, expected_history)?;
        for id in &ids {
            anyhow::ensure!(*id > 0, "Thread IDs must be positive");
            get(&tx, *id)?;
            tx.execute(
                "UPDATE threads SET last_seen_headline_revision=headline_revision WHERE id=?1",
                [id],
            )?;
        }
        let result = ids
            .into_iter()
            .map(|id| get(&tx, id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(result)
    }

    pub(crate) async fn current_thread_publication(
        &self,
        id: u64,
    ) -> anyhow::Result<(tokio::sync::MutexGuard<'_, Connection>, ThreadPublication)> {
        let c = self.conn.lock().await;
        let publication = publication(&c, super::schema::read_history_id(&c)?, id)?;
        Ok((c, publication))
    }

    pub(crate) async fn checked_thread_publication(
        &self,
        expected_history: &str,
        expected: &Thread,
    ) -> anyhow::Result<(tokio::sync::MutexGuard<'_, Connection>, ThreadPublication)> {
        let guard = self.conn.lock().await;
        super::thread_scope::validate_history(&guard, expected_history)?;
        let publication = publication(&guard, expected_history.to_owned(), expected.id)?;
        anyhow::ensure!(
            publication.thread == *expected,
            "Thread publication snapshot is stale"
        );
        Ok((guard, publication))
    }

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
    #[allow(clippy::too_many_arguments)]
    pub async fn create_thread(
        &self,
        client_id: &str,
        title: &str,
        description: &str,
        instrument: Option<&serde_json::Value>,
        needs: ThreadAttention,
        kind: ThreadKind,
        parent_thread_id: Option<u64>,
    ) -> anyhow::Result<(Thread, bool)> {
        validate_instrument(instrument)?;
        anyhow::ensure!(!client_id.is_empty(), "client_id must not be empty");
        validate_thread_title(title)?;
        validate_thread_description(description)?;
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let result = create_in_transaction(
            &tx,
            client_id,
            title,
            description,
            instrument,
            needs,
            kind,
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
        instrument: Option<&serde_json::Value>,
        needs: ThreadAttention,
        kind: ThreadKind,
        parent_thread_id: Option<u64>,
    ) -> anyhow::Result<(Thread, bool)> {
        validate_instrument(instrument)?;
        anyhow::ensure!(!client_id.is_empty(), "client_id must not be empty");
        validate_thread_title(title)?;
        validate_thread_description(description)?;
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
            kind,
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
        instrument: Option<Option<&serde_json::Value>>,
        needs: Option<ThreadAttention>,
    ) -> anyhow::Result<Thread> {
        if let Some(instrument) = instrument {
            validate_instrument(instrument)?;
        }
        if let Some(title) = title {
            validate_thread_title(title)?;
        }
        if let Some(description) = description {
            validate_thread_description(description)?;
        }
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        get(&tx, id)?;
        tx.execute("UPDATE threads SET title=COALESCE(?2,title),description=COALESCE(?3,description),instrument=CASE WHEN ?7 THEN ?4 ELSE instrument END,attention=COALESCE(?5,attention),updated_at=?6,revision=revision+1,read=0 WHERE id=?1",params![id,title,description,instrument.flatten().map(serde_json::to_string).transpose()?,needs.map(attention),Utc::now().to_rfc3339(),instrument.is_some()])?;
        if needs.is_some() {
            crate::thread_rollups::refresh_from(&tx, id)?;
        }
        let updated = get(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }
    /// The Owner's own title/description edit: revision-fenced exactly like an
    /// icon edit, and never marking the Thread unread — the Owner wrote it.
    pub(crate) async fn update_addressed_thread_text(
        &self,
        expected_history: &str,
        id: u64,
        title: Option<&str>,
        description: Option<&str>,
        expected_revision: u64,
    ) -> anyhow::Result<Thread> {
        if let Some(title) = title {
            validate_thread_title(title)?;
        }
        if let Some(description) = description {
            validate_thread_description(description)?;
        }
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(&c, expected_history)?;
        let current = get(&c, id)?;
        anyhow::ensure!(
            current.revision == expected_revision,
            "thread changed; reload before updating it"
        );
        c.execute(
            "UPDATE threads SET title=COALESCE(?2,title),description=COALESCE(?3,description),updated_at=?4,revision=revision+1 WHERE id=?1",
            params![id, title.map(str::trim), description, Utc::now().to_rfc3339()],
        )?;
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
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(&tx, expected_history)?;
        let thread = get(&tx, id)?;
        anyhow::ensure!(
            thread.kind == ThreadKind::Task,
            "only Tasks can be completed or reopened"
        );
        tx.execute(
            "UPDATE threads SET settled_at=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
            params![
                id,
                settled.then(|| Utc::now().to_rfc3339()),
                Utc::now().to_rfc3339()
            ],
        )?;
        crate::thread_rollups::refresh_from(&tx, id)?;
        let updated = get(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }
    /// The Owner archive action. It runs the same [`super::thread_archive`]
    /// path the agent's `threads.archive` tool runs: subtree, cancellation,
    /// attention and the single activity row are identical by construction.
    pub(crate) async fn archive_addressed_thread(
        &self,
        expected_history: &str,
        id: u64,
        archived: bool,
    ) -> anyhow::Result<super::ArchiveOutcome> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(&tx, expected_history)?;
        let outcome = super::thread_archive::apply(
            &tx,
            id,
            archived,
            &super::thread_archive::ArchiveActor {
                thread_id: id,
                turn_id: None,
                actor: "owner",
                keep_turn_id: None,
            },
        )?;
        tx.commit()?;
        Ok(outcome)
    }
    pub(crate) async fn snooze_addressed_thread(
        &self,
        expected_history: &str,
        id: u64,
        until: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Thread> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(&tx, expected_history)?;
        get(&tx, id)?;
        tx.execute(
            "UPDATE threads SET snoozed_until=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
            params![
                id,
                until.map(|value| value.to_rfc3339()),
                Utc::now().to_rfc3339()
            ],
        )?;
        crate::thread_rollups::refresh_from(&tx, id)?;
        let updated = get(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }
    #[cfg(test)]
    pub(crate) async fn settle_thread(&self, id: u64, settled: bool) -> anyhow::Result<Thread> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        let thread = get(&tx, id)?;
        anyhow::ensure!(
            thread.kind == ThreadKind::Task,
            "only Tasks can be completed or reopened"
        );
        tx.execute(
            "UPDATE threads SET settled_at=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
            params![
                id,
                settled.then(|| Utc::now().to_rfc3339()),
                Utc::now().to_rfc3339()
            ],
        )?;
        crate::thread_rollups::refresh_from(&tx, id)?;
        let updated = get(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub(crate) async fn set_addressed_thread_kind(
        &self,
        expected_history: &str,
        id: u64,
        new_kind: ThreadKind,
        expected_revision: u64,
    ) -> anyhow::Result<Thread> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::thread_scope::validate_history(&tx, expected_history)?;
        let current = get(&tx, id)?;
        anyhow::ensure!(
            current.revision == expected_revision,
            "Thread changed; reload it before changing its kind"
        );
        if current.kind == new_kind {
            tx.commit()?;
            return Ok(current);
        }
        if new_kind == ThreadKind::Space {
            anyhow::ensure!(
                current.settled_at.is_none(),
                "reopen a settled Task before converting it to a Space"
            );
            if let Some(parent_id) = current.parent_thread_id {
                anyhow::ensure!(
                    get(&tx, parent_id)?.kind == ThreadKind::Space,
                    "a Task parent can only contain Tasks"
                );
            }
        } else {
            let has_space_child: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM threads WHERE parent_thread_id=?1 AND kind='space')",
                [id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(!has_space_child, "a Task cannot contain Spaces");
        }
        tx.execute(
            "UPDATE threads SET kind=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
            params![id, kind_name(new_kind), Utc::now().to_rfc3339()],
        )?;
        let updated = get(&tx, id)?;
        tx.commit()?;
        Ok(updated)
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
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        get(&tx, id)?;
        tx.execute(
            "UPDATE threads SET snoozed_until=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
            params![
                id,
                until.map(|value| value.to_rfc3339()),
                Utc::now().to_rfc3339()
            ],
        )?;
        crate::thread_rollups::refresh_from(&tx, id)?;
        let updated = get(&tx, id)?;
        tx.commit()?;
        Ok(updated)
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
