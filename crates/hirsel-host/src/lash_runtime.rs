use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use hirsel_drivers::{AgentKind, TerminalOutcome};
use hirsel_proto::{
    AgentActivityState, Blob, HostToClient, ModelSelection, ModelSnapshot, SendMode,
    SubagentModelCatalog, ToolCallSummary, TurnEventKind,
};
use lash::{
    InputItem, PromptLayerSink, QueuedTurnDrain, TurnInput,
    observe::RemoteSessionObservationStreamItem,
    plugins::{
        PluginError, PluginExtensionContribution, PluginFactory, PluginOptions, PluginRegistrar,
        PluginSessionContext, SessionPlugin,
    },
    process::{
        ProcessAwaitOutput, ProcessEventAppendRequest, ProcessEventType, ProcessExecutionEnvSpec,
        ProcessIdentity, ProcessInput, ProcessStartRequest, RecoveryContract, SessionScope,
    },
    provider::{ProviderHandle, ProviderOptions, ReasoningSelection},
    remote::{
        observations::{RemoteSessionCursor, RemoteSessionObservationEventPayload},
        usage::RemoteTurnEvent,
    },
    rlm::{RLM_PROTOCOL_PLUGIN_ID, RlmCreateExtras, RlmDialect},
    runtime::{NativeQueuedWork, QueuedWorkRunHandle, QueuedWorkRunRequest},
    tools::{
        StaticToolExecute, ToolBinding, ToolCall, ToolContract, ToolDefinition,
        ToolDefinitionBindingExt, ToolManifest, ToolOutcome, ToolProvider,
    },
    triggers::LashSchema,
};
use lash_core::{
    ProcessEngine, ProcessEngineRunContext, ProcessEngineValidationContext,
    ProcessEventSemanticsSpec, ProcessOriginator, ProcessRunOutcome, SessionPolicy, TriggerStore,
    TriggerSubscriptionFilter, TurnInputIngress, plugin::ProcessEngineContributionContext,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, broadcast};
use uuid::Uuid;

use crate::{
    BroadcastLog,
    boot_provider::BootPlan,
    config::{AgentMode, Config, DriverMode, ProviderMode},
    host_config::ConfigStore,
    model_selection::ModelSelectionState,
    monitors::{output_tail, run_monitor_tick},
    prompt_config::PromptConfig,
    providers::ProviderRosterState,
    storage::{MonitorRecord, MonitorWakeOn},
    tools::ToolSuite,
};

/// The RLM source dialect the main agent is prompted in and pinned to. Changing
/// this constant rotates the agent session (see `agent_tool_surface`), because
/// a recorded dialect pin is durable for the session's lifetime.
const AGENT_RLM_DIALECT: RlmDialect = RlmDialect::Typescript;

const HIRSEL_MONITOR_ENGINE: &str = "hirsel_monitor";
const MONITOR_WAKE_EVENT: &str = "monitor.wake";
/// Prefix stamped on every queued turn a triage fork escalates (ADR-0015).
///
/// Escalation rides the same `enqueue_turn_input` path an Owner queued turn
/// takes, so the marker is what lets the main prompt — and anything reading the
/// timeline — tell a distilled brief apart from something the Owner said.
const FORK_BRIEF_MARKER: &str = "[fork brief]";
const TIMER_SOURCE_TYPE: &str = "timer.Schedule";
const TIMER_EVENT_TYPE: &str = "timer.Tick";
const TIMER_MIN_RECURRING_SECS: u64 = 60;
#[cfg(not(test))]
const SNOOZE_TICK_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(test)]
const SNOOZE_TICK_INTERVAL: Duration = Duration::from_millis(25);

mod artifact_tools;
mod bridges;
mod condense;
mod executor;
mod lifecycle;
mod plugin;
mod process_engines;
mod provider;
mod runtime;
mod scripted;
mod thread_lanes;
mod thread_queue;
use thread_lanes::ThreadRuntimeRegistry;
mod scoped_tools;
mod thread_schemas;
pub(crate) use scoped_tools::{ScopedThreadTools, scoped_mcp_catalog};
mod timeline;
mod timers;
mod tool_defs;
mod tool_results;
mod tool_schemas;
mod turn;
use thread_schemas::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod thread_recovery_tests;
#[cfg(test)]
mod thread_tests;
#[cfg(test)]
mod upgrade_tests;

#[cfg(test)]
use bridges::*;
use condense::*;
use executor::*;
use lifecycle::*;
use plugin::*;
use process_engines::*;
use provider::*;
use runtime::*;
use scripted::*;
use timeline::*;
use tool_defs::*;
use tool_results::*;
use tool_schemas::*;
use turn::*;

pub use provider::RuntimeConfig;
pub(crate) use provider::agent_host_section;
pub use runtime::{AgentRuntime, CancelQueuedResult, OwnerTurn, ThreadActionContext};

#[cfg(test)]
pub(crate) async fn test_owner_turn_input(
    turn: &OwnerTurn,
    storage: &crate::storage::Storage,
) -> anyhow::Result<TurnInput> {
    turn::owner_turn_input(turn, storage).await
}

mod runtime_tasks;
pub(crate) use runtime_tasks::RuntimeTasks;

mod cli_turn;
use cli_turn::CliTurn;

#[cfg(test)]
#[path = "lash_runtime/resource_tests.rs"]
mod resource_tests;
