use super::*;
use crate::storage::ArtifactDraft;
use hirsel_proto::ArtifactKind;

impl HirselToolExecutor {
    pub(super) async fn artifacts_list(&self, args: &Value) -> Result<Value, String> {
        let thread_id = args.get("thread_id").and_then(Value::as_u64);
        Ok(
            json!({"artifacts":self.tools.storage().artifacts(thread_id).await.map_err(|e|e.to_string())?}),
        )
    }
    pub(super) async fn artifact_mutation(
        &self,
        name: &str,
        args: &Value,
        context: &lash_core::AttemptContext<'_>,
    ) -> Result<Value, String> {
        let key = context
            .replay_key()
            .or_else(|| context.tool_call_id())
            .ok_or("artifact publication requires a durable tool execution identity")?;
        let operation_id = format!(
            "{}:{}:{name}:{key}",
            context.session_id(),
            context.execution_scope_id()
        );
        let thread_id = self
            .anchors
            .lock()
            .await
            .active
            .as_ref()
            .map_or(0, |a| a.thread_id);
        self.publish_artifact_operation(name, args, &operation_id, thread_id)
            .await
    }

    async fn publish_artifact_operation(
        &self,
        name: &str,
        args: &Value,
        operation_id: &str,
        thread_id: u64,
    ) -> Result<Value, String> {
        let input = json!({"tool":name,"args":args});
        let storage = self.tools.storage();
        let result = if let Some(result) = storage
            .artifact_operation(operation_id, thread_id, &input)
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
                        .artifact(id.expect("edit has id"))
                        .await
                        .map_err(|e| e.to_string())?;
                    let edits = args
                        .get("edits")
                        .and_then(Value::as_array)
                        .filter(|v| !v.is_empty() && v.len() <= 100)
                        .ok_or("edits must contain 1–100 exact-match replacements")?;
                    let mut content = current.content.clone();
                    for edit in edits {
                        let old = required_string(edit, "old_string")?;
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
                .publish_artifact(operation_id, &input, thread_id, id, draft)
                .await
                .map_err(|e| e.to_string())?
        };
        self.tools.publish_artifact(result.0.summary.clone());
        if let Some(message) = result.1 {
            self.tools.publish_thread_message(message).await;
        }
        serde_json::to_value(result.0).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn artifact_tools_publish_exact_edits_and_replay_same_card() {
        let (executor, storage, log, _dir) = super::super::tests::test_event_executor().await;
        let before = storage
            .thread_detail(0, None, 100)
            .await
            .unwrap()
            .messages
            .len();
        let create = json!({"title":"Counter","kind":"solid","content":"export default function App() { return <p>One</p>; }"});
        let first = executor
            .publish_artifact_operation("artifacts_create", &create, "create-1", 0)
            .await
            .unwrap();
        let id = first["id"].as_u64().unwrap();
        let edits = json!({"artifact_id":id,"edits":[{"old_string":"One","new_string":"Two"}]});
        executor
            .publish_artifact_operation("artifacts_edit", &edits, "edit-1", 0)
            .await
            .unwrap();
        let replay = executor
            .publish_artifact_operation("artifacts_edit", &edits, "edit-1", 0)
            .await
            .unwrap();
        assert!(replay["content"].as_str().unwrap().contains("Two"));
        assert_eq!(
            storage
                .thread_detail(0, None, 100)
                .await
                .unwrap()
                .messages
                .len(),
            before + 2
        );
        assert!(
            executor
                .publish_artifact_operation("artifacts_edit", &edits, "edit-2", 0)
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
                hirsel_tool_definitions(&crate::subagent_models::registry_catalog())
                    .iter()
                    .any(|t| t.name() == name)
            );
        }
    }
}
