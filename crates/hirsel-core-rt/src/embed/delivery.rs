//! Single-threaded ordered callback delivery loop (EMB-4).
//!
//! Each generation owns one delivery thread. Every callback for a generation
//! runs on that thread in queue order, with no runtime lock held during the
//! callback. Supports:
//! - Delivery-thread identity detection (`stop` -> `HIRSEL_BUSY`, `close` -> `HIRSEL_DEFERRED`).
//! - Synchronous host `close` barrier (waits for admitted session producers,
//!   waits for any active callback of that session, strips queued callbacks,
//!   delivers `session_ended(Closed)` last, waits for it to return, releases).
//! - Deferred delivery-thread `close` (runs the exact same drain right after
//!   the active callback returns).
//! - Spontaneous end and host `stop` ordered session shutdown + `config.ended`.
use super::abi::{HirselBytes, HirselEmbedCallbacks};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle, ThreadId},
};

pub(crate) type EndedFn = unsafe extern "C" fn(*mut std::ffi::c_void, u32, HirselBytes);

#[derive(Clone, Copy)]
pub(crate) struct RuntimeEndedCb {
    pub(crate) context: *mut std::ffi::c_void,
    pub(crate) ended: EndedFn,
}

// SAFETY: Host explicitly authorizes passing opaque `context` and fn ptr to the delivery thread.
unsafe impl Send for RuntimeEndedCb {}
unsafe impl Sync for RuntimeEndedCb {}

#[derive(Clone, Copy)]
pub(crate) struct SessionCbs {
    pub(crate) context: *mut std::ffi::c_void,
    pub(crate) outcome: unsafe extern "C" fn(*mut std::ffi::c_void, u64, HirselBytes),
    pub(crate) change: unsafe extern "C" fn(*mut std::ffi::c_void, u64, HirselBytes),
    pub(crate) sub_ended: unsafe extern "C" fn(*mut std::ffi::c_void, u64, HirselBytes),
    pub(crate) room_event: unsafe extern "C" fn(*mut std::ffi::c_void, u64, HirselBytes),
    pub(crate) session_ended: unsafe extern "C" fn(*mut std::ffi::c_void, HirselBytes),
}

// SAFETY: Host explicitly authorizes passing opaque `context` and fn ptrs to the delivery thread.
unsafe impl Send for SessionCbs {}
unsafe impl Sync for SessionCbs {}

impl SessionCbs {
    pub(crate) fn from_raw(raw: &HirselEmbedCallbacks) -> Option<Self> {
        Some(Self {
            context: raw.context,
            outcome: raw.outcome?,
            change: raw.change?,
            sub_ended: raw.sub_ended?,
            room_event: raw.room_event?,
            session_ended: raw.session_ended?,
        })
    }
}

/// Callback item queued for the delivery thread.
pub(crate) enum DeliveryItem {
    Outcome {
        session: u64,
        call: u64,
        bytes: Vec<u8>,
        charge: u64,
        on_start: Option<Box<dyn FnOnce() + Send>>,
    },
    Change {
        session: u64,
        sub: u64,
        bytes: Vec<u8>,
        charge: u64,
    },
    SubEnded {
        session: u64,
        sub: u64,
        bytes: Vec<u8>,
        charge: u64,
        on_start: Option<Box<dyn FnOnce() + Send>>,
    },
    RoomEvent {
        session: u64,
        room: u64,
        bytes: Vec<u8>,
        charge: u64,
        is_ended: bool,
        on_start: Option<Box<dyn FnOnce() + Send>>,
    },
    SessionEnded {
        session: u64,
        bytes: Vec<u8>,
        on_done: Option<Box<dyn FnOnce() + Send>>,
    },
    RuntimeEnded {
        code: u32,
        reason: Vec<u8>,
        on_done: Option<Box<dyn FnOnce() + Send>>,
    },
    Shutdown,
}

struct SessionSlot {
    cbs: SessionCbs,
    closing: bool,
    ended: bool,
    deferred_close: Option<Box<dyn FnOnce() -> (Vec<u8>, Box<dyn FnOnce() + Send>) + Send>>,
}

struct State {
    thread_id: Option<ThreadId>,
    queue: VecDeque<DeliveryItem>,
    sessions: BTreeMap<u64, SessionSlot>,
    active_session: Option<u64>,
    runtime_ended_delivered: bool,
    stopped: bool,
}

/// Hook invoked whenever a callback finishes so `account.refund(charge)` runs.
pub(crate) type RefundFn = Arc<dyn Fn(u64) + Send + Sync>;

pub(crate) struct Delivery {
    runtime_cb: RuntimeEndedCb,
    refund: RefundFn,
    state: Arc<(Mutex<State>, Condvar)>,
    closed: AtomicBool,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Delivery {
    pub(crate) fn spawn(
        runtime_cb: RuntimeEndedCb,
        refund: RefundFn,
    ) -> std::io::Result<Arc<Self>> {
        let state = Arc::new((
            Mutex::new(State {
                thread_id: None,
                queue: VecDeque::new(),
                sessions: BTreeMap::new(),
                active_session: None,
                runtime_ended_delivered: false,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let d = Arc::new(Self {
            runtime_cb,
            refund,
            state: state.clone(),
            closed: AtomicBool::new(false),
            handle: Mutex::new(None),
        });
        let worker = d.clone();
        let join = thread::Builder::new()
            .name("hirsel-embed-delivery".into())
            .spawn(move || worker.run_loop())?;
        {
            let (lock, cv) = &*state;
            let mut g = lock.lock().unwrap();
            while g.thread_id.is_none() && !g.stopped {
                g = cv.wait(g).unwrap();
            }
        }
        *d.handle.lock().unwrap() = Some(join);
        Ok(d)
    }

    pub(crate) fn is_delivery_thread(&self) -> bool {
        let g = self.state.0.lock().unwrap();
        g.thread_id == Some(thread::current().id())
    }

    pub(crate) fn register_session(&self, session: u64, cbs: SessionCbs) {
        let mut g = self.state.0.lock().unwrap();
        g.sessions.insert(
            session,
            SessionSlot {
                cbs,
                closing: false,
                ended: false,
                deferred_close: None,
            },
        );
    }

    pub(crate) fn enqueue(&self, item: DeliveryItem) -> bool {
        let (lock, cv) = &*self.state;
        let mut g = lock.lock().unwrap();
        if g.stopped {
            return false;
        }
        if let Some(sid) = item_session(&item) {
            let Some(slot) = g.sessions.get(&sid) else {
                return false;
            };
            if slot.ended || (slot.closing && !matches!(item, DeliveryItem::SessionEnded { .. })) {
                return false;
            }
        }
        g.queue.push_back(item);
        cv.notify_all();
        true
    }

    /// Remove a queued (not yet started) callback for `(session, call)` if present.
    /// Returns the refunded charge if removed, or `None` if it was not in the queue.
    pub(crate) fn cancel_queued_call(&self, session: u64, call: u64) -> Option<u64> {
        let mut g = self.state.0.lock().unwrap();
        if let Some(pos) = g.queue.iter().position(|item| {
            matches!(item, DeliveryItem::Outcome { session: s, call: c, .. } if *s == session && *c == call)
        }) {
            if let Some(DeliveryItem::Outcome { charge, .. }) = g.queue.remove(pos) {
                return Some(charge);
            }
        }
        None
    }

    /// Remove all queued (not yet started) callbacks for `(session, sub)`.
    pub(crate) fn cancel_queued_sub(&self, session: u64, sub: u64) -> u64 {
        let mut g = self.state.0.lock().unwrap();
        let mut refunded = 0u64;
        g.queue.retain(|item| match item {
            DeliveryItem::Change {
                session: s,
                sub: id,
                charge,
                ..
            }
            | DeliveryItem::SubEnded {
                session: s,
                sub: id,
                charge,
                ..
            } if *s == session && *id == sub => {
                refunded += *charge;
                false
            }
            _ => true,
        });
        refunded
    }

    /// Remove all queued (not yet started) callbacks for `(session, room)`.
    pub(crate) fn cancel_queued_room(&self, session: u64, room: u64) -> u64 {
        let mut g = self.state.0.lock().unwrap();
        let mut refunded = 0u64;
        g.queue.retain(|item| match item {
            DeliveryItem::RoomEvent {
                session: s,
                room: id,
                charge,
                ..
            } if *s == session && *id == room => {
                refunded += *charge;
                false
            }
            _ => true,
        });
        refunded
    }

    /// Mark session closing on the delivery thread (`HIRSEL_DEFERRED`); `finalize`
    /// runs on the delivery thread right after the current callback returns to
    /// drain admitted producers, obtain the `SessionEnd` payload, and release state.
    pub(crate) fn defer_close_from_delivery(
        &self,
        session: u64,
        finalize: Box<dyn FnOnce() -> (Vec<u8>, Box<dyn FnOnce() + Send>) + Send>,
    ) {
        let (lock, cv) = &*self.state;
        let mut g = lock.lock().unwrap();
        if let Some(slot) = g.sessions.get_mut(&session) {
            slot.closing = true;
            slot.deferred_close = Some(finalize);
            let refunded = strip_session_queue(&mut g.queue, session);
            drop(g);
            if refunded > 0 {
                (self.refund)(refunded);
            }
            cv.notify_all();
        }
    }

    /// Synchronously close `session` from a non-delivery host thread:
    /// strips queued callbacks, waits for any in-flight callback of `session`,
    /// delivers `session_ended(end_bytes)` on the delivery thread, waits for
    /// it to return, and runs `on_done` after `session_ended` returns.
    pub(crate) fn close_session_sync(
        &self,
        session: u64,
        end_bytes: Vec<u8>,
        on_done: Box<dyn FnOnce() + Send>,
    ) {
        let done_flag = Arc::new((Mutex::new(false), Condvar::new()));
        let done_flag_cb = done_flag.clone();
        let refunded = {
            let (lock, cv) = &*self.state;
            let mut g = lock.lock().unwrap();
            if g.stopped {
                drop(g);
                on_done();
                return;
            }
            let Some(slot) = g.sessions.get_mut(&session) else {
                drop(g);
                on_done();
                return;
            };
            if slot.ended {
                drop(g);
                on_done();
                return;
            }
            slot.closing = true;
            let refunded = strip_session_queue(&mut g.queue, session);
            // Push SessionEnded at the front so it runs as soon as any active callback finishes.
            g.queue.push_front(DeliveryItem::SessionEnded {
                session,
                bytes: end_bytes,
                on_done: Some(Box::new(move || {
                    on_done();
                    let mut d = done_flag_cb.0.lock().unwrap();
                    *d = true;
                    done_flag_cb.1.notify_all();
                })),
            });
            cv.notify_all();
            refunded
        };
        if refunded > 0 {
            (self.refund)(refunded);
        }
        let mut d = done_flag.0.lock().unwrap();
        while !*d {
            d = done_flag.1.wait(d).unwrap();
        }
    }

    /// Deliver `session_ended` for every still-live session in ascending session-id
    /// order, then deliver `config.ended(code, reason)`, and wait until all have returned.
    pub(crate) fn finish_generation_sync(
        &self,
        session_end_bytes: Vec<u8>,
        code: u32,
        reason: Vec<u8>,
        release_session: impl Fn(u64) + Send + Sync + 'static,
    ) {
        let done_flag = Arc::new((Mutex::new(false), Condvar::new()));
        let done_flag_cb = done_flag.clone();
        let release_session = Arc::new(release_session);
        let refunded = {
            let (lock, cv) = &*self.state;
            let mut g = lock.lock().unwrap();
            if g.stopped || g.runtime_ended_delivered {
                return;
            }
            let live_sessions: Vec<u64> = g
                .sessions
                .iter()
                .filter(|(_, s)| !s.ended && !s.closing)
                .map(|(&id, _)| id)
                .collect();
            let mut total_refund = 0u64;
            for &sid in &live_sessions {
                if let Some(slot) = g.sessions.get_mut(&sid) {
                    slot.closing = true;
                }
                total_refund += strip_session_queue(&mut g.queue, sid);
            }
            for sid in live_sessions {
                let rel = release_session.clone();
                g.queue.push_back(DeliveryItem::SessionEnded {
                    session: sid,
                    bytes: session_end_bytes.clone(),
                    on_done: Some(Box::new(move || rel(sid))),
                });
            }
            g.queue.push_back(DeliveryItem::RuntimeEnded {
                code,
                reason,
                on_done: Some(Box::new(move || {
                    let mut d = done_flag_cb.0.lock().unwrap();
                    *d = true;
                    done_flag_cb.1.notify_all();
                })),
            });
            cv.notify_all();
            total_refund
        };
        if refunded > 0 {
            (self.refund)(refunded);
        }
        let mut d = done_flag.0.lock().unwrap();
        while !*d {
            d = done_flag.1.wait(d).unwrap();
        }
    }

    /// Stop and join the delivery thread. Must NOT be called on the delivery thread.
    pub(crate) fn shutdown_and_join(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let (lock, cv) = &*self.state;
            let mut g = lock.lock().unwrap();
            g.queue.push_back(DeliveryItem::Shutdown);
            cv.notify_all();
        }
        if let Some(join) = self.handle.lock().unwrap().take() {
            let _ = join.join();
        }
    }

    fn run_loop(&self) {
        {
            let (lock, cv) = &*self.state;
            let mut g = lock.lock().unwrap();
            g.thread_id = Some(thread::current().id());
            cv.notify_all();
        }
        loop {
            let next = {
                let (lock, cv) = &*self.state;
                let mut g = lock.lock().unwrap();
                loop {
                    if let Some(item) = g.queue.pop_front() {
                        break item;
                    }
                    g = cv.wait(g).unwrap();
                }
            };
            match next {
                DeliveryItem::Shutdown => {
                    let (lock, cv) = &*self.state;
                    let mut g = lock.lock().unwrap();
                    g.stopped = true;
                    cv.notify_all();
                    return;
                }
                DeliveryItem::RuntimeEnded {
                    code,
                    reason,
                    on_done,
                } => {
                    let should_run = {
                        let mut g = self.state.0.lock().unwrap();
                        if g.runtime_ended_delivered {
                            false
                        } else {
                            g.runtime_ended_delivered = true;
                            true
                        }
                    };
                    if should_run {
                        let bytes = HirselBytes::from_slice(&reason);
                        // SAFETY: host callback pointer and context remain retained until return.
                        unsafe {
                            (self.runtime_cb.ended)(self.runtime_cb.context, code, bytes);
                        }
                    }
                    if let Some(cb) = on_done {
                        cb();
                    }
                }
                DeliveryItem::SessionEnded {
                    session,
                    bytes,
                    on_done,
                } => {
                    let cbs = {
                        let mut g = self.state.0.lock().unwrap();
                        let Some(slot) = g.sessions.get_mut(&session) else {
                            drop(g);
                            if let Some(cb) = on_done {
                                cb();
                            }
                            continue;
                        };
                        if slot.ended {
                            drop(g);
                            if let Some(cb) = on_done {
                                cb();
                            }
                            continue;
                        }
                        slot.closing = true;
                        slot.ended = true;
                        slot.deferred_close = None;
                        let cbs = slot.cbs;
                        g.active_session = Some(session);
                        cbs
                    };
                    let view = HirselBytes::from_slice(&bytes);
                    // SAFETY: session context is retained until session_ended returns.
                    unsafe {
                        (cbs.session_ended)(cbs.context, view);
                    }
                    {
                        let (lock, cv) = &*self.state;
                        let mut g = lock.lock().unwrap();
                        g.active_session = None;
                        g.sessions.remove(&session);
                        cv.notify_all();
                    }
                    if let Some(cb) = on_done {
                        cb();
                    }
                }
                DeliveryItem::Outcome {
                    session,
                    call,
                    bytes,
                    charge,
                    on_start,
                } => {
                    if let Some(cbs) = self.begin_session_callback(session) {
                        if let Some(f) = on_start {
                            f();
                        }
                        let view = HirselBytes::from_slice(&bytes);
                        // SAFETY: session callbacks and context live through this callback.
                        unsafe {
                            (cbs.outcome)(cbs.context, call, view);
                        }
                        self.end_session_callback(session);
                    }
                    (self.refund)(charge);
                }
                DeliveryItem::Change {
                    session,
                    sub,
                    bytes,
                    charge,
                } => {
                    if let Some(cbs) = self.begin_session_callback(session) {
                        let view = HirselBytes::from_slice(&bytes);
                        // SAFETY: session callbacks and context live through this callback.
                        unsafe {
                            (cbs.change)(cbs.context, sub, view);
                        }
                        self.end_session_callback(session);
                    }
                    (self.refund)(charge);
                }
                DeliveryItem::SubEnded {
                    session,
                    sub,
                    bytes,
                    charge,
                    on_start,
                } => {
                    if let Some(cbs) = self.begin_session_callback(session) {
                        if let Some(f) = on_start {
                            f();
                        }
                        let view = HirselBytes::from_slice(&bytes);
                        // SAFETY: session callbacks and context live through this callback.
                        unsafe {
                            (cbs.sub_ended)(cbs.context, sub, view);
                        }
                        self.end_session_callback(session);
                    }
                    (self.refund)(charge);
                }
                DeliveryItem::RoomEvent {
                    session,
                    room,
                    bytes,
                    charge,
                    is_ended: _,
                    on_start,
                } => {
                    if let Some(cbs) = self.begin_session_callback(session) {
                        if let Some(f) = on_start {
                            f();
                        }
                        let view = HirselBytes::from_slice(&bytes);
                        // SAFETY: session callbacks and context live through this callback.
                        unsafe {
                            (cbs.room_event)(cbs.context, room, view);
                        }
                        self.end_session_callback(session);
                    }
                    (self.refund)(charge);
                }
            }
        }
    }

    fn begin_session_callback(&self, session: u64) -> Option<SessionCbs> {
        let mut g = self.state.0.lock().unwrap();
        let slot = g.sessions.get(&session)?;
        if slot.closing || slot.ended {
            return None;
        }
        let cbs = slot.cbs;
        g.active_session = Some(session);
        Some(cbs)
    }

    fn end_session_callback(&self, session: u64) {
        let deferred = {
            let (lock, cv) = &*self.state;
            let mut g = lock.lock().unwrap();
            g.active_session = None;
            cv.notify_all();
            g.sessions
                .get_mut(&session)
                .and_then(|s| s.deferred_close.take().map(|f| (s.cbs, f)))
        };
        if let Some((cbs, finalize)) = deferred {
            // Drains admitted session producers outside the delivery lock, then delivers
            // `session_ended` and releases session state.
            let (end_bytes, on_done) = finalize();
            let should_deliver = {
                let mut g = self.state.0.lock().unwrap();
                match g.sessions.get_mut(&session) {
                    Some(slot) if !slot.ended => {
                        slot.ended = true;
                        true
                    }
                    _ => false,
                }
            };
            if should_deliver {
                let view = HirselBytes::from_slice(&end_bytes);
                // SAFETY: session context is retained until session_ended returns.
                unsafe {
                    (cbs.session_ended)(cbs.context, view);
                }
                let mut g = self.state.0.lock().unwrap();
                g.sessions.remove(&session);
            }
            on_done();
        }
    }
}

fn item_session(item: &DeliveryItem) -> Option<u64> {
    match item {
        DeliveryItem::Outcome { session, .. }
        | DeliveryItem::Change { session, .. }
        | DeliveryItem::SubEnded { session, .. }
        | DeliveryItem::RoomEvent { session, .. }
        | DeliveryItem::SessionEnded { session, .. } => Some(*session),
        DeliveryItem::RuntimeEnded { .. } | DeliveryItem::Shutdown => None,
    }
}

fn strip_session_queue(queue: &mut VecDeque<DeliveryItem>, session: u64) -> u64 {
    let mut refunded = 0u64;
    queue.retain(|item| match item {
        DeliveryItem::Outcome {
            session: s, charge, ..
        }
        | DeliveryItem::Change {
            session: s, charge, ..
        }
        | DeliveryItem::SubEnded {
            session: s, charge, ..
        }
        | DeliveryItem::RoomEvent {
            session: s, charge, ..
        } if *s == session => {
            refunded += *charge;
            false
        }
        _ => true,
    });
    refunded
}
