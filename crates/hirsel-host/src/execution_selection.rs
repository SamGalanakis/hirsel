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
        hirsel_proto::ThreadExecutionTarget::Native { provider_id, model } => ExecutionSelectors {
            agent: Some("native".into()),
            provider_id: Some(provider_id),
            model: Some(model),
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
    }
}

fn resolved_cwd(requested: Option<PathBuf>) -> Result<PathBuf, String> {
    let cwd = match requested {
        Some(cwd) => cwd,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let cwd =
        std::fs::canonicalize(cwd).map_err(|e| format!("invalid execution directory: {e}"))?;
    if !cwd.is_dir() {
        return Err(format!(
            "invalid execution directory: `{}` is not a directory",
            cwd.display()
        ));
    }
    Ok(cwd)
}

pub(crate) async fn resolve_execution(
    tools: &ToolSuite,
    input: ExecutionSelectors,
) -> Result<crate::storage::ThreadExecution, String> {
    if input.agent.as_deref().unwrap_or("native") == "native" {
        if input.variant.is_some() {
            return Err(
                "Native execution takes a provider, a model and a working directory; variant does not apply"
                    .into(),
            );
        }
        // Naming neither is the configured default Native execution — the
        // Settings choice, still the answer for every Thread that has not
        // overridden it.
        let default = tools
            .storage()
            .native_execution_default()
            .await
            .map_err(|e| e.to_string())?;
        let crate::storage::ThreadExecution::Native {
            provider_id: default_provider_id,
            model: default_model,
            cwd: default_cwd,
            ..
        } = &default
        else {
            return Err("the configured default execution is not a Native backend".into());
        };
        if input.provider_id.is_none() && input.model.is_none() && input.cwd.is_none() {
            return Ok(default.clone());
        }
        let provider_id = input
            .provider_id
            .clone()
            .unwrap_or_else(|| default_provider_id.clone());
        // The Native provider is a roster instance, judged by exactly the rules
        // the Settings picker is judged by.
        let choice = tools
            .native_provider(&provider_id)
            .map_err(|e| e.to_string())?;
        let mode = crate::model_selection::SelectionMode::for_choice(&choice);
        let model_id = match input.model.clone() {
            Some(model) => model,
            // The provider changed but the model did not: the stored model
            // means nothing on the new route, so its own default applies.
            None if provider_id == *default_provider_id => default_model.id.clone(),
            None => choice.default_model.clone(),
        };
        let selection = crate::model_selection::validate_model_id_in_mode(
            &mode,
            hirsel_proto::AgentSlot::Main,
            &model_id,
        )
        .map_err(|e| format!("Native provider `{provider_id}`: {e}"))?;
        let model =
            crate::model_selection::spec_for(&mode, &selection).map_err(|e| e.to_string())?;
        let cwd = match input.cwd {
            Some(cwd) => resolved_cwd(Some(cwd))?,
            None => default_cwd.clone(),
        };
        Ok(crate::storage::ThreadExecution::Native {
            provider_id: choice.id,
            model,
            cwd,
        })
    } else {
        if input.provider_id.is_some() {
            return Err("provider_id applies only to agent `native`".into());
        }
        let agent = crate::lash_runtime::parse_agent_kind(
            input.agent.as_deref().expect("the native branch took None"),
        )?;
        let selected = tools
            .resolve_thread_cli_model(agent, input.model.as_deref(), input.variant.as_deref())
            .map_err(|e| e.to_string())?;
        Ok(crate::storage::ThreadExecution::Cli {
            agent,
            model: selected.model_id,
            variant: selected.variant,
            cwd: resolved_cwd(input.cwd)?,
        })
    }
}
