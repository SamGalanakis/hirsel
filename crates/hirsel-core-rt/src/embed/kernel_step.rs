//! Transition function matching `sdk/client/state.ts::step`.
use super::codec::{Entry, Frame};
use super::kernel::*;
/// Pure transition function matching `sdk/client/state.ts::step` 1:1.
pub fn step(phase: Phase, event: Event, ctx: StepContext) -> Transition {
    if let Some(ev_epoch) = event.epoch()
        && phase.epoch() != ev_epoch
    {
        return Transition {
            phase,
            actions: Vec::new(),
        };
    }
    match event {
        Event::Start => {
            if let Phase::Initial { epoch, backoff } = phase {
                return begin(epoch, backoff, None, ctx);
            }
        }
        Event::Reconnect => {
            if phase.can_reconnect() {
                let (epoch, backoff, cred) = phase.dormant_parts().unwrap();
                let next_cred = cred.map(|c| Credential {
                    bytes: c.bytes,
                    source: CredentialSource::Initial,
                });
                return Transition {
                    phase: Phase::Starting {
                        epoch: epoch + 1,
                        backoff,
                        credential: next_cred,
                    },
                    actions: vec![
                        Action::CancelTimers,
                        Action::ResetReady,
                        Action::State(ClientState::Connecting),
                        Action::Begin,
                    ],
                };
            }
        }
        Event::Begin { .. } => {
            if let Phase::Starting {
                epoch,
                backoff,
                credential,
            } = phase
            {
                return begin(epoch, backoff, credential, ctx);
            }
        }
        Event::SocketOpen { .. } => {
            if let Phase::Opening {
                epoch,
                backoff,
                credential,
            } = phase
            {
                return Transition {
                    phase: Phase::AwaitWelcome {
                        epoch,
                        backoff,
                        credential,
                    },
                    actions: vec![Action::SendHello],
                };
            }
        }
        Event::CredentialResult { bytes, .. } => {
            if let Phase::Acquiring {
                epoch,
                backoff,
                request,
            } = phase
            {
                let refused = match &request {
                    AcquireRequest::Refresh { refused } => refused.as_deref(),
                    AcquireRequest::Initial => None,
                };
                let Some(bytes) = bytes else {
                    return unbound(epoch, backoff, "the credential source gave none", ctx);
                };
                if refused == Some(bytes.as_slice()) {
                    return unbound(
                        epoch,
                        backoff,
                        "the refresh returned the refused credential",
                        ctx,
                    );
                }
                let source = match request {
                    AcquireRequest::Refresh { .. } => CredentialSource::Refreshed,
                    AcquireRequest::Initial => CredentialSource::Initial,
                };
                let mut next = begin(epoch, backoff, Some(Credential { bytes, source }), ctx);
                next.actions
                    .insert(0, Action::State(ClientState::Connecting));
                return next;
            }
        }
        Event::SocketLost { code, .. } => {
            if phase.is_connected() {
                let loss = socket_loss(&phase, code);
                let (epoch, backoff, credential) = phase.dormant_parts().unwrap();
                let mut actions = Vec::new();
                teardown(&mut actions);
                actions.push(Action::RecordLoss);
                return Transition {
                    phase: Phase::Losing {
                        epoch: epoch + 1,
                        backoff,
                        credential,
                        loss,
                    },
                    actions,
                };
            }
        }
        Event::LossRecorded { .. } => {
            if let Phase::Losing {
                epoch,
                backoff,
                credential,
                loss,
            } = phase
            {
                return lose(epoch, backoff, credential, loss, ctx);
            }
        }
        Event::BackoffElapsed { .. } => {
            if let Phase::Backoff {
                epoch,
                backoff,
                credential,
            } = phase
            {
                let mut next = begin(epoch, backoff, credential, ctx);
                next.actions.insert(0, Action::CancelTimers);
                return next;
            }
        }
        Event::StableElapsed { at, .. } => {
            if let Phase::Ready {
                epoch,
                backoff,
                credential,
                welcomed_at,
                version,
                width,
                lanes,
            } = phase
            {
                let next_backoff = if at.saturating_sub(welcomed_at) >= STABLE_MS {
                    INITIAL_BACKOFF_MS
                } else {
                    backoff
                };
                return Transition {
                    phase: Phase::Ready {
                        epoch,
                        backoff: next_backoff,
                        credential,
                        welcomed_at,
                        version,
                        width,
                        lanes,
                    },
                    actions: Vec::new(),
                };
            }
        }
        Event::Transient { .. } => {
            if phase.is_connected() {
                let (epoch, backoff, credential) = phase.dormant_parts().unwrap();
                return lose(epoch, backoff, credential, Loss::Transient, ctx);
            }
        }
        Event::Incompatible { code, .. } => {
            if phase.is_connected() {
                let (epoch, backoff, credential) = phase.dormant_parts().unwrap();
                return lose(epoch, backoff, credential, Loss::Incompatible { code }, ctx);
            }
        }
        Event::CredentialLost { cause } => {
            if phase.is_connected() || matches!(phase, Phase::Backoff { .. }) {
                let (epoch, backoff, credential) = phase.dormant_parts().unwrap();
                return lose(
                    epoch,
                    backoff,
                    credential,
                    Loss::CredentialLost { cause },
                    ctx,
                );
            }
        }
        Event::Unbound { cause } => {
            if phase.is_connected() || matches!(phase, Phase::Backoff { .. }) {
                let (epoch, backoff, _) = phase.dormant_parts().unwrap();
                return unbound(epoch, backoff, &cause, ctx);
            }
        }
        Event::Frame { frame, .. } => {
            if !phase.is_connected() {
                return Transition {
                    phase,
                    actions: Vec::new(),
                };
            }
            match (&phase, &frame) {
                (Phase::AwaitWelcome { .. }, Frame::Welcome { .. }) => {
                    let (epoch, backoff, credential) = phase.dormant_parts().unwrap();
                    return Transition {
                        phase: Phase::Binding {
                            epoch,
                            backoff,
                            credential,
                        },
                        actions: vec![Action::BindWelcome(frame)],
                    };
                }
                (
                    Phase::Placing {
                        epoch,
                        backoff,
                        credential,
                        remaining,
                        welcomed_at,
                        width,
                    },
                    Frame::Placed(entry),
                ) if !matches!(entry, Entry::Gone) => {
                    if *remaining == 1 {
                        return become_ready(
                            *epoch,
                            *backoff,
                            credential.clone(),
                            *welcomed_at,
                            *width,
                        );
                    }
                    return Transition {
                        phase: Phase::Placing {
                            epoch: *epoch,
                            backoff: *backoff,
                            credential: credential.clone(),
                            remaining: remaining - 1,
                            welcomed_at: *welcomed_at,
                            width: *width,
                        },
                        actions: Vec::new(),
                    };
                }
                (Phase::Ready { version, .. }, Frame::Batch { version: fv, .. })
                | (Phase::Ready { version, .. }, Frame::Reset(fv, _))
                    if fv >= version =>
                {
                    return Transition {
                        phase,
                        actions: vec![Action::Update(frame), Action::ContinueFrame],
                    };
                }
                (Phase::Ready { .. }, Frame::Live(_) | Frame::Ended(..)) => {
                    return Transition {
                        phase,
                        actions: vec![Action::Update(frame), Action::ContinueFrame],
                    };
                }
                _ => {
                    let (epoch, backoff, credential) = phase.dormant_parts().unwrap();
                    return lose(
                        epoch,
                        backoff,
                        credential,
                        Loss::Incompatible { code: 1008 },
                        ctx,
                    );
                }
            }
        }
        Event::WelcomeBound { at, width, .. } => {
            if let Phase::Binding {
                epoch,
                backoff,
                credential,
            } = phase
            {
                let accepted_cred = credential.map(|c| Credential {
                    bytes: c.bytes,
                    source: CredentialSource::Initial,
                });
                if ctx.wants == 0 {
                    return become_ready(epoch, backoff, accepted_cred, at, width);
                }
                return Transition {
                    phase: Phase::Placing {
                        epoch,
                        backoff,
                        credential: accepted_cred,
                        remaining: ctx.wants,
                        welcomed_at: at,
                        width,
                    },
                    actions: Vec::new(),
                };
            }
        }
        Event::AdvanceVersion { version, .. } => {
            if let Phase::Ready {
                epoch,
                backoff,
                credential,
                welcomed_at,
                width,
                lanes,
                ..
            } = phase
            {
                return Transition {
                    phase: Phase::Ready {
                        epoch,
                        backoff,
                        credential,
                        welcomed_at,
                        version,
                        width,
                        lanes,
                    },
                    actions: vec![Action::RunAdvance { version }],
                };
            }
        }
        Event::PrepareCall { lane, .. } => {
            if let Phase::Ready {
                epoch,
                backoff,
                credential,
                welcomed_at,
                version,
                width,
                mut lanes,
            } = phase
            {
                let cur = lanes.get(&lane).copied().unwrap_or_default();
                let owed = lanes.values().filter(|l| l.attempt.owes()).count();
                if cur.attempt == Attempt::Idle && owed < width {
                    lanes.insert(
                        lane,
                        LaneState {
                            attempt: Attempt::Preparing,
                            backoff: cur.backoff,
                        },
                    );
                    return Transition {
                        phase: Phase::Ready {
                            epoch,
                            backoff,
                            credential,
                            welcomed_at,
                            version,
                            width,
                            lanes,
                        },
                        actions: vec![Action::RunPrepare { lane }],
                    };
                }
                return Transition {
                    phase: Phase::Ready {
                        epoch,
                        backoff,
                        credential,
                        welcomed_at,
                        version,
                        width,
                        lanes,
                    },
                    actions: Vec::new(),
                };
            }
        }
        Event::SendCall { lane, delivery, .. } => {
            if let Phase::Ready {
                epoch,
                backoff,
                credential,
                welcomed_at,
                version,
                width,
                mut lanes,
            } = phase
            {
                let cur = lanes.get(&lane).copied().unwrap_or_default();
                if cur.attempt == Attempt::Preparing {
                    lanes.insert(
                        lane,
                        LaneState {
                            attempt: delivery,
                            backoff: cur.backoff,
                        },
                    );
                    return Transition {
                        phase: Phase::Ready {
                            epoch,
                            backoff,
                            credential,
                            welcomed_at,
                            version,
                            width,
                            lanes,
                        },
                        actions: vec![Action::RunSend { lane }],
                    };
                }
                return Transition {
                    phase: Phase::Ready {
                        epoch,
                        backoff,
                        credential,
                        welcomed_at,
                        version,
                        width,
                        lanes,
                    },
                    actions: Vec::new(),
                };
            }
        }
        Event::HttpComplete { lane, p, .. } => {
            if let Phase::Ready { ref lanes, .. } = phase {
                if lanes
                    .get(&lane)
                    .is_some_and(|l| l.attempt == Attempt::Http { p })
                {
                    return Transition {
                        phase,
                        actions: vec![Action::RunHttpComplete { lane, p }],
                    };
                }
            }
        }
        Event::CallFinished { lane, .. } => {
            if let Phase::Ready {
                epoch,
                backoff,
                credential,
                welcomed_at,
                version,
                width,
                mut lanes,
            } = phase
            {
                lanes.remove(&lane);
                return Transition {
                    phase: Phase::Ready {
                        epoch,
                        backoff,
                        credential,
                        welcomed_at,
                        version,
                        width,
                        lanes,
                    },
                    actions: vec![Action::SendPending],
                };
            }
        }
        Event::RetryCall { lane, .. } => {
            if let Phase::Ready {
                epoch,
                backoff,
                credential,
                welcomed_at,
                version,
                width,
                mut lanes,
            } = phase
            {
                let cur = lanes.get(&lane).copied().unwrap_or_default();
                lanes.insert(
                    lane,
                    LaneState {
                        attempt: Attempt::Retry,
                        backoff: next_backoff(cur.backoff),
                    },
                );
                return Transition {
                    phase: Phase::Ready {
                        epoch,
                        backoff,
                        credential,
                        welcomed_at,
                        version,
                        width,
                        lanes,
                    },
                    actions: vec![
                        Action::ArmRetry {
                            lane,
                            max_ms: cur.backoff,
                        },
                        Action::SendPending,
                    ],
                };
            }
        }
        Event::RetryElapsed { lane, .. } => {
            if let Phase::Ready {
                epoch,
                backoff,
                credential,
                welcomed_at,
                version,
                width,
                mut lanes,
            } = phase
            {
                let cur = lanes.get(&lane).copied().unwrap_or_default();
                if cur.attempt == Attempt::Retry {
                    lanes.insert(
                        lane,
                        LaneState {
                            attempt: Attempt::Idle,
                            backoff: cur.backoff,
                        },
                    );
                    return Transition {
                        phase: Phase::Ready {
                            epoch,
                            backoff,
                            credential,
                            welcomed_at,
                            version,
                            width,
                            lanes,
                        },
                        actions: vec![Action::SendPending],
                    };
                }
                return Transition {
                    phase: Phase::Ready {
                        epoch,
                        backoff,
                        credential,
                        welcomed_at,
                        version,
                        width,
                        lanes,
                    },
                    actions: Vec::new(),
                };
            }
        }
        Event::Stop {
            state,
            outcome,
            diagnostic,
        } => {
            let already_closed = matches!(
                phase,
                Phase::Stopped {
                    state: StoppedState::Closed,
                    ..
                }
            );
            if !already_closed || state == StoppedState::Closed {
                let mut actions = vec![Action::ClearOverlays];
                teardown(&mut actions);
                actions.push(Action::State(state.into()));
                if let Some(msg) = diagnostic {
                    actions.push(Action::Diagnostic(msg));
                }
                if let Some(out) = outcome {
                    actions.push(Action::Finish(out));
                }
                let code = match state {
                    StoppedState::Fenced => "Fenced",
                    StoppedState::StorageUnavailable => "StorageUnavailable",
                    StoppedState::Closed => "Closed",
                };
                actions.push(Action::RejectReady(code.into()));
                return Transition {
                    phase: Phase::Stopped {
                        epoch: phase.epoch() + 1,
                        state,
                    },
                    actions,
                };
            }
        }
    }
    Transition {
        phase,
        actions: Vec::new(),
    }
}
