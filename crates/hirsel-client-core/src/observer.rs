use crate::ClientSnapshot;

/// Lifecycle information that is useful to UI shells in addition to snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    Connecting { attempt: u32 },
    Online,
    Offline { reason: Option<String> },
    ProtocolError { detail: String },
}

/// Object-safe, owned callback surface intended for a future UniFFI callback interface.
pub trait ClientObserver: Send + Sync + 'static {
    fn on_state_changed(&self, snapshot: ClientSnapshot);

    fn on_lifecycle_event(&self, event: LifecycleEvent);
}
