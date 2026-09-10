//! One coordination facade for Lash attempts and private CLI MCP invocations.
use super::*;
use crate::storage::{Delegation, ThreadCaller, ThreadRef};

#[derive(Clone)]
pub(crate) struct ScopedThreadTools {
    pub(crate) tools: ToolSuite,
    pub(crate) caller: ThreadCaller,
    pub(crate) operation_id: String,
}
fn reference(args: &Value, key: &str) -> Result<ThreadRef, String> {
    args.get(key)
        .map(|v| serde_json::from_value(v.clone()).map_err(|e| e.to_string()))
        .transpose()
        .map(|v| v.unwrap_or_default())
}
fn refs(args: &Value) -> Result<Vec<u64>, String> {
    serde_json::from_value(
        args.get("artifact_ids")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(|e| e.to_string())
}
impl ScopedThreadTools {
    pub(crate) async fn resolve(&self, args: &Value, key: &str) -> Result<u64, String> {
        self.tools
            .storage()
            .resolve_thread(&self.caller, &reference(args, key)?)
            .await
            .map_err(|e| e.to_string())
    }
    pub(crate) async fn execute(&self, name: &str, args: &Value) -> Result<Value, String> {
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
                    .map_err(|e| e.to_string())?,
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
                    .map_err(|e| e.to_string())?;
                let running = crate::process_run::start_bash_command(
                    required_string(args, "cmd")?,
                    optional_path(args, "cwd")?,
                )
                .map_err(|e| e.to_string())?;
                drop(guard);
                let output = running
                    .finish(Duration::from_secs(
                        args.get("timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(30)
                            .min(600),
                    ))
                    .await
                    .map_err(|e| e.to_string())?;
                let output = crate::tools::shell::shell_output(output);
                // Native coding tools retain filesystem access; coordination authority
                // is checked again before exposing an awaited result.
                storage
                    .thread_context(&self.caller)
                    .await
                    .map_err(|e| e.to_string())?;
                shell_run_result(&output)
            }
            "monitors_create" => {
                let record = storage
                    .create_scoped_monitor(
                        &self.caller,
                        required_string(args, "cmd")?,
                        args.get("every_secs").and_then(Value::as_u64).unwrap_or(30),
                        parse_monitor_wake_on(&required_string(args, "wake_on")?)?,
                        optional_string(args, "pattern")?,
                        required_string(args, "label")?,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                self.tools.broadcast_monitor_upsert(&record);
                monitors_create_result(&record)
            }
            "monitors_list" => monitors_list_result(
                &storage
                    .scoped_monitors(&self.caller)
                    .await
                    .map_err(|e| e.to_string())?,
            ),
            "monitors_cancel" => {
                let record = storage
                    .cancel_scoped_monitor(&self.caller, &required_string(args, "monitor_id")?)
                    .await
                    .map_err(|e| e.to_string())?;
                self.tools.broadcast_monitor_upsert(&record);
                Ok(monitors_cancel_result(&record.id))
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
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
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
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
            }
            "threads_create" => {
                let mutation = crate::storage::ThreadMutation::Create {
                    client_id: required_string(args, "client_id")?,
                    title: required_string(args, "title")?,
                    icon: crate::storage::parse_icon(args)
                        .map_err(|e| e.to_string())?
                        .flatten(),
                    parent: reference(args, "parent")?,
                    description: optional_string_any_allow_empty(args, &["description"])?
                        .unwrap_or_default(),
                    instrument: args.get("instrument").cloned().unwrap_or(Value::Null),
                    attention: args
                        .get("attention")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()
                        .map_err(|e| e.to_string())?
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
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(
                    storage
                        .scoped_thread_read(
                            &self.caller,
                            &reference(args, "thread")?,
                            cursor,
                            args.get("limit").and_then(Value::as_u64).unwrap_or(30),
                        )
                        .await
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
            }
            "threads_update" => {
                self.thread_mutation(crate::storage::ThreadMutation::Update {
                    thread: reference(args, "thread")?,
                    title: optional_string_any_allow_empty(args, &["title"])?,
                    icon: crate::storage::parse_icon(args).map_err(|e| e.to_string())?,
                    showcased_artifact_id: crate::storage::parse_showcase(
                        args,
                        "showcased_artifact_id",
                    )
                    .map_err(|e| e.to_string())?,
                    description: optional_string_any_allow_empty(args, &["description"])?,
                    instrument: args.get("instrument").cloned(),
                    attention: args
                        .get("attention")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()
                        .map_err(|e| e.to_string())?,
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
                    .map_err(|e| e.to_string())?
                {
                    return serde_json::to_value(receipt).map_err(|e| e.to_string());
                }
                let assignment = if name == "threads_delegate" {
                    self.resolve_assignment(args).await?
                } else {
                    let child = self.resolve(args, "thread").await?;
                    Delegation {
                        title: "Follow-up".into(),
                        brief: required_string(args, "text")?,
                        artifact_ids: refs(args)?,
                        child_thread_id: Some(child),
                        execution: None,
                    }
                };
                let accepted = storage
                    .delegate_thread(&self.caller, &self.operation_id, &assignment, args)
                    .await
                    .map_err(|e| e.to_string())?;
                // Runtime admission polls the committed outbox; returning never
                // holds the parent lane waiting for child completion.
                let detail = storage
                    .thread_detail(accepted.thread_id, None, 1)
                    .await
                    .map_err(|e| e.to_string())?;
                self.tools.publish_thread(detail.thread).await;
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
                serde_json::to_value(accepted).map_err(|e| e.to_string())
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
                    .map_err(|e| e.to_string())?;
                Ok(json!({"activity_id":id}))
            }
            "artifacts_list" => {
                let under = self.resolve(args, "thread").await?;
                let artifacts = storage
                    .scoped_artifacts(&self.caller, under)
                    .await
                    .map_err(|e| e.to_string())?;
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
                Some(result) => result,
                None => Err(format!("Unknown scoped tool: {name}")),
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
    pub(super) async fn views_show(&self, args: &Value) -> Result<Value, String> {
        let template_id = optional_string(args, "template_id")?;
        let spec = args.get("spec").cloned().filter(|value| !value.is_null());
        let params = args.get("params").cloned().filter(|value| !value.is_null());
        let instance_id = optional_string(args, "instance_id")?;
        let placement = required_string(args, "placement")?;
        let storage = self.tools.storage();
        let _execution = storage
            .execution_guard(&self.caller)
            .await
            .map_err(|e| e.to_string())?;
        let view = self
            .tools
            .views_show(
                &self.caller.history_id,
                self.caller.thread_id,
                template_id,
                spec,
                params,
                instance_id,
                placement,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(view_instance_result(&view))
    }

    pub(super) async fn views_update(&self, args: &Value) -> Result<Value, String> {
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
            .map_err(|e| e.to_string())?;
        let params = args.get("params").cloned().filter(|value| !value.is_null());
        let patch = args.get("patch").cloned().filter(|value| !value.is_null());
        let storage = self.tools.storage();
        let _execution = storage
            .execution_guard(&self.caller)
            .await
            .map_err(|e| e.to_string())?;
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

    pub(super) async fn views_clear(&self, args: &Value) -> Result<Value, String> {
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
            .map_err(|e| e.to_string())?;
        let storage = self.tools.storage();
        let _execution = storage
            .execution_guard(&self.caller)
            .await
            .map_err(|e| e.to_string())?;
        self.tools
            .views_clear(&self.caller.history_id, view.thread_id, &instance_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true, "instance_id": instance_id }))
    }

    pub(super) async fn views_list_templates(&self) -> Result<Value, String> {
        let templates = self
            .tools
            .views_list_templates()
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(templates).map_err(|error| error.to_string())
    }
}

impl ScopedThreadTools {
    async fn thread_mutation(
        &self,
        mutation: crate::storage::ThreadMutation,
    ) -> Result<Value, String> {
        let result = self
            .tools
            .storage()
            .mutate_scoped_thread(&self.caller, &self.operation_id, &mutation)
            .await
            .map_err(|e| e.to_string())?;
        if result.get("related_items").is_some() {
            let links = serde_json::from_value::<crate::storage::ThreadRelated>(result.clone())
                .map_err(|e| e.to_string())?;
            self.tools
                .publish_thread_related(None, links)
                .await
                .map_err(|e| e.to_string())?;
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
                .map_err(|e| e.to_string())?;
        }
        if let Some(thread) = result.get("thread") {
            self.tools
                .publish_thread(serde_json::from_value(thread.clone()).map_err(|e| e.to_string())?)
                .await;
        }
        if let Some(activity) = result.get("activity") {
            self.tools
                .publish_thread_activity(
                    serde_json::from_value(activity.clone()).map_err(|e| e.to_string())?,
                )
                .await;
        }
        Ok(result)
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateInput {
    title: String,
    brief: String,
    artifact_ids: Vec<u64>,
    child_thread_id: Option<u64>,
    agent: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    cwd: Option<std::path::PathBuf>,
}
impl ScopedThreadTools {
    async fn resolve_assignment(&self, args: &Value) -> Result<Delegation, String> {
        let input: DelegateInput =
            serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
        let execution = if input.agent.is_none()
            && input.model.is_none()
            && input.variant.is_none()
            && input.cwd.is_none()
            && input.child_thread_id.is_some()
        {
            None
        } else if input.agent.as_deref() == Some("host") {
            if input.model.is_some() || input.variant.is_some() || input.cwd.is_some() {
                return Err(
                    "host delegation uses configured provider/model; CLI selectors do not apply"
                        .into(),
                );
            }
            Some(
                self.tools
                    .storage()
                    .host_execution_default()
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            let agent = parse_agent_kind(input.agent.as_deref().unwrap_or("claude"))?;
            let selected = self
                .tools
                .resolve_thread_cli_model(agent, input.model.as_deref(), input.variant.as_deref())
                .map_err(|e| e.to_string())?;
            let cwd = input
                .cwd
                .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
            let cwd = std::fs::canonicalize(cwd)
                .map_err(|e| format!("invalid execution directory: {e}"))?;
            Some(crate::storage::ThreadExecution::Cli {
                agent,
                model: selected.model_id,
                variant: selected.variant,
                cwd,
            })
        };
        Ok(Delegation {
            title: input.title,
            brief: input.brief,
            artifact_ids: input.artifact_ids,
            child_thread_id: input.child_thread_id,
            execution,
        })
    }
}
