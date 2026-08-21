//! The lash side of a triage fork: open an ephemeral session, run one turn,
//! delete it.
//!
//! ## Why this is a fresh session and not a graph fork
//!
//! `fork_at` inherits the source session's model, which is exactly what
//! ADR-0015 does not want: the whole point of the fork is that triage runs
//! cheap while the resident interlocutor stays workhorse-grade. So a triage
//! fork is a *new session* on the same core, opened with its own
//! [`SessionSpec`] — its own model from `[fork]`, its own prompt from
//! `prompts/fork.md`, its own small turn budget — and an in-memory store, so
//! nothing it thinks survives it.
//!
//! ## Tool surface
//!
//! The core advertises the main Agent's tools to every session it opens, so
//! the fork's catalog is narrowed *after* open: its own provider is added, and
//! every catalog member that is not one of its five exits is dropped from
//! membership. Membership is lash's execution gate, so a dropped tool does not
//! exist to the fork — it cannot be called, and it is not advertised.
//!
//! ## Ledger note (ADR-0015 ruling 5)
//!
//! Ruling 5 asks for fork usage on the main session's ledger under a `fork`
//! label. At the pinned lash rev, `usage_source` is settable only through
//! `ChildSessionAdmin::create_session`, and that path deliberately exposes no
//! way to run a turn (there is an upstream compile-fail test asserting
//! `start_turn` is not public on it). The fork therefore opens through the
//! session builder and is recorded as a `Child` of the main session, but its
//! tokens are reported on its own session rather than rolled into the main
//! ledger. Reinstating the label needs an upstream affordance, not a host
//! workaround.

use std::sync::Arc;

use anyhow::Context as _;
use lash::{
    LashCore, PromptLayerSink, SessionSpec, TurnBudget, TurnInput, prompt::PromptContribution,
    provider::ProviderHandle, tools::ToolId,
};

use super::{
    dispatch::{TriageRequest, TriageRunner},
    tools::ForkToolProvider,
};
use crate::prompt_config::PromptConfig;

/// Every fork tool id starts with this, which is how the narrowing pass below
/// tells the fork's exits from the main Agent's catalog.
const FORK_TOOL_ID_PREFIX: &str = "hirsel.fork";

/// A triage fork gets two turns, not one: one to read the pack and call its
/// exit, and one to observe the tool result and stop. One would truncate a
/// fork mid-exit; anything larger stops being triage.
const FORK_TURN_BUDGET: usize = 2;

pub struct LashForkRunner {
    core: LashCore,
    provider: ProviderHandle,
    prompts: PromptConfig,
    /// Recorded as the fork's parent so the relation is visible in lash's
    /// session graph.
    main_session_id: String,
}

impl LashForkRunner {
    pub fn new(
        core: LashCore,
        provider: ProviderHandle,
        prompts: PromptConfig,
        main_session_id: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            core,
            provider,
            prompts,
            main_session_id,
        })
    }
}

#[async_trait::async_trait]
impl TriageRunner for LashForkRunner {
    async fn run_triage(&self, request: TriageRequest) -> anyhow::Result<()> {
        let fork = self
            .prompts
            .fork()
            .ok_or_else(|| anyhow::anyhow!("no [fork] configuration for the booted provider"))?;
        let model = self
            .prompts
            .fork_model_spec()
            .context("resolve the fork model")?;
        // A store-less session id may never be reused, and a fork is
        // single-use by construction, so a fresh uuid every time.
        let session_id = format!("fork-{}", uuid::Uuid::new_v4());

        let session = self
            .core
            .session(&session_id)
            // `session_spec` replaces the whole spec, so it precedes
            // `provider` — which rewrites the spec's provider id.
            .session_spec(
                SessionSpec::new()
                    .model(model)
                    .turn_budget(TurnBudget::bounded(FORK_TURN_BUDGET)),
            )
            .provider(self.provider.clone())
            .parent(self.main_session_id.clone())
            // In-memory: no fork state survives the turn (ADR-0015).
            .store(Arc::new(lash::persistence::InMemorySessionStore::new()))
            .prompt_contribution(PromptContribution::guidance(
                "Hirsel Fork",
                fork.prompt.text,
            ))
            .open()
            .await
            .context("open an ephemeral triage fork session")?;

        let outcome = run_one_triage_turn(&session, request).await;
        // Close regardless: a fork that failed still must not linger.
        if let Err(error) = session.close().await {
            tracing::warn!(%error, %session_id, "failed to close a triage fork session");
        }
        outcome
    }
}

async fn run_one_triage_turn(
    session: &lash::LashSession,
    request: TriageRequest,
) -> anyhow::Result<()> {
    session
        .tools()
        .add_provider(Arc::new(ForkToolProvider::new(Arc::clone(&request.tools))))
        .await
        .context("install the fork's exits")?;
    narrow_to_fork_exits(session).await?;

    let output = session
        .turn(TurnInput::text(request.pack))
        .run()
        .await
        .with_context(|| format!("run the triage turn for {}", request.label))?;
    if !output.is_success() {
        anyhow::bail!("the triage turn did not complete successfully");
    }
    Ok(())
}

/// Drop every catalog member that is not one of the fork's exits.
///
/// The core advertises the main Agent's whole catalog — `subagents_spawn`,
/// `pings_send`, `shell_run`, plugin tools — to every session it opens. A fork
/// must see none of it, and "must not" here is capability: membership is
/// lash's execution gate, so a non-member is neither advertised nor callable.
async fn narrow_to_fork_exits(session: &lash::LashSession) -> anyhow::Result<()> {
    let state = session
        .tools()
        .state()
        .await
        .context("read the fork's tool state")?;
    let drops = state
        .iter()
        .filter(|(id, _)| !id.as_str().starts_with(FORK_TOOL_ID_PREFIX))
        .map(|(id, _)| (ToolId::new(id.as_str()), false))
        .collect::<Vec<_>>();
    if drops.is_empty() {
        return Ok(());
    }
    session
        .tools()
        .set_membership_many(&drops)
        .await
        .context("narrow the fork's catalog to its exits")?;
    Ok(())
}
