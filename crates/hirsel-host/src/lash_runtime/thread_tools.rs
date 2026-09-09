use super::*;
use hirsel_proto::ThreadAttention;

fn attention(args: &Value) -> Result<Option<ThreadAttention>, String> {
    args.get("attention")
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|e| format!("invalid attention: {e}"))
        })
        .transpose()
}

impl HirselToolExecutor {
    pub(super) async fn threads_create(&self, args: &Value) -> Result<Value, String> {
        let client_id = required_string(args, "client_id")?;
        let title = required_string(args, "title")?;
        let description =
            optional_string_any_allow_empty(args, &["description"])?.unwrap_or_default();
        let instrument = args.get("instrument").cloned().unwrap_or(Value::Null);
        let (thread, _) = self
            .tools
            .storage()
            .create_thread(
                &client_id,
                &title,
                &description,
                &instrument,
                attention(args)?.unwrap_or_default(),
            )
            .await
            .map_err(|e| e.to_string())?;
        self.tools.publish_thread(thread.clone());
        Ok(json!({"thread_id":thread.id,"thread":thread}))
    }

    pub(super) async fn threads_update(&self, args: &Value) -> Result<Value, String> {
        let id = required_u64_any(args, &["thread_id"])?;
        let title = optional_string(args, "title")?;
        let description = optional_string_any_allow_empty(args, &["description"])?;
        let thread = self
            .tools
            .storage()
            .update_thread(
                id,
                title.as_deref(),
                description.as_deref(),
                args.get("instrument"),
                attention(args)?,
            )
            .await
            .map_err(|e| e.to_string())?;
        self.tools.publish_thread(thread.clone());
        Ok(json!({"thread_id":thread.id,"thread":thread}))
    }

    pub(super) async fn threads_list(&self) -> Result<Value, String> {
        let threads = self
            .tools
            .storage()
            .thread_snapshot()
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({"threads":threads}))
    }

    pub(super) async fn threads_read(&self, args: &Value) -> Result<Value, String> {
        let id = required_u64_any(args, &["thread_id"])?;
        let detail = self
            .tools
            .storage()
            .thread_detail(
                id,
                args.get("before_id").and_then(Value::as_u64),
                args.get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(30)
                    .clamp(1, 100),
            )
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(detail).map_err(|e| e.to_string())
    }

    pub(super) async fn threads_activity(&self, args: &Value) -> Result<Value, String> {
        let id = required_u64_any(args, &["thread_id"])?;
        let kind = required_string(args, "kind")?;
        let data = args
            .get("data")
            .filter(|v| v.is_object())
            .ok_or("data must be an object")?;
        let active = self.anchors.lock().await.active.clone();
        let turn_id = active
            .filter(|a| a.thread_id == id)
            .and_then(|a| a.thread_turn_id);
        let activity = self
            .tools
            .storage()
            .append_thread_activity(id, turn_id, &kind, data)
            .await
            .map_err(|e| e.to_string())?;
        self.tools.publish_thread_activity(activity.clone()).await;
        Ok(json!({"activity":activity}))
    }
}
