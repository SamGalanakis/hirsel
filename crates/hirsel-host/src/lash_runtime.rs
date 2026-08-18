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
    AgentActivityState, Event, HostToClient, ModelSelection, ModelSnapshot, Ping, QuickReply,
    SendMode, SubagentModelCatalog, ToolCallSummary, TurnEventKind,
};
use lash::{
    InputItem, PromptLayerSink, TurnInput,
    observe::RemoteSessionObservationStreamItem,
    plugins::{
        PluginError, PluginExtensionContribution, PluginFactory, PluginRegistrar,
        PluginSessionContext, SessionPlugin,
    },
    process::{
        ProcessAwaitOutput, ProcessAwaiter, ProcessCompletionAuthority, ProcessEventAppendRequest,
        ProcessEventType, ProcessIdentity, ProcessInput, ProcessStartRequest, ProcessStatus,
        ProcessWakeDelivery, ProcessWakeSpec, RecoveryDisposition, SessionScope,
    },
    provider::{ProviderHandle, ProviderOptions, ReasoningSelection},
    remote::{
        observations::{RemoteSessionCursor, RemoteSessionObservationEventPayload},
        usage::RemoteTurnEvent,
    },
    rlm::{RlmDialect, RlmSessionBuilderExt},
    runtime::{QueuedWorkDriver, QueuedWorkRunHandle, QueuedWorkRunRequest},
    tools::{
        LashlangToolBinding, StaticToolExecute, ToolCall, ToolContract, ToolDefinition,
        ToolDefinitionLashlangExt, ToolManifest, ToolProvider, ToolResult,
    },
    triggers::LashSchema,
};
use lash_core::{
    ProcessEngine, ProcessEngineRunContext, ProcessEngineValidationContext,
    ProcessEventSemanticsSpec, ProcessOriginator, ProcessRunOutcome, ProcessTerminalSpec,
    ProcessValueSelector, SessionPolicy, TriggerStore, TriggerSubscriptionFilter,
    TurnInputCheckpointBoundary, TurnInputIngress, plugin::ProcessEngineContributionContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, broadcast};
use uuid::Uuid;

use crate::{
    BroadcastLog,
    config::{AgentMode, Config, DriverMode, ProviderMode},
    host_config::ConfigStore,
    model_selection::ModelSelectionState,
    monitors::{output_tail, run_monitor_tick},
    storage::{MonitorRecord, MonitorWakeOn, StoredBlob},
    subagent_models::SubagentModelState,
    text::short_label,
    tools::{JudgmentOptionInput, ToolSuite},
};

/// The RLM source dialect the main agent is prompted in and pinned to. Changing
/// this constant rotates the agent session (see `agent_tool_surface`), because
/// a recorded dialect pin is durable for the session's lifetime.
const AGENT_RLM_DIALECT: RlmDialect = RlmDialect::Typescript;

/// Page size for the boot-time non-terminal process scan.
const NON_TERMINAL_SCAN_PAGE: std::num::NonZeroUsize =
    std::num::NonZeroUsize::new(256).expect("non-terminal scan page size is non-zero");

const HIRSEL_SUBAGENT_ENGINE: &str = "hirsel_subagent";
const HIRSEL_MONITOR_ENGINE: &str = "hirsel_monitor";
const SUBAGENT_COMPLETED: &str = "subagent.completed";
const SUBAGENT_FAILED: &str = "subagent.failed";
const SUBAGENT_CANCELLED: &str = "subagent.cancelled";
const SUBAGENT_ABANDONED: &str = "subagent.abandoned";
const MONITOR_WAKE_EVENT: &str = "monitor.wake";
const TIMER_SOURCE_TYPE: &str = "timer.Schedule";
const TIMER_EVENT_TYPE: &str = "timer.Tick";
const TIMER_MIN_RECURRING_SECS: u64 = 60;
#[cfg(not(test))]
const SNOOZE_TICK_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(test)]
const SNOOZE_TICK_INTERVAL: Duration = Duration::from_millis(25);
pub(crate) const AGENT_PROMPT: &str = include_str!("../../../prompts/agent.md");

mod bridges;
mod condense;
mod executor;
mod lifecycle;
mod plugin;
mod process_engines;
mod provider;
mod runtime;
mod scripted;
mod timeline;
mod timers;
mod tool_defs;
mod tool_results;
mod tool_schemas;
mod turn;

#[cfg(test)]
mod tests;

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
pub(crate) use provider::agent_guidance;
pub use runtime::{AgentRuntime, CancelQueuedResult, OwnerTurn, TaskActionContext};
pub(crate) use turn::append_mentioned_ping_context;
