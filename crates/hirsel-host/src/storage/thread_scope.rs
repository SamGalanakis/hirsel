//! Thread coordination authority. Topology is immutable; IDs never grant access.
use super::{Storage, thread_activity, threads};
use hirsel_proto::{Thread, ThreadBrief, ThreadTurnState};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum ThreadRef {
    Id(u64),
    Path(String),
}
impl Default for ThreadRef {
    fn default() -> Self {
        Self::Path(".".into())
    }
}

/// Constructed only from a host-owned execution binding, never tool arguments.
#[derive(Debug, Clone)]
pub(crate) struct ThreadCaller {
    pub history_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub thread_id: u64,
    pub turn_id: u64,
}

pub(super) fn authorize(c: &Connection, caller: u64, target: u64) -> anyhow::Result<()> {
    let allowed: bool = c.query_row(
        "WITH RECURSIVE scope(id) AS (SELECT id FROM threads WHERE id=?1 UNION ALL SELECT t.id FROM threads t JOIN scope s ON t.parent_thread_id=s.id) SELECT EXISTS(SELECT 1 FROM scope WHERE id=?2)",
        params![caller,target], |r| r.get(0),
    )?;
    anyhow::ensure!(allowed, "Thread is unavailable in this scope");
    Ok(())
}
pub(super) fn validate_history(c: &Connection, history_id: &str) -> anyhow::Result<()> {
    let current: String =
        c.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })?;
    anyhow::ensure!(current == history_id, "Thread history is no longer current");
    Ok(())
}
pub(super) fn validate_caller(c: &Connection, caller: &ThreadCaller) -> anyhow::Result<()> {
    let active: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread_execution_bindings b
         JOIN thread_turns t ON t.id=b.turn_id
         JOIN meta m ON m.key='history_id' AND m.value=b.history_id
         WHERE b.history_id=?1 AND b.session_id=?2 AND b.execution_id=?3
         AND b.turn_id=?4 AND t.thread_id=?5 AND t.state='running' AND b.revoked=0
         AND NOT EXISTS(SELECT 1 FROM thread_cancellations x WHERE x.turn_id=t.id))",
        params![
            caller.history_id,
            caller.session_id,
            caller.execution_id,
            caller.turn_id,
            caller.thread_id
        ],
        |r| r.get(0),
    )?;
    anyhow::ensure!(active, "Thread execution binding is unavailable");
    Ok(())
}
pub(super) fn resolve(c: &Connection, caller: u64, reference: &ThreadRef) -> anyhow::Result<u64> {
    let target = match reference {
        ThreadRef::Id(id) => *id,
        ThreadRef::Path(path) if path == "." => caller,
        ThreadRef::Path(path) => {
            let suffix = path
                .strip_prefix("./")
                .ok_or_else(|| anyhow::anyhow!("invalid Thread path"))?;
            let parts: Vec<_> = suffix.split('/').collect();
            anyhow::ensure!(
                !parts.is_empty() && parts.len() <= 64,
                "invalid Thread path"
            );
            let mut parent = caller;
            for part in parts {
                anyhow::ensure!(
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
                    "invalid Thread path"
                );
                let id: u64 = part.parse()?;
                let valid: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1 AND parent_thread_id=?2)",
                    params![id, parent],
                    |r| r.get(0),
                )?;
                anyhow::ensure!(valid, "Thread is unavailable in this scope");
                parent = id;
            }
            parent
        }
    };
    authorize(c, caller, target)?;
    Ok(target)
}

pub(super) fn authorize_artifact(
    c: &Connection,
    caller: u64,
    artifact_id: u64,
) -> anyhow::Result<()> {
    let allowed: bool = c.query_row(
        "WITH RECURSIVE scope(id) AS (SELECT id FROM threads WHERE id=?1 UNION ALL SELECT t.id FROM threads t JOIN scope s ON t.parent_thread_id=s.id)
        SELECT EXISTS(SELECT 1 FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id JOIN scope s ON s.id=m.thread_id WHERE r.artifact_id=?2
        UNION ALL SELECT 1 FROM activity_artifacts r JOIN thread_activities a ON a.id=r.activity_id JOIN scope s ON s.id=a.thread_id WHERE r.artifact_id=?2
        UNION ALL SELECT 1 FROM threads t JOIN scope s ON s.id=t.id WHERE t.showcased_artifact_id=?2)",
        params![caller,artifact_id], |r| r.get(0),
    )?;
    anyhow::ensure!(allowed, "Artifact is unavailable in this scope");
    Ok(())
}

#[derive(Debug, Serialize)]
pub(crate) struct ThreadPage {
    pub history_id: String,
    pub threads: Vec<Thread>,
    pub next_after_id: Option<u64>,
}
#[derive(Debug, Serialize)]
pub(crate) struct AncestorIdentity {
    pub id: u64,
    pub title: String,
}
#[derive(Debug, Serialize)]
pub(crate) struct ThreadContext {
    pub history_id: String,
    pub reference_url: String,
    pub related_items: Vec<hirsel_proto::ThreadRelatedItem>,
    #[serde(rename = "self")]
    pub thread: Thread,
    pub ancestors: Vec<AncestorIdentity>,
    pub brief: ThreadBrief,
}
impl Storage {
    pub(crate) async fn resolve_thread(
        &self,
        caller: &ThreadCaller,
        reference: &ThreadRef,
    ) -> anyhow::Result<u64> {
        let c = self.conn.lock().await;
        validate_caller(&c, caller)?;
        resolve(&c, caller.thread_id, reference)
    }
    pub(crate) async fn scoped_thread_list(
        &self,
        caller: &ThreadCaller,
        under: &ThreadRef,
        depth: u32,
        after_id: Option<u64>,
        limit: u32,
    ) -> anyhow::Result<ThreadPage> {
        anyhow::ensure!(
            (1..=8).contains(&depth) && (1..=100).contains(&limit),
            "invalid Thread page bounds"
        );
        let c = self.conn.lock().await;
        validate_caller(&c, caller)?;
        let under = resolve(&c, caller.thread_id, under)?;
        let mut ids = c.prepare("WITH RECURSIVE descendants(id,depth) AS (SELECT id,1 FROM threads WHERE parent_thread_id=?1 UNION ALL SELECT t.id,d.depth+1 FROM threads t JOIN descendants d ON t.parent_thread_id=d.id WHERE d.depth<?2) SELECT id FROM descendants WHERE (?3 IS NULL OR id>?3) ORDER BY id LIMIT ?4")?
            .query_map(params![under,depth,after_id,limit+1], |r|r.get::<_,u64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = ids.len() > limit as usize;
        ids.truncate(limit as usize);
        let next_after_id = more.then(|| *ids.last().expect("nonempty bounded page"));
        let threads = ids
            .into_iter()
            .map(|id| threads::get(&c, id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(ThreadPage {
            history_id: caller.history_id.clone(),
            threads,
            next_after_id,
        })
    }
    pub(crate) async fn thread_context(
        &self,
        caller: &ThreadCaller,
    ) -> anyhow::Result<ThreadContext> {
        let c = self.conn.lock().await;
        validate_caller(&c, caller)?;
        let thread = threads::get(&c, caller.thread_id)?;
        let mut ancestors = vec![];
        let mut parent = thread.parent_thread_id;
        while let Some(id) = parent {
            anyhow::ensure!(
                ancestors.len() < 64,
                "Thread ancestry exceeds context bound"
            );
            let (title, next): (String, Option<u64>) = c.query_row(
                "SELECT title,parent_thread_id FROM threads WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ancestors.push(AncestorIdentity { id, title });
            parent = next;
        }
        ancestors.reverse();
        // An accepted assignment is immutable for this execution. A later queued
        // assignment must not replace the running turn's brief or reference grants.
        let assignment: Option<(u64,String)>=c.query_row("SELECT id,json_extract(data,'$.brief') FROM thread_activities WHERE turn_id=?1 AND kind='delegation_received' ORDER BY id LIMIT 1",[caller.turn_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let brief = if let Some((id, text)) = assignment {
            let artifact_ids=c.prepare("SELECT artifact_id FROM activity_artifacts WHERE activity_id=?1 ORDER BY artifact_id")?.query_map([id],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            ThreadBrief { text, artifact_ids }
        } else {
            ThreadBrief {
                text: String::new(),
                artifact_ids: vec![],
            }
        };
        Ok(ThreadContext {
            history_id: caller.history_id.clone(),
            reference_url: reference_url(&caller.history_id, caller.thread_id),
            related_items: super::thread_related::list_scoped(
                &c,
                caller.thread_id,
                caller.thread_id,
            )?,
            thread,
            ancestors,
            brief,
        })
    }
    pub(crate) async fn authorize_thread_artifact(
        &self,
        caller: &ThreadCaller,
        artifact_id: u64,
    ) -> anyhow::Result<()> {
        let c = self.conn.lock().await;
        validate_caller(&c, caller)?;
        authorize_artifact(&c, caller.thread_id, artifact_id)
    }
}

impl Storage {
    pub(crate) async fn bind_thread_execution(
        &self,
        history_id: &str,
        session_id: &str,
        execution_id: &str,
        turn_id: u64,
    ) -> anyhow::Result<ThreadCaller> {
        let c = self.conn.lock().await;
        let current: String =
            c.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
                r.get(0)
            })?;
        anyhow::ensure!(current == history_id, "Thread history is no longer current");
        let turn = thread_activity::get(&c, turn_id)?;
        anyhow::ensure!(
            turn.state == ThreadTurnState::Running,
            "only a running turn can bind an execution"
        );
        c.execute("INSERT INTO thread_execution_bindings(history_id,session_id,execution_id,turn_id) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id,execution_id) DO NOTHING",params![history_id,session_id,execution_id,turn_id])?;
        let caller = ThreadCaller {
            history_id: history_id.into(),
            session_id: session_id.into(),
            execution_id: execution_id.into(),
            thread_id: turn.thread_id,
            turn_id,
        };
        validate_caller(&c, &caller)?;
        Ok(caller)
    }
    pub(crate) async fn execution_caller(
        &self,
        session_id: &str,
        execution_id: &str,
    ) -> anyhow::Result<ThreadCaller> {
        let c = self.conn.lock().await;
        let caller=c.query_row("SELECT b.history_id,t.thread_id,t.id FROM thread_execution_bindings b JOIN thread_turns t ON t.id=b.turn_id WHERE b.session_id=?1 AND b.execution_id=?2",params![session_id,execution_id],|r|Ok(ThreadCaller{history_id:r.get(0)?,session_id:session_id.into(),execution_id:execution_id.into(),thread_id:r.get(1)?,turn_id:r.get(2)?})).optional()?.ok_or_else(||anyhow::anyhow!("Thread execution binding is unavailable"))?;
        validate_caller(&c, &caller)?;
        Ok(caller)
    }
    pub(crate) async fn revoke_thread_execution(
        &self,
        caller: &ThreadCaller,
    ) -> anyhow::Result<()> {
        self.conn.lock().await.execute("UPDATE thread_execution_bindings SET revoked=1 WHERE history_id=?1 AND session_id=?2 AND execution_id=?3 AND turn_id=?4",params![caller.history_id,caller.session_id,caller.execution_id,caller.turn_id])?;
        Ok(())
    }
}

impl Storage {
    /// Host-only completion of an already-started tool. This is not model
    /// authority: cancellation/revocation may have happened, but the exact
    /// execution must still belong to the current history and original turn.
    pub(crate) async fn execution_telemetry_guard(
        &self,
        caller: &ThreadCaller,
    ) -> anyhow::Result<tokio::sync::MutexGuard<'_, Connection>> {
        let c = self.conn.lock().await;
        let matches: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_execution_bindings b
             JOIN thread_turns t ON t.id=b.turn_id
             JOIN meta m ON m.key='history_id' AND m.value=b.history_id
             WHERE b.history_id=?1 AND b.session_id=?2 AND b.execution_id=?3
             AND b.turn_id=?4 AND t.thread_id=?5)",
            params![
                caller.history_id,
                caller.session_id,
                caller.execution_id,
                caller.turn_id,
                caller.thread_id
            ],
            |r| r.get(0),
        )?;
        anyhow::ensure!(matches, "Thread telemetry execution is no longer current");
        Ok(c)
    }

    /// Retained only across a non-SQL resource commit (e.g. the view map). SQL
    /// writers validate directly inside their own transaction instead.
    pub(crate) async fn execution_guard(
        &self,
        caller: &ThreadCaller,
    ) -> anyhow::Result<tokio::sync::MutexGuard<'_, Connection>> {
        let c = self.conn.lock().await;
        validate_caller(&c, caller)?;
        Ok(c)
    }
}

impl Storage {
    pub(crate) async fn request_thread_cancellation(
        &self,
        history_id: &str,
        thread_id: u64,
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        validate_history(&tx, history_id)?;
        tx.execute("INSERT OR IGNORE INTO thread_cancellations(turn_id) SELECT id FROM thread_turns WHERE thread_id=?1 AND state='running'",[thread_id])?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn reference_url(history_id: &str, thread_id: u64) -> String {
    format!("/t/{thread_id}?history={history_id}")
}
