use std::{
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64},
    },
    time::{Duration, Instant},
};

use tokio::sync::Mutex;

pub(super) struct SideChatSession {
    pub(super) sc: String,
    pub(super) event_id: u64,
    pub(super) legacy_ping: bool,
    pub(super) ping_content: String,
    pub(super) anchor: u64,
    pub(super) lash_session: Mutex<Option<lash::LashSession>>,
    pub(super) turn_lock: Mutex<()>,
    pub(super) seq: AtomicU64,
    pub(super) active_cancel: StdMutex<Option<lash::CancellationToken>>,
    pub(super) last_activity: StdMutex<Instant>,
    pub(super) closed: AtomicBool,
}

impl SideChatSession {
    pub(super) fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Instant::now();
    }

    pub(super) fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .elapsed()
    }

    pub(super) fn set_active_cancel(&self, cancel: Option<lash::CancellationToken>) {
        *self
            .active_cancel
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = cancel;
    }

    pub(super) fn cancel_active_turn(&self) -> bool {
        let cancel = self
            .active_cancel
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(cancel) = cancel {
            cancel.cancel();
            true
        } else {
            false
        }
    }
}
