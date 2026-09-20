//! Agent Thread writes validate the execution and replay input inside the write transaction.
use super::thread_icons::icon_columns;
use super::{Storage, ThreadCaller, ThreadRef, thread_scope, threads};
use hirsel_proto::{ReachTarget, ThreadAttention, ThreadIcon, ThreadKind};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

/// The literal `"root"` a tool writes where a Thread reference would go.
#[derive(Debug, Clone, Copy, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootLiteral {
    Root,
}

/// A grant target as a tool names it: `"root"`, or any Thread reference. Root
/// is tried first, so the string can never be read as a Thread path.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum GrantTargetRef {
    Root(RootLiteral),
    Thread(ThreadRef),
}

#[derive(Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RelatedTargetInput {
    Url { url: String },
    Thread { thread: ThreadRef },
}
#[derive(Serialize)]
#[serde(tag = "operation")]
pub(crate) enum ThreadMutation {
    AddRelated {
        thread: ThreadRef,
        target: RelatedTargetInput,
        title: Option<String>,
    },
    RemoveRelated {
        thread: ThreadRef,
        item_id: u64,
    },
    Cancel {
        thread: ThreadRef,
    },
    /// The only removal. Archiving takes the whole subtree out of the active
    /// tree, cancels its open work and keeps every conversation.
    Archive {
        thread: ThreadRef,
        archived: bool,
    },
    Create {
        client_id: String,
        kind: ThreadKind,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon: Option<ThreadIcon>,
        parent: ThreadRef,
        description: String,
        instrument: Option<Value>,
        attention: ThreadAttention,
    },
    Update {
        thread: ThreadRef,
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon: Option<Option<ThreadIcon>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        showcased_artifact_id: Option<Option<u64>>,
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        instrument: Option<Option<Value>>,
        attention: Option<ThreadAttention>,
    },
    Activity {
        thread: ThreadRef,
        kind: String,
        data: Value,
    },
    /// Widen a descendant's reach to one Thread the caller can already reach,
    /// or to everything, which only a root holder can hand on.
    Grant {
        thread: ThreadRef,
        target: GrantTargetRef,
        note: Option<String>,
    },
    /// Narrow a descendant's reach. Narrowing needs no reach of its own: an
    /// ancestor may always remove a grant, including one the Owner made.
    Revoke {
        thread: ThreadRef,
        target: ReachTarget,
    },
}
impl Storage {
    pub(crate) async fn mutate_scoped_thread(
        &self,
        caller: &ThreadCaller,
        operation_id: &str,
        mutation: &ThreadMutation,
    ) -> anyhow::Result<Value> {
        let payload = serde_json::to_string(mutation)?;
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_caller(&tx, caller)?;
        if let Some((old,result))=tx.query_row("SELECT payload,result FROM thread_mutation_receipts WHERE turn_id=?1 AND operation_id=?2",params![caller.turn_id,operation_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
            anyhow::ensure!(old==payload,"Thread mutation invocation payload changed");
            return Ok(serde_json::from_str(&result)?);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut result = match mutation {
            ThreadMutation::AddRelated {
                thread,
                target,
                title,
            } => {
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                let target = match target {
                    RelatedTargetInput::Url { url } => {
                        hirsel_proto::ThreadRelatedTarget::Url { url: url.clone() }
                    }
                    RelatedTargetInput::Thread { thread } => {
                        hirsel_proto::ThreadRelatedTarget::Thread {
                            history_id: caller.history_id.clone(),
                            thread_id: thread_scope::resolve(&tx, caller.thread_id, thread)?,
                        }
                    }
                };
                let mut result = super::thread_related::add(&tx, id, &target, title.as_deref())?;
                result.related_items =
                    super::thread_related::list_scoped(&tx, id, caller.thread_id)?;
                serde_json::to_value(result)?
            }
            ThreadMutation::RemoveRelated { thread, item_id } => {
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                let mut result = super::thread_related::remove(&tx, id, *item_id)?;
                result.related_items =
                    super::thread_related::list_scoped(&tx, id, caller.thread_id)?;
                serde_json::to_value(result)?
            }
            ThreadMutation::Cancel { thread } => {
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                // A self-cancel revokes the caller as soon as cancellation is
                // requested, so its durable effect must join the transaction
                // before that revocation while the accepted turn is valid.
                super::thread_effects::record(
                    &tx,
                    caller,
                    super::thread_effects::NewEffect {
                        operation_id,
                        effect_index: 0,
                        tool: "threads_cancel",
                        effect: hirsel_proto::ThreadEffectKind::Edited,
                        target: hirsel_proto::ThreadEffectTarget::Thread { thread_id: id },
                        target_turn_id: None,
                        request_client_id: None,
                        refusal: None,
                    },
                )?;
                let turn = super::thread_archive::request_cancel(&tx, id, true, None)?
                    .first()
                    .copied();
                json!({"thread_id":id,"turn_id":turn,"cancellation_requested":turn.is_some()})
            }
            ThreadMutation::Archive { thread, archived } => {
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                // The caller's own turn survives, so a Thread that archives
                // itself finishes this turn and is stopped at the next wake.
                let outcome = super::thread_archive::apply(
                    &tx,
                    id,
                    *archived,
                    &super::thread_archive::ArchiveActor {
                        thread_id: caller.thread_id,
                        turn_id: Some(caller.turn_id),
                        actor: "agent",
                        keep_turn_id: Some(caller.turn_id),
                    },
                )?;
                json!({
                    "thread_id": id,
                    "archived": archived,
                    "threads": outcome.threads,
                    "cancelled_turn_ids": outcome.cancelled_turn_ids,
                    "activity": outcome.activity,
                })
            }
            ThreadMutation::Create {
                client_id,
                kind,
                title,
                icon,
                parent,
                description,
                instrument,
                attention,
            } => {
                anyhow::ensure!(
                    !title.trim().is_empty() && !client_id.is_empty(),
                    "creation requires title and client_id"
                );
                super::thread_icons::validate_icon(icon.as_ref())?;
                threads::validate_instrument(instrument.as_ref())?;
                let parent = thread_scope::resolve(&tx, caller.thread_id, parent)?;
                // 0 is the top of the tree, not a row: a Thread created there
                // hangs on no Thread at all, exactly like one the Owner makes.
                let parent = (parent != 0).then_some(parent);
                let key = format!("agent:{}:{operation_id}:{client_id}", caller.turn_id);
                let (symbol, tint, blob_id) = icon_columns(icon.as_ref());
                tx.execute("INSERT INTO threads(client_id,kind,parent_thread_id,title,description,instrument,attention,read,created_at,updated_at,revision,icon_symbol,icon_tint,icon_blob_id) VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8,?8,1,?9,?10,?11)",params![key,threads::kind_name(*kind),parent,title.trim(),description,instrument.as_ref().map(serde_json::to_string).transpose()?,threads::attention(*attention),now,symbol,tint,blob_id])?;
                let thread = threads::get(&tx, tx.last_insert_rowid() as u64)?;
                json!({"thread_id":thread.id,"thread":thread})
            }
            ThreadMutation::Update {
                thread,
                title,
                icon,
                showcased_artifact_id,
                description,
                instrument,
                attention,
            } => {
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                let previous_showcased_artifact_id = threads::get(&tx, id)?.showcased_artifact_id;
                if let Some(Some(artifact_id)) = showcased_artifact_id {
                    thread_scope::authorize_artifact(&tx, caller.thread_id, *artifact_id)?;
                }
                if let Some(icon) = icon {
                    super::thread_icons::validate_icon(icon.as_ref())?;
                }
                if let Some(title) = title {
                    threads::validate_thread_title(title)?;
                }
                if let Some(description) = description {
                    threads::validate_thread_description(description)?;
                }
                if let Some(instrument) = instrument {
                    threads::validate_instrument(instrument.as_ref())?;
                }
                let (symbol, tint, blob_id) = icon_columns(icon.as_ref().and_then(Option::as_ref));
                tx.execute("UPDATE threads SET title=COALESCE(?2,title),description=COALESCE(?3,description),instrument=CASE WHEN ?13 THEN ?4 ELSE instrument END,attention=COALESCE(?5,attention),icon_symbol=CASE WHEN ?7 THEN ?8 ELSE icon_symbol END,icon_tint=CASE WHEN ?7 THEN ?9 ELSE icon_tint END,icon_blob_id=CASE WHEN ?7 THEN ?10 ELSE icon_blob_id END,showcased_artifact_id=CASE WHEN ?11 THEN ?12 ELSE showcased_artifact_id END,updated_at=?6,revision=revision+1,read=0 WHERE id=?1",params![id,title,description,instrument.as_ref().and_then(Option::as_ref).map(serde_json::to_string).transpose()?,attention.map(threads::attention),now,icon.is_some(),symbol,tint,blob_id,showcased_artifact_id.is_some(),showcased_artifact_id.flatten(),instrument.is_some()])?;
                let mut result = json!({"thread_id":id,"thread":threads::get(&tx,id)?});
                if let Some(new) = showcased_artifact_id {
                    super::thread_showcase::touch_artifacts(
                        &tx,
                        previous_showcased_artifact_id,
                        *new,
                    )?;
                    result["previous_showcased_artifact_id"] =
                        json!(previous_showcased_artifact_id);
                }
                result
            }
            ThreadMutation::Activity { thread, kind, data } => {
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                anyhow::ensure!(
                    !kind.trim().is_empty() && data.is_object(),
                    "activity requires kind and object data"
                );
                anyhow::ensure!(
                    !matches!(
                        kind.as_str(),
                        "delegation_received" | "child_report" | "background_queued" | "refusal"
                    ),
                    "activity kind is reserved for host provenance"
                );
                let turn_id = (id == caller.thread_id).then_some(caller.turn_id);
                tx.execute("INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,?3,?4,?5)",params![id,turn_id,kind,serde_json::to_string(data)?,now])?;
                json!({"activity":{"id":tx.last_insert_rowid(),"thread_id":id,"turn_id":turn_id,"kind":kind,"data":data,"artifact_ids":[],"ts":now}})
            }
            ThreadMutation::Grant {
                thread,
                target,
                note,
            } => {
                // Both ends resolve against the caller's own reach first: a
                // Thread can only hand on what it already holds.
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                let target = match target {
                    GrantTargetRef::Root(_) => ReachTarget::Root,
                    GrantTargetRef::Thread(reference) => ReachTarget::Thread {
                        thread_id: thread_scope::resolve(&tx, caller.thread_id, reference)?,
                    },
                };
                super::thread_grants::authorize_widening(&tx, caller.thread_id, id, Some(target))?;
                serde_json::to_value(super::thread_grants::grant(
                    &tx,
                    id,
                    target,
                    &hirsel_proto::ThreadGrantSource::Thread {
                        thread_id: caller.thread_id,
                    },
                    note.as_deref(),
                )?)?
            }
            ThreadMutation::Revoke { thread, target } => {
                let id = thread_scope::resolve(&tx, caller.thread_id, thread)?;
                super::thread_grants::authorize_widening(&tx, caller.thread_id, id, None)?;
                serde_json::to_value(super::thread_grants::revoke(&tx, id, *target)?)?
            }
        };
        if let Some(id) = result.get("thread_id").and_then(Value::as_u64) {
            result["history_id"] = json!(caller.history_id);
            result["reference_url"] = json!(thread_scope::reference_url(&caller.history_id, id));
            let (tool, effect, request_client_id) = match mutation {
                ThreadMutation::AddRelated { .. } => (
                    "threads_add_related",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::RemoveRelated { .. } => (
                    "threads_remove_related",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::Cancel { .. } => (
                    "threads_cancel",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::Archive { archived: true, .. } => (
                    "threads_archive",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::Archive {
                    archived: false, ..
                } => (
                    "threads_unarchive",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::Create { client_id, .. } => (
                    "threads_create",
                    hirsel_proto::ThreadEffectKind::Created,
                    Some(client_id.as_str()),
                ),
                ThreadMutation::Update { .. } => (
                    "threads_update",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::Activity { .. } => (
                    "threads_activity",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::Grant { .. } => (
                    "threads_grant",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
                ThreadMutation::Revoke { .. } => (
                    "threads_revoke",
                    hirsel_proto::ThreadEffectKind::Edited,
                    None,
                ),
            };
            if !matches!(mutation, ThreadMutation::Cancel { .. }) {
                super::thread_effects::record(
                    &tx,
                    caller,
                    super::thread_effects::NewEffect {
                        operation_id,
                        effect_index: 0,
                        tool,
                        effect,
                        target: hirsel_proto::ThreadEffectTarget::Thread { thread_id: id },
                        target_turn_id: None,
                        request_client_id,
                        refusal: None,
                    },
                )?;
            }
        }
        tx.execute("INSERT INTO thread_mutation_receipts(turn_id,operation_id,payload,result) VALUES(?1,?2,?3,?4)",params![caller.turn_id,operation_id,payload,serde_json::to_string(&result)?])?;
        tx.commit()?;
        Ok(result)
    }
}
