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
/// message is escalated by the fail-open path. Enforced inside the runner, so
/// the fork's session is still closed when it fires.
pub const FORK_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Backstop for the whole dispatch, in case a runner hangs somewhere other
/// than the turn it bounds itself. Deliberately longer than
/// [`FORK_TURN_TIMEOUT`] so the runner's own timeout is what normally fires —
/// this one cancels the runner mid-flight, which is a session leak, and is a
/// last resort rather than the design.
const FORK_DISPATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
            // A panic anywhere in the dispatch body would otherwise take the
            // message with it: the task dies, nobody observes the join handle,
            // and a Sub-agent completion silently never happened. Catching it
            // here means even a bug in triage degrades to the fail-open brief.
            let trigger = message.clone();
            let body = std::panic::AssertUnwindSafe(fork.dispatch_now(message));
            if futures_util::FutureExt::catch_unwind(body).await.is_err() {
                tracing::error!(key = %trigger.key, "triage fork dispatch panicked");
                fork.fallback(&trigger, "the triage fork dispatch panicked")
                    .await;
            }
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
        let anchor = anchor(&context);
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
            tokio::time::timeout(FORK_DISPATCH_TIMEOUT, self.runner.run_triage(request)).await;
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
                    timeout_secs = FORK_DISPATCH_TIMEOUT.as_secs(),
                    "triage fork dispatch hit its backstop timeout"
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

/// The chat message a recorded event hangs off, if the transcript has one.
///
/// Events need an anchor, and a fork has no Owner turn to anchor to, so it
/// reuses the latest chat message. On an empty transcript the answer is `None`
/// and stays `None`: minting a chat line here would be the fork speaking to the
/// Owner, which ADR-0015 forbids outright — and "a fork wrote 'Session
/// started.' into the Owner's transcript" is a worse outcome than any record it
/// was about to make. [`ForkTools`] handles the `None` case by failing toward
/// the main queue: the would-be record is escalated as a brief instead.
fn anchor(context: &PackContext) -> Option<u64> {
    context.recent_chat.last().map(|message| message.id)
}

fn exit_label(exit: &ForkExit) -> &'static str {
    match exit {
        ForkExit::Dropped { .. } => "drop",
        ForkExit::Recorded { .. } => "record",
        ForkExit::Escalated { .. } => "escalate",
    }
}
