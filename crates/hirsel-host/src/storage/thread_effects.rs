//! Durable effect receipts and current Owner action projections.
use super::{ThreadCaller, common::parse_ts, thread_scope};
use hirsel_proto::{
    EffectAction, ThreadEffect, ThreadEffectKind, ThreadEffectReceipt, ThreadEffectRefusal,
    ThreadEffectTarget, ThreadTurnState,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

pub(super) fn replay<T: DeserializeOwned>(
    c: &Connection,
    caller: &ThreadCaller,
    operation_id: &str,
    invocation: &impl Serialize,
) -> anyhow::Result<Option<T>> {
    let payload = serde_json::to_string(invocation)?;
    let stored = c.query_row(
        "SELECT payload,result FROM thread_mutation_receipts WHERE turn_id=?1 AND operation_id=?2",
        params![caller.turn_id, operation_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    ).optional()?;
    let Some((old_payload, result)) = stored else {
        return Ok(None);
    };
    anyhow::ensure!(old_payload == payload, "effect invocation payload changed");
    Ok(Some(serde_json::from_str(&result)?))
}

pub(super) fn remember(
    c: &Connection,
    caller: &ThreadCaller,
    operation_id: &str,
    invocation: &impl Serialize,
    result: &impl Serialize,
) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO thread_mutation_receipts(turn_id,operation_id,payload,result) VALUES(?1,?2,?3,?4)",
        params![caller.turn_id, operation_id, serde_json::to_string(invocation)?, serde_json::to_string(result)?],
    )?;
    Ok(())
}

fn effect_name(effect: ThreadEffectKind) -> &'static str {
    match effect {
        ThreadEffectKind::Created => "created",
        ThreadEffectKind::SentTo => "sent_to",
        ThreadEffectKind::Delegated => "delegated",
        ThreadEffectKind::Read => "read",
        ThreadEffectKind::Edited => "edited",
        ThreadEffectKind::Refused => "refused",
    }
}

fn parse_effect(value: &str) -> anyhow::Result<ThreadEffectKind> {
    Ok(match value {
        "created" => ThreadEffectKind::Created,
        "sent_to" => ThreadEffectKind::SentTo,
        "delegated" => ThreadEffectKind::Delegated,
        "read" => ThreadEffectKind::Read,
        "edited" => ThreadEffectKind::Edited,
        "refused" => ThreadEffectKind::Refused,
        _ => anyhow::bail!("invalid stored Thread effect `{value}`"),
    })
}

pub(super) struct NewEffect<'a> {
    pub operation_id: &'a str,
    pub effect_index: u32,
    pub tool: &'a str,
    pub effect: ThreadEffectKind,
    pub target: ThreadEffectTarget,
    pub target_turn_id: Option<u64>,
    pub request_client_id: Option<&'a str>,
    pub refusal: Option<&'a ThreadEffectRefusal>,
}

/// Insert one effect inside the caller's mutation transaction. Replaying the
/// same operation reads the identical receipt instead of appending another.
pub(super) fn record(
    c: &Connection,
    caller: &ThreadCaller,
    effect: NewEffect<'_>,
) -> anyhow::Result<u64> {
    thread_scope::validate_caller(c, caller)?;
    anyhow::ensure!(
        !effect.operation_id.is_empty(),
        "effect operation identity is required"
    );
    anyhow::ensure!(!effect.tool.is_empty(), "effect tool is required");
    anyhow::ensure!(
        (effect.effect == ThreadEffectKind::Refused) == effect.refusal.is_some(),
        "refusal data belongs exactly to refused effects"
    );
    let target = serde_json::to_string(&effect.target)?;
    let refusal = effect.refusal.map(serde_json::to_string).transpose()?;
    if let Some((id, old_tool, old_effect, old_target, old_turn, old_client, old_refusal)) = c.query_row(
        "SELECT id,tool,effect,target_json,target_turn_id,request_client_id,refusal_json FROM thread_effect_receipts WHERE turn_id=?1 AND operation_id=?2 AND effect_index=?3",
        params![caller.turn_id,effect.operation_id,effect.effect_index],
        |r| Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<u64>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?)),
    ).optional()? {
        anyhow::ensure!(old_tool == effect.tool && old_effect == effect_name(effect.effect) && old_target == target && old_turn == effect.target_turn_id && old_client.as_deref() == effect.request_client_id && old_refusal == refusal, "effect operation payload changed");
        return Ok(id);
    }
    c.execute(
        "INSERT INTO thread_effect_receipts(turn_id,operation_id,effect_index,tool,effect,target_json,target_turn_id,request_client_id,refusal_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![caller.turn_id,effect.operation_id,effect.effect_index,effect.tool,effect_name(effect.effect),target,effect.target_turn_id,effect.request_client_id,refusal,chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(c.last_insert_rowid() as u64)
}

fn receipt_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadEffectReceipt> {
    let effect = parse_effect(&r.get::<_, String>(5)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(ThreadEffectReceipt {
        id: r.get(0)?,
        turn_id: r.get(1)?,
        operation_id: r.get(2)?,
        effect_index: r.get(3)?,
        tool: r.get(4)?,
        effect,
        target: serde_json::from_str(&r.get::<_, String>(6)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, error.into())
        })?,
        target_turn_id: r.get(7)?,
        request_client_id: r.get(8)?,
        refusal: r
            .get::<_, Option<String>>(9)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    error.into(),
                )
            })?,
        created_at: parse_ts(&r.get::<_, String>(10)?)?,
    })
}

fn actions(c: &Connection, receipt: &ThreadEffectReceipt) -> anyhow::Result<Vec<EffectAction>> {
    let mut actions = Vec::new();
    match receipt.target {
        ThreadEffectTarget::Thread { thread_id } => {
            let archived: Option<Option<String>> = c
                .query_row(
                    "SELECT archived_at FROM threads WHERE id=?1",
                    [thread_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(archived) = archived {
                actions.push(EffectAction::Open {
                    target: receipt.target,
                });
                if archived.is_none() {
                    actions.push(EffectAction::Archive { thread_id });
                }
            }
            if let Some(turn_id) = receipt.target_turn_id {
                let state: Option<String> = c
                    .query_row(
                        "SELECT state FROM thread_turns WHERE id=?1 AND thread_id=?2",
                        params![turn_id, thread_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                match state.as_deref() {
                    Some("queued") => {
                        actions.push(EffectAction::CancelQueued { thread_id, turn_id })
                    }
                    Some("running") => actions.push(EffectAction::Stop { thread_id, turn_id }),
                    _ => {}
                }
            }
        }
        ThreadEffectTarget::Artifact { artifact_id } => {
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM artifacts WHERE id=?1)",
                [artifact_id],
                |r| r.get(0),
            )?;
            if exists {
                actions.push(EffectAction::Open {
                    target: receipt.target,
                });
            }
        }
        ThreadEffectTarget::Root => {}
    }
    Ok(actions)
}

#[cfg(test)]
pub(super) fn for_turns(c: &Connection, turn_ids: &[u64]) -> anyhow::Result<Vec<ThreadEffect>> {
    if turn_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", turn_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut statement = c.prepare(&format!("SELECT id,turn_id,operation_id,effect_index,tool,effect,target_json,target_turn_id,request_client_id,refusal_json,created_at FROM thread_effect_receipts WHERE turn_id IN ({placeholders}) ORDER BY turn_id,id"))?;
    let receipts = statement
        .query_map(rusqlite::params_from_iter(turn_ids), receipt_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    receipts
        .into_iter()
        .map(|receipt| {
            Ok(ThreadEffect {
                actions: actions(c, &receipt)?,
                receipt,
            })
        })
        .collect()
}

pub(super) fn page_for_thread(
    c: &Connection,
    thread_id: u64,
    before: Option<u64>,
    limit: u64,
) -> anyhow::Result<(Vec<ThreadEffect>, Option<u64>)> {
    let limit = limit.clamp(1, 100) as usize;
    let mut statement = c.prepare("SELECT e.id,e.turn_id,e.operation_id,e.effect_index,e.tool,e.effect,e.target_json,e.target_turn_id,e.request_client_id,e.refusal_json,e.created_at FROM thread_effect_receipts e JOIN thread_turns t ON t.id=e.turn_id WHERE t.thread_id=?1 AND e.id<?2 ORDER BY e.id DESC LIMIT ?3")?;
    let mut receipts = statement
        .query_map(
            params![
                thread_id,
                before.unwrap_or(i64::MAX as u64).min(i64::MAX as u64),
                limit as u64 + 1
            ],
            receipt_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = receipts.len() > limit;
    receipts.truncate(limit);
    receipts.reverse();
    let next = has_more.then(|| receipts[0].id);
    let effects = receipts
        .into_iter()
        .map(|receipt| {
            Ok(ThreadEffect {
                actions: actions(c, &receipt)?,
                receipt,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((effects, next))
}

impl super::Storage {
    pub(crate) async fn thread_turn_cancellation_requested(
        &self,
        turn_id: u64,
    ) -> anyhow::Result<bool> {
        let c = self.conn.lock().await;
        Ok(c.query_row(
            "SELECT cancel_requested_at IS NOT NULL FROM thread_turns WHERE id=?1",
            [turn_id],
            |row| row.get(0),
        )?)
    }

    #[cfg(test)]
    pub(crate) async fn thread_effects(&self, turn_id: u64) -> anyhow::Result<Vec<ThreadEffect>> {
        let c = self.conn.lock().await;
        for_turns(&c, &[turn_id])
    }

    pub(crate) async fn thread_effect_publication(
        &self,
        turn_id: u64,
        before: Option<u64>,
    ) -> anyhow::Result<(String, u64, Vec<ThreadEffect>, Option<u64>)> {
        let c = self.conn.lock().await;
        let history_id = super::schema::read_history_id(&c)?;
        let thread_id = c.query_row(
            "SELECT thread_id FROM thread_turns WHERE id=?1",
            [turn_id],
            |r| r.get(0),
        )?;
        let mut statement = c.prepare("SELECT id,turn_id,operation_id,effect_index,tool,effect,target_json,target_turn_id,request_client_id,refusal_json,created_at FROM thread_effect_receipts WHERE turn_id=?1 AND id<?2 ORDER BY id DESC LIMIT 101")?;
        let mut receipts = statement
            .query_map(
                params![
                    turn_id,
                    before.unwrap_or(i64::MAX as u64).min(i64::MAX as u64)
                ],
                receipt_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = receipts.len() > 100;
        receipts.truncate(100);
        receipts.reverse();
        let next = has_more.then(|| receipts[0].id);
        let effects = receipts
            .into_iter()
            .map(|receipt| {
                Ok(ThreadEffect {
                    actions: actions(&c, &receipt)?,
                    receipt,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((history_id, thread_id, effects, next))
    }

    pub(crate) async fn thread_effect_operation_publication(
        &self,
        turn_id: u64,
        operation_id: &str,
        before: Option<u64>,
    ) -> anyhow::Result<(String, u64, Vec<ThreadEffect>, Option<u64>)> {
        let c = self.conn.lock().await;
        let history_id = super::schema::read_history_id(&c)?;
        let thread_id = c.query_row(
            "SELECT thread_id FROM thread_turns WHERE id=?1",
            [turn_id],
            |row| row.get(0),
        )?;
        let mut statement = c.prepare("SELECT id,turn_id,operation_id,effect_index,tool,effect,target_json,target_turn_id,request_client_id,refusal_json,created_at FROM thread_effect_receipts WHERE turn_id=?1 AND operation_id=?2 AND id<?3 ORDER BY id DESC LIMIT 101")?;
        let mut receipts = statement
            .query_map(
                params![
                    turn_id,
                    operation_id,
                    before.unwrap_or(i64::MAX as u64).min(i64::MAX as u64)
                ],
                receipt_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = receipts.len() > 100;
        receipts.truncate(100);
        receipts.reverse();
        let next = has_more.then(|| receipts[0].id);
        let effects = receipts
            .into_iter()
            .map(|receipt| {
                Ok(ThreadEffect {
                    actions: actions(&c, &receipt)?,
                    receipt,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((history_id, thread_id, effects, next))
    }

    pub(crate) async fn effect_source_turns_for_target(
        &self,
        thread_id: u64,
    ) -> anyhow::Result<Vec<u64>> {
        let c = self.conn.lock().await;
        Ok(c.prepare("SELECT DISTINCT turn_id FROM thread_effect_receipts WHERE json_extract(target_json,'$.kind')='thread' AND json_extract(target_json,'$.thread_id')=?1 ORDER BY turn_id")?
            .query_map([thread_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(crate) async fn cancel_exact_thread_turn(
        &self,
        history_id: &str,
        thread_id: u64,
        turn_id: u64,
        expected_state: ThreadTurnState,
    ) -> anyhow::Result<hirsel_proto::ThreadTurn> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_history(&tx, history_id)?;
        let turn = super::thread_activity::get(&tx, turn_id)?;
        anyhow::ensure!(
            turn.thread_id == thread_id,
            "turn belongs to another Thread"
        );
        anyhow::ensure!(
            turn.state == expected_state,
            "turn state changed; reload before cancelling"
        );
        anyhow::ensure!(
            matches!(
                turn.state,
                ThreadTurnState::Queued | ThreadTurnState::Running
            ),
            "turn is already terminal"
        );
        tx.execute("UPDATE thread_turns SET cancel_requested_at=COALESCE(cancel_requested_at,?2) WHERE id=?1", params![turn_id,chrono::Utc::now().to_rfc3339()])?;
        let turn = super::thread_activity::get(&tx, turn_id)?;
        tx.commit()?;
        Ok(turn)
    }
}
