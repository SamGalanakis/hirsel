//! Leased-stream lane records (`sdk/client/lanes.ts`) and kernel vector unit tests.
use super::codec::{KeyedDone, Outcome, Refusal, StreamId};
use hirsel_core_ids::embed::EntryKind;
// ---------------------------------------------------------------------------
// Leased-stream lane records (`sdk/client/lanes.ts`)
// ---------------------------------------------------------------------------

/// One pending call on a leased stream lane.
#[derive(Clone, Debug)]
pub struct PendingLaneCall {
    pub p: u64,
    pub want: u32,
    pub deadline_ms: u32,
    pub args: Vec<u8>,
    pub inflight: bool,
    /// Host call id currently observing this lane, or `None` if abandoned.
    pub host_call: Option<u64>,
    /// Reserved host-client byte charge for this call (refunded on settle).
    pub charge: u64,
}

/// One leased store stream (`sdk/client/lanes.ts::Lane`).
#[derive(Clone, Debug)]
pub struct StreamLane {
    pub id: u32,
    pub stream: StreamId,
    pub next: u64,
    pub retired: bool,
    pub pending: Option<PendingLaneCall>,
}

impl StreamLane {
    pub fn new(id: u32, stream: StreamId) -> Self {
        Self {
            id,
            stream,
            next: 0,
            retired: false,
            pending: None,
        }
    }

    pub fn matches_done(&self, done: &KeyedDone) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.inflight && self.stream == done.stream && p.p == done.p)
    }

    /// Advance only when the runner stored a transact reply at this position.
    pub fn settle(&mut self, kind: EntryKind, outcome: &Outcome) -> Option<PendingLaneCall> {
        let pending = self.pending.take()?;
        let retire = matches!(outcome, Outcome::Refused(Refusal::HistoryUnavailable));
        let advance = kind == EntryKind::Transact
            && matches!(outcome, Outcome::Returned(_) | Outcome::Failed(_));
        if advance && pending.p != u64::MAX {
            self.next = pending.p + 1;
        }
        if retire || (advance && pending.p == u64::MAX) {
            self.retired = true;
        }
        Some(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::super::codec::{Caller, Entry, Frame};
    use super::super::kernel::*;
    use super::*;

    #[test]
    fn kernel_vectors_handshake_lanes_and_stale_epoch_drop() {
        let ctx = StepContext {
            wants: 2,
            has_credential_source: false,
        };
        let t = step(Phase::initial(), Event::Start, ctx);
        assert!(matches!(t.phase, Phase::Opening { epoch: 1, .. }));
        assert!(t.actions.contains(&Action::OpenSocket));

        // Stale epoch event is ignored.
        let stale = step(t.phase.clone(), Event::SocketOpen { epoch: 0 }, ctx);
        assert_eq!(stale.phase, t.phase);
        assert!(stale.actions.is_empty());

        let t = step(t.phase, Event::SocketOpen { epoch: 1 }, ctx);
        assert!(matches!(t.phase, Phase::AwaitWelcome { epoch: 1, .. }));
        assert_eq!(t.actions, vec![Action::SendHello]);

        let welcome = Frame::Welcome {
            caller: Caller {
                issuer: 1,
                subject: vec![1, 2],
            },
            q: 2,
            budget: 0,
            digest: vec![],
            target: "x86_64-unknown-linux-gnu".into(),
            n_place: 16 << 20,
            now_ms: 262_144,
            retention_ms: 1000,
        };
        let t = step(
            t.phase,
            Event::Frame {
                epoch: 1,
                frame: welcome,
            },
            ctx,
        );
        assert!(matches!(t.phase, Phase::Binding { epoch: 1, .. }));

        let t = step(
            t.phase,
            Event::WelcomeBound {
                epoch: 1,
                at: 10,
                width: 3, // 1 + q
            },
            ctx,
        );
        assert!(matches!(
            t.phase,
            Phase::Placing {
                remaining: 2,
                width: 3,
                ..
            }
        ));

        let t = step(
            t.phase,
            Event::Frame {
                epoch: 1,
                frame: Frame::Placed(Entry::Ready(0)),
            },
            ctx,
        );
        assert!(matches!(t.phase, Phase::Placing { remaining: 1, .. }));

        let t = step(
            t.phase,
            Event::Frame {
                epoch: 1,
                frame: Frame::Placed(Entry::Ready(1)),
            },
            ctx,
        );
        assert!(matches!(t.phase, Phase::Ready { width: 3, .. }));

        // Width 3 allows 3 concurrent lanes; 4th is refused by width gate.
        let mut p = t.phase;
        for lane in 0..3 {
            let tr = step(p, Event::PrepareCall { epoch: 1, lane }, ctx);
            assert_eq!(tr.actions, vec![Action::RunPrepare { lane }]);
            let tr = step(
                tr.phase,
                Event::SendCall {
                    epoch: 1,
                    lane,
                    delivery: Attempt::WebSocket,
                },
                ctx,
            );
            assert_eq!(tr.actions, vec![Action::RunSend { lane }]);
            p = tr.phase;
        }
        assert_eq!(p.owed_count(), 3);
        let tr4 = step(p.clone(), Event::PrepareCall { epoch: 1, lane: 3 }, ctx);
        assert!(tr4.actions.is_empty());

        // Finishing lane 0 frees a slot and triggers SendPending.
        let tr = step(p, Event::CallFinished { epoch: 1, lane: 0 }, ctx);
        assert_eq!(tr.phase.owed_count(), 2);
        assert_eq!(tr.actions, vec![Action::SendPending]);

        // Socket loss transitions through Losing -> Backoff, bumping epoch so old frames drop.
        let tr = step(
            tr.phase,
            Event::SocketLost {
                epoch: 1,
                code: Some(1006),
            },
            ctx,
        );
        assert!(matches!(tr.phase, Phase::Losing { epoch: 2, .. }));
        let old_frame = step(
            tr.phase.clone(),
            Event::Frame {
                epoch: 1,
                frame: Frame::Batch {
                    version: 1,
                    complete: vec![],
                    groups: vec![],
                    outcomes: vec![],
                },
            },
            ctx,
        );
        assert!(old_frame.actions.is_empty());
    }
}
