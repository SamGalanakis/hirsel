//! Leased-stream lane state machine kernel — faithful Rust port of lane 3145
//! (`sdk/client/state.ts` and `sdk/client/lanes.ts`).
//!
//! Pure transition function (`step`) with zero I/O so both `sdk/client/state.ts`
//! and `hirsel-core-rt` run the exact same executable transition vectors.
use super::codec::{Entry, Frame, KeyedDone, Outcome, Refusal, StreamId};
use std::collections::BTreeMap;

pub const INITIAL_BACKOFF_MS: u64 = 100;
pub const MAX_BACKOFF_MS: u64 = 10_000;
pub const STABLE_MS: u64 = 30_000;

pub(super) fn next_backoff(ms: u64) -> u64 {
    (ms.saturating_mul(2)).min(MAX_BACKOFF_MS)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialSource {
    Initial,
    Refreshed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Credential {
    pub bytes: Vec<u8>,
    pub source: CredentialSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attempt {
    Idle,
    Preparing,
    WebSocket,
    Retry,
    Http { p: u64 },
}

impl Attempt {
    pub fn owes(self) -> bool {
        matches!(self, Self::Preparing | Self::WebSocket | Self::Http { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaneState {
    pub attempt: Attempt,
    pub backoff: u64,
}

impl Default for LaneState {
    fn default() -> Self {
        Self {
            attempt: Attempt::Idle,
            backoff: INITIAL_BACKOFF_MS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoppedState {
    Fenced,
    StorageUnavailable,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcquireRequest {
    Initial,
    Refresh { refused: Option<Vec<u8>> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Loss {
    Transient,
    Incompatible { code: u16 },
    CredentialLost { cause: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LostCall {
    LocallyUnknown,
    RequestMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientState {
    Connecting,
    Ready,
    Backoff,
    CredentialNeeded,
    Incompatible,
    Unbound,
    Fenced,
    StorageUnavailable,
    Closed,
}

impl From<StoppedState> for ClientState {
    fn from(s: StoppedState) -> Self {
        match s {
            StoppedState::Fenced => Self::Fenced,
            StoppedState::StorageUnavailable => Self::StorageUnavailable,
            StoppedState::Closed => Self::Closed,
        }
    }
}

/// Phase of the 3145 client state machine (`sdk/client/state.ts`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Phase {
    Initial {
        epoch: u64,
        backoff: u64,
    },
    Starting {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
    },
    Acquiring {
        epoch: u64,
        backoff: u64,
        request: AcquireRequest,
    },
    Opening {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
    },
    AwaitWelcome {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
    },
    Binding {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
    },
    Placing {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
        remaining: usize,
        welcomed_at: u64,
        width: usize,
    },
    Ready {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
        welcomed_at: u64,
        version: u64,
        width: usize,
        lanes: BTreeMap<u32, LaneState>,
    },
    Losing {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
        loss: Loss,
    },
    Backoff {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
    },
    Incompatible {
        epoch: u64,
        backoff: u64,
        credential: Option<Credential>,
    },
    Unbound {
        epoch: u64,
        backoff: u64,
    },
    Stopped {
        epoch: u64,
        state: StoppedState,
    },
}

impl Phase {
    pub fn initial() -> Self {
        Self::Initial {
            epoch: 0,
            backoff: INITIAL_BACKOFF_MS,
        }
    }

    pub fn epoch(&self) -> u64 {
        match self {
            Self::Initial { epoch, .. }
            | Self::Starting { epoch, .. }
            | Self::Acquiring { epoch, .. }
            | Self::Opening { epoch, .. }
            | Self::AwaitWelcome { epoch, .. }
            | Self::Binding { epoch, .. }
            | Self::Placing { epoch, .. }
            | Self::Ready { epoch, .. }
            | Self::Losing { epoch, .. }
            | Self::Backoff { epoch, .. }
            | Self::Incompatible { epoch, .. }
            | Self::Unbound { epoch, .. }
            | Self::Stopped { epoch, .. } => *epoch,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(
            self,
            Self::Opening { .. }
                | Self::AwaitWelcome { .. }
                | Self::Binding { .. }
                | Self::Placing { .. }
                | Self::Ready { .. }
        )
    }

    pub fn can_reconnect(&self) -> bool {
        matches!(
            self,
            Self::Backoff { .. } | Self::Incompatible { .. } | Self::Unbound { .. }
        )
    }

    pub fn lane_owed(&self, id: u32) -> bool {
        match self {
            Self::Ready { lanes, .. } => lanes.get(&id).is_some_and(|l| l.attempt.owes()),
            _ => false,
        }
    }

    pub fn owed_count(&self) -> usize {
        match self {
            Self::Ready { lanes, .. } => lanes.values().filter(|l| l.attempt.owes()).count(),
            _ => 0,
        }
    }

    pub(super) fn dormant_parts(&self) -> Option<(u64, u64, Option<Credential>)> {
        match self {
            Self::Initial { epoch, backoff } => Some((*epoch, *backoff, None)),
            Self::Starting {
                epoch,
                backoff,
                credential,
            }
            | Self::Opening {
                epoch,
                backoff,
                credential,
            }
            | Self::AwaitWelcome {
                epoch,
                backoff,
                credential,
            }
            | Self::Binding {
                epoch,
                backoff,
                credential,
            }
            | Self::Placing {
                epoch,
                backoff,
                credential,
                ..
            }
            | Self::Ready {
                epoch,
                backoff,
                credential,
                ..
            }
            | Self::Losing {
                epoch,
                backoff,
                credential,
                ..
            }
            | Self::Backoff {
                epoch,
                backoff,
                credential,
            }
            | Self::Incompatible {
                epoch,
                backoff,
                credential,
            } => Some((*epoch, *backoff, credential.clone())),
            Self::Unbound { epoch, backoff } | Self::Acquiring { epoch, backoff, .. } => {
                Some((*epoch, *backoff, None))
            }
            Self::Stopped { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StepContext {
    pub wants: usize,
    pub has_credential_source: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Start,
    Reconnect,
    Begin {
        epoch: u64,
    },
    SocketOpen {
        epoch: u64,
    },
    LossRecorded {
        epoch: u64,
    },
    BackoffElapsed {
        epoch: u64,
    },
    RetryElapsed {
        epoch: u64,
        lane: u32,
    },
    CallFinished {
        epoch: u64,
        lane: u32,
    },
    RetryCall {
        epoch: u64,
        lane: u32,
    },
    StableElapsed {
        epoch: u64,
        at: u64,
    },
    CredentialResult {
        epoch: u64,
        bytes: Option<Vec<u8>>,
    },
    SocketLost {
        epoch: u64,
        code: Option<u16>,
    },
    Frame {
        epoch: u64,
        frame: Frame,
    },
    WelcomeBound {
        epoch: u64,
        at: u64,
        width: usize,
    },
    AdvanceVersion {
        epoch: u64,
        version: u64,
    },
    PrepareCall {
        epoch: u64,
        lane: u32,
    },
    SendCall {
        epoch: u64,
        lane: u32,
        delivery: Attempt,
    },
    HttpComplete {
        epoch: u64,
        lane: u32,
        p: u64,
    },
    Transient {
        epoch: u64,
    },
    Incompatible {
        epoch: u64,
        code: u16,
    },
    CredentialLost {
        cause: String,
    },
    Unbound {
        cause: String,
    },
    Stop {
        state: StoppedState,
        outcome: Option<LostCall>,
        diagnostic: Option<String>,
    },
}

impl Event {
    pub(super) fn epoch(&self) -> Option<u64> {
        match self {
            Self::Start
            | Self::Reconnect
            | Self::CredentialLost { .. }
            | Self::Unbound { .. }
            | Self::Stop { .. } => None,
            Self::Begin { epoch }
            | Self::SocketOpen { epoch }
            | Self::LossRecorded { epoch }
            | Self::BackoffElapsed { epoch }
            | Self::RetryElapsed { epoch, .. }
            | Self::CallFinished { epoch, .. }
            | Self::RetryCall { epoch, .. }
            | Self::StableElapsed { epoch, .. }
            | Self::CredentialResult { epoch, .. }
            | Self::SocketLost { epoch, .. }
            | Self::Frame { epoch, .. }
            | Self::WelcomeBound { epoch, .. }
            | Self::AdvanceVersion { epoch, .. }
            | Self::PrepareCall { epoch, .. }
            | Self::SendCall { epoch, .. }
            | Self::HttpComplete { epoch, .. }
            | Self::Transient { epoch }
            | Self::Incompatible { epoch, .. } => Some(*epoch),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    CancelTimers,
    ClearSubscriptions,
    CloseSocket,
    OpenSocket,
    SendHello,
    RecordLoss,
    ResetReady,
    ContinueFrame,
    Begin,
    SendPending,
    State(ClientState),
    RefreshCredential,
    Diagnostic(String),
    BindWelcome(Frame),
    Update(Frame),
    ArmBackoff { max_ms: u64 },
    ArmRetry { lane: u32, max_ms: u64 },
    BecomeReady { welcomed_at: u64 },
    Finish(LostCall),
    RejectReady(String),
    RejectCalls(String),
    ClearOverlays,
    RunPrepare { lane: u32 },
    RunSend { lane: u32 },
    RunAdvance { version: u64 },
    RunHttpComplete { lane: u32, p: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition {
    pub phase: Phase,
    pub actions: Vec<Action>,
}

pub(super) fn teardown(actions: &mut Vec<Action>) {
    actions.push(Action::CancelTimers);
    actions.push(Action::CloseSocket);
    actions.push(Action::ClearSubscriptions);
}

pub(super) fn begin(
    epoch: u64,
    backoff: u64,
    credential: Option<Credential>,
    ctx: StepContext,
) -> Transition {
    let next_epoch = epoch + 1;
    if ctx.has_credential_source && credential.is_none() {
        return Transition {
            phase: Phase::Acquiring {
                epoch: next_epoch,
                backoff,
                request: AcquireRequest::Initial,
            },
            actions: vec![
                Action::State(ClientState::Connecting),
                Action::RefreshCredential,
            ],
        };
    }
    Transition {
        phase: Phase::Opening {
            epoch: next_epoch,
            backoff,
            credential,
        },
        actions: vec![
            Action::State(ClientState::Connecting),
            Action::ClearSubscriptions,
            Action::OpenSocket,
        ],
    }
}

pub(super) fn unbound(epoch: u64, backoff: u64, cause: &str, ctx: StepContext) -> Transition {
    let message = if ctx.has_credential_source {
        format!(
            "Unbound: the place refused the credential and one refresh did not recover ({cause})"
        )
    } else {
        format!(
            "Unbound: the place binds callers and needs a credential; pass `credential` to Client.open ({cause})"
        )
    };
    let mut actions = Vec::new();
    teardown(&mut actions);
    actions.push(Action::State(ClientState::Unbound));
    actions.push(Action::Diagnostic(message));
    actions.push(Action::RejectReady("Unbound".into()));
    Transition {
        phase: Phase::Unbound {
            epoch: epoch + 1,
            backoff,
        },
        actions,
    }
}

pub(super) fn lose(
    epoch: u64,
    backoff: u64,
    credential: Option<Credential>,
    loss: Loss,
    ctx: StepContext,
) -> Transition {
    let next_epoch = epoch + 1;
    match loss {
        Loss::Transient => {
            let mut actions = Vec::new();
            teardown(&mut actions);
            actions.push(Action::ArmBackoff { max_ms: backoff });
            actions.push(Action::State(ClientState::Backoff));
            Transition {
                phase: Phase::Backoff {
                    epoch: next_epoch,
                    backoff: next_backoff(backoff),
                    credential,
                },
                actions,
            }
        }
        Loss::Incompatible { code } => {
            let message = if code == 1009 {
                "Stored reply exceeds the server message limit (1009)".to_owned()
            } else {
                format!("Incompatible ({code}); reload bindings or reconnect explicitly")
            };
            let mut actions = Vec::new();
            teardown(&mut actions);
            actions.push(Action::State(ClientState::Incompatible));
            actions.push(Action::Diagnostic(message));
            if code == 1009 {
                actions.push(Action::RejectCalls("ReplyTooLarge".into()));
            }
            actions.push(Action::RejectReady(if code == 1009 {
                "ReplyTooLarge".into()
            } else {
                "Incompatible".into()
            }));
            Transition {
                phase: Phase::Incompatible {
                    epoch: next_epoch,
                    backoff,
                    credential,
                },
                actions,
            }
        }
        Loss::CredentialLost { cause } => {
            if !ctx.has_credential_source
                || credential
                    .as_ref()
                    .is_some_and(|c| c.source == CredentialSource::Refreshed)
            {
                return unbound(epoch, backoff, &cause, ctx);
            }
            let mut actions = Vec::new();
            teardown(&mut actions);
            actions.push(Action::State(ClientState::CredentialNeeded));
            actions.push(Action::Diagnostic(format!(
                "CredentialNeeded: the place refused the session credential ({cause}); refreshing once"
            )));
            actions.push(Action::RefreshCredential);
            Transition {
                phase: Phase::Acquiring {
                    epoch: next_epoch,
                    backoff,
                    request: AcquireRequest::Refresh {
                        refused: credential.map(|c| c.bytes),
                    },
                },
                actions,
            }
        }
    }
}

pub(super) fn socket_loss(phase: &Phase, code: Option<u16>) -> Loss {
    if code == Some(4401) {
        return Loss::CredentialLost {
            cause: "Close 4401".into(),
        };
    }
    if code == Some(1008) && !matches!(phase, Phase::Ready { .. } | Phase::Placing { .. }) {
        return Loss::CredentialLost {
            cause: "Close 1008 before Welcome".into(),
        };
    }
    if matches!(code, Some(1007 | 1008 | 1009)) {
        return Loss::Incompatible {
            code: code.unwrap(),
        };
    }
    Loss::Transient
}

pub(super) fn become_ready(
    epoch: u64,
    backoff: u64,
    credential: Option<Credential>,
    welcomed_at: u64,
    width: usize,
) -> Transition {
    Transition {
        phase: Phase::Ready {
            epoch,
            backoff,
            credential,
            welcomed_at,
            version: 0,
            width,
            lanes: BTreeMap::new(),
        },
        actions: vec![
            Action::State(ClientState::Ready),
            Action::BecomeReady { welcomed_at },
            Action::ContinueFrame,
        ],
    }
}

pub use super::kernel_step::step;
pub use super::lanes::{PendingLaneCall, StreamLane};
