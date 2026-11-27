//! Inbound WebSocket framing, `Batch`/`Reset`/`Live`/`Ended` handling, and lane pumping.
use super::*;
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    sync::atomic::AtomicU64,
};

fn next_unique_stream_n() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(seq);
    h.finish()
}

impl ClientEngine {
    /// Entry point invoked by [`super::dial::Transport`] for every in-process transport event.
    /// Returns `true` when inbound reading on `session` should be paused for backpressure.
    pub(crate) fn on_transport_event(&self, event: TransportEvent) -> bool {
        match event {
            TransportEvent::DialAccepted { session } => {
                let hello = {
                    let mut sessions = self.sessions.lock().unwrap();
                    let Some(st) = sessions.get_mut(&session) else {
                        return false;
                    };
                    let epoch = st.phase.epoch();
                    let ctx = StepContext {
                        wants: self.wants.len(),
                        has_credential_source: false,
                    };
                    let tr = kernel::step(st.phase.clone(), Event::SocketOpen { epoch }, ctx);
                    st.phase = tr.phase;
                    if tr.actions.contains(&Action::SendHello) {
                        Some(Frame::Hello(st.credential.clone(), self.wants.clone()))
                    } else {
                        None
                    }
                };
                if let Some(frame) = hello {
                    self.send_frames(session, vec![frame]);
                }
                false
            }
            TransportEvent::DialRefused { session, code } => {
                self.end_session_from_place(session, SessionEnd::DialRefused(code));
                false
            }
            TransportEvent::PeerClosed { session } => {
                self.end_session_from_place(session, SessionEnd::ClosedByPlace(1006));
                false
            }
            TransportEvent::InboundBytes { session, bytes } => {
                self.handle_inbound_bytes(session, &bytes)
            }
        }
    }

    fn handle_inbound_bytes(&self, session: u64, bytes: &[u8]) -> bool {
        let mut outbound_raw: Vec<Vec<u8>> = Vec::new();
        let mut outbound_frames: Vec<Frame> = Vec::new();
        let mut close_code: Option<u16> = None;
        let mut pause_reads = false;

        {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(st) = sessions.get_mut(&session) else {
                return false;
            };
            if st.closed {
                return false;
            }
            st.decoder.push(bytes);
            while let Some(ev) = st.decoder.pop() {
                match ev {
                    WsEvent::Ping(payload) => {
                        outbound_raw.push(ws::encode_pong(&payload));
                    }
                    WsEvent::Pong(_) => {}
                    WsEvent::Close(code) => {
                        close_code = Some(code);
                        break;
                    }
                    WsEvent::ProtocolError(code) => {
                        close_code = Some(code);
                        break;
                    }
                    WsEvent::Binary(payload) => match Frame::decode(&payload) {
                        Ok(frame) => {
                            let (more, paused) = self.handle_frame_locked(st, frame);
                            outbound_frames.extend(more);
                            if paused {
                                pause_reads = true;
                            }
                        }
                        Err(_) => {
                            close_code = Some(1007);
                            break;
                        }
                    },
                }
            }
        }

        if let Some(t) = self.transport.lock().unwrap().as_ref() {
            for raw in outbound_raw {
                t.send_cmd(TransportCmd::SendBytes {
                    session,
                    bytes: raw,
                });
            }
        }
        if !outbound_frames.is_empty() {
            self.send_frames(session, outbound_frames);
        }
        if let Some(code) = close_code {
            self.end_session_from_place(session, SessionEnd::ClosedByPlace(code));
        }
        pause_reads
    }

    fn handle_frame_locked(&self, st: &mut SessionState, frame: Frame) -> (Vec<Frame>, bool) {
        let ctx = StepContext {
            wants: self.wants.len(),
            has_credential_source: false,
        };
        let epoch = st.phase.epoch();
        let tr = kernel::step(st.phase.clone(), Event::Frame { epoch, frame }, ctx);
        st.phase = tr.phase;
        let mut to_send = Vec::new();
        let mut pause_reads = false;

        for action in tr.actions {
            match action {
                Action::BindWelcome(Frame::Welcome {
                    q, target, now_ms, ..
                }) => {
                    st.welcome_target = Some(target.clone());
                    *self.last_welcome_target.lock().unwrap() = Some(target);
                    st.stream_birth_ms = now_ms;
                    let width = 1usize.saturating_add(q as usize);
                    while st.lanes.len() < width {
                        let lane_id = st.lanes.len() as u32;
                        let stream = StreamId {
                            t: st.stream_birth_ms,
                            n: next_unique_stream_n(),
                        };
                        st.lanes.push(StreamLane::new(lane_id, stream));
                    }
                    let tr2 = kernel::step(
                        st.phase.clone(),
                        Event::WelcomeBound {
                            epoch: st.phase.epoch(),
                            at: 0,
                            width,
                        },
                        ctx,
                    );
                    st.phase = tr2.phase;
                    if tr2
                        .actions
                        .iter()
                        .any(|a| matches!(a, Action::BecomeReady { .. }))
                    {
                        to_send.extend(self.on_ready_locked(st));
                    }
                }
                Action::BecomeReady { .. } => {
                    to_send.extend(self.on_ready_locked(st));
                }
                Action::Update(update_frame) => {
                    let (more, paused) = self.apply_update_locked(st, update_frame);
                    to_send.extend(more);
                    if paused {
                        pause_reads = true;
                    }
                }
                _ => {}
            }
        }
        (to_send, pause_reads)
    }

    fn on_ready_locked(&self, st: &mut SessionState) -> Vec<Frame> {
        let mut out = Vec::new();
        // Send any subscriptions/rooms that were opened before Welcome/Placed completed.
        for sub in st.wire_subs.values() {
            out.push(Frame::Subscribe(
                sub.wire_sub,
                sub.want,
                sub.args.clone(),
                None,
            ));
        }
        out.extend(self.pump_calls_locked(st));
        out
    }

    pub(super) fn pump_calls_locked(&self, st: &mut SessionState) -> Vec<Frame> {
        let mut out = Vec::new();
        if !matches!(st.phase, Phase::Ready { .. }) {
            return out;
        }
        let ctx = StepContext {
            wants: self.wants.len(),
            has_credential_source: false,
        };
        // First replay any pending non-inflight lane calls (e.g. after reconnect).
        for lane_idx in 0..st.lanes.len() {
            if st.lanes[lane_idx].retired {
                continue;
            }
            if st.lanes[lane_idx]
                .pending
                .as_ref()
                .is_some_and(|p| !p.inflight)
            {
                let lane_id = st.lanes[lane_idx].id;
                let epoch = st.phase.epoch();
                let tr = kernel::step(
                    st.phase.clone(),
                    Event::PrepareCall {
                        epoch,
                        lane: lane_id,
                    },
                    ctx,
                );
                st.phase = tr.phase;
                if tr
                    .actions
                    .iter()
                    .any(|a| matches!(a, Action::RunPrepare { .. }))
                {
                    let tr2 = kernel::step(
                        st.phase.clone(),
                        Event::SendCall {
                            epoch,
                            lane: lane_id,
                            delivery: Attempt::WebSocket,
                        },
                        ctx,
                    );
                    st.phase = tr2.phase;
                    let stream = st.lanes[lane_idx].stream;
                    if let Some(p) = st.lanes[lane_idx].pending.as_mut() {
                        p.inflight = true;
                        out.push(Frame::Call(
                            stream,
                            p.p,
                            p.want,
                            p.deadline_ms,
                            p.args.clone(),
                        ));
                    }
                }
            }
        }
        // Then assign queued calls to idle non-retired lanes up to width `1 + q`.
        while !st.call_queue.is_empty() {
            let free_idx = st
                .lanes
                .iter()
                .position(|l| !l.retired && l.pending.is_none() && !st.phase.lane_owed(l.id));
            let Some(idx) = free_idx else {
                break;
            };
            let lane_id = st.lanes[idx].id;
            let epoch = st.phase.epoch();
            let tr = kernel::step(
                st.phase.clone(),
                Event::PrepareCall {
                    epoch,
                    lane: lane_id,
                },
                ctx,
            );
            st.phase = tr.phase;
            if !tr
                .actions
                .iter()
                .any(|a| matches!(a, Action::RunPrepare { .. }))
            {
                break;
            }
            let queued = st.call_queue.pop_front().unwrap();
            let tr2 = kernel::step(
                st.phase.clone(),
                Event::SendCall {
                    epoch,
                    lane: lane_id,
                    delivery: Attempt::WebSocket,
                },
                ctx,
            );
            st.phase = tr2.phase;
            let p_num = st.lanes[idx].next;
            let stream = st.lanes[idx].stream;
            out.push(Frame::Call(
                stream,
                p_num,
                queued.want,
                queued.deadline_ms,
                queued.args.clone(),
            ));
            st.lanes[idx].pending = Some(PendingLaneCall {
                p: p_num,
                want: queued.want,
                deadline_ms: queued.deadline_ms,
                args: queued.args,
                inflight: true,
                host_call: Some(queued.host_call),
                charge: queued.charge,
            });
        }
        out
    }

    fn apply_update_locked(&self, st: &mut SessionState, frame: Frame) -> (Vec<Frame>, bool) {
        let mut out = Vec::new();
        let mut pause_reads = false;
        let ctx = StepContext {
            wants: self.wants.len(),
            has_credential_source: false,
        };
        match frame {
            Frame::Batch {
                version,
                complete,
                groups,
                outcomes,
            } => {
                let epoch = st.phase.epoch();
                let tr = kernel::step(
                    st.phase.clone(),
                    Event::AdvanceVersion { epoch, version },
                    ctx,
                );
                st.phase = tr.phase;

                // 1. Apply subscription groups.
                let mut completed_set: BTreeSet<u32> = complete.into_iter().collect();
                let mut seen_groups: BTreeSet<u32> = BTreeSet::new();
                for group in &groups {
                    seen_groups.insert(group.sub);
                    let is_comp = completed_set.remove(&group.sub);
                    let (more, paused) = self.apply_group_locked(st, group, is_comp);
                    out.extend(more);
                    if paused {
                        pause_reads = true;
                    }
                }
                // Any completed sub not mentioned in `groups` completed with empty image.
                for sub_id in completed_set {
                    if !seen_groups.contains(&sub_id) {
                        let empty = Group {
                            sub: sub_id,
                            value: Vec::new(),
                            rows: Vec::new(),
                        };
                        let (more, paused) = self.apply_group_locked(st, &empty, true);
                        out.extend(more);
                        if paused {
                            pause_reads = true;
                        }
                    }
                }
                // 2. Apply call outcomes.
                let (more, paused) = self.apply_outcomes_locked(st, outcomes);
                out.extend(more);
                if paused {
                    pause_reads = true;
                }
            }
            Frame::Reset(version, outcomes) => {
                let epoch = st.phase.epoch();
                let tr = kernel::step(
                    st.phase.clone(),
                    Event::AdvanceVersion { epoch, version },
                    ctx,
                );
                st.phase = tr.phase;
                // Invalidate all live subscriptions and resubscribe with resume: None.
                let old_subs: Vec<u32> = st.wire_subs.keys().copied().collect();
                for old_wire in old_subs {
                    if let Some(mut sub) = st.wire_subs.remove(&old_wire) {
                        let change = sub.image.clear_for_reset(&self.account);
                        let bytes = change.encode_bytes();
                        if sub.is_room {
                            let ev = RoomEvent::Admission(change).encode_bytes();
                            let _ = self.delivery.enqueue(DeliveryItem::RoomEvent {
                                session: st.session_id,
                                room: sub.host_id,
                                bytes: ev,
                                charge: 0,
                                is_ended: false,
                                on_start: None,
                            });
                            if let Some(r) = sub.room.as_mut() {
                                r.self_peer = None;
                                r.full = false;
                                r.owed_state = r.stored_state.is_some();
                            }
                        } else {
                            let _ = self.delivery.enqueue(DeliveryItem::Change {
                                session: st.session_id,
                                sub: sub.host_id,
                                bytes,
                                charge: 0,
                            });
                        }
                        let new_wire = st.next_wire_sub;
                        st.next_wire_sub = st.next_wire_sub.wrapping_add(1).max(1);
                        sub.wire_sub = new_wire;
                        if sub.is_room {
                            st.host_rooms.insert(sub.host_id, new_wire);
                        } else {
                            st.host_subs.insert(sub.host_id, new_wire);
                        }
                        out.push(Frame::Subscribe(new_wire, sub.want, sub.args.clone(), None));
                        st.wire_subs.insert(new_wire, sub);
                    }
                }
                let (more, paused) = self.apply_outcomes_locked(st, outcomes);
                out.extend(more);
                if paused {
                    pause_reads = true;
                }
            }
            Frame::Ended(wire_sub, _outcome, raw_outcome) => {
                if let Some(mut sub) = st.wire_subs.remove(&wire_sub) {
                    sub.image.release(&self.account);
                    self.account.refund(sub.base_charge);
                    let host_id = sub.host_id;
                    let is_room = sub.is_room;
                    let started = sub.terminal_started.clone();
                    let sub_end = SubEnd::Server(raw_outcome);
                    if is_room {
                        st.host_rooms.remove(&host_id);
                        let ev = RoomEvent::Ended(sub_end).encode_bytes();
                        let _ = self.delivery.enqueue(DeliveryItem::RoomEvent {
                            session: st.session_id,
                            room: host_id,
                            bytes: ev,
                            charge: 0,
                            is_ended: true,
                            on_start: Some(Box::new(move || {
                                started.store(true, Ordering::Release);
                            })),
                        });
                    } else {
                        st.host_subs.remove(&host_id);
                        let ev = sub_end.encode_bytes();
                        let _ = self.delivery.enqueue(DeliveryItem::SubEnded {
                            session: st.session_id,
                            sub: host_id,
                            bytes: ev,
                            charge: 0,
                            on_start: Some(Box::new(move || {
                                started.store(true, Ordering::Release);
                            })),
                        });
                    }
                }
            }
            Frame::Live(parts) => {
                for part in &parts {
                    let Some(sub) = st.wire_subs.get_mut(&part.sub) else {
                        continue;
                    };
                    let host_room = sub.host_id;
                    let Some(room) = sub.room.as_mut() else {
                        continue;
                    };
                    let events = room.apply_part(part);
                    let flush_set = if room.owed_state && room.self_peer.is_some() {
                        room.owed_state = false;
                        room.stored_state
                            .clone()
                            .map(|state| Frame::Set(sub.wire_sub, state))
                    } else {
                        None
                    };
                    if let Some(f) = flush_set {
                        out.push(f);
                    }
                    for ev in events {
                        let bytes = ev.encode_bytes();
                        let charge = match self.account.try_reserve(bytes.len() as u64) {
                            Ok(c) => c,
                            Err(_) => {
                                pause_reads = true;
                                0
                            }
                        };
                        let _ = self.delivery.enqueue(DeliveryItem::RoomEvent {
                            session: st.session_id,
                            room: host_room,
                            bytes,
                            charge,
                            is_ended: false,
                            on_start: None,
                        });
                    }
                }
            }
            _ => {}
        }
        (out, pause_reads)
    }

    fn apply_group_locked(
        &self,
        st: &mut SessionState,
        group: &Group,
        is_complete: bool,
    ) -> (Vec<Frame>, bool) {
        let mut out = Vec::new();
        let mut pause_reads = false;
        let Some(sub) = st.wire_subs.get_mut(&group.sub) else {
            return (out, false);
        };
        let host_id = sub.host_id;
        let is_room = sub.is_room;
        match sub.image.apply_group(group, is_complete, &self.account) {
            Ok(Some(change)) => {
                let bytes = if is_room {
                    RoomEvent::Admission(change).encode_bytes()
                } else {
                    change.encode_bytes()
                };
                let charge = match self.account.try_reserve(bytes.len() as u64) {
                    Ok(c) => c,
                    Err(_) => {
                        pause_reads = true;
                        0
                    }
                };
                if is_room {
                    let _ = self.delivery.enqueue(DeliveryItem::RoomEvent {
                        session: st.session_id,
                        room: host_id,
                        bytes,
                        charge,
                        is_ended: false,
                        on_start: None,
                    });
                } else {
                    let _ = self.delivery.enqueue(DeliveryItem::Change {
                        session: st.session_id,
                        sub: host_id,
                        bytes,
                        charge,
                    });
                }
            }
            Ok(None) => {}
            Err(_) => {
                // EMB-4: image growth exceeded client_bytes -> end sub with SubEnd::ClientCapacity,
                // free image, send Unsubscribe.
                if let Some(mut removed) = st.wire_subs.remove(&group.sub) {
                    removed.image.release(&self.account);
                    self.account.refund(removed.base_charge);
                    let started = removed.terminal_started.clone();
                    if is_room {
                        st.host_rooms.remove(&host_id);
                        let ev = RoomEvent::Ended(SubEnd::ClientCapacity).encode_bytes();
                        let _ = self.delivery.enqueue(DeliveryItem::RoomEvent {
                            session: st.session_id,
                            room: host_id,
                            bytes: ev,
                            charge: 0,
                            is_ended: true,
                            on_start: Some(Box::new(move || {
                                started.store(true, Ordering::Release);
                            })),
                        });
                    } else {
                        st.host_subs.remove(&host_id);
                        let ev = SubEnd::ClientCapacity.encode_bytes();
                        let _ = self.delivery.enqueue(DeliveryItem::SubEnded {
                            session: st.session_id,
                            sub: host_id,
                            bytes: ev,
                            charge: 0,
                            on_start: Some(Box::new(move || {
                                started.store(true, Ordering::Release);
                            })),
                        });
                    }
                    out.push(Frame::Unsubscribe(group.sub));
                }
            }
        }
        (out, pause_reads)
    }

    fn apply_outcomes_locked(
        &self,
        st: &mut SessionState,
        outcomes: Vec<KeyedDone>,
    ) -> (Vec<Frame>, bool) {
        let mut pause_reads = false;
        let ctx = StepContext {
            wants: self.wants.len(),
            has_credential_source: false,
        };
        for done in outcomes {
            let Some(idx) = st.lanes.iter().position(|l| l.matches_done(&done)) else {
                continue;
            };
            let lane_id = st.lanes[idx].id;
            let Some(pending) = st.lanes[idx].settle(&done.outcome) else {
                continue;
            };
            // Refund the call's request charge now that its lane settled.
            self.account.refund(pending.charge);

            let epoch = st.phase.epoch();
            let tr = kernel::step(
                st.phase.clone(),
                Event::CallFinished {
                    epoch,
                    lane: lane_id,
                },
                ctx,
            );
            st.phase = tr.phase;

            // Replace retired lane with a fresh stream id so width `1 + q` stays available.
            if st.lanes[idx].retired {
                let fresh = StreamId {
                    t: st.stream_birth_ms,
                    n: next_unique_stream_n(),
                };
                st.lanes[idx] = StreamLane::new(lane_id, fresh);
            }

            if let Some(host_call) = pending.host_call {
                if let Some(started) = st.live_calls.remove(&host_call) {
                    let charge = match self.account.try_reserve(done.raw_outcome.len() as u64) {
                        Ok(c) => c,
                        Err(_) => {
                            pause_reads = true;
                            0
                        }
                    };
                    let _ = self.delivery.enqueue(DeliveryItem::Outcome {
                        session: st.session_id,
                        call: host_call,
                        bytes: done.raw_outcome,
                        charge,
                        on_start: Some(Box::new(move || {
                            started.store(true, Ordering::Release);
                        })),
                    });
                }
            }
        }
        let more = self.pump_calls_locked(st);
        (more, pause_reads)
    }

    pub(super) fn send_frames(&self, session: u64, frames: Vec<Frame>) {
        if frames.is_empty() {
            return;
        }
        let Some(t) = self.transport.lock().unwrap().as_ref().cloned() else {
            return;
        };
        for frame in frames {
            let payload = frame.encode();
            let ws_bytes = ws::encode_client_frame(ws::OP_BINARY, &payload);
            t.send_cmd(TransportCmd::SendBytes {
                session,
                bytes: ws_bytes,
            });
        }
    }

    fn end_session_from_place(&self, session: u64, end: SessionEnd) {
        let removed = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(mut st) = sessions.remove(&session) else {
                return;
            };
            st.closed = true;
            self.refund_session(&mut st);
            true
        };
        if removed {
            let _ = self.delivery.enqueue(DeliveryItem::SessionEnded {
                session,
                bytes: end.encode_bytes(),
                on_done: None,
            });
        }
    }

    /// Resume reading paused sessions after callbacks release admissible bytes.
    pub(crate) fn resume_paused_reads(&self) {
        let ids: Vec<u64> = self.sessions.lock().unwrap().keys().copied().collect();
        if let Some(t) = self.transport.lock().unwrap().as_ref() {
            for session in ids {
                t.send_cmd(TransportCmd::ResumeRead { session });
            }
        }
    }

    /// Close all sessions without sending frames (used during `stop` / spontaneous end).
    pub(crate) fn release_all_sessions(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        for (_, mut st) in sessions.drain() {
            st.closed = true;
            self.refund_session(&mut st);
        }
    }

    /// Expose the manifest for entry checks.
    pub(crate) fn manifest(&self) -> &ParsedManifest {
        &self.manifest
    }
}
