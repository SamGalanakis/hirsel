//! Contained FAIL-15 run stops and per-generation stop handoff (EMB-3).
//!
//! Every runtime stop site returns or raises a [`Stop`] value instead of
//! diverging via `process::stop`. Inside a contained generation, the first stop
//! wins atomically, abandons the current fiber back to its worker's host stack
//! (with all Rust lock guards already dropped), wakes peer workers, and returns
//! the stop to the entry. Only the standalone entry translates a returned stop
//! into `process::stop` (exit status 102 + `run stop: <Name>\n`).
use std::{
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering, compiler_fence},
};

const MAX_STOP_BYTES: usize = 128;

/// A FAIL-15 run stop name stored inline with zero heap allocation so stops
/// work even under `HostMemory` and inside signal handlers.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Stop {
    buf: [u8; MAX_STOP_BYTES],
    len: u8,
}

impl Stop {
    pub const fn from_static(name: &'static [u8]) -> Self {
        let mut buf = [0u8; MAX_STOP_BYTES];
        let n = if name.len() < MAX_STOP_BYTES {
            name.len()
        } else {
            MAX_STOP_BYTES
        };
        let mut i = 0;
        while i < n {
            buf[i] = name[i];
            i += 1;
        }
        Self { buf, len: n as u8 }
    }

    pub fn from_bytes(name: &[u8]) -> Self {
        let mut buf = [0u8; MAX_STOP_BYTES];
        let n = name.len().min(MAX_STOP_BYTES);
        buf[..n].copy_from_slice(&name[..n]);
        Self { buf, len: n as u8 }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    pub fn name(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Debug for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Stop").field(&self.as_str()).finish()
    }
}

/// Why a runtime generation ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndReason {
    /// A FAIL-15 run stop ended the generation (`HIRSEL_END_RUN_STOP`).
    RunStop(Stop),
    /// The host requested an orderly stop (`HIRSEL_END_STOPPED`).
    HostStop,
}

/// What a pool run starts each unit with: the program's entry and the
/// configuration every unit of the execution begins from.
pub(crate) struct Launch {
    pub(crate) run: &'static (dyn Fn(&crate::entry::OsConfig) -> u8 + Sync),
    pub(crate) base: crate::entry::OsConfig,
}
// SAFETY: the launcher pins the entry and the configuration for the whole run.
unsafe impl Send for Launch {}
// SAFETY: as above; both are read-only once set.
unsafe impl Sync for Launch {}

struct PinTable {
    slots: Vec<Option<crate::code::CodePin>>,
    free: Vec<u32>,
}

impl PinTable {
    const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

/// One Place execution (PACK-13): its stop domain, and for a pool run its
/// entry, base configuration and worst unit status. A task's `Context` names
/// its execution at the frozen offset `CONTEXT_EXECUTION`.
///
/// The stop state is an async-signal-safe, lock-free, allocation-free
/// first-stop-wins handoff (EMB-3).
pub struct Execution {
    pub(crate) launch: std::sync::OnceLock<Launch>,
    /// The worst status any unit of a pool run has returned.
    pub(crate) worst: AtomicU8,
    /// 0 = Running, 1 = Recording RunStop, 2 = RunStop published, 3 = HostStop.
    state: AtomicU8,
    /// Fast-path flag checked at scheduling points and `hirsel_core_step_out`.
    ended: AtomicBool,
    /// True when running in embed/contained mode (disables process-wide SIGPIPE
    /// mutation, uses 65,536-step cooperative check, never calls `process::stop`).
    contained: bool,
    len: AtomicU8,
    bytes: [AtomicU8; MAX_STOP_BYTES],
    /// The first hardware fault in loaded code (PACK-9).
    pub(crate) code_fault: crate::code::Record,
    /// Host-owned loaded-code pins (`Held` and `Resource` origin glue leases)
    /// released on normal drop or execution teardown without running Hirsel
    /// cleanup at FAIL-15 run stops.
    pins: std::sync::Mutex<PinTable>,
}

impl Execution {
    pub const fn new(contained: bool) -> Self {
        Self {
            launch: std::sync::OnceLock::new(),
            worst: AtomicU8::new(0),
            state: AtomicU8::new(0),
            ended: AtomicBool::new(false),
            contained,
            len: AtomicU8::new(0),
            bytes: [const { AtomicU8::new(0) }; MAX_STOP_BYTES],
            code_fault: crate::code::Record::new(),
            pins: std::sync::Mutex::new(PinTable::new()),
        }
    }

    pub(crate) fn register_pin(&self, pin: crate::code::CodePin) -> u32 {
        let mut table = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = table.free.pop() {
            table.slots[slot as usize] = Some(pin);
            slot
        } else {
            let slot = u32::try_from(table.slots.len()).expect("pin table overflow");
            table.slots.push(Some(pin));
            slot
        }
    }

    pub(crate) fn unregister_pin(&self, slot: u32) {
        let pin = {
            let mut table = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            let idx = slot as usize;
            if idx >= table.slots.len() {
                None
            } else {
                let pin = table.slots[idx].take();
                if pin.is_some() {
                    table.free.push(slot);
                }
                pin
            }
        };
        drop(pin);
    }

    /// Drop every still-live loaded-code pin owned by this execution (FAIL-15
    /// teardown). Drops pins outside the lock and leaves slot indices valid so
    /// any concurrent unregister safely observes `None`.
    pub(crate) fn release_pins(&self) {
        let released: Vec<crate::code::CodePin> = {
            let mut guard = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            let table = &mut *guard;
            let mut out = Vec::new();
            for (idx, slot) in table.slots.iter_mut().enumerate() {
                if let Some(pin) = slot.take() {
                    out.push(pin);
                    table.free.push(idx as u32);
                }
            }
            out
        };
        drop(released);
    }

    /// The hardware fault in loaded code that stopped this execution with `Code`, for
    /// the reporter to read after the pool has stopped (PACK-9).
    pub fn code_fault(&self) -> Option<crate::code::Fault> {
        self.code_fault.get()
    }

    pub const fn contained() -> Self {
        Self::new(true)
    }

    pub const fn standalone() -> Self {
        Self::new(false)
    }

    #[inline(always)]
    pub fn is_contained(&self) -> bool {
        self.contained
    }

    #[inline(always)]
    pub fn is_ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Async-signal-safe first-stop-wins publication. No locks, no allocation,
    /// no stdio. Returns true if this call was the winning first end.
    pub fn record_stop_bytes(&self, name: &[u8]) -> bool {
        let won = self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if won {
            let n = name.len().min(MAX_STOP_BYTES);
            let mut i = 0;
            while i < n {
                self.bytes[i].store(name[i], Ordering::Relaxed);
                i += 1;
            }
            self.len.store(n as u8, Ordering::Relaxed);
            self.state.store(2, Ordering::Release);
            self.ended.store(true, Ordering::Release);
            true
        } else {
            self.ended.store(true, Ordering::Release);
            false
        }
    }

    pub fn record_stop(&self, stop: Stop) -> bool {
        self.record_stop_bytes(stop.as_bytes())
    }

    /// Request an orderly host stop (sets `ended` so busy pure-Hirsel loops and
    /// peer workers leave promptly). Does not overwrite an earlier run stop.
    pub fn request_host_stop(&self) -> bool {
        let won = self
            .state
            .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        self.ended.store(true, Ordering::Release);
        crate::tasks::wake_all_waiters();
        crate::units::wake_all_for_stop();
        won
    }

    /// Read the winning end reason once `is_ended()` is true. Never called from
    /// a signal handler.
    pub fn end_reason(&self) -> Option<EndReason> {
        loop {
            match self.state.load(Ordering::Acquire) {
                0 => return None,
                1 => std::hint::spin_loop(),
                2 => {
                    let n = (self.len.load(Ordering::Acquire) as usize).min(MAX_STOP_BYTES);
                    let mut buf = [0u8; MAX_STOP_BYTES];
                    for (i, slot) in buf.iter_mut().take(n).enumerate() {
                        *slot = self.bytes[i].load(Ordering::Relaxed);
                    }
                    return Some(EndReason::RunStop(Stop { buf, len: n as u8 }));
                }
                _ => return Some(EndReason::HostStop),
            }
        }
    }

    pub fn stop(&self) -> Option<Stop> {
        match self.end_reason() {
            Some(EndReason::RunStop(stop)) => Some(stop),
            _ => None,
        }
    }
}

/// Classification of the active guarded stack at the fault boundary (EMB-3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StackKind {
    None = 0,
    /// Hirsel fiber stack: guard hits -> contained `Stack`; refused growth ->
    /// contained `HostMemory` + stack retirement.
    Recoverable = 1,
    /// Synchronous foreign thunk / host stack (`Stack::call`): guard hits or
    /// refused growth NEVER rewrite `ucontext`; forwarded to previous action.
    Native = 2,
}

impl StackKind {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Recoverable,
            2 => Self::Native,
            _ => Self::None,
        }
    }
}

/// Per-worker fault and stop descriptor stored in a process-static table so the
/// signal handler never touches Rust `thread_local!` on arbitrary host threads.
#[repr(align(64))]
pub(crate) struct WorkerFaultSlot {
    tid: AtomicUsize,
    pub(crate) base: AtomicUsize,
    pub(crate) guard_end: AtomicUsize,
    pub(crate) top: AtomicUsize,
    pub(crate) kind: AtomicU8,
    /// Pointer to the worker's `idle: *mut u8` stack pointer slot saved by
    /// `hirsel_stack::switch`. Null when not running a recoverable fiber.
    pub(crate) idle_ptr: AtomicPtr<*mut u8>,
    /// Sanitizer fiber metadata for returning to `idle_ptr` on abort/stop.
    idle_asan_bottom: AtomicPtr<u8>,
    idle_asan_size: AtomicUsize,
    idle_tsan_fiber: AtomicPtr<u8>,
    /// Active generation end slot for this thread, or null outside a run.
    pub(crate) generation: AtomicPtr<Execution>,
}

impl WorkerFaultSlot {
    const fn empty() -> Self {
        Self {
            tid: AtomicUsize::new(0),
            base: AtomicUsize::new(0),
            guard_end: AtomicUsize::new(0),
            top: AtomicUsize::new(0),
            kind: AtomicU8::new(StackKind::None as u8),
            idle_ptr: AtomicPtr::new(ptr::null_mut()),
            idle_asan_bottom: AtomicPtr::new(ptr::null_mut()),
            idle_asan_size: AtomicUsize::new(0),
            idle_tsan_fiber: AtomicPtr::new(ptr::null_mut()),
            generation: AtomicPtr::new(ptr::null_mut()),
        }
    }

    #[inline(always)]
    fn get_idle_sanitizer(
        &self,
    ) -> (
        hirsel_stack::sanitizer::StackBounds,
        hirsel_stack::sanitizer::TsanFiber,
    ) {
        let bottom = self.idle_asan_bottom.load(Ordering::Relaxed);
        let size = self.idle_asan_size.load(Ordering::Relaxed);
        let tsan = self.idle_tsan_fiber.load(Ordering::Relaxed);
        (
            hirsel_stack::sanitizer::StackBounds::from_raw(bottom, size),
            hirsel_stack::sanitizer::TsanFiber::from_raw(tsan),
        )
    }

    #[inline(always)]
    pub(crate) fn set_guard(&self, base: usize, guard_end: usize, top: usize, kind: StackKind) {
        // Clear kind first so a signal mid-update never sees mismatched bounds.
        self.kind.store(StackKind::None as u8, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
        self.base.store(base, Ordering::Relaxed);
        self.guard_end.store(guard_end, Ordering::Relaxed);
        self.top.store(top, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
        self.kind.store(kind as u8, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
    }

    #[inline(always)]
    pub(crate) fn get_guard(&self) -> (usize, usize, usize, StackKind) {
        let kind = StackKind::from_u8(self.kind.load(Ordering::Relaxed));
        if kind == StackKind::None {
            return (0, 0, 0, StackKind::None);
        }
        (
            self.base.load(Ordering::Relaxed),
            self.guard_end.load(Ordering::Relaxed),
            self.top.load(Ordering::Relaxed),
            kind,
        )
    }
}

const MAX_SLOTS: usize = 1024;
static SLOTS: [WorkerFaultSlot; MAX_SLOTS] = [const { WorkerFaultSlot::empty() }; MAX_SLOTS];
static ACTIVE_SLOTS: AtomicUsize = AtomicUsize::new(0);
static HIGH_WATER: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
fn current_tid() -> usize {
    // SAFETY: pthread_self is async-signal-safe and returns a non-zero thread id.
    let raw = unsafe { libc::pthread_self() as usize };
    // Guard against theoretical zero tid.
    if raw == 0 { 1 } else { raw }
}

/// Async-signal-safe lookup of the current thread's fault slot. Returns `None`
/// on host threads that have never registered a Hirsel stack or generation.
#[inline]
pub(crate) fn find_current_slot_signal_safe() -> Option<&'static WorkerFaultSlot> {
    if ACTIVE_SLOTS.load(Ordering::Acquire) == 0 {
        return None;
    }
    let tid = current_tid();
    let limit = HIGH_WATER.load(Ordering::Acquire).min(MAX_SLOTS);
    let mut i = 0;
    while i < limit {
        if SLOTS[i].tid.load(Ordering::Acquire) == tid {
            return Some(&SLOTS[i]);
        }
        i += 1;
    }
    None
}

// Per-thread cached index into SLOTS (or usize::MAX when unregistered).
// `moved_tls!` ensures no address is cached across a unit stack switch.
crate::units::moved_tls! { static SLOT_INDEX: usize = usize::MAX; }
crate::units::moved_tls! { static SLOT_DEPTH: usize = 0; }

struct SlotThreadCleanup;
impl Drop for SlotThreadCleanup {
    fn drop(&mut self) {
        let idx = SLOT_INDEX.get();
        if idx != usize::MAX {
            let slot = &SLOTS[idx];
            slot.set_guard(0, 0, 0, StackKind::None);
            slot.idle_ptr.store(ptr::null_mut(), Ordering::Relaxed);
            slot.generation.store(ptr::null_mut(), Ordering::Release);
            SLOT_DEPTH.set(0);
            SLOT_INDEX.set(usize::MAX);
            slot.tid.store(0, Ordering::Release);
            ACTIVE_SLOTS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

thread_local! {
    static SLOT_CLEANUP: SlotThreadCleanup = const { SlotThreadCleanup };
}

fn acquire_slot() -> usize {
    let existing = SLOT_INDEX.get();
    if existing != usize::MAX {
        return existing;
    }
    let _ = SLOT_CLEANUP.try_with(|_| ());
    let tid = current_tid();
    // Reclaim any stale slot left behind by an exited thread whose pthread_t
    // was recycled by the OS (SLOT_INDEX is usize::MAX on this live thread).
    let limit = HIGH_WATER.load(Ordering::Acquire).min(MAX_SLOTS);
    for (i, slot) in SLOTS[..limit].iter().enumerate() {
        if slot.tid.load(Ordering::Acquire) == tid {
            slot.set_guard(0, 0, 0, StackKind::None);
            slot.idle_ptr.store(ptr::null_mut(), Ordering::Relaxed);
            slot.generation.store(ptr::null_mut(), Ordering::Release);
            SLOT_INDEX.set(i);
            return i;
        }
    }
    for (i, slot) in SLOTS.iter().enumerate() {
        if slot
            .tid
            .compare_exchange(0, tid, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            HIGH_WATER.fetch_max(i + 1, Ordering::AcqRel);
            ACTIVE_SLOTS.fetch_add(1, Ordering::AcqRel);
            SLOT_INDEX.set(i);
            return i;
        }
    }
    panic!("hirsel-core-rt: exceeded MAX_SLOTS ({MAX_SLOTS}) concurrent worker threads");
}

fn release_slot_if_idle() {
    if SLOT_DEPTH.get() != 0 {
        return;
    }
    let idx = SLOT_INDEX.get();
    if idx == usize::MAX {
        return;
    }
    let slot = &SLOTS[idx];
    if slot.kind.load(Ordering::Relaxed) != StackKind::None as u8
        || !slot.generation.load(Ordering::Relaxed).is_null()
        || !slot.idle_ptr.load(Ordering::Relaxed).is_null()
    {
        return;
    }
    SLOT_INDEX.set(usize::MAX);
    slot.tid.store(0, Ordering::Release);
    ACTIVE_SLOTS.fetch_sub(1, Ordering::AcqRel);
}

#[inline(always)]
pub(crate) fn current_slot_or_acquire() -> &'static WorkerFaultSlot {
    &SLOTS[acquire_slot()]
}

#[inline(always)]
pub(crate) fn current_slot() -> Option<&'static WorkerFaultSlot> {
    let idx = SLOT_INDEX.get();
    (idx != usize::MAX).then(|| &SLOTS[idx])
}

#[inline(always)]
pub(crate) fn current_slot_here() -> Option<&'static WorkerFaultSlot> {
    let idx = SLOT_INDEX.get_here();
    (idx != usize::MAX).then(|| &SLOTS[idx])
}

pub(crate) fn maybe_release_slot() {
    release_slot_if_idle();
}

/// Scoped attachment of a [`Execution`] to the current thread. Restores the
/// previous generation pointer on drop and releases the fault slot if idle.
pub struct ExecutionScope {
    slot_idx: usize,
    previous: *mut Execution,
}

impl ExecutionScope {
    /// # Safety
    /// `generation` must outlive this scope.
    pub unsafe fn enter(generation: &Execution) -> Self {
        let slot_idx = acquire_slot();
        SLOT_DEPTH.set(SLOT_DEPTH.get() + 1);
        let slot = &SLOTS[slot_idx];
        let previous = slot
            .generation
            .swap(ptr::from_ref(generation).cast_mut(), Ordering::AcqRel);
        Self { slot_idx, previous }
    }
}

impl Drop for ExecutionScope {
    fn drop(&mut self) {
        let slot = &SLOTS[self.slot_idx];
        slot.generation.store(self.previous, Ordering::Release);
        SLOT_DEPTH.set(SLOT_DEPTH.get() - 1);
        release_slot_if_idle();
    }
}

/// Set or clear the current fiber's worker-host resume pointer (`&mut run.idle`).
#[inline(always)]
pub(crate) fn set_idle_ptr(idle: *const *mut u8) -> *mut *mut u8 {
    if idle.is_null() {
        if let Some(slot) = current_slot() {
            let prev = slot.idle_ptr.swap(ptr::null_mut(), Ordering::Relaxed);
            compiler_fence(Ordering::SeqCst);
            release_slot_if_idle();
            return prev;
        }
        return ptr::null_mut();
    }
    let slot = current_slot_or_acquire();
    let prev = slot.idle_ptr.swap(idle.cast_mut(), Ordering::Relaxed);
    compiler_fence(Ordering::SeqCst);
    prev
}

/// Record the active scheduler's ASan stack bounds and TSan fiber handle on
/// the current worker slot so `raise`/`leave_on_ended` annotate their exit switch.
#[inline(always)]
pub(crate) fn set_idle_sanitizer(
    asan: hirsel_stack::sanitizer::StackBounds,
    tsan: hirsel_stack::sanitizer::TsanFiber,
) {
    if let Some(slot) = current_slot() {
        let (bottom, size) = asan.into_raw();
        slot.idle_asan_bottom
            .store(bottom.cast_mut(), Ordering::Relaxed);
        slot.idle_asan_size.store(size, Ordering::Relaxed);
        slot.idle_tsan_fiber
            .store(tsan.into_raw(), Ordering::Relaxed);
    }
}

/// Active generation pointer on this thread, if any.
#[inline(always)]
pub(crate) fn current_execution() -> Option<&'static Execution> {
    let slot = current_slot()?;
    let ptr = slot.generation.load(Ordering::Acquire);
    // SAFETY: ExecutionScope pins the Execution for its entire scope.
    (!ptr.is_null()).then(|| unsafe { &*ptr })
}

/// True when the current thread runs inside a contained (embedded) generation.
#[inline(always)]
pub fn is_contained() -> bool {
    current_execution().is_some_and(Execution::is_contained)
}

/// True when the current thread's generation has recorded a run stop or host stop.
#[inline(always)]
pub fn is_ended() -> bool {
    current_execution().is_some_and(Execution::is_ended)
}

/// Record a run stop on the active generation (if any) and wake peer workers.
/// Does not diverge. Used on host-stack callback boundaries that return to the
/// scheduler loop which checks [`is_ended`].
#[cold]
#[inline(never)]
pub fn record(stop: Stop) {
    if let Some(generation) = current_execution() {
        generation.record_stop(stop);
        crate::tasks::wake_all_waiters();
        crate::units::wake_all_for_stop();
    } else {
        crate::process::uncontained_stop(stop);
    }
}

/// Record `stop` and leave the current fiber back to its worker's host stack
/// (`idle`). Caller MUST hold zero locks.
///
/// If no fiber is active and no generation is active (e.g. a standalone unit
/// test calling an internal runtime helper directly), delegates to the single
/// standalone stop exit in `process.rs`.
#[cold]
#[inline(never)]
pub fn raise(stop: Stop) -> ! {
    if let Some(slot) = current_slot() {
        let generation = slot.generation.load(Ordering::Acquire);
        if !generation.is_null() {
            // SAFETY: ExecutionScope pins `generation` for this thread's run.
            unsafe { (*generation).record_stop(stop) };
        }
        let idle_ptr = slot.idle_ptr.load(Ordering::Relaxed);
        if !idle_ptr.is_null() {
            // Clear guard before leaving the fiber stack.
            slot.set_guard(0, 0, 0, StackKind::None);
            crate::tasks::wake_all_waiters();
            crate::units::wake_all_for_stop();
            // SAFETY: `idle_ptr` points to the worker's saved host stack slot
            // written by `hirsel_stack::switch` when entering this fiber.
            unsafe {
                let target = *idle_ptr;
                if !target.is_null() {
                    let (asan, tsan) = slot.get_idle_sanitizer();
                    hirsel_stack::sanitizer::asan_start_switch_exit(asan);
                    tsan.switch_to();
                    hirsel_stack::switch_abandon(target);
                }
            }
        }
    }
    crate::process::uncontained_stop(stop)
}

/// Leave the current fiber immediately after observing `is_ended()` (e.g., from
/// `hirsel_core_step_out` in a busy pure-Hirsel peer worker).
#[cold]
#[inline(never)]
pub(crate) fn leave_on_ended() -> ! {
    if let Some(slot) = current_slot() {
        let idle_ptr = slot.idle_ptr.load(Ordering::Relaxed);
        if !idle_ptr.is_null() {
            slot.set_guard(0, 0, 0, StackKind::None);
            // SAFETY: `idle_ptr` points to the worker's saved host stack slot.
            unsafe {
                let target = *idle_ptr;
                if !target.is_null() {
                    let (asan, tsan) = slot.get_idle_sanitizer();
                    hirsel_stack::sanitizer::asan_start_switch_exit(asan);
                    tsan.switch_to();
                    hirsel_stack::switch_abandon(target);
                }
            }
        }
    }
    // Outside a fiber, there is no fiber to abandon; fall back to recording.
    if let Some(stop) = current_execution().and_then(Execution::stop) {
        crate::process::uncontained_stop(stop)
    }
    crate::process::uncontained_stop(Stop::from_static(b"Stopped"))
}

/// Run `entry` inside a contained generation on a dedicated runtime worker
/// thread (never touching the calling host thread's signal stack or the
/// process-wide `SIGPIPE` disposition).
///
/// # Safety
/// `config` and `entry` must remain live until this function returns.
pub unsafe fn run_contained_worker(
    generation: &Execution,
    config: &crate::process::Config,
    entry: crate::process::Entry,
) -> Result<u8, EndReason> {
    if let Err(err) = crate::fiber::arm_embedded() {
        let name = match err {
            crate::fiber::ArmError::SignalStack => b"SignalStack".as_slice(),
            crate::fiber::ArmError::FaultHandlerReplaced => b"FaultHandlerReplaced".as_slice(),
        };
        let stop = Stop::from_bytes(name);
        generation.record_stop(stop);
        return Err(EndReason::RunStop(stop));
    }
    // SAFETY: worker thread is joined before this function returns, so `config`
    // and `generation` outlive the worker thread.
    struct SendPtr<T>(*const T);
    unsafe impl<T> Send for SendPtr<T> {}
    let cfg_ptr = SendPtr(config as *const _);
    let gen_ptr = SendPtr(generation as *const _);
    let handle = std::thread::Builder::new()
        .name("hirsel-embed-worker".into())
        .spawn(move || {
            let cfg_ptr = cfg_ptr;
            let gen_ptr = gen_ptr;
            // SAFETY: caller pins generation and config until join completes.
            unsafe {
                let gen_ref = &*gen_ptr.0;
                let _scope = ExecutionScope::enter(gen_ref);
                let _alt = match crate::fiber::Alternate::install() {
                    Ok(alt) => alt,
                    Err(stop) => {
                        gen_ref.record_stop(stop);
                        return Err(EndReason::RunStop(stop));
                    }
                };
                crate::process::run_config_contained(&*cfg_ptr.0, entry, gen_ref)
            }
        });
    match handle {
        Ok(join) => match join.join() {
            Ok(result) => result,
            Err(_) => {
                let stop = Stop::from_static(b"WorkerPanic");
                generation.record_stop(stop);
                Err(EndReason::RunStop(stop))
            }
        },
        Err(_) => {
            let stop = Stop::from_static(b"HostThreads");
            generation.record_stop(stop);
            Err(EndReason::RunStop(stop))
        }
    }
}
