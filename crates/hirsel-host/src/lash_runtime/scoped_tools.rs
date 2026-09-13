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
                self.resolve(args, "parent").await?;
                let icon = self.prepared_icon(args).await?.flatten();
                let mutation = crate::storage::ThreadMutation::Create {
                    client_id: required_string(args, "client_id")?,
                    kind: serde_json::from_value(args.get("kind").cloned().ok_or("kind required")?)
                        .map_err(|e| e.to_string())?,
                    title: required_string(args, "title")?,
                    icon,
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
    async fn prepared_icon(
        &self,
        args: &Value,
    ) -> Result<Option<Option<hirsel_proto::ThreadIcon>>, String> {
        let parsed = crate::storage::parse_agent_icon(args).map_err(|e| e.to_string())?;
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
                    .map_err(|e| e.to_string())
            }
        }
    }

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
                .publish_thread(
                    &self.caller.history_id,
                    serde_json::from_value(thread.clone()).map_err(|e| e.to_string())?,
                )
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
struct DelegateCommon {
    title: String,
    brief: String,
    artifact_ids: Vec<u64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum HostAgent {
    Host,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum NativeAgent {
    Lash,
}
#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum CliAgent {
    #[default]
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
    Host {
        #[serde(flatten)]
        common: DelegateCommon,
        child_thread_id: Option<u64>,
        agent: HostAgent,
    },
    Native {
        #[serde(flatten)]
        common: DelegateCommon,
        child_thread_id: Option<u64>,
        agent: NativeAgent,
        provider_id: Option<String>,
        model: Option<String>,
        variant: Option<String>,
        cwd: Option<std::path::PathBuf>,
    },
    Cli {
        #[serde(flatten)]
        common: DelegateCommon,
        child_thread_id: Option<u64>,
        #[serde(default)]
        agent: CliAgent,
        model: Option<String>,
        variant: Option<String>,
        cwd: Option<std::path::PathBuf>,
    },
}
impl ScopedThreadTools {
    async fn resolve_assignment(&self, args: &Value) -> Result<Delegation, String> {
        let input: DelegateInput =
            serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
        use crate::execution_selection::{ExecutionSelectors, resolve_execution};
        let (input, child_thread_id, selectors) = match input {
            DelegateInput::Existing {
                common,
                child_thread_id,
            } => (common, Some(child_thread_id), None),
            DelegateInput::Host {
                common,
                child_thread_id,
                agent: HostAgent::Host,
            } => (
                common,
                child_thread_id,
                Some(ExecutionSelectors {
                    agent: Some("host".into()),
                    ..Default::default()
                }),
            ),
            DelegateInput::Native {
                common,
                child_thread_id,
                agent: NativeAgent::Lash,
                provider_id,
                model,
                variant,
                cwd,
            } => (
                common,
                child_thread_id,
                Some(ExecutionSelectors {
                    agent: Some("lash".into()),
                    provider_id,
                    model,
                    variant,
                    cwd,
                }),
            ),
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
        let execution = if let Some(selectors) = selectors {
            Some(resolve_execution(&self.tools, selectors).await?)
        } else {
            let effective = self
                .tools
                .storage()
                .effective_child_execution(
                    &self.caller,
                    child_thread_id.expect("existing-child variant has an identity"),
                )
                .await
                .map_err(|error| error.to_string())?;
            matches!(
                effective,
                crate::storage::ThreadExecution::LashWorker { .. }
            )
            .then_some(effective)
        };
        let native_worker = matches!(
            &execution,
            Some(crate::storage::ThreadExecution::LashWorker { .. })
        );
        if native_worker && !input.artifact_ids.is_empty() {
            return Err(
                "native Lash worker artifact references are not supported yet; remove artifact_ids or delegate to another backend"
                    .into(),
            );
        }
        let brief = if native_worker {
            self.tools
                .expand_skill(&input.brief)
                .map_err(|e| e.to_string())?
        } else {
            input.brief
        };
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
        assert!(matches!(
            parse(json!({"agent":"host"})).unwrap(),
            DelegateInput::Host { .. }
        ));
        assert!(matches!(
            parse(json!({"agent":"lash","provider_id":"router","model":"m"})).unwrap(),
            DelegateInput::Native { .. }
        ));
        for agent in ["claude", "codex"] {
            assert!(matches!(
                parse(json!({"agent":agent,"model":"m","variant":"high"})).unwrap(),
                DelegateInput::Cli { .. }
            ));
        }
        assert!(matches!(
            parse(json!({})).unwrap(),
            DelegateInput::Cli {
                agent: CliAgent::Claude,
                ..
            }
        ));
        for invalid in [
            json!({"agent":"host","model":"m"}),
            json!({"agent":"host","cwd":"/tmp"}),
            json!({"agent":"codex","provider_id":"router"}),
            json!({"child_thread_id":7,"provider_id":"router"}),
            json!({"agent":"typo"}),
            json!({"unknown":true}),
        ] {
            assert!(parse(invalid.clone()).is_err(), "{invalid}");
        }
    }
}
