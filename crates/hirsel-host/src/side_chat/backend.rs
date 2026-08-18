use std::{sync::Arc, time::Duration};

#[derive(Clone)]
pub(crate) enum SideChatBackend {
    Lash {
        core: Arc<lash::LashCore>,
        agent_guidance: Arc<str>,
    },
    Scripted,
    Degraded(String),
}

#[derive(Clone)]
pub(crate) struct SideSessionCompatibility {
    pub(super) backend: SideChatBackend,
    pub(super) ttl: Duration,
}

impl SideSessionCompatibility {
    pub(crate) fn new(backend: SideChatBackend, ttl: Duration) -> Self {
        Self { backend, ttl }
    }
}
