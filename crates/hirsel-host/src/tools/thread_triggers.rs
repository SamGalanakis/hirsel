use std::{collections::HashMap, sync::Weak};

use serde_json::json;
use tokio::sync::Mutex;

use super::ToolSuite;

#[derive(Default)]
pub(crate) struct ThreadTriggerHub {
    runtimes: Mutex<HashMap<u64, Weak<crate::lash_runtime::LashAgentRuntime>>>,
    #[cfg(test)]
    recorded: Mutex<Vec<ThreadTriggerEvent>>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadTriggerEvent {
    pub(crate) source_type: String,
    pub(crate) event_type: String,
    pub(crate) thread_id: u64,
    pub(crate) payload: String,
}

impl ThreadTriggerHub {
    pub(crate) async fn register(
        &self,
        thread_id: u64,
        runtime: Weak<crate::lash_runtime::LashAgentRuntime>,
    ) {
        self.runtimes.lock().await.insert(thread_id, runtime);
    }

    async fn live(&self) -> Vec<std::sync::Arc<crate::lash_runtime::LashAgentRuntime>> {
        let mut runtimes = self.runtimes.lock().await;
        let live = runtimes
            .values()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        runtimes.retain(|_, runtime| runtime.strong_count() > 0);
        live
    }

    pub(crate) async fn clear(&self) {
        self.runtimes.lock().await.clear();
        #[cfg(test)]
        self.recorded.lock().await.clear();
    }
}

impl ToolSuite {
    pub(crate) async fn register_thread_trigger_runtime(
        &self,
        thread_id: u64,
        runtime: Weak<crate::lash_runtime::LashAgentRuntime>,
    ) {
        self.thread_triggers.register(thread_id, runtime).await;
    }

    pub(crate) async fn emit_thread_trigger(
        &self,
        source_type: &str,
        event_type: &str,
        thread_id: u64,
        payload: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) {
        let payload = payload.into();
        let idempotency_key = idempotency_key.into();
        let Some(thread) = self.storage.thread(thread_id).await.ok().flatten() else {
            return;
        };
        #[cfg(test)]
        self.thread_triggers
            .recorded
            .lock()
            .await
            .push(ThreadTriggerEvent {
                source_type: source_type.to_string(),
                event_type: event_type.to_string(),
                thread_id,
                payload: payload.clone(),
            });
        let event = json!({
            "thread_id": thread_id,
            "title": thread.title,
            "payload": payload.chars().take(8 * 1024).collect::<String>(),
        });
        for runtime in self.thread_triggers.live().await {
            match self
                .storage
                .thread_in_scope(runtime.thread_id(), thread_id)
                .await
            {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(%error, subscriber_thread_id = runtime.thread_id(), thread_id, "Thread trigger scope check failed");
                    continue;
                }
            }
            if let Err(error) = runtime
                .emit_thread_occurrence(
                    source_type,
                    event_type,
                    thread_id,
                    event.clone(),
                    &idempotency_key,
                )
                .await
            {
                tracing::warn!(%error, source_type, thread_id, "Thread trigger emission failed");
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn recorded_thread_triggers(&self) -> Vec<ThreadTriggerEvent> {
        self.thread_triggers.recorded.lock().await.clone()
    }
}
