use std::{sync::Arc, time::Duration};

#[derive(Clone)]
pub(crate) enum SideChatBackend {
    Lash {
        core: Arc<lash::LashCore>,
        /// Read at open time, not captured at startup, so a legacy side session
        /// opened after a prompt edit is prompted with the current prompt.
        prompts: crate::prompt_config::PromptConfig,
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
