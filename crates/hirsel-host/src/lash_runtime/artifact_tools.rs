use super::*;
use crate::storage::ArtifactDraft;
use hirsel_proto::ArtifactKind;

impl ScopedThreadTools {
    pub(super) async fn artifact_mutation(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<Value, String> {
        if name != "artifacts_create" {
            self.tools
                .storage()
                .authorize_thread_artifact(&self.caller, required_u64_any(args, &["artifact_id"])?)
                .await
                .map_err(|e| e.to_string())?;
        }
        self.publish_artifact_operation(name, args, &self.operation_id)
            .await
    }

    async fn publish_artifact_operation(
        &self,
        name: &str,
        args: &Value,
        operation_id: &str,
    ) -> Result<Value, String> {
        let input = json!({"tool":name,"args":args});
        let storage = self.tools.storage();
        let result = if let Some(result) = storage
            .artifact_operation(operation_id, &self.caller, &input)
            .await
            .map_err(|e| e.to_string())?
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
                    let kind: ArtifactKind = serde_json::from_value(
                        args.get("kind").cloned().ok_or("kind is required")?,
                    )
                    .map_err(|e| e.to_string())?;
                    let mime = optional_string(args, "mime")?.unwrap_or_else(|| {
                        match kind {
                            ArtifactKind::Solid => "text/jsx",
                            ArtifactKind::Html => "text/html",
                            ArtifactKind::File => "text/plain",
                        }
                        .into()
                    });
                    Some(ArtifactDraft {
                        title: required_string(args, "title")?,
                        kind,
                        mime,
                        filename: optional_string(args, "filename")?,
                        content: required_string(args, "content")?,
                        expected_content: None,
                    })
                }
                "artifacts_edit" => {
                    let current = storage
                        .scoped_artifact(&self.caller, id.expect("edit has id"))
                        .await
                        .map_err(|e| e.to_string())?;
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
                        mime: current.summary.mime,
                        filename: current.summary.filename,
                        content,
                        expected_content: Some(current.content),
                    })
                }
                _ => None,
            };
            storage
                .publish_artifact(operation_id, &input, &self.caller, id, draft)
                .await
                .map_err(|e| e.to_string())?
        };
        self.tools.publish_artifact(result.0.summary.clone());
        if let Some(message) = result.1 {
            self.tools.publish_thread_message(message).await;
        }
        let scoped = storage
            .scoped_artifact(&self.caller, result.0.summary.id)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(scoped).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                hirsel_tool_definitions(&crate::subagent_models::registry_catalog(), &[])
                    .iter()
                    .any(|t| t.name() == name)
            );
        }
    }
}
