//! Fork dispatch: one ephemeral triage fork per incoming non-owner message.
//!
//! This is the whole of ADR-0015's mechanical change to the host. Every
//! non-owner message that used to turn the resident main Agent now lands here
//! instead, and the main Agent turns only if a fork decides it should.
//!
//! The rulings this implements, and where:
//!
//! 1. **One fork per message, no batching.** [`ForkWake::dispatch`] spawns a
//!    task per call. Callers call it once per message; nothing here coalesces.
//! 2. **Fail-open.** A fork that errors, times out, produces no exit, or
//!    cannot be built at all (no `[fork]` config, no provider key) ends in
//!    [`ForkWake::fallback`], which injects a minimal brief naming the
//!    original message. A non-owner message is never silently lost.
//! 3. **Bounded concurrency.** [`MAX_CONCURRENT_FORKS`] simultaneous forks.
//!    Beyond that a burst queues on the semaphore rather than stampeding the
//!    provider. Interleaving is expected; brief ordering is not guaranteed.
//! 4. **Owner bypass.** Owner messages never reach this module — they keep
//!    going straight to `AgentRuntime::enqueue`. There is deliberately no
//!    entry point here that an Owner message could take.

use std::sync::Arc;

use tokio::sync::Semaphore;

use super::{
    pack::{PackContext, WakeMessage, build_pack},
    tools::{BriefSink, ForkExit, ForkTools},
};
use crate::{storage::Storage, tools::ToolSuite};

/// How many triage forks may be in flight at once.
///
/// Four: enough that a Sub-agent fan-out finishing together is triaged
/// promptly, small enough that a monitor storm cannot open dozens of provider
/// sessions at once. Forks are single-turn and short, so the queue behind this
/// drains quickly; the alternative — unbounded spawning — turns a burst of
/// cheap events into a rate-limit incident on the provider the *main* Agent
/// also rides.
pub const MAX_CONCURRENT_FORKS: usize = 4;

/// How long a single triage turn may take before it is abandoned and the
/// message is escalated by the fail-open path.
pub const FORK_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Everything a triage fork needs to run one turn.
pub struct TriageRequest {
    /// The rendered context pack (see [`super::pack`]).
    pub pack: String,
    /// The fork's exits. The runner exposes these — and only these — as the
    /// child session's tool surface.
    pub tools: Arc<ForkTools>,
    /// Log/trace label for the triggering message.
    pub label: String,
}

/// Runs one ephemeral triage turn.
///
/// The trait exists so dispatch is testable without a provider: the lash
/// implementation opens a child session, runs one turn against a small turn
/// budget, and deletes the session; tests substitute a fake that drives
/// [`ForkTools`] directly.
#[async_trait::async_trait]
pub trait TriageRunner: Send + Sync {
    async fn run_triage(&self, request: TriageRequest) -> anyhow::Result<()>;
}

/// The host's fork-wake dispatcher.
pub struct ForkWake {
    runner: Arc<dyn TriageRunner>,
    sink: Arc<dyn BriefSink>,
    tools: ToolSuite,
    storage: Storage,
    permits: Arc<Semaphore>,
}

impl ForkWake {
    pub fn new(
        runner: Arc<dyn TriageRunner>,
        sink: Arc<dyn BriefSink>,
        tools: ToolSuite,
        storage: Storage,
    ) -> Arc<Self> {
        Arc::new(Self {
            runner,
            sink,
            tools,
            storage,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_FORKS)),
        })
    }

    /// Spawn exactly one triage fork for one non-owner message and return
    /// immediately. Wake sites are on hot paths (a process bridge, a monitor
    /// loop) and must not block on a model turn.
    pub fn dispatch(self: &Arc<Self>, message: WakeMessage) {
        let fork = Arc::clone(self);
        tokio::spawn(async move {
            fork.dispatch_now(message).await;
        });
    }

    /// The dispatch body, awaited. Never returns an error: every failure mode
    /// is converted into the fail-open brief, because the one thing a wake
    /// path may not do is drop the message.
    pub async fn dispatch_now(self: &Arc<Self>, message: WakeMessage) {
        let label = message.source.label();
        let _permit = match self.permits.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                // The semaphore is never closed while the host runs; if it
                // somehow is, the message still must not be lost.
                self.fallback(&message, "fork dispatch is shut down").await;
                return;
            }
        };
        tracing::info!(
            source = message.source.kind(),
            key = %message.key,
            "spawning triage fork"
        );

        let context = self.pack_context().await;
        let anchor = match self.anchor(&context).await {
            Ok(anchor) => anchor,
            Err(error) => {
                tracing::warn!(%error, key = %message.key, "failed to resolve a fork record anchor");
                self.fallback(&message, "the host could not prepare fork context")
                    .await;
                return;
            }
        };
        let pack = build_pack(&message, &context);
        let tools = Arc::new(ForkTools::new(
            self.tools.clone(),
            Arc::clone(&self.sink),
            message.clone(),
            anchor,
        ));
        let request = TriageRequest {
            pack,
            tools: Arc::clone(&tools),
            label: label.clone(),
        };

        let outcome =
            tokio::time::timeout(FORK_TURN_TIMEOUT, self.runner.run_triage(request)).await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                // Never log the error alongside prompt text — the pack is
                // Owner data. The error string is provider/runtime detail.
                // `{:#}` so the anyhow context chain is visible; the pack is
                // never part of it.
                tracing::warn!(error = %format!("{error:#}"), key = %message.key, "triage fork failed");
            }
            Err(_) => {
                tracing::warn!(
                    key = %message.key,
                    timeout_secs = FORK_TURN_TIMEOUT.as_secs(),
                    "triage fork timed out"
                );
            }
        }

        match tools.exit().await {
            Some(exit) => {
                tracing::info!(
                    key = %message.key,
                    exit = exit_label(&exit),
                    "triage fork took its exit"
                );
            }
            None => {
                // Ruling 2: no exit is a failure, not a drop. Drop is
                // affirmative (`fork_drop`); silence is a lost message.
                self.fallback(&message, "the triage fork ended without taking an exit")
                    .await;
            }
        }
    }

    /// Read the slice the pack is built from. Failures degrade to an empty
    /// slice rather than aborting the fork: a fork with a thin pack still
    /// beats a lost message.
    async fn pack_context(&self) -> PackContext {
        let events = self.storage.ping_snapshot().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to read the live event inventory for a fork pack");
            Vec::new()
        });
        let recent_chat = self
            .storage
            .recent_chat(super::pack::PACK_CHAT_LIMIT as u64)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "failed to read the conversation tail for a fork pack");
                Vec::new()
            });
        let rules = self
            .storage
            .taste_decisions()
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "failed to read recorded rules for a fork pack");
                Vec::new()
            });
        PackContext {
            events,
            recent_chat,
            rules,
        }
    }

    /// The chat message a recorded event hangs off.
    ///
    /// Events need an anchor, and a fork has no Owner turn to anchor to. It
    /// reuses the latest chat message rather than writing an Owner-facing line
    /// of its own — a fork must never speak to the Owner, and an anchor
    /// message would be exactly that. Only an empty transcript forces the host
    /// to mint one.
    async fn anchor(&self, context: &PackContext) -> anyhow::Result<u64> {
        if let Some(latest) = context.recent_chat.last() {
            return Ok(latest.id);
        }
        Ok(self
            .storage
            .append_chat(
                hirsel_proto::ChatAuthor::Agent,
                "Session started.".to_string(),
                None,
            )
            .await?
            .id)
    }

    /// The fail-open path (ruling 2). Injects a minimal brief that names the
    /// original message verbatim, so the main Agent has everything it needs
    /// even though no fork distilled it.
    async fn fallback(&self, message: &WakeMessage, reason: &str) {
        let brief = format!(
            "Triage unavailable ({reason}), so this arrived undistilled.\n\nSource: {}\n\n{}",
            message.source.label(),
            message.text.trim()
        );
        tracing::warn!(
            key = %message.key,
            source = message.source.kind(),
            %reason,
            "escalating a non-owner message without triage"
        );
        if let Err(error) = self.sink.inject(message, &brief).await {
            tracing::error!(
                %error,
                key = %message.key,
                "failed to inject the fallback brief; this non-owner message is lost"
            );
        }
    }
}

/// A late-bound reference to the dispatcher.
///
/// The wake sites that feed forks are wired before the dispatcher can exist:
/// the monitor process engine is registered into the lash core *while that
/// core is being built*, and the dispatcher needs the finished runtime to
/// escalate into. This handle closes that loop — cloned into the wake sites at
/// build time, filled in once at the end of `LashAgentRuntime::start`.
///
/// Backends with no lash session (scripted, degraded) simply never install
/// one, and keep their pre-ADR-0015 wake behaviour.
#[derive(Clone, Default)]
pub struct ForkWakeHandle {
    inner: Arc<std::sync::OnceLock<Arc<ForkWake>>>,
}

impl ForkWakeHandle {
    /// Install the dispatcher. Idempotent; a second install is ignored.
    pub fn install(&self, fork: Arc<ForkWake>) {
        if self.inner.set(fork).is_err() {
            tracing::warn!("fork-wake dispatch was already installed");
        }
    }

    pub fn is_installed(&self) -> bool {
        self.inner.get().is_some()
    }

    /// Route one non-owner message to a triage fork.
    ///
    /// Returns `false` when no dispatcher is installed, so the caller can fall
    /// back to its pre-ADR-0015 behaviour rather than lose the message.
    #[must_use]
    pub fn dispatch(&self, message: WakeMessage) -> bool {
        match self.inner.get() {
            Some(fork) => {
                fork.dispatch(message);
                true
            }
            None => false,
        }
    }
}

fn exit_label(exit: &ForkExit) -> &'static str {
    match exit {
        ForkExit::Dropped { .. } => "drop",
        ForkExit::Recorded { .. } => "record",
        ForkExit::Escalated { .. } => "escalate",
    }
}
