use hirsel_proto::{Event, HostToClient, ViewInstance};
use std::collections::HashMap;

pub(super) struct HelloBroadcastDedupe {
    latest_msg_id: u64,
    events: HashMap<u64, Event>,
    views: HashMap<String, ViewInstance>,
    threads: HashMap<u64, hirsel_proto::Thread>,
}

impl HelloBroadcastDedupe {
    pub(super) fn new(latest_msg_id: u64, events: Vec<Event>, views: Vec<ViewInstance>) -> Self {
        Self {
            latest_msg_id,
            threads: HashMap::new(),
            events: events.into_iter().map(|event| (event.id, event)).collect(),
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
            HostToClient::Msg { message, sc } => sc.is_some() || message.id > self.latest_msg_id,
            HostToClient::EventUpsert { event } => self
                .events
                .remove(&event.id)
                .is_none_or(|snapshot| snapshot != *event),
            HostToClient::ViewUpsert {
                instance_id,
                placement,
                spec,
            } => self
                .views
                .remove(instance_id)
                .is_none_or(|snapshot| snapshot.placement != *placement || snapshot.spec != *spec),
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use hirsel_proto::{
        Event, EventKind, EventSource, EventSourceKind, EventStatus, HostToClient, ViewInstance,
    };
    use serde_json::json;

    use super::HelloBroadcastDedupe;

    fn snapshot_event(id: u64) -> Event {
        Event {
            id,
            kind: EventKind::Info,
            source: EventSource {
                kind: EventSourceKind::Agent,
                r#ref: None,
            },
            name: "Snapshot event".to_string(),
            description: "Delivered in hello".to_string(),
            ui: json!({}),
            anchor: 1,
            requires_response: false,
            quick_replies: Vec::new(),
            status: EventStatus::Open,
            read: false,
            archived: false,
            snoozed_until: None,
            archived_at: None,
            fork_sc: None,
            ts: Utc::now(),
        }
    }

    fn event_upsert(event: Event) -> HostToClient {
        HostToClient::EventUpsert { event }
    }

    fn view_upsert(view: &ViewInstance) -> HostToClient {
        HostToClient::ViewUpsert {
            instance_id: view.instance_id.clone(),
            placement: view.placement.clone(),
            spec: view.spec.clone(),
        }
    }

    #[test]
    fn hello_snapshot_event_echo_is_suppressed_once() {
        let event = snapshot_event(1);
        let mut dedupe = HelloBroadcastDedupe::new(0, vec![event.clone()], Vec::new());

        assert!(!dedupe.should_send(&event_upsert(event.clone())));
        assert!(dedupe.should_send(&event_upsert(event)));
    }

    #[test]
    fn mutated_event_then_reverted_to_snapshot_is_delivered() {
        let event = snapshot_event(1);
        let mut changed = event.clone();
        changed.archived = true;
        let mut dedupe = HelloBroadcastDedupe::new(0, vec![event.clone()], Vec::new());

        assert!(dedupe.should_send(&event_upsert(changed)));
        assert!(dedupe.should_send(&event_upsert(event)));
    }

    #[test]
    fn hello_snapshot_view_echo_is_suppressed_once() {
        let view = ViewInstance {
            instance_id: "view-1".to_string(),
            placement: "canvas".to_string(),
            spec: json!({ "type": "text", "text": "Snapshot" }),
        };
        let mut dedupe = HelloBroadcastDedupe::new(0, Vec::new(), vec![view.clone()]);

        assert!(!dedupe.should_send(&view_upsert(&view)));
        assert!(dedupe.should_send(&view_upsert(&view)));

        let mut changed = view.clone();
        changed.spec = json!({ "type": "text", "text": "Changed" });
        let mut dedupe = HelloBroadcastDedupe::new(0, Vec::new(), vec![view.clone()]);
        assert!(dedupe.should_send(&view_upsert(&changed)));
        assert!(dedupe.should_send(&view_upsert(&view)));
    }

    #[test]
    fn unknown_event_and_view_ids_always_pass() {
        let mut dedupe = HelloBroadcastDedupe::new(0, Vec::new(), Vec::new());
        let event = event_upsert(snapshot_event(99));
        let view = view_upsert(&ViewInstance {
            instance_id: "unknown-view".to_string(),
            placement: "canvas".to_string(),
            spec: json!({}),
        });

        assert!(dedupe.should_send(&event));
        assert!(dedupe.should_send(&event));
        assert!(dedupe.should_send(&view));
        assert!(dedupe.should_send(&view));
    }
}
