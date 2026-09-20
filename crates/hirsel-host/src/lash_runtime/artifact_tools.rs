use super::*;
use crate::storage::ArtifactDraft;
use hirsel_proto::ArtifactKind;

impl ScopedThreadTools {
    pub(super) async fn artifact_mutation(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<Value, ToolError> {
        if name != "artifacts_create" {
            self.tools
                .storage()
                .authorize_thread_artifact(&self.caller, required_u64_any(args, &["artifact_id"])?)
                .await
                .map_err(ToolError::from)?;
        }
        self.publish_artifact_operation(name, args, &self.operation_id)
            .await
    }

    async fn publish_artifact_operation(
        &self,
        name: &str,
        args: &Value,
        operation_id: &str,
    ) -> Result<Value, ToolError> {
        let input = json!({"tool":name,"args":args});
        let storage = self.tools.storage();
        let result = if let Some(result) = storage
            .artifact_operation(operation_id, &self.caller, &input)
            .await
            .map_err(ToolError::from)?
        {
            result
        } else {
            let id = if name == "artifacts_create" {
                None
            } else {
                Some(required_u64_any(args, &["artifact_id"])?)
            };
            let draft = match name {
                "artifacts_create" => {
                    // The tool keeps the ergonomic inputs an agent knows; the
                    // render mode is decided once, here, and stored as the only
                    // discriminator.
                    let tag = args
                        .get("kind")
                        .and_then(Value::as_str)
                        .ok_or("kind is required")?
                        .to_string();
                    let mime = optional_string(args, "mime")?;
                    let filename = optional_string(args, "filename")?;
                    let kind = ArtifactKind::from_publish_inputs(
                        &tag,
                        mime.as_deref(),
                        filename.as_deref(),
                    )?;
                    Some(ArtifactDraft {
                        title: required_string(args, "title")?,
                        kind,
                        content: required_string(args, "content")?,
                        expected_content: None,
                    })
                }
                "artifacts_edit" => {
                    let current = storage
                        .scoped_artifact(&self.caller, id.expect("edit has id"))
                        .await
                        .map_err(ToolError::from)?;
                    let edits = args
                        .get("edits")
                        .and_then(Value::as_array)
                        .filter(|v| !v.is_empty() && v.len() <= 100)
                        .ok_or("edits must contain 1–100 exact-match replacements")?;
                    let mut content = current.content.clone();
                    for edit in edits {
                        let old = optional_string_any_allow_empty(edit, &["old_string"])?
                            .filter(|value| !value.is_empty())
                            .ok_or("old_string must be a nonempty source string")?;
                        let new = optional_string_any_allow_empty(edit, &["new_string"])?
                            .ok_or("new_string is required")?;
                        if content.matches(&old).count() != 1 {
                            return Err("old_string must match exactly once; use artifacts.show to read the current source".into());
                        }
                        content = content.replacen(&old, &new, 1);
                    }
                    Some(ArtifactDraft {
                        title: optional_string(args, "title")?.unwrap_or(current.summary.title),
                        kind: current.summary.kind,
                        content,
                        expected_content: Some(current.content),
                    })
                }
                _ => None,
            };
            storage
                .publish_artifact(operation_id, &input, &self.caller, id, draft)
                .await
                .map_err(ToolError::from)?
        };
        for activity in storage
            .outside_change_activities(self.caller.turn_id)
            .await
            .map_err(ToolError::from)?
        {
            self.tools.publish_thread_activity(activity).await;
        }
        self.tools.publish_artifact(result.0.summary.clone());
        let material_threads = if name == "artifacts_edit" {
            result.0.summary.thread_ids.clone()
        } else {
            Vec::new()
        };
        if let Some(message) = result.1 {
            self.tools.publish_thread_message(message).await;
        }
        for thread_id in material_threads {
            self.tools.publish_thread_summary(thread_id).await;
        }
        let scoped = storage
            .scoped_artifact(&self.caller, result.0.summary.id)
            .await
            .map_err(ToolError::from)?;
        serde_json::to_value(scoped).map_err(ToolError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn artifact_tools_publish_openui_as_its_own_kind() {
        let (executor, storage, _log, _dir) = super::super::tests::test_event_executor().await;
        let caller = storage.test_running_caller().await;
        let executor = ScopedThreadTools {
            tools: executor.tools,
            caller,
            operation_id: "fixture".into(),
        };
        // The body is data, not a program: the tool stores it as text and the
        // kind alone decides that the app draws it natively.
        let create = json!({"title":"Board","kind":"openui","content":"root = Stack([lede])\nlede = Heading(\"Live\", 2)"});
        let created = executor
            .publish_artifact_operation("artifacts_create", &create, "create-openui")
            .await
            .unwrap();
        assert_eq!(created["kind"], "openui");
        let id = created["id"].as_u64().unwrap();
        let edits = json!({"artifact_id":id,"edits":[{"old_string":"lede = Heading(\"Live\", 2)","new_string":"lede = Heading(\"Settled\", 2)"}]});
        let edited = executor
            .publish_artifact_operation("artifacts_edit", &edits, "edit-openui")
            .await
            .unwrap();
        assert_eq!(edited["kind"], "openui");
        assert!(edited["content"].as_str().unwrap().contains("Settled"));
        let stored = storage.artifacts(None).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].kind, hirsel_proto::ArtifactKind::OpenUi);
    }

    #[tokio::test]
    async fn artifact_tools_publish_exact_edits_and_replay_same_card() {
        let (executor, storage, log, _dir) = super::super::tests::test_event_executor().await;
        let caller = storage.test_running_caller().await;
        let executor = ScopedThreadTools {
            tools: executor.tools,
            caller: caller.clone(),
            operation_id: "fixture".into(),
        };
        let before = storage
            .thread_detail(caller.thread_id, None, 100)
            .await
            .unwrap()
            .messages
            .len();
        let create = json!({"title":"Counter","kind":"solid","content":"export default function App() { return <p>One</p>; }"});
        let first = executor
            .publish_artifact_operation("artifacts_create", &create, "create-1")
            .await
            .unwrap();
        let id = first["id"].as_u64().unwrap();
        let edits = json!({"artifact_id":id,"edits":[{"old_string":"One","new_string":"Two"}]});
        executor
            .publish_artifact_operation("artifacts_edit", &edits, "edit-1")
            .await
            .unwrap();
        let replay = executor
            .publish_artifact_operation("artifacts_edit", &edits, "edit-1")
            .await
            .unwrap();
        assert!(replay["content"].as_str().unwrap().contains("Two"));
        assert_eq!(
            storage
                .thread_detail(caller.thread_id, None, 100)
                .await
                .unwrap()
                .messages
                .len(),
            before + 2
        );
        assert!(
            executor
                .publish_artifact_operation("artifacts_edit", &edits, "edit-2")
                .await
                .unwrap_err()
                .to_string()
                .contains("exactly once")
        );
        assert_eq!(storage.artifacts(None).await.unwrap().len(), 1);
        assert!(
            log.recent().iter().any(
                |f| matches!(f,HostToClient::Msg{message,..} if message.artifact_ids==vec![id])
            )
        );
        for name in [
            "artifacts_create",
            "artifacts_edit",
            "artifacts_list",
            "artifacts_show",
        ] {
            assert!(
                hirsel_tool_definitions(&crate::subagent_models::registry_catalog())
                    .iter()
                    .any(|t| t.name() == name)
            );
        }
    }
}
