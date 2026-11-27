//! In-process `Dialer` effect adapter driving `std.wsock.local_server` over
//! `UnitCtx::unfunded()` and `ConnCore` (EMB-3, RUNNER-SPEC §14.11).
//!
//! One lightweight I/O thread per generation owns a single `UnitCtx::unfunded()`
//! and multiplexes all open embedded sessions' `Dial`, `Accepted`, and `Bytes`
//! connections without sockets, environment reads, or polling loops.
use crate::{
    units::ledger::{
        Took, UnitCtx,
        conn::{Client, Inbox, Sent},
    },
    units_lang::{ClientH, CrossSend},
};
use hirsel_core_layout::cross::{CLIENT, SEQ};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

/// `std.wsock.LOCAL_CHUNK` (16 KiB).
const LOCAL_CHUNK: usize = 16 * 1024;
/// `std.wsock.LOCAL_ITEMS` (8 slots).
const LOCAL_ITEMS: u64 = 8;
/// `std.wsock.LOCAL_ROOM` (64 KiB).
const LOCAL_ROOM: u64 = 64 * 1024;

/// `Bytes` crossing rows: `[0, SEQ, 1, 1]`, flat size 24 (`[ptr, len, cap]`).
const BYTES_ROWS: [[u64; 4]; 1] = [[0, SEQ, 1, 1]];
/// `Accepted` crossing rows: `to: Client[Bytes]` at offset 0 (align 8), `code: u16` at offset 8.
const ACCEPTED_ROWS: [[u64; 4]; 1] = [[0, CLIENT, 0, 0]];
/// Empty wake connection rows.
const EMPTY_ROWS: [[u64; 4]; 0] = [];

#[inline]
fn round8(n: u64) -> u64 {
    n.div_ceil(8) * 8
}

/// Commands sent from host threads into the generation's transport I/O thread.
pub(crate) enum TransportCmd {
    Dial { session: u64 },
    SendBytes { session: u64, bytes: Vec<u8> },
    ResumeRead { session: u64 },
    CloseSession { session: u64 },
    Shutdown,
}

/// Events delivered from the transport I/O thread up to the session engine.
pub(crate) enum TransportEvent {
    DialAccepted { session: u64 },
    DialRefused { session: u64, code: u16 },
    InboundBytes { session: u64, bytes: Vec<u8> },
    PeerClosed { session: u64 },
}

/// Callback invoked by the transport thread for each [`TransportEvent`].
/// Returning `true` on `InboundBytes` pauses further reads on that session's
/// `back_inbox` (backpressure so the place sees full slots / slow reader).
pub(crate) type TransportHandler = Arc<dyn Fn(TransportEvent) -> bool + Send + Sync>;

struct SessionIo {
    /// Set once `Accepted(101, to)` arrives.
    to_place: Option<Client>,
    /// Inbox receiving `Bytes` chunks from the place.
    back_inbox: Inbox,
    /// Pending `Accepted` inbox while dial handshake is in flight.
    accept_inbox: Option<Inbox>,
    /// Outbound raw bytes queued for sending in <= 16 KiB chunks.
    outbound: VecDeque<Vec<u8>>,
    /// Currently waiting `send_cross` buffer pinned until `Took::Sent`.
    in_flight: Option<InFlightSend>,
    /// True when the engine asked to pause reading `back_inbox` (backpressure).
    read_paused: bool,
    /// True once `Dial` has been accepted.
    accepted: bool,
}

struct InFlightSend {
    send_id: u32,
    /// Heap-pinned chunk + 24-byte `Bytes` carrier so `CrossSend` pointers stay live.
    _chunk: Box<[u8]>,
    _carrier: Box<[u64; 3]>,
}

/// Handle to the generation's in-process transport thread.
pub(crate) struct Transport {
    cmds: Arc<Mutex<VecDeque<TransportCmd>>>,
    wake_client: Client,
    stopped: AtomicBool,
    handle: Mutex<Option<JoinHandle<()>>>,
}

// SAFETY: `Client` wraps `Arc<ConnCore>`, whose atomic fast-path send is thread-safe.
unsafe impl Send for Transport {}
unsafe impl Sync for Transport {}

impl Transport {
    pub(crate) fn spawn(dialer: Client, handler: TransportHandler) -> std::io::Result<Arc<Self>> {
        let mut unit = UnitCtx::unfunded();
        let (wake_inbox, wake_client) = unit.conn_open(4, 0, 0, 0, &EMPTY_ROWS);
        let wake_for_handle = wake_client.clone_client();
        let cmds = Arc::new(Mutex::new(VecDeque::new()));
        let cmds_bg = cmds.clone();
        let join = thread::Builder::new()
            .name("hirsel-embed-io".into())
            .spawn(move || {
                run_io_loop(unit, dialer, wake_inbox, wake_client, cmds_bg, handler);
            })?;
        Ok(Arc::new(Self {
            cmds,
            wake_client: wake_for_handle,
            stopped: AtomicBool::new(false),
            handle: Mutex::new(Some(join)),
        }))
    }

    pub(crate) fn send_cmd(&self, cmd: TransportCmd) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        self.cmds.lock().unwrap().push_back(cmd);
        self.wake();
    }

    /// Approximate queued outbound bytes (used for room 64 KiB backlog check).
    pub(crate) fn wake(&self) {
        let dummy: u8 = 0;
        let mut scratch: Option<UnitCtx> = None;
        // SAFETY: zero-byte non-blocking wake item; if the 4-slot wake inbox is
        // already full, `issue_slow` with `blocking = false` returns `Sent::Full`
        // immediately without registering any waiter on `scratch`.
        let _ = unsafe {
            UnitCtx::send_cross(&self.wake_client, CrossSend::new(&dummy), 0, false, || {
                scratch.get_or_insert_with(UnitCtx::unfunded)
            })
        };
    }

    pub(crate) fn shutdown_and_join(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            self.cmds.lock().unwrap().push_back(TransportCmd::Shutdown);
            self.wake();
        }
        if let Some(join) = self.handle.lock().unwrap().take() {
            let _ = join.join();
        }
    }
}

fn push_released(
    pending: &mut VecDeque<(Took, usize)>,
    released: Vec<crate::units::ledger::Released>,
) {
    for (took, ident, _) in released {
        pending.push_back((took, ident));
    }
}

fn next_take(unit: &mut UnitCtx, pending: &mut VecDeque<(Took, usize)>) -> Option<(Took, usize)> {
    if let Some(pair) = pending.pop_front() {
        return Some(pair);
    }
    unit.take().map(|(t, _)| (t, unit.took_ident))
}

fn wait_on_thread(unit: &mut UnitCtx, pending: &mut VecDeque<(Took, usize)>) -> (Took, usize) {
    loop {
        if let Some(pair) = next_take(unit, pending) {
            return pair;
        }
        if unit.port.arm() {
            thread::park();
        }
    }
}

fn run_io_loop(
    mut unit: UnitCtx,
    dialer: Client,
    wake_inbox: Inbox,
    wake_client: Client,
    cmds: Arc<Mutex<VecDeque<TransportCmd>>>,
    handler: TransportHandler,
) {
    let _readies = crate::tasks::Readies::for_unit(crate::units::NONE);
    unit.port.set_waker(&_readies);
    let wake_key = Arc::as_ptr(&wake_inbox.0) as usize;
    let mut sessions: BTreeMap<u64, SessionIo> = BTreeMap::new();
    let mut inbox_to_session: HashMap<usize, (u64, bool)> = HashMap::new(); // (sid, is_accept)
    let mut send_to_session: HashMap<u32, u64> = HashMap::new();
    let mut sealing: HashMap<usize, Inbox> = HashMap::new();
    let mut pending_takes: VecDeque<(Took, usize)> = VecDeque::new();
    let mut shutting_down = false;

    loop {
        // 1. Drain pending host commands.
        loop {
            let cmd = cmds.lock().unwrap().pop_front();
            let Some(cmd) = cmd else { break };
            match cmd {
                TransportCmd::Shutdown => {
                    shutting_down = true;
                    break;
                }
                TransportCmd::Dial { session } => {
                    let (back_inbox, back_client) =
                        unit.conn_open(LOCAL_ITEMS, 24, LOCAL_ROOM, 24, &BYTES_ROWS);
                    let (accept_inbox, accept_client) =
                        unit.conn_open(1, 16, 0, 16, &ACCEPTED_ROWS);
                    let back_ptr = Box::into_raw(Box::new(ClientH {
                        client: back_client,
                    }));
                    let accept_ptr = Box::into_raw(Box::new(ClientH {
                        client: accept_client,
                    }));
                    let dial_flat: [usize; 2] = [back_ptr as usize, accept_ptr as usize];
                    // SAFETY: `dial_flat` holds two initialized `ClientH*` pointers matching `std.wsock.Dial`.
                    let sent = unsafe {
                        UnitCtx::send_cross(
                            &dialer,
                            CrossSend::new(dial_flat.as_ptr().cast()),
                            0,
                            false,
                            || &mut unit,
                        )
                    };
                    match sent {
                        Sent::Accepted => {
                            let back_key = Arc::as_ptr(&back_inbox.0) as usize;
                            let acc_key = Arc::as_ptr(&accept_inbox.0) as usize;
                            inbox_to_session.insert(back_key, (session, false));
                            inbox_to_session.insert(acc_key, (session, true));
                            sessions.insert(
                                session,
                                SessionIo {
                                    to_place: None,
                                    back_inbox,
                                    accept_inbox: Some(accept_inbox),
                                    outbound: VecDeque::new(),
                                    in_flight: None,
                                    read_paused: false,
                                    accepted: false,
                                },
                            );
                        }
                        Sent::Closed(_) | Sent::Full(_) | Sent::Bounds => {
                            // Reclaim unsent ClientH boxes cleanly.
                            // SAFETY: `send_cross` did not take ownership on refusal.
                            unsafe {
                                let b = Box::from_raw(back_ptr);
                                let a = Box::from_raw(accept_ptr);
                                push_released(&mut pending_takes, unit.drop_client(b.client));
                                push_released(&mut pending_takes, unit.drop_client(a.client));
                            }
                            begin_seal(&mut unit, accept_inbox, &mut sealing);
                            begin_seal(&mut unit, back_inbox, &mut sealing);
                            let code = if matches!(sent, Sent::Full(_)) {
                                503
                            } else {
                                503
                            };
                            handler(TransportEvent::DialRefused { session, code });
                        }
                        Sent::Waiting(_) => unreachable!("non-blocking dial send"),
                    }
                }
                TransportCmd::SendBytes { session, bytes } => {
                    if let Some(sio) = sessions.get_mut(&session) {
                        if !bytes.is_empty() {
                            for chunk in bytes.chunks(LOCAL_CHUNK) {
                                sio.outbound.push_back(chunk.to_vec());
                            }
                            pump_outbound(&mut unit, session, sio, &mut send_to_session);
                        }
                    }
                }
                TransportCmd::ResumeRead { session } => {
                    if let Some(sio) = sessions.get_mut(&session) {
                        sio.read_paused = false;
                        drain_back_inbox(&mut unit, session, sio, &handler);
                    }
                }
                TransportCmd::CloseSession { session } => {
                    if let Some(sio) = sessions.remove(&session) {
                        cleanup_session(
                            &mut unit,
                            sio,
                            &mut inbox_to_session,
                            &mut send_to_session,
                            &mut sealing,
                            &mut pending_takes,
                        );
                    }
                }
            }
        }
        if shutting_down {
            break;
        }

        // 2. Drain any ready arrivals on `unit` non-blockingly, then wait for next arrival.
        let mut progressed = false;
        while let Some((took, ident)) = next_take(&mut unit, &mut pending_takes) {
            progressed = true;
            handle_took(
                &mut unit,
                took,
                ident,
                wake_key,
                &wake_inbox,
                &mut sessions,
                &mut inbox_to_session,
                &mut send_to_session,
                &mut sealing,
                &mut pending_takes,
                &handler,
            );
        }
        if progressed {
            continue;
        }
        // Check if any command arrived while draining.
        if !cmds.lock().unwrap().is_empty() {
            continue;
        }
        let (took, ident) = wait_on_thread(&mut unit, &mut pending_takes);
        handle_took(
            &mut unit,
            took,
            ident,
            wake_key,
            &wake_inbox,
            &mut sessions,
            &mut inbox_to_session,
            &mut send_to_session,
            &mut sealing,
            &mut pending_takes,
            &handler,
        );
    }

    // Teardown: close every remaining session, wake inbox, and dialer client.
    for (_, sio) in sessions {
        cleanup_session(
            &mut unit,
            sio,
            &mut inbox_to_session,
            &mut send_to_session,
            &mut sealing,
            &mut pending_takes,
        );
    }
    begin_seal(&mut unit, wake_inbox, &mut sealing);
    push_released(&mut pending_takes, unit.drop_client(wake_client));
    push_released(&mut pending_takes, unit.drop_client(dialer));
    while !sealing.is_empty() {
        let (took, ident) = wait_on_thread(&mut unit, &mut pending_takes);
        if matches!(took, Took::SealAnswer) {
            if let Some(inbox) = sealing.remove(&ident) {
                let (_, rel) = unit.clean(&inbox);
                push_released(&mut pending_takes, rel);
            }
        }
    }
}

fn begin_seal(unit: &mut UnitCtx, inbox: Inbox, sealing: &mut HashMap<usize, Inbox>) {
    let key = Arc::as_ptr(&inbox.0) as usize;
    unit.seal(&inbox);
    sealing.insert(key, inbox);
}

#[allow(clippy::too_many_arguments)]
fn handle_took(
    unit: &mut UnitCtx,
    took: Took,
    ident: usize,
    wake_key: usize,
    wake_inbox: &Inbox,
    sessions: &mut BTreeMap<u64, SessionIo>,
    inbox_to_session: &mut HashMap<usize, (u64, bool)>,
    send_to_session: &mut HashMap<u32, u64>,
    sealing: &mut HashMap<usize, Inbox>,
    pending_takes: &mut VecDeque<(Took, usize)>,
    handler: &TransportHandler,
) {
    match took {
        Took::SealAnswer => {
            if let Some(inbox) = sealing.remove(&ident) {
                let (_, rel) = unit.clean(&inbox);
                push_released(pending_takes, rel);
            }
        }
        Took::Item => {
            let conn = ident;
            if conn == wake_key {
                while wake_inbox
                    .receive_cross(|_, _, _, _| Ok::<(), ()>(()))
                    .is_some()
                {}
                return;
            }
            let Some(&(sid, is_accept)) = inbox_to_session.get(&conn) else {
                return;
            };
            if is_accept {
                let outcome = {
                    let Some(sio) = sessions.get_mut(&sid) else {
                        return;
                    };
                    let Some(acc) = sio.accept_inbox.as_ref() else {
                        return;
                    };
                    let mut parsed: Option<(u16, Client)> = None;
                    let _ = acc.receive_cross(|_, flat, _, _| {
                        // SAFETY: `std.wsock.Accepted` has `to: *mut ClientH` at 0, `code: u16` at 8.
                        unsafe {
                            let to_ptr = flat.cast::<*mut ClientH>().read_unaligned();
                            let code = flat.add(8).cast::<u16>().read_unaligned();
                            let client = Box::from_raw(to_ptr).client;
                            parsed = Some((code, client));
                        }
                        Ok::<(), ()>(())
                    });
                    parsed
                };
                if let Some((code, to_client)) = outcome {
                    if code == 101 {
                        if let Some(sio) = sessions.get_mut(&sid) {
                            sio.to_place = Some(to_client);
                            sio.accepted = true;
                            if let Some(acc) = sio.accept_inbox.take() {
                                let acc_key = Arc::as_ptr(&acc.0) as usize;
                                inbox_to_session.remove(&acc_key);
                                begin_seal(unit, acc, sealing);
                            }
                            handler(TransportEvent::DialAccepted { session: sid });
                            pump_outbound(unit, sid, sio, send_to_session);
                            drain_back_inbox(unit, sid, sio, handler);
                        } else {
                            push_released(pending_takes, unit.drop_client(to_client));
                        }
                    } else {
                        push_released(pending_takes, unit.drop_client(to_client));
                        if let Some(sio) = sessions.remove(&sid) {
                            cleanup_session(
                                unit,
                                sio,
                                inbox_to_session,
                                send_to_session,
                                sealing,
                                pending_takes,
                            );
                        }
                        handler(TransportEvent::DialRefused { session: sid, code });
                    }
                }
            } else if let Some(sio) = sessions.get_mut(&sid) {
                drain_back_inbox(unit, sid, sio, handler);
            }
        }
        Took::Close => {
            let conn = ident;
            let Some(&(sid, is_accept)) = inbox_to_session.get(&conn) else {
                return;
            };
            if is_accept {
                let not_accepted = sessions.get(&sid).is_some_and(|s| !s.accepted);
                if not_accepted {
                    if let Some(sio) = sessions.remove(&sid) {
                        cleanup_session(
                            unit,
                            sio,
                            inbox_to_session,
                            send_to_session,
                            sealing,
                            pending_takes,
                        );
                    }
                    handler(TransportEvent::DialRefused {
                        session: sid,
                        code: 503,
                    });
                }
            } else {
                if let Some(sio) = sessions.get_mut(&sid) {
                    sio.read_paused = false;
                    drain_back_inbox(unit, sid, sio, handler);
                }
                if let Some(sio) = sessions.remove(&sid) {
                    cleanup_session(
                        unit,
                        sio,
                        inbox_to_session,
                        send_to_session,
                        sealing,
                        pending_takes,
                    );
                }
                handler(TransportEvent::PeerClosed { session: sid });
            }
        }
        Took::Accepted(id) => {
            if let Some(sid) = send_to_session.remove(&id) {
                if let Some(sio) = sessions.get_mut(&sid) {
                    sio.in_flight = None;
                    pump_outbound(unit, sid, sio, send_to_session);
                }
            }
        }
        Took::Closed(id) => {
            if let Some(sid) = send_to_session.remove(&id) {
                if let Some(sio) = sessions.get_mut(&sid) {
                    sio.in_flight = None;
                    sio.outbound.clear();
                }
            }
        }
        _ => {}
    }
}

fn drain_back_inbox(unit: &mut UnitCtx, sid: u64, sio: &mut SessionIo, handler: &TransportHandler) {
    let _ = unit;
    while !sio.read_paused {
        let mut chunk: Option<Vec<u8>> = None;
        let received = sio.back_inbox.receive_cross(|_, flat, content, _| {
            // SAFETY: `Bytes` has `[ptr, len, cap]` at offsets 0, 8, 16; `content` starts its bytes.
            unsafe {
                let len = flat.add(8).cast::<u64>().read_unaligned() as usize;
                let data = if len == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(content, len).to_vec()
                };
                chunk = Some(data);
            }
            Ok::<(), ()>(())
        });
        if received.is_none() {
            break;
        }
        if let Some(bytes) = chunk {
            if !bytes.is_empty() {
                let pause = handler(TransportEvent::InboundBytes {
                    session: sid,
                    bytes,
                });
                if pause {
                    sio.read_paused = true;
                    break;
                }
            }
        }
    }
}

fn pump_outbound(
    unit: &mut UnitCtx,
    sid: u64,
    sio: &mut SessionIo,
    send_to_session: &mut HashMap<u32, u64>,
) {
    if sio.in_flight.is_some() {
        return;
    }
    let Some(to) = sio.to_place.as_ref() else {
        return;
    };
    while let Some(chunk) = sio.outbound.pop_front() {
        let boxed = chunk.into_boxed_slice();
        let len = boxed.len() as u64;
        let carrier = Box::new([boxed.as_ptr() as u64, len, len]);
        let charge = round8(len);
        // SAFETY: `carrier` and `boxed` stay pinned in `InFlightSend` if `Sent::Waiting`.
        let sent = unsafe {
            UnitCtx::send_cross(
                to,
                CrossSend::new(carrier.as_ptr().cast()),
                charge,
                true,
                || unit,
            )
        };
        match sent {
            Sent::Accepted => {}
            Sent::Waiting(send_id) => {
                send_to_session.insert(send_id, sid);
                sio.in_flight = Some(InFlightSend {
                    send_id,
                    _chunk: boxed,
                    _carrier: carrier,
                });
                return;
            }
            Sent::Closed(_) | Sent::Full(_) | Sent::Bounds => {
                sio.outbound.clear();
                return;
            }
        }
    }
}

fn cleanup_session(
    unit: &mut UnitCtx,
    mut sio: SessionIo,
    inbox_to_session: &mut HashMap<usize, (u64, bool)>,
    send_to_session: &mut HashMap<u32, u64>,
    sealing: &mut HashMap<usize, Inbox>,
    pending_takes: &mut VecDeque<(Took, usize)>,
) {
    if let Some(inflight) = sio.in_flight.take() {
        if let Some(to) = sio.to_place.as_ref() {
            let _ = unit.withdraw(to, inflight.send_id);
        }
        send_to_session.remove(&inflight.send_id);
    }
    if let Some(to) = sio.to_place.take() {
        push_released(pending_takes, unit.drop_client(to));
    }
    if let Some(acc) = sio.accept_inbox.take() {
        let k = Arc::as_ptr(&acc.0) as usize;
        inbox_to_session.remove(&k);
        begin_seal(unit, acc, sealing);
    }
    let bk = Arc::as_ptr(&sio.back_inbox.0) as usize;
    inbox_to_session.remove(&bk);
    begin_seal(unit, sio.back_inbox, sealing);
}

const DIAL_ROWS: [[u64; 4]; 2] = [[0, CLIENT, 0, 0], [8, CLIENT, 0, 0]];

/// Events produced by [`DialServer::poll_event`].
pub enum DialServerEvent {
    /// New session connection accepted (`conn_id`).
    Connected(usize),
    /// Raw bytes received from `conn_id`.
    Bytes(usize, Vec<u8>),
    /// Peer closed `conn_id`.
    Closed(usize),
    /// Dialer client dropped; server should shut down.
    DialerClosed,
}

/// Minimal in-process `Dial` server over `UnitCtx::unfunded()` for harness fixtures
/// that emulate a Hirsel runner place without linking `units`/`units_lang` directly.
pub struct DialServer {
    unit: UnitCtx,
    _readies: Arc<crate::tasks::Readies>,
    dial_inbox: Inbox,
    dial_key: usize,
    sessions: HashMap<usize, (Inbox, Client)>,
    sealing: HashMap<usize, Inbox>,
    pending: VecDeque<(Took, usize)>,
}

impl DialServer {
    /// Open `Inbox[Dial]` and offer its client to `launch::maybe_capture_dialer`.
    pub fn bind_and_capture() -> Self {
        let mut unit = UnitCtx::unfunded();
        let readies = crate::tasks::Readies::for_unit(crate::units::NONE);
        unit.port.set_waker(&readies);
        let (dial_inbox, dial_client) = unit.conn_open(16, 16, 0, 16, &DIAL_ROWS);
        super::launch::maybe_capture_dialer(&dial_client);
        let mut pending = VecDeque::new();
        push_released(&mut pending, unit.drop_client(dial_client));
        let dial_key = Arc::as_ptr(&dial_inbox.0) as usize;
        Self {
            unit,
            _readies: readies,
            dial_inbox,
            dial_key,
            sessions: HashMap::new(),
            sealing: HashMap::new(),
            pending,
        }
    }

    /// Block until the next server event arrives.
    pub fn next_event(&mut self) -> DialServerEvent {
        loop {
            let (took, ident) = wait_on_thread(&mut self.unit, &mut self.pending);
            match took {
                Took::SealAnswer => {
                    if let Some(inbox) = self.sealing.remove(&ident) {
                        let (_, rel) = self.unit.clean(&inbox);
                        push_released(&mut self.pending, rel);
                    }
                }
                Took::Close if ident == self.dial_key => return DialServerEvent::DialerClosed,
                Took::Close => {
                    if let Some((inbox, out_c)) = self.sessions.remove(&ident) {
                        push_released(&mut self.pending, self.unit.drop_client(out_c));
                        begin_seal(&mut self.unit, inbox, &mut self.sealing);
                        return DialServerEvent::Closed(ident);
                    }
                }
                Took::Item if ident == self.dial_key => {
                    let mut pair: Option<(Client, Client)> = None;
                    let _ = self.dial_inbox.receive_cross(|_, flat, _, _| {
                        // SAFETY: `Dial` holds `[back: *mut ClientH, accept: *mut ClientH]`.
                        unsafe {
                            let back =
                                Box::from_raw(flat.cast::<*mut ClientH>().read_unaligned()).client;
                            let acc =
                                Box::from_raw(flat.add(8).cast::<*mut ClientH>().read_unaligned())
                                    .client;
                            pair = Some((back, acc));
                        }
                        Ok::<(), ()>(())
                    });
                    if let Some((back, acc)) = pair {
                        let (to_inbox, to_client) =
                            self.unit.conn_open(8, 24, 65536, 24, &BYTES_ROWS);
                        let to_key = Arc::as_ptr(&to_inbox.0) as usize;
                        let to_ptr = Box::into_raw(Box::new(ClientH { client: to_client }));
                        let mut acc_flat = [0u8; 16];
                        // SAFETY: `Accepted` layout: `to` @ 0, `code` @ 8.
                        unsafe {
                            acc_flat
                                .as_mut_ptr()
                                .cast::<*mut ClientH>()
                                .write_unaligned(to_ptr);
                            acc_flat
                                .as_mut_ptr()
                                .add(8)
                                .cast::<u16>()
                                .write_unaligned(101);
                            let _ = UnitCtx::send_cross(
                                &acc,
                                CrossSend::new(acc_flat.as_ptr()),
                                0,
                                false,
                                || &mut self.unit,
                            );
                        }
                        push_released(&mut self.pending, self.unit.drop_client(acc));
                        self.sessions.insert(to_key, (to_inbox, back));
                        return DialServerEvent::Connected(to_key);
                    }
                }
                Took::Item => {
                    let mut buf = Vec::new();
                    if let Some((inbox, _)) = self.sessions.get(&ident) {
                        while inbox
                            .receive_cross(|_, flat, content, _| {
                                // SAFETY: `Bytes` flat header + `content`.
                                unsafe {
                                    let len = flat.add(8).cast::<u64>().read_unaligned() as usize;
                                    if len > 0 {
                                        buf.extend_from_slice(std::slice::from_raw_parts(
                                            content, len,
                                        ));
                                    }
                                }
                                Ok::<(), ()>(())
                            })
                            .is_some()
                        {}
                    }
                    if !buf.is_empty() {
                        return DialServerEvent::Bytes(ident, buf);
                    }
                }
                _ => {}
            }
        }
    }

    /// Send raw bytes back to `conn_id`.
    pub fn send_bytes(&mut self, conn_id: usize, raw: &[u8]) {
        let Some((_, out_c)) = self.sessions.get(&conn_id) else {
            return;
        };
        let out_c = out_c.clone_client();
        let len = raw.len() as u64;
        let carrier = [raw.as_ptr() as u64, len, len];
        let charge = round8(len);
        // SAFETY: non-blocking send keeps `carrier` and `raw` pinned.
        let _ = unsafe {
            UnitCtx::send_cross(
                &out_c,
                CrossSend::new(carrier.as_ptr().cast()),
                charge,
                false,
                || &mut self.unit,
            )
        };
        push_released(&mut self.pending, self.unit.drop_client(out_c));
    }

    /// Seal and drain all inboxes cleanly before returning from the place worker.
    pub fn shutdown(mut self) {
        for (_, (inbox, out_c)) in self.sessions.drain() {
            push_released(&mut self.pending, self.unit.drop_client(out_c));
            begin_seal(&mut self.unit, inbox, &mut self.sealing);
        }
        begin_seal(&mut self.unit, self.dial_inbox, &mut self.sealing);
        while !self.sealing.is_empty() {
            let (t, k) = wait_on_thread(&mut self.unit, &mut self.pending);
            if matches!(t, Took::SealAnswer) {
                if let Some(inbox) = self.sealing.remove(&k) {
                    let (_, rel) = self.unit.clean(&inbox);
                    push_released(&mut self.pending, rel);
                }
            }
        }
    }
}
