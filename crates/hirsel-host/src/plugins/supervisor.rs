//! Daemon supervision for plugin `run()` bodies.
//!
//! A plugin runs in-process with full trust, so the one failure the host can
//! actually contain is a panic. The supervisor catches it (via `JoinError`),
//! restarts with exponential backoff, and — when a plugin panics repeatedly in
//! a short window — parks it in the `errored` state instead of restarting
//! forever. A daemon that *returns* is finished, not crashed: it is never
//! restarted, which is what makes the no-op default `run()` free.

use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use hirsel_plugin_api::{Plugin, PluginCtx};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::{PluginRuntime, RuntimeTable, tools::PluginToolRegistry};

/// Restart policy. Test builds shorten every window; production uses the
/// documented defaults.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SupervisorConfig {
    pub(crate) base_backoff: Duration,
    pub(crate) max_backoff: Duration,
    pub(crate) crash_window: Duration,
    pub(crate) crash_limit: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            crash_window: Duration::from_secs(60),
            crash_limit: 5,
        }
    }
}

pub(crate) struct SpawnedSupervisor {
    pub(crate) task: JoinHandle<()>,
    pub(crate) start: oneshot::Sender<()>,
}

/// Spawn the supervised daemon for one plugin. The returned handle is aborted
/// when the plugin is disabled, which cancels the daemon and the supervisor
/// together.
pub(crate) fn spawn(
    plugin: Arc<dyn Plugin>,
    ctx: PluginCtx,
    runtimes: RuntimeTable,
    tool_registry: PluginToolRegistry,
    config: SupervisorConfig,
) -> SpawnedSupervisor {
    let (start, started) = oneshot::channel();
    let task = tokio::spawn(async move {
        if started.await.is_err() {
            return;
        }
        let id = plugin.id().to_string();
        let mut crashes: VecDeque<Instant> = VecDeque::new();
        let mut attempt = 0u32;
        loop {
            let plugin_for_run = Arc::clone(&plugin);
            let ctx_for_run = ctx.clone();
            let mut run = AbortOnDrop(tokio::spawn(async move {
                plugin_for_run.run(ctx_for_run).await;
            }));
            match (&mut run.0).await {
                // The daemon finished on its own: nothing to supervise.
                Ok(()) => return,
                Err(error) if error.is_cancelled() => return,
                Err(error) => {
                    let detail = panic_detail(&error);
                    tracing::error!(plugin = %id, error = %detail, "plugin daemon panicked");
                    let now = Instant::now();
                    crashes.push_back(now);
                    while crashes
                        .front()
                        .is_some_and(|first| now.duration_since(*first) > config.crash_window)
                    {
                        crashes.pop_front();
                    }
                    if crashes.len() >= config.crash_limit {
                        tracing::error!(
                            plugin = %id,
                            crashes = crashes.len(),
                            "plugin daemon crash-looped; parking the plugin as errored"
                        );
                        set_errored(&runtimes, &tool_registry, &id, detail).await;
                        return;
                    }
                    let backoff = backoff_for(attempt, config);
                    attempt = attempt.saturating_add(1);
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    });
    SpawnedSupervisor { task, start }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn backoff_for(attempt: u32, config: SupervisorConfig) -> Duration {
    config
        .base_backoff
        .saturating_mul(1u32 << attempt.min(6))
        .min(config.max_backoff)
}

async fn set_errored(
    runtimes: &RuntimeTable,
    tool_registry: &PluginToolRegistry,
    id: &str,
    detail: String,
) {
    let supervisor_id = tokio::task::id();
    let mut table = runtimes.lock().await;
    let is_current = matches!(
        table.get(id),
        Some(PluginRuntime::Running { task }) if task.id() == supervisor_id
    );
    if is_current {
        tool_registry.unregister(id);
        table.insert(id.to_string(), PluginRuntime::Errored { detail });
    }
}

fn panic_detail(error: &tokio::task::JoinError) -> String {
    if !error.is_panic() {
        return error.to_string();
    }
    // `JoinError::into_panic` needs ownership, so reconstruct the message from
    // the Display form, which already carries the payload for string panics.
    error.to_string()
}
