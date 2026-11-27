//! Embedded host-side runner protocol engine (EMB-4).
//!
//! Drives the shared 3145 leased-stream lane kernel ([`super::kernel`]) over
//! the in-process [`super::dial::Transport`], maintaining:
//! - Separation between host call/sub/room IDs and wire `(stream, p)` / `sub_id`.
//! - Concurrent leased stream lanes up to `1 + q`, FIFO queue beyond `1 + q`.
//! - Subscription image caching with 4-byte collection-ID prefix stripping and
//!   `ClientAccount` enforcement (`SubEnd::ClientCapacity`).
//! - Room state machine matching `sdk/client/room.ts` + EMB-4 pre/post-message
//!   `Peers` split and first-`Full` rule.
use super::{
    abi::{
        HIRSEL_FULL, HIRSEL_ID_IN_USE, HIRSEL_NOT_LIVE, HIRSEL_OK, HIRSEL_OVER_BOUND,
        HIRSEL_UNKNOWN_ENTRY, HirselStatus,
    },
    account::{AccountError, CALL_BASE_BYTES, ClientAccount, SUB_BASE_BYTES},
    codec::{
        Change, Frame, Group, KeyedDone, MessageIn, Outcome, Part, Peer, PeerState, PeersChange,
        Refusal, RoomEvent, RowChange, SessionEnd, StreamId, SubEnd, ValueChange, Want,
    },
    delivery::{Delivery, DeliveryItem},
    dial::{Transport, TransportCmd, TransportEvent},
    image::SubImage,
    kernel::{self, Action, Attempt, Event, PendingLaneCall, Phase, StepContext, StreamLane},
    room::RoomState,
    ws::{self, WsDecoder, WsEvent},
};
use hirsel_core_ids::embed::{EntryKind, ManifestEntry, ParsedManifest};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const STORE_KEY_PREFIX_BYTES: usize = 4;
const LIVE_BUFFER_BYTES: usize = 65_536;

/// One queued call waiting for a leased stream lane (FIFO in submission order).
struct QueuedCall {
    host_call: u64,
    want: u32,
    deadline_ms: u32,
    args: Vec<u8>,
    charge: u64,
}

pub(super) struct LiveSub {
    host_id: u64,
    wire_sub: u32,
    want: u32,
    args: Vec<u8>,
    is_room: bool,
    image: SubImage,
    room: Option<RoomState>,
    base_charge: u64,
    /// True once terminal callback has started on delivery thread (for race rule).
    terminal_started: Arc<AtomicBool>,
}

pub(super) struct SessionState {
    session_id: u64,
    credential: Option<Vec<u8>>,
    base_charge: u64,
    phase: Phase,
    decoder: WsDecoder,
    max_frame: usize,
    stream_birth_ms: u64,
    welcome_target: Option<String>,
    /// Leased stream lanes (`0..width`), each owning a unique `StreamId`.
    lanes: Vec<StreamLane>,
    /// Pending host calls waiting for a free lane (FIFO).
    call_queue: VecDeque<QueuedCall>,
    /// Live host call ids -> `terminal_started` flag (for `abandon` race rule).
    live_calls: HashMap<u64, Arc<AtomicBool>>,
    /// Live host subscription ids -> wire sub id.
    host_subs: HashMap<u64, u32>,
    /// Live host room ids -> wire sub id.
    host_rooms: HashMap<u64, u32>,
    /// Wire sub id -> `LiveSub`.
    wire_subs: HashMap<u32, LiveSub>,
    next_wire_sub: u32,
    /// Buffered unsent room Live bytes (approx for 64 KiB drop check).
    live_backlog: usize,
    closed: bool,
}

/// Manages all sessions' protocol state machines for one embedded runtime generation.
pub(crate) struct ClientEngine {
    manifest: Arc<ParsedManifest>,
    wants: Vec<Want>,
    account: Arc<ClientAccount>,
    delivery: Arc<Delivery>,
    transport: Mutex<Option<Arc<Transport>>>,
    sessions: Mutex<HashMap<u64, SessionState>>,
    /// Test-visible last Welcome target string.
    last_welcome_target: Mutex<Option<String>>,
}

impl ClientEngine {
    pub(crate) fn new(
        manifest: Arc<ParsedManifest>,
        account: Arc<ClientAccount>,
        delivery: Arc<Delivery>,
    ) -> Arc<Self> {
        let wants = manifest
            .entries
            .iter()
            .map(|e| Want {
                identity: e.identity.clone(),
                args_fp: e.args_fp,
                result_fp: e.result_fp,
            })
            .collect();
        Arc::new(Self {
            manifest,
            wants,
            account,
            delivery,
            transport: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            last_welcome_target: Mutex::new(None),
        })
    }

    pub(crate) fn attach_transport(&self, transport: Arc<Transport>) {
        *self.transport.lock().unwrap() = Some(transport);
    }

    pub(crate) fn last_welcome_target(&self) -> Option<String> {
        self.last_welcome_target.lock().unwrap().clone()
    }

    pub(crate) fn open_session(
        &self,
        session_id: u64,
        credential: Option<Vec<u8>>,
        base_charge: u64,
    ) {
        let mut st = SessionState {
            session_id,
            credential,
            base_charge,
            phase: Phase::initial(),
            decoder: WsDecoder::new(super::account::DEFAULT_MAX_FRAME as usize),
            max_frame: super::account::DEFAULT_MAX_FRAME as usize,
            stream_birth_ms: 0,
            welcome_target: None,
            lanes: Vec::new(),
            call_queue: VecDeque::new(),
            live_calls: HashMap::new(),
            host_subs: HashMap::new(),
            host_rooms: HashMap::new(),
            wire_subs: HashMap::new(),
            next_wire_sub: 1,
            live_backlog: 0,
            closed: false,
        };
        let ctx = StepContext {
            wants: self.wants.len(),
            has_credential_source: false,
        };
        let tr = kernel::step(st.phase, Event::Start, ctx);
        st.phase = tr.phase;
        self.sessions.lock().unwrap().insert(session_id, st);
        if let Some(t) = self.transport.lock().unwrap().as_ref() {
            t.send_cmd(TransportCmd::Dial {
                session: session_id,
            });
        }
    }

    /// Close a session from the host side; sends WS Close 1000 and refunds its charges.
    pub(crate) fn close_session(&self, session_id: u64) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(mut st) = sessions.remove(&session_id) else {
            return;
        };
        st.closed = true;
        self.refund_session(&mut st);
        drop(sessions);
        if let Some(t) = self.transport.lock().unwrap().as_ref() {
            t.send_cmd(TransportCmd::SendBytes {
                session: session_id,
                bytes: ws::encode_close(1000),
            });
            t.send_cmd(TransportCmd::CloseSession {
                session: session_id,
            });
        }
    }

    pub(super) fn refund_session(&self, st: &mut SessionState) {
        for q in st.call_queue.drain(..) {
            self.account.refund(q.charge);
        }
        for lane in &mut st.lanes {
            if let Some(p) = lane.pending.take() {
                self.account.refund(p.charge);
            }
        }
        for (_, mut sub) in st.wire_subs.drain() {
            sub.image.release(&self.account);
            self.account.refund(sub.base_charge);
        }
        if st.base_charge > 0 {
            self.account.refund(st.base_charge);
            st.base_charge = 0;
        }
    }

    pub(crate) fn call(
        &self,
        session_id: u64,
        call_id: u64,
        want: u32,
        args: &[u8],
        deadline_ms: u32,
    ) -> HirselStatus {
        let Some(entry) = self.manifest.entries.get(want as usize) else {
            return HIRSEL_UNKNOWN_ENTRY;
        };
        if !entry.kind.is_callable() {
            return HIRSEL_UNKNOWN_ENTRY;
        }
        let mut sessions = self.sessions.lock().unwrap();
        let Some(st) = sessions.get_mut(&session_id) else {
            return super::abi::HIRSEL_STOPPED;
        };
        if st.closed {
            return super::abi::HIRSEL_STOPPED;
        }
        if st.live_calls.contains_key(&call_id) {
            return HIRSEL_ID_IN_USE;
        }
        let (copied_args, charge) = match self.account.try_copy_charged(args, CALL_BASE_BYTES) {
            Ok(v) => v,
            Err(AccountError::Full) => return HIRSEL_FULL,
            Err(AccountError::HostMemory) => return super::abi::HIRSEL_HOST_MEMORY,
        };
        st.live_calls
            .insert(call_id, Arc::new(AtomicBool::new(false)));
        st.call_queue.push_back(QueuedCall {
            host_call: call_id,
            want,
            deadline_ms,
            args: copied_args,
            charge,
        });
        let frames = self.pump_calls_locked(st);
        drop(sessions);
        self.send_frames(session_id, frames);
        HIRSEL_OK
    }

    pub(crate) fn abandon(&self, session_id: u64, call_id: u64) -> HirselStatus {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(st) = sessions.get_mut(&session_id) else {
            return super::abi::HIRSEL_STOPPED;
        };
        if st.closed {
            return super::abi::HIRSEL_STOPPED;
        }
        let Some(started) = st.live_calls.get(&call_id) else {
            return HIRSEL_NOT_LIVE;
        };
        if started.load(Ordering::Acquire) {
            return HIRSEL_NOT_LIVE;
        }
        st.live_calls.remove(&call_id);

        // 1. If still queued before dispatch, remove and refund immediately.
        if let Some(pos) = st.call_queue.iter().position(|q| q.host_call == call_id) {
            if let Some(q) = st.call_queue.remove(pos) {
                self.account.refund(q.charge);
            }
            return HIRSEL_OK;
        }
        // 2. If in-flight on a lane, detach host observer (lane stays occupied until wire answer!).
        for lane in &mut st.lanes {
            if let Some(p) = lane.pending.as_mut()
                && p.host_call == Some(call_id)
            {
                p.host_call = None;
                return HIRSEL_OK;
            }
        }
        // 3. Otherwise outcome might already be sitting in the delivery queue (not yet started).
        drop(sessions);
        if let Some(refunded) = self.delivery.cancel_queued_call(session_id, call_id) {
            self.account.refund(refunded);
            return HIRSEL_OK;
        }
        // Started racing right between checks.
        HIRSEL_NOT_LIVE
    }

    pub(crate) fn subscribe(
        &self,
        session_id: u64,
        sub_id: u64,
        want: u32,
        args: &[u8],
        is_room: bool,
    ) -> HirselStatus {
        let Some(entry) = self.manifest.entries.get(want as usize) else {
            return HIRSEL_UNKNOWN_ENTRY;
        };
        let ok_kind = if is_room {
            entry.kind.is_room()
        } else {
            entry.kind.is_subscribable()
        };
        if !ok_kind {
            return HIRSEL_UNKNOWN_ENTRY;
        }
        let mut sessions = self.sessions.lock().unwrap();
        let Some(st) = sessions.get_mut(&session_id) else {
            return super::abi::HIRSEL_STOPPED;
        };
        if st.closed {
            return super::abi::HIRSEL_STOPPED;
        }
        let exists = if is_room {
            st.host_rooms.contains_key(&sub_id)
        } else {
            st.host_subs.contains_key(&sub_id)
        };
        if exists {
            return HIRSEL_ID_IN_USE;
        }
        let (copied_args, charge) = match self.account.try_copy_charged(args, SUB_BASE_BYTES) {
            Ok(v) => v,
            Err(AccountError::Full) => return HIRSEL_FULL,
            Err(AccountError::HostMemory) => return super::abi::HIRSEL_HOST_MEMORY,
        };
        let wire_sub = st.next_wire_sub;
        st.next_wire_sub = st.next_wire_sub.wrapping_add(1).max(1);
        let ready = matches!(st.phase, Phase::Ready { .. });
        let frame = ready.then(|| Frame::Subscribe(wire_sub, want, copied_args.clone(), None));
        let sub = LiveSub {
            host_id: sub_id,
            wire_sub,
            want,
            args: copied_args,
            is_room,
            image: SubImage::new(),
            room: is_room.then(RoomState::new),
            base_charge: charge,
            terminal_started: Arc::new(AtomicBool::new(false)),
        };
        st.wire_subs.insert(wire_sub, sub);
        if is_room {
            st.host_rooms.insert(sub_id, wire_sub);
        } else {
            st.host_subs.insert(sub_id, wire_sub);
        }
        drop(sessions);
        if let Some(f) = frame {
            self.send_frames(session_id, vec![f]);
        }
        HIRSEL_OK
    }

    pub(crate) fn unsubscribe(&self, session_id: u64, id: u64, is_room: bool) -> HirselStatus {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(st) = sessions.get_mut(&session_id) else {
            return super::abi::HIRSEL_STOPPED;
        };
        if st.closed {
            return super::abi::HIRSEL_STOPPED;
        }
        let map = if is_room {
            &mut st.host_rooms
        } else {
            &mut st.host_subs
        };
        let Some(&wire_sub) = map.get(&id) else {
            return HIRSEL_NOT_LIVE;
        };
        if let Some(sub) = st.wire_subs.get(&wire_sub)
            && sub.terminal_started.load(Ordering::Acquire)
        {
            return HIRSEL_NOT_LIVE;
        }
        map.remove(&id);
        let ready = matches!(st.phase, Phase::Ready { .. });
        if let Some(mut sub) = st.wire_subs.remove(&wire_sub) {
            sub.image.release(&self.account);
            self.account.refund(sub.base_charge);
        }
        drop(sessions);
        let refunded = if is_room {
            self.delivery.cancel_queued_room(session_id, id)
        } else {
            self.delivery.cancel_queued_sub(session_id, id)
        };
        if refunded > 0 {
            self.account.refund(refunded);
        }
        if ready {
            self.send_frames(session_id, vec![Frame::Unsubscribe(wire_sub)]);
        }
        HIRSEL_OK
    }

    pub(crate) fn room_set(
        &self,
        session_id: u64,
        room_id: u64,
        present: bool,
        state_bytes: &[u8],
    ) -> HirselStatus {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(st) = sessions.get_mut(&session_id) else {
            return super::abi::HIRSEL_STOPPED;
        };
        if st.closed {
            return super::abi::HIRSEL_STOPPED;
        }
        let Some(&wire_sub) = st.host_rooms.get(&room_id) else {
            return HIRSEL_NOT_LIVE;
        };
        let Some(sub) = st.wire_subs.get_mut(&wire_sub) else {
            return HIRSEL_NOT_LIVE;
        };
        let Some(entry) = self.manifest.entries.get(sub.want as usize) else {
            return HIRSEL_UNKNOWN_ENTRY;
        };
        let Some(limits) = entry.room else {
            return HIRSEL_UNKNOWN_ENTRY;
        };
        if present && (state_bytes.len() as u64) > limits.state_max {
            return HIRSEL_OVER_BOUND;
        }
        let next_state = if present {
            match super::account::try_copy_slice(state_bytes) {
                Ok(v) => Some(v),
                Err(AccountError::HostMemory) => return super::abi::HIRSEL_HOST_MEMORY,
                Err(AccountError::Full) => return HIRSEL_FULL,
            }
        } else {
            None
        };
        let ready = matches!(st.phase, Phase::Ready { .. });
        let frame = if let Some(room) = sub.room.as_mut() {
            room.stored_state = Some(next_state.clone());
            room.owed_state = true;
            if ready && room.self_peer.is_some() {
                room.owed_state = false;
                Some(Frame::Set(wire_sub, next_state))
            } else {
                None
            }
        } else {
            None
        };
        drop(sessions);
        if let Some(f) = frame {
            self.send_frames(session_id, vec![f]);
        }
        HIRSEL_OK
    }

    pub(crate) fn room_send(
        &self,
        session_id: u64,
        room_id: u64,
        to: u64,
        message: &[u8],
    ) -> HirselStatus {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(st) = sessions.get_mut(&session_id) else {
            return super::abi::HIRSEL_STOPPED;
        };
        if st.closed {
            return super::abi::HIRSEL_STOPPED;
        }
        let Some(&wire_sub) = st.host_rooms.get(&room_id) else {
            return HIRSEL_NOT_LIVE;
        };
        let Some(sub) = st.wire_subs.get(&wire_sub) else {
            return HIRSEL_NOT_LIVE;
        };
        let Some(entry) = self.manifest.entries.get(sub.want as usize) else {
            return HIRSEL_UNKNOWN_ENTRY;
        };
        let Some(limits) = entry.room else {
            return HIRSEL_UNKNOWN_ENTRY;
        };
        if (message.len() as u64) > limits.message_max {
            return HIRSEL_OVER_BOUND;
        }
        let msg_copy = match super::account::try_copy_slice(message) {
            Ok(v) => v,
            Err(AccountError::HostMemory) => return super::abi::HIRSEL_HOST_MEMORY,
            Err(AccountError::Full) => return HIRSEL_FULL,
        };
        let ready = matches!(st.phase, Phase::Ready { .. });
        let joined = sub.room.as_ref().is_some_and(|r| r.self_peer.is_some());
        if !ready || !joined || st.live_backlog > LIVE_BUFFER_BYTES {
            drop(sessions);
            let ev = RoomEvent::Lost(1).encode_bytes();
            let _ = self.delivery.enqueue(DeliveryItem::RoomEvent {
                session: session_id,
                room: room_id,
                bytes: ev,
                charge: 0,
                is_ended: false,
                on_start: None,
            });
            return HIRSEL_OK;
        }
        let target = if to == 0 {
            None
        } else {
            u32::try_from(to).ok()
        };
        let frame = Frame::Send(wire_sub, target, msg_copy);
        drop(sessions);
        self.send_frames(session_id, vec![frame]);
        HIRSEL_OK
    }
}

#[path = "client_wire.rs"]
mod wire;
