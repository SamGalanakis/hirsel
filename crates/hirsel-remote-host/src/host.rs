//! A runner host's protocol (RUN-8, RUN-9, RUN-11, RUN-13), with no I/O: sessions,
//! per-caller call streams, arrival, queueing, cancellation and status. The transport
//! feeds it connections and frames, the executor feeds it callee ends, and it answers with
//! [`Action`]s.
//!
//! **Streams.** Each caller run opens one ordered stream per runner; the host keys it by the
//! caller's authenticated name and the stream bytes. Positions are decided strictly in
//! order, and every decided position keeps its request digest and, once known, its
//! outcome until the caller acknowledges it, so many calls may be in flight:
//! - the next position (`applied + 1`) is decided: refused with `NotRun(..)`, or admitted
//!   to run;
//! - a decided position with the same digest is a duplicate: it gets the stored outcome
//!   (or, while its callee runs, the outcome when it ends) and never runs again; with a
//!   different digest it is `NotRun(Rejected())`;
//! - a position beyond the next but within the stream's window (`applied + runs + queue`,
//!   the farthest position capacity could admit) is a gap, refused with
//!   `NotRun(Rejected())`, and that refusal is sticky: when the gap closes the position is
//!   decided `Rejected` too, so the host never contradicts an answer it gave;
//! - a position beyond the window is refused with `NotRun(Overloaded())` before anything
//!   is stored, and the session refuses every position from it on the same way (deciding
//!   each `Overloaded` when it is the next) until the session reopens, so a session keeps
//!   at most `runs + queue - 1` gaps and one frame decides at most that many;
//! - a position at or below the acknowledged one is stale and refused.
//! - at `unacknowledged` retained positions a new position gets the session-level
//!   `Refused(Overloaded())` and Close, without a decision or journal record. Acknowledging
//!   finished outcomes and recovering existing decisions remain available.
//!
//! The stream position is decided before the code and authority checks of RUN-8 (whose
//! order the spec gives for a single request), so every decision, refusals included, is
//! recorded at its position and a duplicate is answered without being checked again.
//!
//! **Streams per caller.** A caller holds at most `streams` streams; opening another is
//! `Refused(Overloaded())` before its journal record is written.
//!
//! **Sessions.** A session is one connection's view of a stream at an epoch. Opening at a
//! higher epoch fences the previous session: its connection is closed and frames carrying
//! an older epoch are discarded, so after a caller reconnects nothing it sent before can
//! still arrive and be decided. Gap refusals belong to the session that saw them.
//!
//! **Callees.** An admitted call queues for the executor, which runs at most `runs` callees
//! at once, with at most `queue` waiting; beyond that the call is `NotRun(Overloaded())`.
//! A cancellation of a queued call decides it `Canceled(cause)`, proved never started; of a
//! running one it is forwarded once and acknowledged. A caller whose session is lost has
//! its running callees cancelled with cause `Parent` and its queued ones decided
//! `Canceled(Parent())` (RUN-9); the caller learns those outcomes through status queries.
//!
//! **Served calls.** At a `db@1` place a transact or query call is not decided here: its
//! stream position is the store's ([STORE-12](../../../conformance/old-spec/storage.md)). The host makes
//! RUN-8 checks 1, 2, 4 without the place's own list of served entries, 6 and 7 (the call
//! key is the stream instance's), then hands the call to the place's head as an
//! [`Arrival`]; the head makes checks 3, the served-entry list, 5 and 8, and the host sends
//! the end it reports ([`Host::arrival_ended`]) to the stream's current session. Nothing of
//! a served call is journaled or retained once it ends: a caller that lost its outcome
//! sends the call again, and the store answers it from the stream rows it recorded.
//!
//! **A failed journal stops the host.** Every record is appended before it is acted on: a
//! stream before it opens, an admission before its callee may start, a decision before its
//! outcome is sent, an acknowledgement before its outcomes are dropped. When an append
//! fails the journal is poisoned and the host stops: it acts on nothing that record would
//! have made durable, cancels its running callees, closes every session, refuses every
//! later frame, and reports [`Host::failure`] so its process exits and recovers.
//!
//! **Compaction.** After each frame, callee end and disconnect, once every record so far is
//! reflected in its state, the host compacts a journal that is due: the checkpoint holds
//! each stream, its acknowledged position, and every position above it, decided or
//! admitted, so recovering the checkpoint gives what recovering the whole journal would.
use crate::journal::{Entry, Journal, StreamKey};
use hirsel_remote::frame::{Call, CallRef, Frame, Outcome};
use hirsel_remote::wire::Digests;
use hirsel_run_schema::{CallOutcome, IoGrants, call_key};
use std::collections::{BTreeMap, VecDeque};

pub type SessionId = u64;
pub type RunId = u64;

pub const DENIED: u32 = 0;
pub const UNREACHABLE: u32 = 2;
pub const OVERLOADED: u32 = 3;
pub const REJECTED: u32 = 4;
pub const ABANDONED: u32 = 2;
/// `CancelCause.Parent()`.
pub const PARENT: u32 = 1;

/// The caller an authenticated session belongs to: the name the host's configuration gives
/// its identity, and the grant set and allowlist its calls run within.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Caller {
    pub name: String,
    pub grants: IoGrants,
    pub definitions: Option<Vec<String>>,
}

/// Why a place refuses a request it could decide: a `NotRunReason` ordinal, and the
/// definition hashes it lacks for `VersionMismatch`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub reason: u32,
    pub missing: Vec<[u8; 32]>,
}

/// The place's own checks of one request (RUN-8 checks 3, 4 and 8): its code, the
/// authority the request asks for within the caller's grants and allowlists, the code
/// policy and the argument's schema. On success, the grant set the callee runs under.
pub trait Place {
    fn admit(&mut self, caller: &Caller, call: &Call) -> Result<IoGrants, Refusal>;
    /// The host's share of RUN-8 check 4 for a transact or query call at a `db@1` place:
    /// the requested grant set and both allowlists against the caller's, and the code
    /// policy; the head checks the entries it serves. A place without a head serves none.
    fn admit_served(&mut self, _caller: &Caller, _call: &Call) -> Result<(), Refusal> {
        Err(Refusal {
            reason: DENIED,
            missing: vec![],
        })
    }
}

/// What the host asks its transport and executor to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Send(SessionId, Frame),
    /// Close the session's connection.
    Close(SessionId),
    Start(Start),
    /// Forward a cancellation to a running callee (cause ordinal).
    Cancel(RunId, u32),
    /// Hand a served call to the place's head.
    Arrive(Box<Arrival>),
}

/// A transact or query call that passed the host's checks, for the place's head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Arrival {
    pub id: u64,
    /// The admitted caller's name.
    pub caller: String,
    pub stream: [u8; 32],
    pub digest: [u8; 32],
    pub call: Call,
}

/// A callee to run as its own run (RUN-10).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Start {
    pub run: RunId,
    pub caller: String,
    /// The caller's run instance, the request ordinal and digest: the callee's cause.
    pub instance: [u8; 16],
    pub digest: [u8; 32],
    pub call: Call,
    pub grants: IoGrants,
}

/// The host's limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Callees running at once.
    pub runs: u64,
    /// Admitted calls waiting for a run.
    pub queue: u64,
    /// Streams one caller holds.
    pub streams: u64,
    /// Decided positions one stream retains until acknowledged.
    pub unacknowledged: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            runs: 1,
            queue: 64,
            streams: 65_536,
            unacknowledged: 65,
        }
    }
}

/// What a host keeps in memory: its streams, their sessions' gap refusals, and the decided
/// positions their callers have not acknowledged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Retained {
    pub streams: usize,
    pub gaps: usize,
    pub decided: usize,
}

#[derive(Clone, Debug)]
enum State {
    Queued(RunId),
    Running {
        run: RunId,
        forwarded: bool,
    },
    Done {
        end: CallOutcome,
        callee: Option<[u8; 16]>,
        missing: Vec<[u8; 32]>,
    },
}
#[derive(Clone, Debug)]
struct Decided {
    key: [u8; 32],
    digest: [u8; 32],
    state: State,
}
struct Stream {
    instance: [u8; 16],
    epoch: u64,
    session: Option<SessionId>,
    applied: u64,
    acknowledged: u64,
    decided: BTreeMap<u64, Decided>,
    /// This session's gap refusals above `applied + 1`: key and digest.
    gaps: BTreeMap<u64, ([u8; 32], [u8; 32])>,
    /// The lowest position this session refused beyond its window: it and every later
    /// position are refused `Overloaded` until the session reopens.
    walled: Option<u64>,
}
impl Stream {
    /// A new session, or none: its gap refusals end with it.
    fn reset_session(&mut self) {
        self.gaps.clear();
        self.walled = None;
    }
}
fn stream_of<'a>(streams: &'a mut BTreeMap<StreamKey, Stream>, key: &StreamKey) -> &'a mut Stream {
    streams.entry(key.clone()).or_insert_with(|| Stream {
        instance: [0; 16],
        epoch: 0,
        session: None,
        applied: 0,
        acknowledged: 0,
        decided: BTreeMap::new(),
        gaps: BTreeMap::new(),
        walled: None,
    })
}
struct Session {
    caller: Option<Caller>,
    stream: Option<StreamKey>,
    /// A refusal was sent; discard further frames until the transport disconnects.
    closing: bool,
}
/// A served call the head holds: where its end goes.
struct Served {
    stream: StreamKey,
    position: u64,
    key: [u8; 32],
    digest: [u8; 32],
}
struct Run {
    stream: StreamKey,
    position: u64,
    start: Start,
}

/// One runner host's protocol state.
pub struct Host {
    journal: Box<dyn Journal>,
    limits: Limits,
    sessions: BTreeMap<SessionId, Session>,
    streams: BTreeMap<StreamKey, Stream>,
    /// Streams held per caller name.
    held: BTreeMap<String, u64>,
    runs: BTreeMap<RunId, Run>,
    queued: VecDeque<RunId>,
    running: u64,
    next_run: RunId,
    /// Served calls handed to the head and not yet ended.
    served: BTreeMap<u64, Served>,
    next_arrival: u64,
    /// The place's head ended: no served call reaches it again.
    head_ended: bool,
    /// The append failure that stopped the host.
    failure: Option<String>,
}

impl Host {
    /// A host over `journal`, recovering what a previous incarnation decided: a call it
    /// admitted and never decided is decided `InDoubt(Abandoned())` now.
    pub fn new(mut journal: Box<dyn Journal>, limits: Limits) -> std::io::Result<Self> {
        let mut streams: BTreeMap<StreamKey, Stream> = BTreeMap::new();
        let mut admitted: BTreeMap<(StreamKey, u64), ([u8; 32], [u8; 32])> = BTreeMap::new();
        let mut decided = vec![];
        for entry in journal.recover()? {
            match entry {
                Entry::Stream {
                    stream: key,
                    instance,
                } => {
                    stream_of(&mut streams, &key).instance = instance;
                }
                Entry::Admitted {
                    stream: key,
                    position,
                    key: call,
                    digest,
                } => {
                    admitted.insert((key, position), (call, digest));
                }
                Entry::Decided {
                    stream: key,
                    position,
                    key: call,
                    digest,
                    end,
                    callee,
                } => {
                    admitted.remove(&(key.clone(), position));
                    decided.push((key, position, call, digest, end, callee));
                }
                Entry::Acknowledged {
                    stream: key,
                    position,
                } => {
                    // A checkpoint keeps no decision the caller acknowledged.
                    let s = stream_of(&mut streams, &key);
                    s.acknowledged = s.acknowledged.max(position);
                    s.applied = s.applied.max(position);
                    s.decided.retain(|p, _| *p > position);
                }
            }
            for (key, position, call, digest, end, callee) in decided.drain(..) {
                let s = stream_of(&mut streams, &key);
                s.applied = s.applied.max(position);
                if position > s.acknowledged {
                    s.decided.insert(
                        position,
                        Decided {
                            key: call,
                            digest,
                            state: State::Done {
                                end,
                                callee,
                                missing: vec![],
                            },
                        },
                    );
                }
            }
        }
        // A call admitted and never decided may have run in part: its place ended it
        // without an outcome.
        for ((key, position), (call, digest)) in admitted {
            let end = CallOutcome::InDoubt(ABANDONED);
            journal.append(&Entry::Decided {
                stream: key.clone(),
                position,
                key: call,
                digest,
                end: end.clone(),
                callee: None,
            })?;
            let s = stream_of(&mut streams, &key);
            s.applied = s.applied.max(position);
            s.decided.insert(
                position,
                Decided {
                    key: call,
                    digest,
                    state: State::Done {
                        end,
                        callee: None,
                        missing: vec![],
                    },
                },
            );
        }
        let mut held = BTreeMap::new();
        for key in streams.keys() {
            *held.entry(key.caller.clone()).or_insert(0) += 1;
        }
        Ok(Self {
            journal,
            limits,
            sessions: BTreeMap::new(),
            streams,
            held,
            runs: BTreeMap::new(),
            queued: VecDeque::new(),
            running: 0,
            next_run: 1,
            served: BTreeMap::new(),
            next_arrival: 1,
            head_ended: false,
            failure: None,
        })
    }

    /// The journal append failure that stopped this host, if one did. A stopped host
    /// refuses all work; its process must exit and recover from the journal.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// What this host keeps in memory.
    pub fn retained(&self) -> Retained {
        Retained {
            streams: self.streams.len(),
            gaps: self.streams.values().map(|s| s.gaps.len()).sum(),
            decided: self.streams.values().map(|s| s.decided.len()).sum(),
        }
    }

    /// Append `entry`, or stop the host: cancel every running callee and close every
    /// session. False when the host is (now) stopped, and nothing may act on `entry`.
    fn append(&mut self, entry: &Entry, actions: &mut Vec<Action>) -> bool {
        if self.failure.is_some() {
            return false;
        }
        let Err(error) = self.journal.append(entry) else {
            return true;
        };
        self.stop(error, actions);
        false
    }

    /// Stop on a failed journal: cancel every running callee and close every session.
    fn stop(&mut self, error: std::io::Error, actions: &mut Vec<Action>) {
        self.failure = Some(error.to_string());
        self.queued.clear();
        for (run, r) in &self.runs {
            if let Some(Decided {
                state: State::Running { .. },
                ..
            }) = self.streams[&r.stream].decided.get(&r.position)
            {
                actions.push(Action::Cancel(*run, PARENT));
            }
        }
        for session in std::mem::take(&mut self.sessions).into_keys() {
            actions.push(Action::Close(session));
        }
    }

    /// Compact a journal that is due, once every record appended so far is reflected in
    /// this host's state. A failed compaction stops the host.
    fn checkpoint(&mut self, actions: &mut Vec<Action>) {
        if self.failure.is_some() || !self.journal.due() {
            return;
        }
        let live = self.live();
        if let Err(error) = self.journal.compact(&live) {
            self.stop(error, actions);
        }
    }

    /// The records whose recovery gives this host's state: each stream, its acknowledged
    /// position, and each position above it decided, or admitted and not yet decided.
    fn live(&self) -> Vec<Entry> {
        let mut live = vec![];
        for (key, stream) in &self.streams {
            live.push(Entry::Stream {
                stream: key.clone(),
                instance: stream.instance,
            });
            if stream.acknowledged > 0 {
                live.push(Entry::Acknowledged {
                    stream: key.clone(),
                    position: stream.acknowledged,
                });
            }
            for (&position, decided) in &stream.decided {
                live.push(match &decided.state {
                    State::Done { end, callee, .. } => Entry::Decided {
                        stream: key.clone(),
                        position,
                        key: decided.key,
                        digest: decided.digest,
                        end: end.clone(),
                        callee: *callee,
                    },
                    State::Queued(_) | State::Running { .. } => Entry::Admitted {
                        stream: key.clone(),
                        position,
                        key: decided.key,
                        digest: decided.digest,
                    },
                });
            }
        }
        live
    }

    /// A connection authenticated as `caller`, or as no admitted caller (None).
    pub fn connected(&mut self, session: SessionId, caller: Option<Caller>) {
        self.sessions.insert(
            session,
            Session {
                caller,
                stream: None,
                closing: false,
            },
        );
    }

    /// A session's connection ended. When it was its stream's current session, the
    /// caller's queued calls are decided `Canceled(Parent())` and its running callees
    /// cancelled.
    pub fn disconnected(&mut self, session: SessionId) -> Vec<Action> {
        let mut actions = self.disconnect(session);
        self.checkpoint(&mut actions);
        actions
    }

    fn disconnect(&mut self, session: SessionId) -> Vec<Action> {
        let mut actions = vec![];
        if self.failure.is_some() {
            return actions;
        }
        let Some(Session {
            stream: Some(key), ..
        }) = self.sessions.remove(&session)
        else {
            return actions;
        };
        let Some(stream) = self.streams.get_mut(&key) else {
            return actions;
        };
        if stream.session != Some(session) {
            return actions;
        }
        stream.session = None;
        stream.reset_session();
        self.cancel_stream(&key, PARENT, &mut actions);
        actions
    }

    fn cancel_stream(&mut self, key: &StreamKey, cause: u32, actions: &mut Vec<Action>) {
        let positions: Vec<u64> = self.streams[key]
            .decided
            .iter()
            .filter(|(_, d)| !matches!(d.state, State::Done { .. }))
            .map(|(p, _)| *p)
            .collect();
        for position in positions {
            self.cancel(key, position, cause, actions);
            if self.failure.is_some() {
                return;
            }
        }
    }

    /// Cancel the call at `position`: a queued one is decided `Canceled`, a running one has
    /// the cancellation forwarded once. True when the callee had started.
    fn cancel(
        &mut self,
        key: &StreamKey,
        position: u64,
        cause: u32,
        actions: &mut Vec<Action>,
    ) -> bool {
        let stream = self.streams.get_mut(key).unwrap();
        let decided = stream.decided.get_mut(&position).unwrap();
        match &mut decided.state {
            State::Queued(run) => {
                let run = *run;
                self.queued.retain(|r| *r != run);
                self.runs.remove(&run);
                self.decide_end(key, position, CallOutcome::Canceled(cause), None, actions);
                false
            }
            State::Running { run, forwarded } => {
                if !*forwarded {
                    *forwarded = true;
                    actions.push(Action::Cancel(*run, cause));
                }
                true
            }
            State::Done { .. } => false,
        }
    }

    /// One frame from a session.
    pub fn frame(
        &mut self,
        session: SessionId,
        frame: Frame,
        place: &mut dyn Place,
        digests: &dyn Digests,
    ) -> Vec<Action> {
        let mut actions = self.receive(session, frame, place, digests);
        self.checkpoint(&mut actions);
        actions
    }

    fn receive(
        &mut self,
        session: SessionId,
        frame: Frame,
        place: &mut dyn Place,
        digests: &dyn Digests,
    ) -> Vec<Action> {
        let mut actions = vec![];
        let Some(current) = self.sessions.get(&session) else {
            return actions;
        };
        if current.closing {
            return actions;
        }
        if self.failure.is_some() {
            self.sessions.remove(&session);
            actions.push(Action::Close(session));
            return actions;
        }
        if let Frame::Open {
            stream,
            instance,
            epoch,
        } = frame
        {
            self.open(session, stream, instance, epoch, &mut actions);
            return actions;
        }
        // Every other frame belongs to the session's stream at its current epoch.
        let Some(key) = current.stream.clone() else {
            return actions;
        };
        let caller = current.caller.clone().unwrap();
        let stream = &self.streams[&key];
        if stream.session != Some(session) || frame.epoch() != Some(stream.epoch) {
            return actions;
        }
        match frame {
            Frame::Call(call) if call.form.served() => {
                self.serve(session, &key, &caller, *call, place, digests, &mut actions)
            }
            Frame::Call(call) => {
                self.call(session, &key, &caller, *call, place, digests, &mut actions)
            }
            Frame::Cancel(r, cause) => match self.streams[&key].decided.get(&r.position) {
                Some(d) if d.key == r.key => {
                    if self.cancel(&key, r.position, cause, &mut actions) {
                        actions.push(Action::Send(session, Frame::Acknowledged(r)));
                    } else {
                        self.resend(session, &key, r, &mut actions);
                    }
                }
                _ => actions.push(Action::Send(session, Frame::Unknown(r))),
            },
            Frame::Status(r) => self.status(session, &key, r, &mut actions),
            Frame::Close { acknowledged, .. } => {
                if !self.acknowledge(&key, acknowledged, &mut actions) {
                    return actions;
                }
                let stream = self.streams.get_mut(&key).unwrap();
                stream.session = None;
                stream.reset_session();
                self.sessions.remove(&session);
                actions.push(Action::Close(session));
            }
            // Host-to-caller frames are never accepted from a caller.
            _ => (),
        }
        actions
    }

    fn open(
        &mut self,
        session: SessionId,
        stream_id: [u8; 32],
        instance: [u8; 16],
        epoch: u64,
        actions: &mut Vec<Action>,
    ) {
        let Some(caller) = self.sessions[&session].caller.clone() else {
            actions.push(Action::Send(session, Frame::Refused(DENIED)));
            actions.push(Action::Close(session));
            return;
        };
        let key = StreamKey {
            caller: caller.name.clone(),
            stream: stream_id,
        };
        if !self.streams.contains_key(&key) {
            let held = self.held.get(&caller.name).copied().unwrap_or(0);
            if held >= self.limits.streams {
                actions.push(Action::Send(session, Frame::Refused(OVERLOADED)));
                actions.push(Action::Close(session));
                return;
            }
            let entry = Entry::Stream {
                stream: key.clone(),
                instance,
            };
            if !self.append(&entry, actions) {
                return;
            }
            stream_of(&mut self.streams, &key).instance = instance;
            *self.held.entry(caller.name.clone()).or_insert(0) += 1;
        }
        let stream = self.streams.get_mut(&key).unwrap();
        // A stale epoch, or another run's stream bytes, is refused.
        if epoch <= stream.epoch || stream.instance != instance {
            actions.push(Action::Send(session, Frame::Refused(DENIED)));
            actions.push(Action::Close(session));
            return;
        }
        // The previous session is fenced.
        if let Some(old) = stream.session.replace(session)
            && old != session
        {
            self.sessions.remove(&old);
            actions.push(Action::Close(old));
        }
        stream.epoch = epoch;
        stream.reset_session();
        self.sessions.get_mut(&session).unwrap().stream = Some(key);
        actions.push(Action::Send(
            session,
            Frame::Opened {
                epoch,
                incarnation: self.journal.incarnation(),
                applied: stream.applied,
                reconciles: self.journal.durable(),
            },
        ));
    }

    /// Drop the outcomes the caller holds; false when the host stopped instead.
    fn acknowledge(
        &mut self,
        key: &StreamKey,
        acknowledged: u64,
        actions: &mut Vec<Action>,
    ) -> bool {
        let stream = &self.streams[key];
        // Only outcomes the caller holds can be dropped: decided, finished positions.
        let mut upto = stream.acknowledged;
        while upto < acknowledged.min(stream.applied)
            && matches!(
                stream.decided.get(&(upto + 1)).map(|d| &d.state),
                Some(State::Done { .. })
            )
        {
            upto += 1;
        }
        if upto > stream.acknowledged {
            let entry = Entry::Acknowledged {
                stream: key.clone(),
                position: upto,
            };
            if !self.append(&entry, actions) {
                return false;
            }
            let stream = self.streams.get_mut(key).unwrap();
            stream.acknowledged = upto;
            stream.decided.retain(|p, _| *p > upto);
        }
        true
    }

    fn outcome_frame(stream: &Stream, position: u64, decided: &Decided) -> Option<Frame> {
        let State::Done {
            end,
            callee,
            missing,
        } = &decided.state
        else {
            return None;
        };
        Some(Frame::Outcome(Box::new(Outcome {
            epoch: stream.epoch,
            position,
            key: decided.key,
            digest: decided.digest,
            end: end.clone(),
            callee: *callee,
            missing: missing.clone(),
        })))
    }

    /// Send the stored outcome of `r`'s position to `session`, if it has one.
    fn resend(&self, session: SessionId, key: &StreamKey, r: CallRef, actions: &mut Vec<Action>) {
        let stream = &self.streams[key];
        if let Some(decided) = stream.decided.get(&r.position)
            && let Some(frame) = Self::outcome_frame(stream, r.position, decided)
        {
            actions.push(Action::Send(session, frame));
        }
    }

    fn status(
        &mut self,
        session: SessionId,
        key: &StreamKey,
        r: CallRef,
        actions: &mut Vec<Action>,
    ) {
        let stream = &self.streams[key];
        if r.position <= stream.acknowledged {
            return;
        }
        match stream.decided.get(&r.position) {
            Some(d) if d.key == r.key => match d.state {
                State::Done { .. } => self.resend(session, key, r, actions),
                _ => actions.push(Action::Send(session, Frame::Running(r))),
            },
            _ => actions.push(Action::Send(session, Frame::Unknown(r))),
        }
    }

    fn refuse(
        session: SessionId,
        epoch: u64,
        call: &Call,
        digest: [u8; 32],
        reason: u32,
        actions: &mut Vec<Action>,
    ) {
        actions.push(Action::Send(
            session,
            Frame::Outcome(Box::new(Outcome {
                epoch,
                position: call.position,
                key: call.key,
                digest,
                end: CallOutcome::NotRun(reason),
                callee: None,
                missing: vec![],
            })),
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        session: SessionId,
        key: &StreamKey,
        caller: &Caller,
        call: Call,
        place: &mut dyn Place,
        digests: &dyn Digests,
        actions: &mut Vec<Action>,
    ) {
        if !self.acknowledge(key, call.acknowledged, actions) {
            return;
        }
        let digest = digests.digest(&call);
        let stream = self.streams.get_mut(key).unwrap();
        let epoch = stream.epoch;
        let position = call.position;
        if position <= stream.acknowledged {
            return Self::refuse(session, epoch, &call, digest, REJECTED, actions);
        }
        if position <= stream.applied {
            let decided = &stream.decided[&position];
            if decided.digest != digest || decided.key != call.key {
                return Self::refuse(session, epoch, &call, digest, REJECTED, actions);
            }
            // A duplicate: its outcome now if it has one, else when its callee ends.
            return self.resend(session, key, call.reference(), actions);
        }
        if self.retention_full(session, key, actions) {
            return;
        }
        let stream = self.streams.get_mut(key).unwrap();
        // Past the window no capacity could admit the position: it is refused before
        // anything is stored, and so is every later one this session sends.
        let window = stream
            .applied
            .saturating_add(self.limits.runs)
            .saturating_add(self.limits.queue);
        if position > window {
            stream.walled = Some(stream.walled.map_or(position, |w| w.min(position)));
        }
        let walled = stream.walled.is_some_and(|w| position >= w);
        if position > stream.applied + 1 {
            if walled {
                return Self::refuse(session, epoch, &call, digest, OVERLOADED, actions);
            }
            match stream.gaps.get(&position) {
                Some(&(k, d)) if (k, d) != (call.key, digest) => (),
                _ => {
                    stream.gaps.insert(position, (call.key, digest));
                }
            }
            return Self::refuse(session, epoch, &call, digest, REJECTED, actions);
        }
        // The next position: decide it.
        if walled {
            self.decide_refused(
                key,
                position,
                call.key,
                digest,
                OVERLOADED,
                vec![],
                session,
                actions,
            );
        } else if stream.gaps.remove(&position).is_some() {
            // Refused while it was a gap: that answer stands for whatever arrives here.
            self.decide_refused(
                key,
                position,
                call.key,
                digest,
                REJECTED,
                vec![],
                session,
                actions,
            );
        } else if call.key != call_key(&stream.instance, call.ordinal) {
            self.decide_refused(
                key,
                position,
                call.key,
                digest,
                REJECTED,
                vec![],
                session,
                actions,
            );
        } else {
            match place.admit(caller, &call) {
                Err(refusal) => self.decide_refused(
                    key,
                    position,
                    call.key,
                    digest,
                    refusal.reason,
                    refusal.missing,
                    session,
                    actions,
                ),
                Ok(_)
                    if self.running + self.queued.len() as u64
                        >= self.limits.runs + self.limits.queue =>
                {
                    self.decide_refused(
                        key,
                        position,
                        call.key,
                        digest,
                        OVERLOADED,
                        vec![],
                        session,
                        actions,
                    )
                }
                Ok(grants) => {
                    let admitted = Entry::Admitted {
                        stream: key.clone(),
                        position,
                        key: call.key,
                        digest,
                    };
                    if !self.append(&admitted, actions) {
                        return;
                    }
                    let run = self.next_run;
                    self.next_run += 1;
                    let stream = self.streams.get_mut(key).unwrap();
                    stream.applied = position;
                    stream.decided.insert(
                        position,
                        Decided {
                            key: call.key,
                            digest,
                            state: State::Queued(run),
                        },
                    );
                    let instance = stream.instance;
                    self.runs.insert(
                        run,
                        Run {
                            stream: key.clone(),
                            position,
                            start: Start {
                                run,
                                caller: caller.name.clone(),
                                instance,
                                digest,
                                call,
                                grants,
                            },
                        },
                    );
                    self.queued.push_back(run);
                    self.dispatch(actions);
                }
            }
        }
        // Gap refusals the decided position now reaches are decided too.
        while self.failure.is_none() {
            let stream = self.streams.get_mut(key).unwrap();
            let next = stream.applied + 1;
            if !stream.gaps.contains_key(&next) {
                break;
            }
            if self.retention_full(session, key, actions) {
                break;
            }
            let stream = self.streams.get_mut(key).unwrap();
            let Some((k, d)) = stream.gaps.remove(&next) else {
                break;
            };
            self.decide_refused(key, next, k, d, REJECTED, vec![], session, actions);
        }
    }

    /// A transact or query call: the host's checks, then the head (module docs).
    #[allow(clippy::too_many_arguments)]
    fn serve(
        &mut self,
        session: SessionId,
        key: &StreamKey,
        caller: &Caller,
        call: Call,
        place: &mut dyn Place,
        digests: &dyn Digests,
        actions: &mut Vec<Action>,
    ) {
        let digest = digests.digest(&call);
        let stream = &self.streams[key];
        let epoch = stream.epoch;
        if call.key != call_key(&stream.instance, call.ordinal) {
            return Self::refuse(session, epoch, &call, digest, REJECTED, actions);
        }
        if let Err(refusal) = place.admit_served(caller, &call) {
            return Self::refuse(session, epoch, &call, digest, refusal.reason, actions);
        }
        if self.head_ended {
            return Self::refuse(session, epoch, &call, digest, UNREACHABLE, actions);
        }
        if self.served.len() as u64 >= self.limits.runs + self.limits.queue {
            return Self::refuse(session, epoch, &call, digest, OVERLOADED, actions);
        }
        let id = self.next_arrival;
        self.next_arrival += 1;
        self.served.insert(
            id,
            Served {
                stream: key.clone(),
                position: call.position,
                key: call.key,
                digest,
            },
        );
        actions.push(Action::Arrive(Box::new(Arrival {
            id,
            caller: caller.name.clone(),
            stream: key.stream,
            digest,
            call,
        })));
    }

    /// The head ended served call `id`: its outcome goes to its stream's current session.
    pub fn arrival_ended(&mut self, id: u64, end: CallOutcome) -> Vec<Action> {
        let mut actions = vec![];
        let Some(served) = self.served.remove(&id) else {
            return actions;
        };
        if let Some(stream) = self.streams.get(&served.stream)
            && let Some(session) = stream.session
        {
            actions.push(Action::Send(
                session,
                Frame::Outcome(Box::new(Outcome {
                    epoch: stream.epoch,
                    position: served.position,
                    key: served.key,
                    digest: served.digest,
                    end,
                    callee: None,
                    missing: vec![],
                })),
            ));
        }
        actions
    }

    /// The place's head ended. It reported the end of every call it held; any it did not
    /// is in doubt, and later served calls are `NotRun(Unreachable())`.
    pub fn head_ended(&mut self) -> Vec<Action> {
        self.head_ended = true;
        let ids: Vec<u64> = self.served.keys().copied().collect();
        ids.into_iter()
            .flat_map(|id| self.arrival_ended(id, CallOutcome::InDoubt(ABANDONED)))
            .collect()
    }

    /// Refuse the session before another decision would exceed the stream's retention.
    /// Keep its binding until disconnect so the normal lost-session cancellation runs.
    fn retention_full(
        &mut self,
        session: SessionId,
        key: &StreamKey,
        actions: &mut Vec<Action>,
    ) -> bool {
        let stream = &self.streams[key];
        if stream.applied - stream.acknowledged < self.limits.unacknowledged {
            return false;
        }
        self.sessions.get_mut(&session).unwrap().closing = true;
        actions.push(Action::Send(session, Frame::Refused(OVERLOADED)));
        actions.push(Action::Close(session));
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn decide_refused(
        &mut self,
        key: &StreamKey,
        position: u64,
        call: [u8; 32],
        digest: [u8; 32],
        reason: u32,
        missing: Vec<[u8; 32]>,
        _session: SessionId,
        actions: &mut Vec<Action>,
    ) {
        let stream = self.streams.get_mut(key).unwrap();
        stream.applied = position;
        stream.decided.insert(
            position,
            Decided {
                key: call,
                digest,
                state: State::Queued(0),
            },
        );
        self.decide(
            key,
            position,
            CallOutcome::NotRun(reason),
            None,
            missing,
            actions,
        );
    }

    fn decide_end(
        &mut self,
        key: &StreamKey,
        position: u64,
        end: CallOutcome,
        callee: Option<[u8; 16]>,
        actions: &mut Vec<Action>,
    ) {
        self.decide(key, position, end, callee, vec![], actions)
    }

    /// Record the position's outcome and send it to the stream's current session.
    fn decide(
        &mut self,
        key: &StreamKey,
        position: u64,
        end: CallOutcome,
        callee: Option<[u8; 16]>,
        missing: Vec<[u8; 32]>,
        actions: &mut Vec<Action>,
    ) {
        let decided = &self.streams[key].decided[&position];
        let entry = Entry::Decided {
            stream: key.clone(),
            position,
            key: decided.key,
            digest: decided.digest,
            end: end.clone(),
            callee,
        };
        if !self.append(&entry, actions) {
            return;
        }
        let stream = self.streams.get_mut(key).unwrap();
        let decided = stream.decided.get_mut(&position).unwrap();
        decided.state = State::Done {
            end,
            callee,
            missing,
        };
        if let Some(session) = stream.session
            && let Some(frame) = Self::outcome_frame(stream, position, &stream.decided[&position])
        {
            actions.push(Action::Send(session, frame));
        }
    }

    /// Start queued callees while the executor has room.
    fn dispatch(&mut self, actions: &mut Vec<Action>) {
        while self.failure.is_none() && self.running < self.limits.runs {
            let Some(run) = self.queued.pop_front() else {
                break;
            };
            let Run {
                stream,
                position,
                start,
                ..
            } = &self.runs[&run];
            let decided = self
                .streams
                .get_mut(stream)
                .unwrap()
                .decided
                .get_mut(position)
                .unwrap();
            decided.state = State::Running {
                run,
                forwarded: false,
            };
            self.running += 1;
            actions.push(Action::Start(start.clone()));
        }
    }

    /// The executor reports a callee's end: its outcome, and its run instance when it ran.
    pub fn ended(&mut self, run: RunId, end: CallOutcome, callee: Option<[u8; 16]>) -> Vec<Action> {
        let mut actions = vec![];
        let Some(Run {
            stream, position, ..
        }) = self.runs.remove(&run)
        else {
            return actions;
        };
        self.running -= 1;
        self.decide_end(&stream, position, end, callee, &mut actions);
        self.dispatch(&mut actions);
        self.checkpoint(&mut actions);
        actions
    }

    /// Whether any callee is queued or running.
    pub fn busy(&self) -> bool {
        self.running > 0 || !self.queued.is_empty()
    }
}
