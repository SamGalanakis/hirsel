//! Process-static never-freed registry, generation state machine, and
//! per-generation / per-session admission barriers (EMB-4).
use super::{
    account::ClientAccount, client::ClientEngine, codec::SessionEnd, delivery::Delivery,
    dial::Transport, launch::LaunchedPlace,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// Admission barrier held by an accepted producer lookup for its short
/// non-blocking duration (never held across a blocking wait or callback).
pub(crate) struct AdmissionBarrier {
    closed: AtomicBool,
    active: AtomicUsize,
    wait_lock: Mutex<()>,
    wait_cv: Condvar,
}

impl AdmissionBarrier {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            closed: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            wait_cv: Condvar::new(),
        })
    }

    /// Try to enter the barrier. Returns `None` if already closed.
    pub(crate) fn try_enter(self: &Arc<Self>) -> Option<AdmissionGuard> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        if self.closed.load(Ordering::Acquire) {
            if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
                let _g = self.wait_lock.lock().unwrap();
                self.wait_cv.notify_all();
            }
            return None;
        }
        Some(AdmissionGuard {
            barrier: self.clone(),
        })
    }

    /// Close admission and wait until all already-admitted producers drop their guards.
    pub(crate) fn close_and_drain(&self) {
        self.closed.store(true, Ordering::Release);
        let mut g = self.wait_lock.lock().unwrap();
        while self.active.load(Ordering::Acquire) > 0 {
            g = self.wait_cv.wait(g).unwrap();
        }
    }

    pub(crate) fn close_only(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

pub(crate) struct AdmissionGuard {
    barrier: Arc<AdmissionBarrier>,
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if self.barrier.active.fetch_sub(1, Ordering::AcqRel) == 1
            && self.barrier.closed.load(Ordering::Acquire)
        {
            let _g = self.barrier.wait_lock.lock().unwrap();
            self.barrier.wait_cv.notify_all();
        }
    }
}

pub(crate) struct SessionRecord {
    pub(crate) id: u64,
    pub(crate) barrier: Arc<AdmissionBarrier>,
    pub(crate) closing: bool,
}

/// Active generation's live resources.
pub(crate) struct Generation {
    pub(crate) id: u64,
    pub(crate) barrier: Arc<AdmissionBarrier>,
    pub(crate) account: Arc<ClientAccount>,
    pub(crate) delivery: Arc<Delivery>,
    pub(crate) engine: Arc<ClientEngine>,
    pub(crate) transport: Arc<Transport>,
    pub(crate) place: LaunchedPlace,
    pub(crate) spontaneous_done: Arc<(Mutex<bool>, Condvar)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeState {
    Stopped,
    Starting,
    Running,
    Ended,
    Stopping,
}

pub(crate) struct RegistryInner {
    pub(crate) state: RuntimeState,
    pub(crate) current_generation: u64,
    pub(crate) next_generation: u64,
    pub(crate) next_session: u64,
    pub(crate) active: Option<Arc<Generation>>,
    pub(crate) sessions: HashMap<u64, SessionRecord>,
}

pub(crate) struct Registry {
    inner: Mutex<RegistryInner>,
    cv: Condvar,
}

impl Registry {
    fn new() -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                state: RuntimeState::Stopped,
                current_generation: 0,
                next_generation: 1,
                next_session: 1,
                active: None,
                sessions: HashMap::new(),
            }),
            cv: Condvar::new(),
        }
    }

    /// Reserve the registry for a new `start`: `Stopped -> Starting`.
    /// Returns `Some(generation_id)` on success, `None` if not `Stopped`.
    pub(crate) fn reserve_start(&self) -> Option<u64> {
        let mut g = self.inner.lock().unwrap();
        if g.state != RuntimeState::Stopped {
            return None;
        }
        let gen_id = g.next_generation;
        g.next_generation += 1;
        g.current_generation = gen_id;
        g.next_session = 1;
        g.sessions.clear();
        g.state = RuntimeState::Starting;
        Some(gen_id)
    }

    /// Complete a failed start after `config.ended` has returned synchronously: `Starting -> Stopped`.
    pub(crate) fn abort_start(&self, gen_id: u64) {
        let mut g = self.inner.lock().unwrap();
        if g.current_generation == gen_id && g.state == RuntimeState::Starting {
            g.active = None;
            g.sessions.clear();
            g.state = RuntimeState::Stopped;
            self.cv.notify_all();
        }
    }

    /// Publish a ready generation: `Starting -> Running`.
    pub(crate) fn publish_running(&self, gen_rec: Arc<Generation>) {
        let mut g = self.inner.lock().unwrap();
        debug_assert_eq!(g.state, RuntimeState::Starting);
        debug_assert_eq!(g.current_generation, gen_rec.id);
        g.active = Some(gen_rec);
        g.state = RuntimeState::Running;
        self.cv.notify_all();
    }

    /// Acquire a generation admission guard for `open`.
    pub(crate) fn admit_runtime(&self, gen_id: u64) -> Option<(Arc<Generation>, AdmissionGuard)> {
        let g = self.inner.lock().unwrap();
        if g.state != RuntimeState::Running || g.current_generation != gen_id || gen_id == 0 {
            return None;
        }
        let active = g.active.as_ref()?.clone();
        let guard = active.barrier.try_enter()?;
        Some((active, guard))
    }

    /// Register a newly admitted session under the registry lock; returns `None` if
    /// the generation stopped concurrently.
    pub(crate) fn register_session(&self, gen_id: u64) -> Option<u64> {
        let mut g = self.inner.lock().unwrap();
        if g.state != RuntimeState::Running || g.current_generation != gen_id {
            return None;
        }
        let sid = g.next_session;
        g.next_session += 1;
        g.sessions.insert(
            sid,
            SessionRecord {
                id: sid,
                barrier: AdmissionBarrier::new(),
                closing: false,
            },
        );
        Some(sid)
    }

    /// Acquire both generation and session admission guards for a session operation.
    pub(crate) fn admit_session(
        &self,
        gen_id: u64,
        session_id: u64,
    ) -> Option<(Arc<Generation>, AdmissionGuard, AdmissionGuard)> {
        let g = self.inner.lock().unwrap();
        if g.state != RuntimeState::Running
            || g.current_generation != gen_id
            || gen_id == 0
            || session_id == 0
        {
            return None;
        }
        let active = g.active.as_ref()?.clone();
        let rec = g.sessions.get(&session_id)?;
        if rec.closing {
            return None;
        }
        let gen_guard = active.barrier.try_enter()?;
        let sess_guard = rec.barrier.try_enter()?;
        Some((active, gen_guard, sess_guard))
    }

    /// Begin closing a session. Returns `None` immediately (without waiting) if the
    /// handle is stale, runtime is not Running, or the session is already closing/ended.
    pub(crate) fn begin_close_session(
        &self,
        gen_id: u64,
        session_id: u64,
    ) -> Option<(Arc<Generation>, Arc<AdmissionBarrier>)> {
        let mut g = self.inner.lock().unwrap();
        if g.state != RuntimeState::Running
            || g.current_generation != gen_id
            || gen_id == 0
            || session_id == 0
        {
            return None;
        }
        let active = g.active.as_ref()?.clone();
        let rec = g.sessions.get_mut(&session_id)?;
        if rec.closing {
            return None;
        }
        rec.closing = true;
        rec.barrier.close_only();
        Some((active, rec.barrier.clone()))
    }

    /// Remove a session record after `session_ended` has returned.
    pub(crate) fn remove_session(&self, gen_id: u64, session_id: u64) {
        let mut g = self.inner.lock().unwrap();
        if g.current_generation == gen_id {
            g.sessions.remove(&session_id);
        }
    }

    /// Handle spontaneous place failure or run stop (`Running -> Ended`).
    pub(crate) fn on_spontaneous_end(&self, gen_id: u64, code: u32, reason: Vec<u8>) {
        let (active, session_barriers) = {
            let mut g = self.inner.lock().unwrap();
            if g.current_generation != gen_id || g.state != RuntimeState::Running {
                return;
            }
            let Some(active) = g.active.clone() else {
                return;
            };
            g.state = RuntimeState::Ended;
            active.barrier.close_only();
            let barriers: Vec<Arc<AdmissionBarrier>> = g
                .sessions
                .values_mut()
                .map(|s| {
                    s.closing = true;
                    s.barrier.close_only();
                    s.barrier.clone()
                })
                .collect();
            self.cv.notify_all();
            (active, barriers)
        };

        // Wait for every already-admitted host ABI call to drop its guard.
        active.barrier.close_and_drain();
        for b in session_barriers {
            b.close_and_drain();
        }
        active.transport.shutdown_and_join();

        let session_end_bytes = SessionEnd::RuntimeEnded(code, reason.clone()).encode_bytes();
        let engine = active.engine.clone();
        active
            .delivery
            .finish_generation_sync(session_end_bytes, code, reason, move |sid| {
                registry().remove_session(gen_id, sid);
                engine.close_session(sid);
            });
        active.engine.release_all_sessions();
        {
            let (lock, cv) = &*active.spontaneous_done;
            let mut done = lock.lock().unwrap();
            *done = true;
            cv.notify_all();
        }
    }

    /// Begin `stop(handle)`.
    pub(crate) fn stop(&self, gen_id: u64) -> StopDecision {
        let mut g = self.inner.lock().unwrap();
        if gen_id == 0 || g.current_generation != gen_id {
            return StopDecision::Stopped;
        }
        if let Some(active) = g.active.as_ref()
            && active.delivery.is_delivery_thread()
        {
            return StopDecision::Busy;
        }
        match g.state {
            RuntimeState::Stopped | RuntimeState::Starting => StopDecision::Stopped,
            RuntimeState::Stopping => {
                while g.current_generation == gen_id && g.state == RuntimeState::Stopping {
                    g = self.cv.wait(g).unwrap();
                }
                StopDecision::Stopped
            }
            RuntimeState::Running => {
                let active = g.active.take().unwrap();
                g.state = RuntimeState::Stopping;
                active.barrier.close_only();
                let barriers: Vec<Arc<AdmissionBarrier>> = g
                    .sessions
                    .values_mut()
                    .map(|s| {
                        s.closing = true;
                        s.barrier.close_only();
                        s.barrier.clone()
                    })
                    .collect();
                self.cv.notify_all();
                StopDecision::RunTeardown {
                    active,
                    session_barriers: barriers,
                    was_ended: false,
                }
            }
            RuntimeState::Ended => {
                let active = g.active.take().unwrap();
                g.state = RuntimeState::Stopping;
                self.cv.notify_all();
                StopDecision::RunTeardown {
                    active,
                    session_barriers: Vec::new(),
                    was_ended: true,
                }
            }
        }
    }

    pub(crate) fn finish_stop(&self, gen_id: u64) {
        let mut g = self.inner.lock().unwrap();
        if g.current_generation == gen_id && g.state == RuntimeState::Stopping {
            g.sessions.clear();
            g.active = None;
            g.state = RuntimeState::Stopped;
            self.cv.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn current_state(&self) -> RuntimeState {
        self.inner.lock().unwrap().state
    }
}

pub(crate) enum StopDecision {
    Stopped,
    Busy,
    RunTeardown {
        active: Arc<Generation>,
        session_barriers: Vec<Arc<AdmissionBarrier>>,
        was_ended: bool,
    },
}

/// Never-freed process-static registry (EMB-4).
pub(crate) fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(Registry::new)
}

// ---------------------------------------------------------------------------
// Test-only deterministic producer-admission pause hook (for B1/B2 laws)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::{Arc, Condvar, Mutex};

    struct PauseGate {
        entered: bool,
        release: bool,
    }

    static GATE: Mutex<Option<Arc<(Mutex<PauseGate>, Condvar)>>> = Mutex::new(None);

    pub(crate) struct PauseHandle {
        gate: Arc<(Mutex<PauseGate>, Condvar)>,
    }

    impl PauseHandle {
        pub(crate) fn arm() -> Self {
            let gate = Arc::new((
                Mutex::new(PauseGate {
                    entered: false,
                    release: false,
                }),
                Condvar::new(),
            ));
            *GATE.lock().unwrap() = Some(gate.clone());
            Self { gate }
        }

        pub(crate) fn wait_until_entered(&self) {
            let mut g = self.gate.0.lock().unwrap();
            while !g.entered {
                g = self.gate.1.wait(g).unwrap();
            }
        }

        pub(crate) fn release(&self) {
            let mut g = self.gate.0.lock().unwrap();
            g.release = true;
            self.gate.1.notify_all();
        }
    }

    impl Drop for PauseHandle {
        fn drop(&mut self) {
            self.release();
            *GATE.lock().unwrap() = None;
        }
    }

    pub(crate) fn maybe_pause_admitted() {
        let gate = GATE.lock().unwrap().clone();
        if let Some(gate) = gate {
            let mut g = gate.0.lock().unwrap();
            if !g.entered {
                g.entered = true;
                gate.1.notify_all();
                while !g.release {
                    g = gate.1.wait(g).unwrap();
                }
            }
        }
    }
}
