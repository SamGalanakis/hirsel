//! Agent calls receive resource capabilities bound to their actual execution.
use crate::{
    storage::{ThreadCaller, ThreadMutation, ThreadRef},
    tools::ToolSuite,
};
use async_trait::async_trait;
use hirsel_plugin_api::{
    ActivityReceipt, NewActivity, NewThread, PluginCtx, PluginKv, PluginThreads,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub(super) fn context(
    base: &PluginCtx,
    tools: ToolSuite,
    caller: ThreadCaller,
    operation_id: String,
) -> PluginCtx {
    let threads = Arc::new(ScopedThreads {
        plugin_id: base.id().into(),
        tools: tools.clone(),
        caller: caller.clone(),
        operation_id,
        sequence: AtomicU64::new(0),
    });
    let kv = Arc::new(ScopedKv {
        plugin_id: base.id().into(),
        tools,
        caller,
    });
    base.with_resource_capabilities(threads, kv)
}
struct ScopedThreads {
    plugin_id: String,
    tools: ToolSuite,
    caller: ThreadCaller,
    operation_id: String,
    sequence: AtomicU64,
}
impl ScopedThreads {
    fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.plugin_id,
            self.operation_id,
            self.sequence.fetch_add(1, Ordering::SeqCst)
        )
    }
}
#[async_trait]
impl PluginThreads for ScopedThreads {
    async fn create(&self, input: NewThread) -> Result<u64, String> {
        let key = self.key();
        let result = self
            .tools
            .storage()
            .mutate_scoped_thread(
                &self.caller,
                &key,
                &ThreadMutation::Create {
                    icon: None,
                    client_id: key.clone(),
                    kind: input.kind,
                    title: input.title,
                    parent: ThreadRef::default(),
                    description: input.description,
                    instrument: input.instrument,
                    attention: if input.needs_owner {
                        hirsel_proto::ThreadAttention::NeedsOwner
                    } else {
                        hirsel_proto::ThreadAttention::Quiet
                    },
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        let thread: hirsel_proto::Thread =
            serde_json::from_value(result["thread"].clone()).map_err(|e| e.to_string())?;
        let id = thread.id;
        self.tools
            .publish_thread(&self.caller.history_id, thread)
            .await;
        Ok(id)
    }
    async fn append_activity(&self, input: NewActivity) -> Result<ActivityReceipt, String> {
        let result = self
            .tools
            .storage()
            .mutate_scoped_thread(
                &self.caller,
                &self.key(),
                &ThreadMutation::Activity {
                    thread: ThreadRef::Id(input.thread_id),
                    kind: format!("plugin.{}", input.kind),
                    data: json!({"plugin":self.plugin_id,"payload":input.data}),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        let activity: hirsel_proto::ThreadActivity =
            serde_json::from_value(result["activity"].clone()).map_err(|e| e.to_string())?;
        let receipt = ActivityReceipt {
            activity_id: activity.id,
            thread_id: activity.thread_id,
        };
        self.tools.publish_thread_activity(activity).await;
        Ok(receipt)
    }
}
struct ScopedKv {
    plugin_id: String,
    tools: ToolSuite,
    caller: ThreadCaller,
}
#[async_trait]
impl PluginKv for ScopedKv {
    async fn get(&self, key: &str) -> Result<Option<Value>, String> {
        let storage = self.tools.storage();
        let c = storage
            .execution_guard(&self.caller)
            .await
            .map_err(|e| e.to_string())?;
        let row = c
            .query_row(
                "SELECT value FROM plugin_thread_kv WHERE plugin_id=?1 AND thread_id=?2 AND key=?3",
                params![self.plugin_id, self.caller.thread_id, key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        row.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
            .transpose()
    }
    async fn set(&self, key: &str, value: Value) -> Result<(), String> {
        let storage = self.tools.storage();
        let c = storage
            .execution_guard(&self.caller)
            .await
            .map_err(|e| e.to_string())?;
        c.execute("INSERT INTO plugin_thread_kv(plugin_id,thread_id,key,value) VALUES(?1,?2,?3,?4) ON CONFLICT(plugin_id,thread_id,key) DO UPDATE SET value=excluded.value",params![self.plugin_id,self.caller.thread_id,key,serde_json::to_string(&value).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<(), String> {
        let storage = self.tools.storage();
        let c = storage
            .execution_guard(&self.caller)
            .await
            .map_err(|e| e.to_string())?;
        c.execute(
            "DELETE FROM plugin_thread_kv WHERE plugin_id=?1 AND thread_id=?2 AND key=?3",
            params![self.plugin_id, self.caller.thread_id, key],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
    async fn entries(&self) -> Result<Vec<(String, Value)>, String> {
        let storage = self.tools.storage();
        let c = storage
            .execution_guard(&self.caller)
            .await
            .map_err(|e| e.to_string())?;
        let rows=c.prepare("SELECT key,value FROM plugin_thread_kv WHERE plugin_id=?1 AND thread_id=?2 ORDER BY key").map_err(|e|e.to_string())?.query_map(params![self.plugin_id,self.caller.thread_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(|e|e.to_string())?.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())?;
        rows.into_iter()
            .map(|(key, value)| {
                Ok((
                    key,
                    serde_json::from_str(&value).map_err(|e| e.to_string())?,
                ))
            })
            .collect()
    }
}
