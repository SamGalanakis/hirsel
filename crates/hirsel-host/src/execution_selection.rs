//! One resolution of an execution target from public selectors.
//!
//! The Agent's `threads.delegate` and the Owner's `set_execution` name the same
//! backend the same way, so they must accept and refuse exactly the same
//! things: this is the single place that decides.
use crate::tools::ToolSuite;
use std::path::PathBuf;

/// The public way to name a backend: an agent plus the selectors that agent
/// understands. Every field is optional; the defaults are the configured ones.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionSelectors {
    pub agent: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// Translate the public `ThreadExecutionTarget` shape the client sends into the
/// selectors the shared resolver understands. The client never names a working
/// directory, so the configured default applies.
pub(crate) fn selectors_from_target(
    target: &hirsel_proto::ThreadExecutionTarget,
) -> ExecutionSelectors {
    match target.clone() {
        hirsel_proto::ThreadExecutionTarget::Host { .. } => ExecutionSelectors {
            agent: Some("host".into()),
            ..ExecutionSelectors::default()
        },
        hirsel_proto::ThreadExecutionTarget::Cli {
            agent,
            model,
            variant,
        } => ExecutionSelectors {
            agent: Some(agent),
            model: Some(model),
            variant: Some(variant),
            ..ExecutionSelectors::default()
        },
        hirsel_proto::ThreadExecutionTarget::Lash {
            provider_id,
            model,
            variant,
        } => ExecutionSelectors {
            agent: Some("lash".into()),
            provider_id: Some(provider_id),
            model: Some(model),
            variant: Some(variant),
            ..ExecutionSelectors::default()
        },
    }
}

pub(crate) async fn resolve_execution(
    tools: &ToolSuite,
    input: ExecutionSelectors,
) -> Result<crate::storage::ThreadExecution, String> {
    if input.agent.as_deref() == Some("host") {
        if input.provider_id.is_some()
            || input.model.is_some()
            || input.variant.is_some()
            || input.cwd.is_some()
        {
            return Err(
                "host delegation uses configured provider/model; worker selectors do not apply"
                    .into(),
            );
        }
        Ok(tools
            .storage()
            .host_execution_default()
            .await
            .map_err(|e| e.to_string())?)
    } else if input.agent.as_deref() == Some("lash") {
        // The Owner's row is the gate. The delegation schema already drops
        // the branch while the worker is off, so this refusal is for a
        // stale tool surface, not the ordinary path.
        let native_worker = tools.subagent_model_snapshot().native_worker;
        if !native_worker.enabled {
            return Err(
                    "the native Lash worker is turned off in Settings; enable it to delegate with agent `lash`"
                        .into(),
                );
        }
        let provider = tools
            .capture_native_worker_provider(input.provider_id.as_deref())
            .map_err(|e| e.to_string())?;
        let model = match input.model {
            Some(model) => {
                crate::model_selection::validate_free_text(&model)
                    .map_err(|e| e.to_string())?
                    .id
            }
            // The Owner's model override is the default for this route;
            // an explicit `model` above still wins.
            None if provider.id == crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID => {
                native_worker.model.clone()
            }
            None => {
                return Err(format!(
                    "native Lash worker provider `{}` requires an explicit model",
                    provider.id
                ));
            }
        };
        let variant = input.variant.unwrap_or_else(|| "default".to_string());
        if variant != "default" {
            return Err(format!(
                "native Lash worker variant `{variant}` is unsupported; available variants: default"
            ));
        }
        let cwd = input
            .cwd
            .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
        let cwd =
            std::fs::canonicalize(cwd).map_err(|e| format!("invalid execution directory: {e}"))?;
        if !cwd.is_dir() {
            return Err(format!(
                "invalid execution directory: `{}` is not a directory",
                cwd.display()
            ));
        }
        Ok(crate::storage::ThreadExecution::LashWorker {
            provider,
            model,
            variant,
            cwd,
            tool_profile: crate::storage::NATIVE_CODING_TOOL_PROFILE.to_string(),
        })
    } else {
        if input.provider_id.is_some() {
            return Err("provider_id applies only to agent `lash`".into());
        }
        let agent =
            crate::lash_runtime::parse_agent_kind(input.agent.as_deref().unwrap_or("claude"))?;
        let selected = tools
            .resolve_thread_cli_model(agent, input.model.as_deref(), input.variant.as_deref())
            .map_err(|e| e.to_string())?;
        let cwd = input
            .cwd
            .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
        let cwd =
            std::fs::canonicalize(cwd).map_err(|e| format!("invalid execution directory: {e}"))?;
        Ok(crate::storage::ThreadExecution::Cli {
            agent,
            model: selected.model_id,
            variant: selected.variant,
            cwd,
        })
    }
}
