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
                placement,
                spec,
            } => self.views.remove(instance_id).is_none_or(|snapshot| {
                snapshot.thread_id != *thread_id
                    || snapshot.placement != *placement
                    || snapshot.spec != *spec
            }),
            _ => true,
        }
    }
}
