use hirsel_proto::{HostToClient, ViewInstance};
use std::collections::HashMap;

pub(super) struct HelloBroadcastDedupe {
    views: HashMap<String, ViewInstance>,
    threads: HashMap<u64, hirsel_proto::Thread>,
}

impl HelloBroadcastDedupe {
    pub(super) fn new(views: Vec<ViewInstance>) -> Self {
        Self {
            threads: HashMap::new(),
            views: views
                .into_iter()
                .map(|view| (view.instance_id.clone(), view))
                .collect(),
        }
    }

    pub(super) fn include_threads(&mut self, threads: &[hirsel_proto::Thread]) {
        self.threads
            .extend(threads.iter().map(|t| (t.id, t.clone())));
    }

    pub(super) fn before_request(&mut self, frame: &hirsel_proto::ClientToHost) {
        // Direct replies can update the client's summary without an upsert.
        // Forget its previous value so a subsequent rollback to that value is
        // delivered too. A retried create can resolve any existing Thread ID.
        match frame {
            hirsel_proto::ClientToHost::OpenThread { thread_id, .. } => {
                self.threads.remove(thread_id);
            }
            hirsel_proto::ClientToHost::CreateThread { .. } => self.threads.clear(),
            _ => {}
        }
    }

    pub(super) fn should_send(&mut self, event: &HostToClient) -> bool {
        match event {
            HostToClient::ThreadUpsert { thread } => {
                if self.threads.get(&thread.id).is_some_and(|previous| {
                    thread.revision < previous.revision || thread == previous
                }) {
                    return false;
                }
                self.threads.insert(thread.id, thread.clone());
                true
            }
            HostToClient::ViewUpsert {
                thread_id,
                instance_id,
                spec,
            } => self
                .views
                .remove(instance_id)
                .is_none_or(|snapshot| snapshot.thread_id != *thread_id || snapshot.spec != *spec),
            HostToClient::ViewRemoved { instance_id } => {
                self.views.remove(instance_id);
                true
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use hirsel_proto::{HostToClient, ViewInstance};
    use serde_json::json;
    use tokio::sync::broadcast;

    use super::HelloBroadcastDedupe;
    use crate::{
        BroadcastLog,
        templates::{TemplateStore, ViewManager},
    };

    fn view(spec: serde_json::Value) -> ViewInstance {
        ViewInstance {
            thread_id: 7,
            instance_id: "status".to_string(),
            spec,
        }
    }

    fn upsert(view: &ViewInstance) -> HostToClient {
        HostToClient::ViewUpsert {
            thread_id: view.thread_id,
            instance_id: view.instance_id.clone(),
            spec: view.spec.clone(),
        }
    }

    #[tokio::test]
    async fn removed_snapshot_view_can_be_recreated_with_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let templates = TemplateStore::load(dir.path().to_path_buf()).await.unwrap();
        let (broadcaster, mut broadcasts) = broadcast::channel(4);
        let views = ViewManager::new(
            "fixture-history".to_string(),
            templates,
            broadcaster,
            BroadcastLog::default(),
        );
        let spec = json!({ "type": "text", "text": "Ready" });
        let original = views
            .show(
                "fixture-history",
                7,
                None,
                Some(spec.clone()),
                None,
                Some("status".to_string()),
            )
            .await
            .unwrap();
        let initial_upsert = broadcasts.recv().await.unwrap();
        let snapshot = views.snapshot().await;

        let mut duplicate_dedupe = HelloBroadcastDedupe::new(snapshot.clone());
        assert!(!duplicate_dedupe.should_send(&initial_upsert));

        let mut dedupe = HelloBroadcastDedupe::new(snapshot);
        views.clear("fixture-history", 7, "status").await.unwrap();
        assert!(dedupe.should_send(&broadcasts.recv().await.unwrap()));

        let recreated = views
            .show(
                "fixture-history",
                7,
                None,
                Some(spec),
                None,
                Some("status".to_string()),
            )
            .await
            .unwrap();
        assert_eq!(recreated, original);
        assert!(dedupe.should_send(&broadcasts.recv().await.unwrap()));
    }

    #[test]
    fn changed_snapshot_view_is_still_delivered() {
        let original = view(json!({ "type": "text", "text": "Ready" }));
        let changed = view(json!({ "type": "text", "text": "Done" }));
        let mut dedupe = HelloBroadcastDedupe::new(vec![original]);

        assert!(dedupe.should_send(&upsert(&changed)));
    }
}
