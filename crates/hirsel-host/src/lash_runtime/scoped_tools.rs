//! One coordination facade for Lash attempts and private CLI MCP invocations.
use super::*;
use crate::storage::{Delegation, ThreadCaller, ThreadRef};

#[derive(Clone)]
pub(crate) struct ScopedThreadTools {
    pub(crate) tools: ToolSuite,
    pub(crate) caller: ThreadCaller,
    pub(crate) operation_id: String,
}
fn reference(args: &Value, key: &str) -> Result<ThreadRef, ToolError> {
    args.get(key)
        .map(|v| serde_json::from_value(v.clone()).map_err(ToolError::from))
        .transpose()
        .map(|v| v.unwrap_or_default())
}
/// A grant names a Thread — by ID or caller-relative path — or `"root"`.
fn grant_target(args: &Value) -> Result<crate::storage::GrantTargetRef, ToolError> {
    serde_json::from_value(args.get("target").cloned().ok_or("target required")?)
        .map_err(ToolError::from)
}
/// Revoking names the stored target exactly: a Thread ID, or `"root"`.
fn reach_target(args: &Value) -> Result<hirsel_proto::ReachTarget, ToolError> {
    serde_json::from_value(args.get("target").cloned().ok_or("target required")?)
        .map_err(ToolError::from)
}
fn refs(args: &Value) -> Result<Vec<u64>, ToolError> {
    serde_json::from_value(
        args.get("artifact_ids")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(ToolError::from)
}
impl ScopedThreadTools {
    pub(crate) async fn resolve(&self, args: &Value, key: &str) -> Result<u64, ToolError> {
        self.tools
            .storage()
            .resolve_thread(&self.caller, &reference(args, key)?)
            .await
            .map_err(ToolError::from)
    }
    /// Every ID is addressable. A call that lands outside this Thread's reach
    /// comes back as a readable result — `{refused:true,...}` — and leaves one
    /// durable activity row per operation, so the Owner and the Thread's
    /// requester both see exactly what was tried. Distinct probes log distinct
    /// facts; transport replay of one operation reuses its original receipt.
    pub(crate) async fn execute(&self, name: &str, args: &Value) -> Result<Value, String> {
        match self.dispatch(name, args).await {
            Ok(value) => Ok(value),
            Err(ToolError::Message(message)) => Err(message),
            Err(ToolError::Refused(refusal)) => self.refuse(name, refusal).await,
        }
    }
    async fn refuse(
        &self,
        name: &str,
        refusal: crate::storage::OutsideGrant,
    ) -> Result<Value, String> {
        let storage = self.tools.storage();
        let reach = storage
            .thread_reach(&self.caller)
            .await
            .unwrap_or_else(|_| "self + subtree".into());
        let result = json!({
            "refused": true,
            "reason": refusal.reason.as_str(),
            "target": refusal.target,
            "tool": name,
            "grant_summary": reach,
            "detail": refusal.to_string(),
        });
        let activity = storage
            .record_refusal(&self.caller, &self.operation_id, name, &result)
            .await
            .map_err(|e| e.to_string())?;
        self.tools.publish_thread_activity(activity).await;
        Ok(result)
    }
    async fn dispatch(&self, name: &str, args: &Value) -> Result<Value, ToolError> {
        let definition = scoped_mcp_catalog(&self.tools)
            .into_iter()
            .find(|tool| tool["name"] == name)
            .ok_or_else(|| "tool is unavailable in this scope".to_string())?;
        let validator = jsonschema::JSONSchema::compile(&definition["inputSchema"])
            .map_err(|e| e.to_string())?;
        validator
            .validate(args)
            .map_err(|errors| errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; "))?;
        let storage = self.tools.storage();
        match name {
            "threads_add_related" => {
                self.thread_mutation(crate::storage::ThreadMutation::AddRelated {
                    thread: reference(args, "thread")?,
                    target: serde_json::from_value::<crate::storage::RelatedTargetInput>(
                        args["target"].clone(),
                    )
                    .map_err(ToolError::from)?,
                    title: optional_string_any_allow_empty(args, &["title"])?,
                })
                .await
            }
            "threads_remove_related" => {
                self.thread_mutation(crate::storage::ThreadMutation::RemoveRelated {
                    thread: reference(args, "thread")?,
                    item_id: args["item_id"]
                        .as_u64()
                        .ok_or("item_id must be a positive integer")?,
                })
                .await
            }
            "threads_archive" | "threads_unarchive" => {
                self.thread_mutation(crate::storage::ThreadMutation::Archive {
                    thread: reference(args, "thread")?,
                    archived: name == "threads_archive",
                })
                .await
            }
            "threads_cancel" => {
                self.thread_mutation(crate::storage::ThreadMutation::Cancel {
                    thread: reference(args, "thread")?,
                })
                .await
            }
            "shell_run" => {
                let guard = storage
                    .execution_guard(&self.caller)
                    .await
                    .map_err(ToolError::from)?;
                let running = crate::process_run::start_bash_command(
                    required_string(args, "cmd")?,
                    optional_path(args, "cwd")?,
                )
                .map_err(ToolError::from)?;
                drop(guard);
                let output = running
                    .finish(Duration::from_secs(
                        args.get("timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(30)
                            .min(600),
                    ))
                    .await
                    .map_err(ToolError::from)?;
                let output = crate::tools::shell::shell_output(output);
                // Native coding tools retain filesystem access; coordination authority
                // is checked again before exposing an awaited result.
                storage
                    .thread_context(&self.caller)
                    .await
                    .map_err(ToolError::from)?;
                shell_run_result(&output)
            }
            "views_show" => self.views_show(args).await,
            "views_update" => self.views_update(args).await,
            "views_clear" => self.views_clear(args).await,
            "views_list_templates" => self.views_list_templates().await,
            "artifacts_create" | "artifacts_edit" | "artifacts_show" => {
                self.artifact_mutation(name, args).await
            }
            "threads_context" => serde_json::to_value(
                storage
                    .thread_context(&self.caller)
                    .await
                    .map_err(ToolError::from)?,
            )
            .map_err(ToolError::from),
            "threads_list" => {
                let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(1);
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50);
                if depth > 8 || limit > 100 {
                    return Err("invalid Thread page bounds".into());
                }
                serde_json::to_value(
                    storage
                        .scoped_thread_list(
                            &self.caller,
                            &reference(args, "under")?,
                            depth as u32,
                            args.get("after_id").and_then(Value::as_u64),
                            limit as u32,
                        )
                        .await
                        .map_err(ToolError::from)?,
                )
                .map_err(ToolError::from)
            }
            "threads_create" => {
                self.resolve(args, "parent").await?;
                let icon = self.prepared_icon(args).await?.flatten();
                let mutation = crate::storage::ThreadMutation::Create {
                    client_id: required_string(args, "client_id")?,
                    kind: serde_json::from_value(args.get("kind").cloned().ok_or("kind required")?)
                        .map_err(ToolError::from)?,
                    title: required_string(args, "title")?,
                    icon,
                    parent: reference(args, "parent")?,
                    description: optional_string_any_allow_empty(args, &["description"])?
                        .unwrap_or_default(),
                    instrument: args
                        .get("instrument")
                        .filter(|value| !value.is_null())
                        .cloned(),
                    attention: args
                        .get("attention")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()
                        .map_err(ToolError::from)?
                        .unwrap_or_default(),
                };
                self.thread_mutation(mutation).await
            }
            "threads_read" => {
                let cursor = args
                    .get("cursor")
                    .filter(|v| !v.is_null())
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(ToolError::from)?;
                serde_json::to_value(
                    storage
                        .scoped_thread_read(
                            &self.caller,
                            Some(&self.operation_id),
                            &reference(args, "thread")?,
                            cursor,
                            args.get("limit").and_then(Value::as_u64).unwrap_or(30),
                        )
                        .await
                        .map_err(ToolError::from)?,
                )
                .map_err(ToolError::from)
            }
            "threads_update" => {
                self.resolve(args, "thread").await?;
                let icon = self.prepared_icon(args).await?;
                self.thread_mutation(crate::storage::ThreadMutation::Update {
                    thread: reference(args, "thread")?,
                    title: optional_string_any_allow_empty(args, &["title"])?,
                    icon,
                    showcased_artifact_id: crate::storage::parse_showcase(
                        args,
                        "showcased_artifact_id",
                    )
                    .map_err(ToolError::from)?,
                    description: optional_string_any_allow_empty(args, &["description"])?,
                    instrument: args
                        .get("instrument")
                        .map(|value| (!value.is_null()).then(|| value.clone())),
                    attention: args
                        .get("attention")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()
                        .map_err(ToolError::from)?,
                })
                .await
            }
            "threads_grant" => {
                self.thread_mutation(crate::storage::ThreadMutation::Grant {
                    thread: reference(args, "thread")?,
                    target: grant_target(args)?,
                    note: optional_string_any_allow_empty(args, &["note"])?,
                })
                .await
            }
            "threads_revoke" => {
                self.thread_mutation(crate::storage::ThreadMutation::Revoke {
                    thread: reference(args, "thread")?,
                    target: reach_target(args)?,
                })
                .await
            }
            "threads_activity" => {
                self.thread_mutation(crate::storage::ThreadMutation::Activity {
                    thread: reference(args, "thread")?,
                    kind: required_string(args, "kind")?,
                    data: args.get("data").cloned().ok_or("data required")?,
                })
                .await
            }
            "threads_delegate" | "threads_send" => {
                if let Some(receipt) = storage
                    .delegation_receipt(&self.caller, &self.operation_id, args)
                    .await
                    .map_err(ToolError::from)?
                {
                    return serde_json::to_value(receipt).map_err(ToolError::from);
                }
                let assignment = if name == "threads_delegate" {
                    self.resolve_assignment(args).await?
                } else {
                    let child = self.resolve(args, "thread").await?;
                    // The fence only root reach opens: work reports upward, it
                    // never messages upward unless it addresses everything.
                    storage.refuse_upward(&self.caller, child).await?;
                    Delegation {
                        title: "Follow-up".into(),
                        brief: required_string(args, "text")?,
                        artifact_ids: refs(args)?,
                        child_thread_id: Some(child),
                        execution: None,
                    }
                };
                let accepted = storage
                    .delegate_thread(&self.caller, &self.operation_id, name, &assignment, args)
                    .await
                    .map_err(ToolError::from)?;
                // Runtime admission polls the committed outbox; returning never
                // holds the parent lane waiting for child completion.
                let detail = storage
                    .thread_detail(accepted.thread_id, None, 1)
                    .await
                    .map_err(ToolError::from)?;
                self.tools
                    .publish_thread(&self.caller.history_id, detail.thread)
                    .await;
                for turn in detail
                    .turns
                    .into_iter()
                    .filter(|t| t.id == accepted.turn_id)
                {
                    self.tools.publish_thread_turn(turn).await;
                }
                for activity in detail
                    .activities
                    .into_iter()
                    .filter(|a| a.turn_id == Some(accepted.turn_id))
                {
                    self.tools.publish_thread_activity(activity).await;
                }
                serde_json::to_value(accepted).map_err(ToolError::from)
            }
            "threads_report" => {
                let id = storage
                    .report_thread_progress(
                        &self.caller,
                        &self.operation_id,
                        &required_string(args, "summary")?,
                        &refs(args)?,
                    )
                    .await
                    .map_err(ToolError::from)?;
                self.tools
                    .emit_thread_trigger(
                        THREAD_REPORTED_SOURCE_TYPE,
                        THREAD_REPORTED_EVENT_TYPE,
                        self.caller.thread_id,
                        required_string(args, "summary")?,
                        format!("thread-report:{id}"),
                    )
                    .await;
                Ok(json!({"activity_id":id}))
            }
            "artifacts_list" => {
                let under = self.resolve(args, "thread").await?;
                let artifacts = storage
                    .scoped_artifacts(&self.caller, Some(&self.operation_id), under)
                    .await
                    .map_err(ToolError::from)?;
                Ok(json!({"artifacts":artifacts}))
            }
            _ => match self
                .tools
                .plugin_tools()
                .call(
                    name,
                    args.clone(),
                    self.tools.clone(),
                    self.caller.clone(),
                    self.operation_id.clone(),
                )
                .await
            {
                Some(result) => result.map_err(ToolError::from),
                None => Err(format!("Unknown scoped tool: {name}").into()),
            },
        }
    }
}

pub(crate) fn scoped_mcp_catalog(tools: &ToolSuite) -> Vec<Value> {
    let mut definitions = hirsel_tool_definitions(&tools.subagent_model_snapshot());
    definitions.extend(tools.plugin_tools().definitions());
    definitions.into_iter().map(|d|json!({"name":d.name(),"description":d.manifest.description,"inputSchema":d.contract.input_schema.canonical})).collect()
}

impl ScopedThreadTools {
    pub(super) async fn views_show(&self, args: &Value) -> Result<Value, ToolError> {
        let template_id = optional_string(args, "template_id")?;
        let spec = args.get("spec").cloned().filter(|value| !value.is_null());
        let params = args.get("params").cloned().filter(|value| !value.is_null());
        let instance_id = optional_string(args, "instance_id")?;
        let storage = self.tools.storage();
        let _execution = storage
            .execution_guard(&self.caller)
            .await
            .map_err(ToolError::from)?;
        let view = self
            .tools
            .views_show(
                &self.caller.history_id,
                self.caller.thread_id,
                template_id,
                spec,
                params,
                instance_id,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(view_instance_result(&view))
    }

    pub(super) async fn views_update(&self, args: &Value) -> Result<Value, ToolError> {
        let instance_id = required_string(args, "instance_id")?;
        let view = self
            .tools
            .view(&instance_id)
            .await
            .ok_or("view is unavailable")?;
        self.tools
            .storage()
            .resolve_thread(&self.caller, &crate::storage::ThreadRef::Id(view.thread_id))
            .await
            .map_err(ToolError::from)?;
        let params = args.get("params").cloned().filter(|value| !value.is_null());
        let patch = args.get("patch").cloned().filter(|value| !value.is_null());
        let storage = self.tools.storage();
        let _execution = storage
            .execution_guard(&self.caller)
            .await
            .map_err(ToolError::from)?;
        let view = self
            .tools
            .views_update(
                &self.caller.history_id,
                view.thread_id,
                &instance_id,
                params,
                patch,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(view_instance_result(&view))
    }

    pub(super) async fn views_clear(&self, args: &Value) -> Result<Value, ToolError> {
        let instance_id = required_string(args, "instance_id")?;
        let view = self
            .tools
            .view(&instance_id)
            .await
            .ok_or("view is unavailable")?;
        self.tools
            .storage()
            .resolve_thread(&self.caller, &crate::storage::ThreadRef::Id(view.thread_id))
            .await
            .map_err(ToolError::from)?;
        let storage = self.tools.storage();
        let _execution = storage
            .execution_guard(&self.caller)
            .await
            .map_err(ToolError::from)?;
        self.tools
            .views_clear(&self.caller.history_id, view.thread_id, &instance_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true, "instance_id": instance_id }))
    }

    pub(super) async fn views_list_templates(&self) -> Result<Value, ToolError> {
        let templates = self
            .tools
            .views_list_templates()
            .await
            .map_err(|error| error.to_string())?;
        Ok(serde_json::to_value(templates)?)
    }
}

impl ScopedThreadTools {
    async fn prepared_icon(
        &self,
        args: &Value,
    ) -> Result<Option<Option<hirsel_proto::ThreadIcon>>, ToolError> {
        let parsed = crate::storage::parse_agent_icon(args).map_err(ToolError::from)?;
        match parsed {
            None => Ok(None),
            Some(None) => Ok(Some(None)),
            Some(Some(source)) => {
                let client_id = format!(
                    "agent-thread-icon:{}:{}",
                    self.caller.turn_id, self.operation_id
                );
                self.tools
                    .storage()
                    .prepare_agent_thread_icon(&self.caller, source, &client_id)
                    .await
                    .map(Some)
                    .map(Some)
                    .map_err(ToolError::from)
            }
        }
    }

    async fn thread_mutation(
        &self,
        mutation: crate::storage::ThreadMutation,
    ) -> Result<Value, ToolError> {
        let result = self
            .tools
            .storage()
            .mutate_scoped_thread(&self.caller, &self.operation_id, &mutation)
            .await
            .map_err(ToolError::from)?;
        if result.get("grants").is_some() {
            let snapshot = serde_json::from_value::<crate::storage::ThreadGrants>(result.clone())?;
            self.tools.publish_thread_grants(None, snapshot).await?;
            return Ok(result);
        }
        if result.get("related_items").is_some() {
            let links = serde_json::from_value::<crate::storage::ThreadRelated>(result.clone())
                .map_err(ToolError::from)?;
            self.tools
                .publish_thread_related(None, links)
                .await
                .map_err(ToolError::from)?;
            return Ok(result);
        }
        if result.get("previous_showcased_artifact_id").is_some() {
            let ids = result["previous_showcased_artifact_id"]
                .as_u64()
                .into_iter()
                .chain(result["thread"]["showcased_artifact_id"].as_u64())
                .collect::<Vec<_>>();
            self.tools
                .publish_showcase_artifacts(&self.caller.history_id, &ids)
                .await
                .map_err(ToolError::from)?;
        }
        if let Some(threads) = result.get("threads").and_then(Value::as_array) {
            // An archive moves a whole subtree at once; every affected Thread
            // is published so each client's tree updates live.
            for thread in threads {
                self.tools
                    .publish_thread(
                        &self.caller.history_id,
                        serde_json::from_value(thread.clone()).map_err(ToolError::from)?,
                    )
                    .await;
            }
        }
        if let Some(thread) = result.get("thread") {
            self.tools
                .publish_thread(
                    &self.caller.history_id,
                    serde_json::from_value(thread.clone()).map_err(ToolError::from)?,
                )
                .await;
        }
        if let Some(activity) = result.get("activity") {
            self.tools
                .publish_thread_activity(
                    serde_json::from_value(activity.clone()).map_err(ToolError::from)?,
                )
                .await;
        }
        Ok(result)
    }
}

#[derive(serde::Deserialize)]
struct DelegateCommon {
    title: String,
    brief: String,
    artifact_ids: Vec<u64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum NativeAgent {
    Native,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum CliAgent {
    Claude,
    Codex,
}

#[derive(serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum DelegateInput {
    Existing {
        #[serde(flatten)]
        common: DelegateCommon,
        child_thread_id: u64,
    },
    Cli {
        #[serde(flatten)]
        common: DelegateCommon,
        child_thread_id: Option<u64>,
        agent: CliAgent,
        model: Option<String>,
        variant: Option<String>,
        cwd: Option<std::path::PathBuf>,
    },
    /// Naming no agent at all is Native: the ordinary delegation is a child
    /// Thread that runs the way this one does.
    Native {
        #[serde(flatten)]
        common: DelegateCommon,
        child_thread_id: Option<u64>,
        agent: Option<NativeAgent>,
        provider_id: Option<String>,
        model: Option<String>,
        cwd: Option<std::path::PathBuf>,
    },
}
impl ScopedThreadTools {
    /// The provider and model this Thread's own accepted turn runs on, so a
    /// delegation that names neither lands on the same route. A caller without
    /// a captured Native execution (a CLI Thread, or a turn whose capture is
    /// gone) inherits nothing and falls back to the Settings default.
    async fn inherited_native_selectors(&self) -> (Option<String>, Option<String>) {
        match self
            .tools
            .storage()
            .turn_execution(self.caller.turn_id)
            .await
        {
            Ok(crate::storage::ThreadExecution::Native {
                provider_id, model, ..
            }) => (Some(provider_id), Some(model.id)),
            _ => (None, None),
        }
    }

    async fn resolve_assignment(&self, args: &Value) -> Result<Delegation, ToolError> {
        let input: DelegateInput = serde_json::from_value(args.clone()).map_err(ToolError::from)?;
        use crate::execution_selection::{ExecutionSelectors, resolve_execution};
        let (input, child_thread_id, selectors) = match input {
            DelegateInput::Existing {
                common,
                child_thread_id,
            } => (common, Some(child_thread_id), None),
            DelegateInput::Native {
                common,
                child_thread_id,
                agent,
                provider_id,
                model,
                cwd,
            } => {
                let agent = match agent {
                    None | Some(NativeAgent::Native) => "native",
                };
                // A child inherits the provider and model this Thread runs on
                // unless the delegation names its own: the default answer to
                // "where does this run" is "here".
                let (provider_id, model) = match (provider_id, model) {
                    (None, None) => self.inherited_native_selectors().await,
                    named => named,
                };
                (
                    common,
                    child_thread_id,
                    Some(ExecutionSelectors {
                        agent: Some(agent.into()),
                        provider_id,
                        model,
                        variant: None,
                        cwd,
                    }),
                )
            }
            DelegateInput::Cli {
                common,
                child_thread_id,
                agent,
                model,
                variant,
                cwd,
            } => (
                common,
                child_thread_id,
                Some(ExecutionSelectors {
                    agent: Some(
                        match agent {
                            CliAgent::Claude => "claude",
                            CliAgent::Codex => "codex",
                        }
                        .into(),
                    ),
                    provider_id: None,
                    model,
                    variant,
                    cwd,
                }),
            ),
        };
        let execution = match selectors {
            Some(selectors) => Some(resolve_execution(&self.tools, selectors).await?),
            // An existing child with no new selectors keeps its accepted
            // backend; the delegation path captures it atomically.
            None => None,
        };
        let brief = self
            .tools
            .expand_skill(&input.brief)
            .map_err(ToolError::from)?;
        Ok(Delegation {
            title: input.title,
            brief,
            artifact_ids: input.artifact_ids,
            child_thread_id,
            execution,
        })
    }
}

#[cfg(test)]
mod delegate_input_tests {
    use super::*;

    #[test]
    fn delegation_variants_reject_mixed_selectors() {
        let base = json!({"title":"Work", "brief":"Do it", "artifact_ids":[]});
        let parse = |extra: Value| {
            let mut value = base.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<DelegateInput>(value)
        };
        assert!(matches!(
            parse(json!({"child_thread_id":7})).unwrap(),
            DelegateInput::Existing {
                child_thread_id: 7,
                ..
            }
        ));
        // Naming no agent at all is Native, and it inherits both selectors.
        assert!(matches!(
            parse(json!({})).unwrap(),
            DelegateInput::Native {
                agent: None,
                provider_id: None,
                model: None,
                ..
            }
        ));
        assert!(matches!(
            parse(json!({"agent":"native"})).unwrap(),
            DelegateInput::Native {
                agent: Some(NativeAgent::Native),
                ..
            }
        ));
        // Native takes a provider, a model and a working directory of its own.
        assert!(matches!(
            parse(json!({"agent":"native","provider_id":"acme","model":"m","cwd":"/tmp"})).unwrap(),
            DelegateInput::Native { .. }
        ));
        assert!(matches!(
            parse(json!({"model":"m"})).unwrap(),
            DelegateInput::Native {
                agent: None,
                model: Some(_),
                ..
            }
        ));
        for agent in ["claude", "codex"] {
            assert!(matches!(
                parse(json!({"agent":agent,"model":"m","variant":"high"})).unwrap(),
                DelegateInput::Cli { .. }
            ));
        }
        for invalid in [
            // Native has no reasoning variant: that is a CLI selector.
            json!({"agent":"native","variant":"high"}),
            json!({"variant":"high"}),
            json!({"agent":"codex","provider_id":"router"}),
            json!({"child_thread_id":7,"provider_id":"router","agent":"codex"}),
            json!({"agent":"typo"}),
            json!({"unknown":true}),
        ] {
            assert!(parse(invalid.clone()).is_err(), "{invalid}");
        }
    }
}
