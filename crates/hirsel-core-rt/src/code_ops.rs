//! PACK-14 runtime handle, slot, worker-borrow, and continuation/Unit pin hooks.
//!
//! `hirsel-core-rt` does not link `hirsel-code`; a pack host (`hirsel-pack`)
//! registers its [`CodeOps`] implementation here. `code.bind(f)` of an entry outside
//! every pool's text band gives a bit-0 tagged descriptor pointer (`ptr | 1`) with
//! process-lifetime backing, so it works in standalone binaries without a pack host.

use super::custody::Owner;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Type-erased owning pin on a loaded image generation (`hirsel_code::CodePin`).
/// Stored on suspended [`crate::tasks::Task`] continuations and spawned Units so
/// code generations remain live across suspension, worker migration, and Unit end.
pub struct CodePin {
    data: *const (),
    drop_fn: Option<unsafe fn(*const ())>,
}

// SAFETY: `hirsel_code::CodePin` wraps `Option<Arc<Image>>`, which is `Send + Sync`.
unsafe impl Send for CodePin {}
// SAFETY: as above.
unsafe impl Sync for CodePin {}

impl CodePin {
    /// Create a pin from a raw pointer and its destructor callback.
    ///
    /// # Safety
    /// `drop_fn` must be safe to call at most once on `data` from any thread.
    pub const unsafe fn from_raw(data: *const (), drop_fn: unsafe fn(*const ())) -> Self {
        Self {
            data,
            drop_fn: Some(drop_fn),
        }
    }

    pub const fn empty() -> Self {
        Self {
            data: ptr::null(),
            drop_fn: None,
        }
    }
}

impl Drop for CodePin {
    fn drop(&mut self) {
        if let Some(drop_fn) = self.drop_fn.take() {
            // SAFETY: constructor guarantees `drop_fn(data)` is valid once.
            unsafe { drop_fn(self.data) };
        }
    }
}

/// Host loader operations registered by `hirsel-pack`.
pub struct CodeOps {
    pub load_by_key: unsafe fn(
        loader: u64,
        key: *const [u8; 32],
        entry_id: *const [u8; 32],
        sig: *const [u8; 32],
        waits: u8,
        out_code: *mut *mut u8,
        out_word: *mut u64,
        out_bytes: *mut u8,
    ) -> u8,
    pub code_share: unsafe fn(code: *mut u8) -> *mut u8,
    pub code_drop: unsafe fn(code: *mut u8),
    pub slot_new: unsafe fn(loader: u64) -> *mut u8,
    pub slot_drop: unsafe fn(slot: *mut u8),
    pub slot_activate:
        unsafe fn(slot: *mut u8, expected: u64, code: *mut u8, out_version: *mut u64) -> u8,
    pub dispatch_begin: unsafe fn(slot: *mut u8, task: usize, out_token: *mut u64) -> *mut u8,
    pub dispatch_end: unsafe fn(token: u64) -> bool,
    pub promote_borrows: unsafe fn(task: usize, out: &mut Vec<(u64, CodePin)>),
    pub worker_park: unsafe fn(),
}

// SAFETY: function pointers in `CodeOps` are immutable and thread-safe.
unsafe impl Sync for CodeOps {}

static CODE_OPS: AtomicPtr<CodeOps> = AtomicPtr::new(ptr::null_mut());

/// Register the pack host's [`CodeOps`] table once per process.
pub fn register_ops(ops: &'static CodeOps) {
    let _ = CODE_OPS.compare_exchange(
        ptr::null_mut(),
        ptr::from_ref(ops).cast_mut(),
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

#[inline(always)]
pub(super) fn ops() -> Option<&'static CodeOps> {
    let p = CODE_OPS.load(Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        // SAFETY: registered `CodeOps` is `'static`.
        Some(unsafe { &*p })
    }
}

/// Pin the live image owning `pc`. A `pc` outside every pool's text band costs the
/// band comparisons alone, and one inside a band takes no lock.
pub(crate) fn pin_pc(pc: usize) -> Option<CodePin> {
    let band = super::text(pc)?;
    // SAFETY: `pin` takes the loader it was registered with.
    unsafe { (band.pin)(band.loader, pc) }
}

/// Execution-scoped custody handle for a [`CodePin`] (PACK-16 / FAIL-15).
///
/// Normal drop releases the underlying pin immediately from any task in the
/// execution (including after cross-task `Held[send]` transfer). When the
/// execution stops abnormally (skipping Hirsel cleanup per FAIL-15),
/// the execution owns the remaining slots until generation teardown.
pub(crate) struct ExecutionPin {
    execution: *const crate::run_stop::Execution,
    slot: u32,
    fallback: Option<CodePin>,
}

// SAFETY: `Execution` outlives every task and unit of its execution (PACK-13)
// and synchronizes pin slots under its mutex; `CodePin` is `Send + Sync`.
unsafe impl Send for ExecutionPin {}
// SAFETY: as above.
unsafe impl Sync for ExecutionPin {}

impl ExecutionPin {
    pub(crate) fn recipe(
        ctx: &crate::Context,
        recipe: Option<crate::counted::Recipe>,
    ) -> Option<Self> {
        recipe.and_then(|f| Self::pin_pc(Some(ctx), f as usize))
    }

    pub(crate) fn pin_pc(ctx: Option<&crate::Context>, pc: usize) -> Option<Self> {
        let pin = pin_pc(pc)?;
        let execution = ctx
            .map(|c| c.execution)
            .filter(|p| !p.is_null())
            .or_else(|| crate::run_stop::current_execution().map(ptr::from_ref));
        if let Some(execution) = execution {
            // SAFETY: `Execution` outlives every task running under it.
            let slot = unsafe { (*execution).register_pin(pin) };
            Some(Self {
                execution,
                slot,
                fallback: None,
            })
        } else {
            Some(Self {
                execution: ptr::null(),
                slot: 0,
                fallback: Some(pin),
            })
        }
    }
}

impl Drop for ExecutionPin {
    fn drop(&mut self) {
        if !self.execution.is_null() {
            // SAFETY: `Execution` outlives every value dropped during the run.
            unsafe { (*self.execution).unregister_pin(self.slot) };
        } else {
            drop(self.fallback.take());
        }
    }
}

/// Promote all active worker borrows belonging to `task` into owning [`CodePin`]s
/// on `task` before the task leaves worker custody (suspension or migration).
#[inline]
pub(crate) unsafe fn promote_worker_borrows(task: *mut crate::tasks::Task) {
    if task.is_null() {
        return;
    }
    let Some(ops) = ops() else {
        return;
    };
    // SAFETY: caller owns `task` during the switch off the worker.
    unsafe {
        (ops.promote_borrows)(task.addr(), &mut (*task).code_pins);
    }
}

/// Release stale unborrowed rows in the current worker's private dispatch table.
#[inline]
pub fn worker_park() {
    if let Some(ops) = ops() {
        // SAFETY: registered pack host callback.
        unsafe { (ops.worker_park)() };
    }
}

/// LoadError::Unsupported tag (index 10 + 1 = 11 in core/code.hirsel).
const LOAD_UNSUPPORTED: u8 = 11;
/// ActivateError::Capacity tag (3 in core/code.hirsel).
const ACTIVATE_CAPACITY: u8 = 3;

/// `code.bind(f)`: produce a `Code[F]` handle for the PROG-9 descriptor `descriptor`.
/// An entry outside every pool's text band is statically linked and gets
/// process-lifetime backing (`descriptor | 1`). An entry inside one gets a handle that
/// owns a pin on its image. An entry inside a band that no live image holds is the
/// text of an unloaded image, which only code that outlived its image can name: it
/// gets no handle, and the process stops.
///
/// # Safety
/// `descriptor` points to a valid PROG-9 descriptor `{code, length, revision, id...}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_bind(descriptor: *const u8) -> *mut u8 {
    if descriptor.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: descriptor word 0 is the PACK-15 adapter entry address.
    let entry = unsafe { descriptor.cast::<usize>().read() };
    let Some(band) = super::text(entry) else {
        return (descriptor.addr() | 1) as *mut u8;
    };
    // SAFETY: `bind` takes the loader it was registered with.
    let handle = unsafe { (band.bind)(band.loader, entry, descriptor.addr()) };
    if handle.is_null() {
        eprintln!("hirsel: code.bind named the entry {entry:#x} of an unloaded image");
        std::process::abort();
    }
    Owner::Code.acquire(handle);
    handle
}

/// `code.load[F](loader, key, entry_id)`: validate and load a typed entry.
///
/// # Safety
/// Pointer arguments must be valid for their respective byte counts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_load(
    loader: u64,
    key: *const u8,
    entry_id: *const u8,
    sig: *const u8,
    waits: u8,
    out_code: *mut *mut u8,
    out_word: *mut u64,
    out_bytes: *mut u8,
) -> u8 {
    let Some(ops) = ops() else {
        return LOAD_UNSUPPORTED;
    };
    // SAFETY: caller provides 32-byte key/entry_id/sig buffers and output pointers.
    unsafe {
        let status = (ops.load_by_key)(
            loader,
            key.cast::<[u8; 32]>(),
            entry_id.cast::<[u8; 32]>(),
            sig.cast::<[u8; 32]>(),
            waits,
            out_code,
            out_word,
            out_bytes,
        );
        if status == 0 {
            Owner::Code.acquire(*out_code);
        }
        status
    }
}

/// `code.share(c)`: retain one more counted `Code[F]` handle.
///
/// # Safety
/// `code` is a live `Code[F]` handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_share(code: *mut u8) -> *mut u8 {
    if code.is_null() || (code.addr() & 1 != 0) {
        return code;
    }
    match ops() {
        // SAFETY: `code` is a live dynamic handle.
        Some(ops) => {
            let shared = unsafe { (ops.code_share)(code) };
            Owner::Code.acquire(shared);
            shared
        }
        None => code,
    }
}

/// Drop one `Code[F]` handle.
///
/// # Safety
/// `code` is an owned `Code[F]` handle or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_drop(code: *mut u8) {
    if code.is_null() || (code.addr() & 1 != 0) {
        return;
    }
    if let Some(ops) = ops() {
        // SAFETY: `code` is an owned dynamic handle.
        Owner::Code.transfer(code);
        unsafe { (ops.code_drop)(code) };
    }
}

/// `code.slot[F](loader)`: create an empty version-0 `CodeSlot[F]`.
///
/// # Safety
/// `loader` is a host-issued `Loader` capability handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_slot_new(loader: u64) -> *mut u8 {
    match ops() {
        // SAFETY: registered pack host callback.
        Some(ops) => {
            let slot = unsafe { (ops.slot_new)(loader) };
            Owner::Slot.acquire(slot);
            slot
        }
        None => ptr::null_mut(),
    }
}

/// Drop one `CodeSlot[F]` handle.
///
/// # Safety
/// `slot` is an owned `CodeSlot[F]` handle or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_slot_drop(slot: *mut u8) {
    if slot.is_null() {
        return;
    }
    if let Some(ops) = ops() {
        // SAFETY: `slot` is an owned slot handle.
        Owner::Slot.transfer(slot);
        unsafe { (ops.slot_drop)(slot) };
    }
}

/// `code.activate(mut s, expected, c)`: linearised slot update.
///
/// # Safety
/// `slot`, `code`, and `out_version` are valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_activate(
    slot: *mut u8,
    expected: u64,
    code: *mut u8,
    out_version: *mut u64,
) -> u8 {
    let Some(ops) = ops() else {
        return ACTIVATE_CAPACITY;
    };
    if slot.is_null() || code.is_null() {
        return ACTIVATE_CAPACITY;
    }
    // SAFETY: registered pack host callback.
    unsafe { (ops.slot_activate)(slot, expected, code, out_version) }
}

/// Begin `code.dispatch(slot, ...)`: borrow worker-cached generation, or return null if empty.
///
/// # Safety
/// `slot` is a live `CodeSlot[F]` handle and `out_token` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_dispatch_begin(
    slot: *mut u8,
    out_token: *mut u64,
) -> *mut u8 {
    if slot.is_null() {
        return ptr::null_mut();
    }
    let Some(ops) = ops() else {
        return ptr::null_mut();
    };
    let task = crate::tasks::current_task().addr();
    // SAFETY: registered pack host callback.
    unsafe { (ops.dispatch_begin)(slot, task, out_token) }
}

/// End `code.dispatch(slot, ...)`: release worker borrow or promoted continuation pin.
///
/// # Safety
/// `token` was produced by a matching `hirsel_core_code_dispatch_begin`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_dispatch_end(token: u64) {
    let Some(ops) = ops() else {
        return;
    };
    // Fast path: release active borrow on current worker.
    // SAFETY: registered pack host callback.
    if unsafe { (ops.dispatch_end)(token) } {
        return;
    }
    // Promoted path: the task suspended or migrated; drop the owning pin on `Task`.
    let task = crate::tasks::current_task();
    if !task.is_null() {
        // SAFETY: `task` is the currently running task on this thread.
        let pins = unsafe { &mut (*task).code_pins };
        if let Some(pos) = pins.iter().rposition(|(id, _)| *id == token) {
            pins.swap_remove(pos);
        }
    }
}

#[inline]
unsafe fn descriptor_of(code: *mut u8) -> *const u8 {
    if code.is_null() {
        return ptr::null();
    }
    if code.addr() & 1 != 0 {
        (code.addr() & !1) as *const u8
    } else {
        // Dynamic CodeHeader: offset 0 = entry, offset 8 = descriptor.
        // SAFETY: live dynamic `CodeHeader` has `descriptor` at word 1.
        unsafe { code.cast::<usize>().add(1).read() as *const u8 }
    }
}

/// `code.identity(c)`: write `{ptr, len, cap=0}` Bytes carrier anchored in `c`.
///
/// # Safety
/// `code` is live and `out_bytes` points to a 24-byte aligned `Bytes` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_identity(code: *mut u8, out_bytes: *mut u8) {
    // SAFETY: `code` is a live handle and `out_bytes` is a 24-byte `Bytes` carrier.
    unsafe {
        let desc = descriptor_of(code);
        let out = out_bytes.cast::<[u64; 3]>();
        if desc.is_null() {
            out.write([0, 0, 0]);
            return;
        }
        let len = desc.cast::<u64>().add(1).read();
        let data = if len == 0 {
            0
        } else {
            desc.add(24).addr() as u64
        };
        out.write([data, len, 0]);
    }
}

/// `code.revision(c)`: write `{ptr, len=32, cap=0}` and return 1 if revision is
/// present, else return 0 (`None`).
///
/// # Safety
/// `code` is live and `out_bytes` points to a 24-byte aligned `Bytes` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_code_revision(code: *mut u8, out_bytes: *mut u8) -> u8 {
    // SAFETY: `code` is a live handle and `out_bytes` is a 24-byte `Bytes` carrier.
    unsafe {
        let desc = descriptor_of(code);
        if desc.is_null() {
            return 0;
        }
        let rev_ptr = desc.cast::<*const u64>().add(2).read();
        if rev_ptr.is_null() {
            return 0;
        }
        let data = rev_ptr.read();
        let len = rev_ptr.add(1).read();
        if len == 0 {
            return 0;
        }
        out_bytes.cast::<[u64; 3]>().write([data, len, 0]);
        1
    }
}
