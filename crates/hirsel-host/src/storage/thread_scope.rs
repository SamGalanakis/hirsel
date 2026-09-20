//! Thread coordination authority. Topology is immutable; reach is durable data.
//!
//! Every ID is addressable: an out-of-reach target is refused in the open, as a
//! typed [`OutsideGrant`] the tool layer turns into a readable refusal, never a
//! pretence that the Thread does not exist.
use super::{Storage, thread_activity, threads};
use crate::thread_identity::{ThreadIdentity, ThreadIdentityRef};
use hirsel_proto::{Thread, ThreadBrief, ThreadGrant, ThreadTurnState};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// The reachable set: the caller's own subtree, plus the subtree of every
/// Thread its durable grants name — and, when it holds a root grant, every
/// Thread in the history, at every level and whenever it was created, its own
/// ancestors included. Bound `?1` is the caller.
pub(super) const REACH_CTE: &str = "WITH RECURSIVE roots(id) AS (
    SELECT ?1
    UNION SELECT target_thread_id FROM thread_grants WHERE thread_id=?1 AND target_thread_id IS NOT NULL
    UNION SELECT t.id FROM threads t WHERE EXISTS(SELECT 1 FROM thread_grants WHERE thread_id=?1 AND target_thread_id IS NULL)
), scope(id) AS (
    SELECT id FROM roots UNION SELECT t.id FROM threads t JOIN scope s ON t.parent_thread_id=s.id
)";

/// What the caller addressed and could not reach. Carried as a typed error so
/// one refusal reads the same at every layer it crosses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutsideGrant {
    pub target: RefusedTarget,
    pub reason: RefusalReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RefusedTarget {
    Thread {
        thread_id: u64,
    },
    Artifact {
        artifact_id: u64,
    },
    /// Reach over everything, asked for by a Thread that does not hold it.
    Root,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefusalReason {
    /// The target exists and is simply not in this Thread's reach.
    OutsideGrant,
    /// The fence only a root grant opens: a Thread without root reach never
    /// messages upward.
    OwnerFence,
}

impl RefusalReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::OutsideGrant => "outside_grant",
            Self::OwnerFence => "owner_fence",
        }
    }
}
impl std::fmt::Display for OutsideGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let target = match self.target {
            RefusedTarget::Thread { thread_id } => format!("Thread #{thread_id}"),
            RefusedTarget::Artifact { artifact_id } => format!("Artifact {artifact_id}"),
            RefusedTarget::Root => "Everything (root)".to_string(),
        };
        match self.reason {
            RefusalReason::OutsideGrant => {
                write!(f, "{target} is outside this Thread's grant")
            }
            RefusalReason::OwnerFence => write!(
                f,
                "{target} is an ancestor; report to your requester instead of messaging upward"
            ),
        }
    }
}
impl std::error::Error for OutsideGrant {}
impl OutsideGrant {
    fn thread(thread_id: u64) -> anyhow::Error {
        Self {
            target: RefusedTarget::Thread { thread_id },
            reason: RefusalReason::OutsideGrant,
        }
        .into()
    }
    pub(super) fn root() -> anyhow::Error {
        Self {
            target: RefusedTarget::Root,
            reason: RefusalReason::OutsideGrant,
        }
        .into()
    }
    pub(super) fn owner_fence(thread_id: u64) -> anyhow::Error {
        Self {
            target: RefusedTarget::Thread { thread_id },
            reason: RefusalReason::OwnerFence,
        }
        .into()
    }
}

/// Where a tool points: the caller itself (`.`), or any Thread by id — and
/// `0`, which is the one address that is no Thread at all: the top of the
/// tree. One encoding, no path algebra; anything outside reach is refused.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum ThreadRef {
    /// `0` is the root address; Thread ids start at 1.
    Id(u64),
    /// The literal `.`. Any other string is an invalid reference.
    Dot(String),
}
impl Default for ThreadRef {
    fn default() -> Self {
        Self::Dot(".".into())
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
    authority: ThreadCallerAuthority,
}

#[derive(Debug, Clone)]
enum ThreadCallerAuthority {
    Turn,
    Process { process_id: String },
}

pub(super) fn authorize(c: &Connection, caller: u64, target: u64) -> anyhow::Result<()> {
    let allowed: bool = c.query_row(
        &format!("{REACH_CTE} SELECT EXISTS(SELECT 1 FROM scope WHERE id=?2)"),
        params![caller, target],
        |r| r.get(0),
    )?;
    if allowed {
        return Ok(());
    }
    Err(OutsideGrant::thread(target))
}
/// True when `ancestor` is a strict ancestor of `thread`. Topology is
/// immutable, so this relation is the shape of the owner fence: only a root
/// grant reaches back across it (see [`owner_fence`]).
pub(super) fn is_ancestor(c: &Connection, ancestor: u64, thread: u64) -> anyhow::Result<bool> {
    Ok(c.query_row(
        "WITH RECURSIVE up(id) AS (
            SELECT parent_thread_id FROM threads WHERE id=?2
            UNION SELECT t.parent_thread_id FROM threads t JOIN up u ON t.id=u.id
        ) SELECT EXISTS(SELECT 1 FROM up WHERE id=?1)",
        params![ancestor, thread],
        |r| r.get(0),
    )?)
}
/// The owner fence: a Thread reports to its requester instead of messaging an
/// ancestor. Root is the one reach that opens it — a Thread holding a root
/// grant addresses every Thread at every level, ancestors included. For
/// everyone else the fence stays closed.
pub(super) fn owner_fence(c: &Connection, caller: u64, target: u64) -> anyhow::Result<()> {
    if is_ancestor(c, target, caller)? && !super::thread_grants::holds_root(c, caller)? {
        return Err(OutsideGrant::owner_fence(target));
    }
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
    if let ThreadCallerAuthority::Process { process_id } = &caller.authority {
        let active: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_process_authorities a
             JOIN thread_process_sessions s ON s.session_id=a.session_id
             JOIN thread_turns t ON t.id=a.turn_id AND t.thread_id=s.thread_id
             JOIN meta m ON m.key='history_id' AND m.value=s.history_id
             WHERE s.history_id=?1 AND s.session_id=?2 AND a.process_id=?3
             AND a.turn_id=?4 AND s.thread_id=?5)",
            params![
                caller.history_id,
                caller.session_id,
                process_id,
                caller.turn_id,
                caller.thread_id
            ],
            |r| r.get(0),
        )?;
        anyhow::ensure!(active, "Thread process authority is unavailable");
        return Ok(());
    }
    let active: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread_execution_bindings b
         JOIN thread_turns t ON t.id=b.turn_id
         JOIN meta m ON m.key='history_id' AND m.value=b.history_id
         WHERE b.history_id=?1 AND b.session_id=?2 AND b.execution_id=?3
         AND b.turn_id=?4 AND t.thread_id=?5 AND t.state='running' AND b.revoked=0
         AND t.cancel_requested_at IS NULL)",
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
        ThreadRef::Dot(path) => {
            anyhow::ensure!(path == ".", "invalid Thread reference");
            caller
        }
    };
    // 0 names the top of the tree, where a creation hangs on no Thread and
    // ancestors become peers. The only reach that opens it is the root grant
    // — without one, the refusal is typed like any other.
    if target == 0 {
        anyhow::ensure!(
            super::thread_grants::holds_root(c, caller)?,
            OutsideGrant::root()
        );
        return Ok(0);
    }
    authorize(c, caller, target)?;
    Ok(target)
}

/// The subtree a listing hangs under: a Thread, or — for the root address —
/// no Thread at all, which lists the top level.
pub(super) fn resolve_under(
    c: &Connection,
    caller: u64,
    reference: &ThreadRef,
) -> anyhow::Result<Option<u64>> {
    let target = resolve(c, caller, reference)?;
    Ok((target != 0).then_some(target))
}

pub(super) fn authorize_artifact(
    c: &Connection,
    caller: u64,
    artifact_id: u64,
) -> anyhow::Result<()> {
    let allowed: bool = c.query_row(
        &format!("{REACH_CTE}
        SELECT EXISTS(SELECT 1 FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id JOIN scope s ON s.id=m.thread_id WHERE r.artifact_id=?2
        UNION ALL SELECT 1 FROM activity_artifacts r JOIN thread_activities a ON a.id=r.activity_id JOIN scope s ON s.id=a.thread_id WHERE r.artifact_id=?2
        UNION ALL SELECT 1 FROM threads t JOIN scope s ON s.id=t.id WHERE t.showcased_artifact_id=?2)"),
        params![caller, artifact_id],
        |r| r.get(0),
    )?;
    if allowed {
        return Ok(());
    }
    Err(OutsideGrant {
        target: RefusedTarget::Artifact { artifact_id },
        reason: RefusalReason::OutsideGrant,
    }
    .into())
}

#[derive(Debug, Serialize)]
pub(crate) struct ThreadPage {
    pub history_id: String,
    pub threads: Vec<Thread>,
    pub next_after_id: Option<u64>,
}
#[derive(Debug, Serialize)]
pub(crate) struct ThreadContext {
    pub history_id: String,
    pub reference_url: String,
    pub related_items: Vec<hirsel_proto::ThreadRelatedItem>,
    #[serde(rename = "self")]
    pub thread: Thread,
    pub ancestors: Vec<ThreadIdentityRef>,
    pub brief: ThreadBrief,
    /// Durable widenings beyond self + descendants, with the one-line summary
    /// the Owner sees in the web reach strip.
    pub grants: Vec<ThreadGrant>,
    pub reach: String,
}
impl Storage {
    /// A Thread without root reach never addresses its own ancestors with work:
    /// it reports to the requester that asked for it. Reach widens sideways and
    /// downward by grant; only root widens it upward as well.
    pub(crate) async fn refuse_upward(
        &self,
        caller: &ThreadCaller,
        target: u64,
    ) -> anyhow::Result<()> {
        let c = self.conn.lock().await;
        owner_fence(&c, caller.thread_id, target)
    }
    pub(crate) async fn thread_in_scope(
        &self,
        caller_thread_id: u64,
        target_thread_id: u64,
    ) -> anyhow::Result<bool> {
        let c = self.conn.lock().await;
        Ok(authorize(&c, caller_thread_id, target_thread_id).is_ok())
    }

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
        // The root address (None) lists the top level; any Thread lists what
        // hangs below it.
        let under = resolve_under(&c, caller.thread_id, under)?;
        let mut ids = c.prepare("WITH RECURSIVE descendants(id,depth) AS (SELECT id,1 FROM threads WHERE (?1 IS NULL AND parent_thread_id IS NULL) OR parent_thread_id=?1 UNION ALL SELECT t.id,d.depth+1 FROM threads t JOIN descendants d ON t.parent_thread_id=d.id WHERE d.depth<?2) SELECT id FROM descendants WHERE (?3 IS NULL OR id>?3) ORDER BY id LIMIT ?4")?
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
        // The identity block in every turn prompt is rendered from this exact
        // read, so the prompt and this tool can never name different ancestry
        // or reach.
        let identity = identity(&c, &thread)?;
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
            ancestors: identity.ancestors,
            brief,
            grants: super::thread_grants::list(&c, caller.thread_id)?,
            reach: identity.reach,
        })
    }

    /// What the agent running in this Thread is told about itself, read
    /// without a turn binding so the turn prompt can be rebuilt before the
    /// turn exists.
    pub(crate) async fn thread_identity(&self, thread_id: u64) -> anyhow::Result<ThreadIdentity> {
        let c = self.conn.lock().await;
        let thread = threads::get(&c, thread_id)?;
        identity(&c, &thread)
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
            authority: ThreadCallerAuthority::Turn,
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
        let caller=c.query_row("SELECT b.history_id,t.thread_id,t.id FROM thread_execution_bindings b JOIN thread_turns t ON t.id=b.turn_id WHERE b.session_id=?1 AND b.execution_id=?2",params![session_id,execution_id],|r|Ok(ThreadCaller{history_id:r.get(0)?,session_id:session_id.into(),execution_id:execution_id.into(),thread_id:r.get(1)?,turn_id:r.get(2)?,authority:ThreadCallerAuthority::Turn})).optional()?.ok_or_else(||anyhow::anyhow!("Thread execution binding is unavailable"))?;
        validate_caller(&c, &caller)?;
        Ok(caller)
    }

    /// Resolve a durable process to its owning Thread. The first tool call for
    /// one process pins the Thread's latest turn as a receipt namespace; that
    /// turn is attribution only and does not need to remain running.
    pub(crate) async fn process_caller(
        &self,
        session_id: &str,
        process_id: &str,
        execution_id: &str,
    ) -> anyhow::Result<ThreadCaller> {
        let c = self.conn.lock().await;
        c.execute(
            "INSERT OR IGNORE INTO thread_process_authorities(session_id,process_id,turn_id)
             SELECT s.session_id,?2,t.id FROM thread_process_sessions s
             JOIN thread_turns t ON t.thread_id=s.thread_id
             JOIN meta m ON m.key='history_id' AND m.value=s.history_id
             WHERE s.session_id=?1 ORDER BY t.id DESC LIMIT 1",
            params![session_id, process_id],
        )?;
        let caller = c
            .query_row(
                "SELECT s.history_id,s.thread_id,a.turn_id FROM thread_process_authorities a
                 JOIN thread_process_sessions s ON s.session_id=a.session_id
                 WHERE a.session_id=?1 AND a.process_id=?2",
                params![session_id, process_id],
                |r| {
                    Ok(ThreadCaller {
                        history_id: r.get(0)?,
                        session_id: session_id.into(),
                        execution_id: execution_id.into(),
                        thread_id: r.get(1)?,
                        turn_id: r.get(2)?,
                        authority: ThreadCallerAuthority::Process {
                            process_id: process_id.into(),
                        },
                    })
                },
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("Thread process authority is unavailable"))?;
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
        tx.execute("UPDATE thread_turns SET cancel_requested_at=COALESCE(cancel_requested_at,strftime('%Y-%m-%dT%H:%M:%fZ','now')) WHERE thread_id=?1 AND state='running'",[thread_id])?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn reference_url(history_id: &str, thread_id: u64) -> String {
    format!("/t/{thread_id}?history={history_id}")
}

/// Read one Thread's place in the tree: who it is, what it is for, who it sits
/// under, and how far it may address. The single source for both the
/// `threads.context` payload and the turn-prompt identity block.
fn identity(c: &Connection, thread: &Thread) -> anyhow::Result<ThreadIdentity> {
    let mut ancestors = vec![];
    let mut parent = thread.parent_thread_id;
    while let Some(id) = parent {
        anyhow::ensure!(
            ancestors.len() < 64,
            "Thread ancestry exceeds context bound"
        );
        let (kind, title, next): (String, String, Option<u64>) = c.query_row(
            "SELECT kind,title,parent_thread_id FROM threads WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        ancestors.push(ThreadIdentityRef {
            id,
            kind: threads::parse_kind(&kind)
                .ok_or_else(|| anyhow::anyhow!("invalid Thread kind `{kind}`"))?,
            title,
        });
        parent = next;
    }
    ancestors.reverse();
    Ok(ThreadIdentity {
        thread: ThreadIdentityRef {
            id: thread.id,
            kind: thread.kind,
            title: thread.title.clone(),
        },
        description: thread.description.clone(),
        ancestors,
        reach: super::thread_grants::reach_summary(c, thread.id)?,
        tool_profile: super::ToolProfile::for_thread(c, thread.id)?,
    })
}
