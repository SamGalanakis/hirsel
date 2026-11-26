//! FFI-12 to FFI-14 on Linux: the contained engine, its null imports and
//! the calling task's instances.
use crate::abi_layout as abi;
use crate::foreign::contained::{Call, Module};
use crate::foreign::handles::{Argument, Operands};
use crate::limit::Payer;
use crate::{Capacity, Context, causes, steps, tasks};
use hirsel_core_contained::load::{ImageEngine, is_host_memory};
use hirsel_core_contained::wasmtime::{
    Caller, Error, Extern, Instance, InstancePre, Linker, Memory, ResourceLimiter, Store, Trap, Val,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::Poll;

/// The calling task's live instances, one per contained module.
#[derive(Default)]
pub(crate) struct Instances(Vec<(usize, Box<Live>)>);

impl Instances {
    fn take(&mut self, key: usize) -> Option<Box<Live>> {
        let at = self.0.iter().position(|(k, _)| *k == key)?;
        Some(self.0.swap_remove(at).1)
    }
}

pub(crate) struct Live {
    store: Store<State>,
    instance: Instance,
    memory: Memory,
    /// This instance's generation, unique in the process.
    id: u64,
}

/// `proc_exit(c)` ends the call as the trap `exit c` (FFI-12).
#[derive(Debug)]
struct Exit(u32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

/// A store's data: the bytes its instance holds against the program's
/// limits (FFI-13 Bytes). Dropping the store refunds them.
#[derive(Default)]
pub(crate) struct State {
    /// The payer current at the running entry.
    payer: Option<Payer>,
    charges: Vec<(Payer, u64)>,
    /// The last refused admission of the running entry.
    refused: Option<Capacity>,
}

// SAFETY: a store moves only with its task, which owns every payer it names.
unsafe impl Send for State {}

impl State {
    /// Admit `bytes`. A refusal answers the guest as WebAssembly's own
    /// failure does: `memory.grow` and `table.grow` return −1.
    fn admit(&mut self, bytes: u64) -> bool {
        if bytes == 0 {
            return true;
        }
        let payer = self.payer.expect("an entry sets its payer");
        match payer.admit(Some(bytes)) {
            Ok(()) => {
                match self.charges.last_mut() {
                    Some((last, held)) if *last == payer => *held += bytes,
                    _ => self.charges.push((payer, bytes)),
                }
                true
            }
            Err(capacity) => {
                self.refused = Some(capacity);
                false
            }
        }
    }
}

/// Growth past the declared maximum fails as WebAssembly says, before any
/// admission, so it is no refusal of the limit.
impl ResourceLimiter for State {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, Error> {
        Ok(maximum.is_none_or(|maximum| desired <= maximum)
            && self.admit((desired - current) as u64))
    }

    /// FFI-13 Bytes / FFI-14 Determinism: once `memory_growing` admitted the
    /// growth, a host refusal of the pages (`mprotect`/`mmap` `ENOMEM` or
    /// `OutOfMemory`) stops the run as `HostMemory`, never returning a
    /// nondeterministic −1 to the guest.
    fn memory_grow_failed(&mut self, error: Error) -> Result<(), Error> {
        if is_host_memory(&error) {
            crate::process::stop(b"HostMemory");
        }
        Ok(())
    }

    /// Eight bytes per table element.
    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, Error> {
        Ok(maximum.is_none_or(|maximum| desired <= maximum)
            && self.admit(((desired - current) as u64).saturating_mul(8)))
    }

    fn table_grow_failed(&mut self, error: Error) -> Result<(), Error> {
        if is_host_memory(&error) {
            crate::process::stop(b"HostMemory");
        }
        Ok(())
    }
}

fn unwrap_host<T>(result: Result<T, Error>) -> T {
    result.unwrap_or_else(|error| {
        if is_host_memory(&error) {
            crate::process::stop(b"HostMemory");
        }
        std::process::abort()
    })
}

impl Drop for State {
    fn drop(&mut self) {
        for (payer, bytes) in self.charges.drain(..) {
            payer.refund(bytes);
        }
    }
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn engine() -> &'static ImageEngine {
    static ENGINE: OnceLock<ImageEngine> = OnceLock::new();
    // The runtime's own signal handlers are installed before any task
    // runs, so Wasmtime's chain to them (FFI-15 Signals).
    ENGINE.get_or_init(|| unwrap_host(ImageEngine::new()))
}

const WASI: &str = "wasi_snapshot_preview1";

/// FFI-12's null imports, each with its preview-1 core type.
fn linker() -> &'static Linker<State> {
    static LINKER: OnceLock<Linker<State>> = OnceLock::new();
    LINKER.get_or_init(|| {
        let mut linker = Linker::new(engine().engine());
        unwrap_host(define(&mut linker));
        linker
    })
}

fn define(linker: &mut Linker<State>) -> Result<(), Error> {
    const EBADF: i32 = 8;
    const ENOSYS: i32 = 52;
    for name in ["args_sizes_get", "environ_sizes_get"] {
        linker.func_wrap(WASI, name, sizes)?;
    }
    for name in ["args_get", "environ_get"] {
        linker.func_wrap(WASI, name, |_: i32, _: i32| 0i32)?;
    }
    for name in ["fd_fdstat_get", "fd_prestat_get"] {
        linker.func_wrap(WASI, name, |_: i32, _: i32| EBADF)?;
    }
    for name in ["fd_read", "fd_write"] {
        linker.func_wrap(WASI, name, |_: i32, _: i32, _: i32, _: i32| EBADF)?;
    }
    linker.func_wrap(WASI, "fd_close", |_: i32| EBADF)?;
    linker.func_wrap(WASI, "fd_seek", |_: i32, _: i64, _: i32, _: i32| EBADF)?;
    linker.func_wrap(WASI, "fd_prestat_dir_name", |_: i32, _: i32, _: i32| EBADF)?;
    for name in ["clock_res_get", "random_get"] {
        linker.func_wrap(WASI, name, |_: i32, _: i32| ENOSYS)?;
    }
    linker.func_wrap(WASI, "clock_time_get", |_: i32, _: i64, _: i32| ENOSYS)?;
    linker.func_wrap(WASI, "poll_oneoff", |_: i32, _: i32, _: i32, _: i32| ENOSYS)?;
    linker.func_wrap(WASI, "sched_yield", || 0i32)?;
    linker.func_wrap(WASI, "proc_exit", |code: i32| -> Result<(), Error> {
        Err(Error::new(Exit(code as u32)))
    })?;
    Ok(())
}

/// `*_sizes_get` writes two zero words, each bounds-checked as a guest store.
fn sizes(mut caller: Caller<'_, State>, count: i32, size: i32) -> Result<i32, Error> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .expect("an admitted module exports its memory");
    for at in [count, size] {
        memory
            .write(&mut caller, at as u32 as usize, &[0; 4])
            .map_err(|_| Error::from(Trap::MemoryOutOfBounds))?;
    }
    Ok(0)
}

fn prepared(module: &'static Module) -> InstancePre<State> {
    static PREPARED: OnceLock<Mutex<HashMap<usize, InstancePre<State>>>> = OnceLock::new();
    let mut prepared = PREPARED.get_or_init(Default::default).lock().unwrap();
    prepared
        .entry(std::ptr::from_ref(module) as usize)
        .or_insert_with(|| {
            // SAFETY: the linker placed the unmodified artifact at a 64 KiB
            // boundary of the image's read-execute segment (FFI-15).
            let artifact = unsafe { std::slice::from_raw_parts(module.artifact, module.len) };
            let loaded = unwrap_host(unsafe { engine().load(artifact) });
            unwrap_host(linker().instantiate_pre(&loaded))
        })
        .clone()
}

/// FFI-13's granule: 16,384 fuel, 1,024 steps.
const GRANULE: u64 = 16_384;
const FUEL_PER_STEP: u64 = 16;

/// How an entry ended without a value.
enum Stop {
    /// A trap, charged its steps (FFI-14 Fuel at a trap).
    Trap(Error),
    /// The bound ran out; its steps are not yet taken.
    OutOfTime,
    /// The initial memory or tables were refused (FFI-13 Bytes).
    Capacity(Capacity),
}

/// Start an entry with `F + 1` fuel, or `2^64 − 1` with no bound current.
///
/// # Safety
/// The current task owns `context`.
unsafe fn arm(context: *mut Context, store: &mut Store<State>) -> u64 {
    // SAFETY: as above.
    let (left, payer) = unsafe { (steps::remaining(&*context), (*context).payer()) };
    let fuel = match left {
        Some(left) => left.saturating_mul(FUEL_PER_STEP).min(u64::MAX - 2) + 1,
        None => u64::MAX,
    };
    store
        .set_fuel(fuel)
        .unwrap_or_else(|_| std::process::abort());
    let state = store.data_mut();
    state.payer = Some(payer);
    state.refused = None;
    fuel
}

/// Poll an entry on the host stack. Each granule end counts as 1,024 steps
/// and may end the task's slice (FFI-13 Slice ends).
///
/// # Safety
/// As [`arm`]; the entry's store is pinned by the caller.
unsafe fn drive<T>(
    context: *mut Context,
    site: u64,
    entry: impl Future<Output = Result<T, Error>>,
) -> (Result<T, Error>, u64) {
    let mut entry = std::pin::pin!(entry);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut granules = 0;
    loop {
        // SAFETY: as above.
        match unsafe { host(site, || entry.as_mut().poll(&mut cx)) } {
            Poll::Ready(outcome) => return (outcome, granules),
            Poll::Pending => {
                granules += 1;
                // Each granule consumed at least G of the fuel the bound pays
                // for, so the steps counted never exhaust it.
                if !unsafe { steps::take_many(context, GRANULE / FUEL_PER_STEP, site) } {
                    std::process::abort();
                }
            }
        }
    }
}

/// Charge an ended entry its steps less those its granule ends counted
/// (FFI-13 Bounds, FFI-14 Fuel at a trap).
///
/// # Safety
/// As [`arm`].
unsafe fn settle<T>(
    context: *mut Context,
    store: &Store<State>,
    fuel: u64,
    granules: u64,
    outcome: Result<T, Error>,
    initial: bool,
    site: u64,
) -> Result<T, Stop> {
    let bounded = fuel != u64::MAX;
    let left = store.get_fuel().unwrap_or_else(|_| std::process::abort());
    let used = fuel - left;
    let charge = |fuel: u64| {
        let steps = fuel.div_ceil(FUEL_PER_STEP) - granules * (GRANULE / FUEL_PER_STEP);
        // SAFETY: as above; the charge stays within the bound's steps.
        if !unsafe { steps::take_many(context, steps, site) } {
            std::process::abort();
        }
    };
    match outcome {
        Ok(_) if bounded && left == 0 => Err(Stop::OutOfTime),
        Ok(value) => {
            charge(used);
            Ok(value)
        }
        Err(error) => {
            // FFI-13 Bytes / FAIL-15: host memory refusal (`mmap`/`mprotect`
            // `ENOMEM` on linear memory or fiber stack, or `OutOfMemory` on
            // table/instance allocation) is a `HostMemory` run stop, never a
            // catchable trap or process abort.
            if is_host_memory(&error) {
                crate::process::stop(b"HostMemory");
            }
            let trap = error.downcast_ref::<Trap>();
            if bounded && (left == 0 || trap == Some(&Trap::OutOfFuel)) {
                return Err(Stop::OutOfTime);
            }
            if initial
                && let Some(capacity) = store.data().refused
                && trap.is_none()
                && error.downcast_ref::<Exit>().is_none()
            {
                return Err(Stop::Capacity(capacity));
            }
            // The store's fuel lags a trap by up to one granule.
            charge(
                used.saturating_add(GRANULE)
                    .min(if bounded { fuel - 1 } else { u64::MAX }),
            );
            Err(Stop::Trap(error))
        }
    }
}

/// One metered entry into a store.
macro_rules! metered {
    ($context:expr, $store:expr, $site:expr, $entry:expr) => {
        metered!($context, $store, $site, false, $entry)
    };
    ($context:expr, $store:expr, $site:expr, $initial:expr, $entry:expr) => {{
        let fuel = arm($context, &mut $store);
        let (outcome, granules) = drive($context, $site, $entry);
        settle($context, &$store, fuel, granules, outcome, $initial, $site)
    }};
}

/// FFI-13: instantiation runs `_initialize`, each a metered entry. A refusal
/// or trap makes no instance, and the store's charges are refunded.
///
/// # Safety
/// As [`arm`].
unsafe fn instantiate(
    context: *mut Context,
    module: &'static Module,
    site: u64,
) -> Result<Box<Live>, Stop> {
    let mut store = Store::new(engine().engine(), State::default());
    store.limiter(|state| state as &mut dyn ResourceLimiter);
    store
        .fuel_async_yield_interval(Some(GRANULE))
        .unwrap_or_else(|_| std::process::abort());
    // SAFETY: as above.
    let instance = unsafe {
        metered!(
            context,
            store,
            site,
            true,
            prepared(module).instantiate_async(&mut store)
        )?
    };
    if let Some(initialize) = instance.get_func(&mut store, "_initialize") {
        // SAFETY: as above.
        unsafe {
            metered!(
                context,
                store,
                site,
                initialize.call_async(&mut store, &[], &mut [])
            )?
        };
    }
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("an admitted module exports its memory");
    Ok(Box::new(Live {
        store,
        instance,
        memory,
        id: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
    }))
}

/// FFI-14's closed trap map. Any other ending is a failed runtime invariant:
/// under Wasmtime 48.0.10 (`wasmtime-environ` `trap_encoding.rs`), the other 28
/// `Trap` variants belong to threads, GC, function references, exceptions,
/// stack switching, components or epochs, none admitted by `ADMITTED_FEATURES`
/// or `epoch_interruption(false)`; `ResourceLimiter` refusals return
/// `Ok(false)` (−1 or `Stop::Capacity`); host functions return only `Exit` or
/// `Trap::MemoryOutOfBounds`; and host `OutOfMemory` / `ENOMEM` errors stop as
/// `HostMemory` in `settle`, `memory_grow_failed` and `unwrap_host` before
/// reaching this map (contained-trap-lead-3092 REPORT.md).
fn trap_text(error: &Error) -> &'static [u8] {
    if let Some(Exit(code)) = error.downcast_ref::<Exit>() {
        return exit_text(*code);
    }
    match error.downcast_ref::<Trap>() {
        Some(Trap::UnreachableCodeReached) => b"contained trap: unreachable",
        Some(Trap::MemoryOutOfBounds) => b"contained trap: memory out of bounds",
        Some(Trap::TableOutOfBounds) => b"contained trap: table out of bounds",
        Some(Trap::IndirectCallToNull) => b"contained trap: indirect call to null",
        Some(Trap::BadSignature) => b"contained trap: bad signature",
        Some(Trap::IntegerOverflow) => b"contained trap: integer overflow",
        Some(Trap::IntegerDivisionByZero) => b"contained trap: integer division by zero",
        Some(Trap::BadConversionToInteger) => b"contained trap: bad conversion to integer",
        Some(Trap::StackOverflow) => b"contained trap: stack overflow",
        Some(Trap::OutOfFuel) => b"contained trap: out of fuel",
        // Closed under Wasmtime 48.0.10 + ADMITTED_FEATURES + HostMemory stops above.
        _ => std::process::abort(),
    }
}

/// One permanent text per exit code, so a cause can lend it.
fn exit_text(code: u32) -> &'static [u8] {
    static TEXTS: OnceLock<Mutex<HashMap<u32, &'static [u8]>>> = OnceLock::new();
    let mut texts = TEXTS.get_or_init(Default::default).lock().unwrap();
    texts.entry(code).or_insert_with(|| {
        Box::leak(
            format!("contained trap: exit {code}")
                .into_bytes()
                .into_boxed_slice(),
        )
    })
}

/// # Safety
/// The current task owns `context`; `call` runs on its host stack.
unsafe fn host<T>(site: u64, call: impl FnOnce() -> T) -> T {
    #[cfg(not(feature = "prof"))]
    let _ = site;
    // SAFETY: the contained call is synchronous and pins its operands.
    unsafe {
        tasks::on_host_stack(
            true,
            #[cfg(feature = "prof")]
            site,
            call,
        )
    }
}

fn round8(len: u64) -> u64 {
    len.div_ceil(8) * 8
}

/// The packed argument types.
///
/// # Safety
/// The descriptor's type row has `count` entries.
unsafe fn types(call: &Call) -> &[u8] {
    if call.count == 0 {
        &[]
    } else {
        // SAFETY: as above.
        unsafe { std::slice::from_raw_parts(call.types, call.count) }
    }
}

fn is_loan(ty: u8) -> bool {
    matches!(ty, abi::CONTAINED_ARG_VIEW | abi::CONTAINED_ARG_MUT)
}

/// The loan area's size, or `None` past `2^32 − 8` (FFI-12 Loans).
///
/// # Safety
/// The argument words have one row per type.
unsafe fn loan_total(types: &[u8], arguments: *const Argument) -> Option<u64> {
    let mut total = 0u64;
    for (i, &ty) in types.iter().enumerate() {
        if is_loan(ty) {
            // SAFETY: as above.
            let len = unsafe { (*arguments.add(i))[1] };
            total = total.checked_add(round8(len))?;
        }
    }
    (total <= (1 << 32) - 8).then_some(total)
}

/// Ask the module for its loan area and copy the loans in. The outer
/// error is a trap; the inner one refuses the call with the instance kept.
///
/// # Safety
/// The loans' Hirsel bytes are live and pinned.
unsafe fn lend(
    context: *mut Context,
    live: &mut Live,
    module: &Module,
    types: &[u8],
    arguments: *const Argument,
    total: u64,
    site: u64,
) -> Result<Result<u32, &'static [u8]>, Stop> {
    if !types.iter().any(|&ty| is_loan(ty)) {
        return Ok(Ok(0));
    }
    // SAFETY: the descriptor names a static export name.
    let name = unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(
            module.loan_area,
            module.loan_area_len,
        ))
    };
    let loan_area = live
        .instance
        .get_func(&mut live.store, name)
        .expect("an admitted module exports its loan area");
    let mut area = [Val::I32(0)];
    let total_word = [Val::I32(total as u32 as i32)];
    // SAFETY: as above.
    unsafe {
        metered!(
            context,
            live.store,
            site,
            loan_area.call_async(&mut live.store, &total_word, &mut area)
        )?
    };
    let Val::I32(area) = area[0] else {
        unreachable!("loan_area is [i32] -> [i32]")
    };
    let area = area as u32;
    let size = live.memory.data_size(&live.store) as u64;
    if area == 0 || area % 8 != 0 || u64::from(area) + total > size {
        return Ok(Err(b"contained loan area out of bounds"));
    }
    let memory = live.memory.data_mut(&mut live.store);
    let mut at = area as usize;
    for (i, &ty) in types.iter().enumerate() {
        if is_loan(ty) {
            // SAFETY: the Hirsel loan is live for its length.
            let [pointer, len] = unsafe { *arguments.add(i) };
            let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, len as usize) };
            memory[at..at + bytes.len()].copy_from_slice(bytes);
            at += round8(len) as usize;
        }
    }
    Ok(Ok(area))
}

fn canonical_f32(bits: u32) -> u32 {
    if bits & 0x7fff_ffff > 0x7f80_0000 {
        0x7fc0_0000
    } else {
        bits
    }
}

fn canonical_f64(bits: u64) -> u64 {
    if bits & 0x7fff_ffff_ffff_ffff > 0x7ff0_0000_0000_0000 {
        0x7ff8_0000_0000_0000
    } else {
        bits
    }
}

/// Enter the function and return its result bits. On a normal return,
/// each exclusive loan is copied back.
///
/// # Safety
/// As [`lend`]; handle words hold their resolved pointers.
unsafe fn enter(
    context: *mut Context,
    live: &mut Live,
    call: &Call,
    types: &[u8],
    arguments: *const Argument,
    area: u32,
    site: u64,
) -> Result<u64, Stop> {
    let mut params = Vec::with_capacity(types.len() + 2);
    let mut at = area;
    for (i, &ty) in types.iter().enumerate() {
        // SAFETY: one argument row per type.
        let [word, second] = unsafe { *arguments.add(i) };
        match ty {
            abi::CONTAINED_ARG_U8 | abi::CONTAINED_ARG_U16 | abi::CONTAINED_ARG_U32 => {
                params.push(Val::I32(word as u32 as i32));
            }
            abi::CONTAINED_ARG_I8 => params.push(Val::I32(i32::from(word as u8 as i8))),
            abi::CONTAINED_ARG_I16 => params.push(Val::I32(i32::from(word as u16 as i16))),
            abi::CONTAINED_ARG_I32 => params.push(Val::I32(word as u32 as i32)),
            abi::CONTAINED_ARG_I64 => params.push(Val::I64(word as i64)),
            abi::CONTAINED_ARG_F32 => params.push(Val::F32(canonical_f32(word as u32))),
            abi::CONTAINED_ARG_F64 => params.push(Val::F64(canonical_f64(word))),
            abi::CONTAINED_ARG_VIEW | abi::CONTAINED_ARG_MUT => {
                params.extend([Val::I32(at as i32), Val::I32(second as u32 as i32)]);
                at += round8(second) as u32;
            }
            abi::CONTAINED_ARG_HANDLE => params.push(Val::I32(second as u32 as i32)),
            _ => unreachable!("contained argument type"),
        }
    }
    // SAFETY: the descriptor names a static export name.
    let name = unsafe {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(call.name, call.name_len))
    };
    let function = live
        .instance
        .get_func(&mut live.store, name)
        .expect("an admitted module exports each reached function");
    let mut results = [Val::I32(0)];
    let results = &mut results[..usize::from(call.result != abi::CONTAINED_RESULT_UNIT)];
    // SAFETY: as above.
    unsafe {
        metered!(
            context,
            live.store,
            site,
            function.call_async(&mut live.store, &params, results)
        )?
    };
    let bits = match results.first() {
        None => 0,
        Some(Val::I32(value)) => u64::from(*value as u32),
        Some(Val::I64(value)) => *value as u64,
        Some(Val::F32(bits)) => u64::from(canonical_f32(*bits)),
        Some(Val::F64(bits)) => canonical_f64(*bits),
        Some(_) => unreachable!("an admitted export's result type"),
    };
    let memory = live.memory.data(&live.store);
    let mut at = area as usize;
    for (i, &ty) in types.iter().enumerate() {
        if is_loan(ty) {
            // SAFETY: the Hirsel loan is live for its length, and an
            // exclusive one is writable.
            let [pointer, len] = unsafe { *arguments.add(i) };
            if ty == abi::CONTAINED_ARG_MUT && len != 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        memory[at..at + len as usize].as_ptr(),
                        pointer as *mut u8,
                        len as usize,
                    );
                }
            }
            at += round8(len) as usize;
        }
    }
    Ok(bits)
}

/// FFI-13: discard the instance, orphaning its handles, and end the call
/// with its trap, `OutOfTime` or `Capacity`. Dropping the instance refunds
/// its memory charge.
///
/// # Safety
/// The current task owns its prepaid cause cell.
unsafe fn stopped(
    context: *mut Context,
    operands: Operands,
    live: Option<Box<Live>>,
    stop: Stop,
    site: u64,
    quiet: bool,
) -> u8 {
    // SAFETY: the pinned run owns the table.
    let table = unsafe { &*operands.table };
    match live {
        Some(live) => table.orphan(live.id, operands.result.is_some()),
        // SAFETY: the call was prepared and never issued.
        None => unsafe { operands.rollback() },
    }
    // SAFETY: as above.
    unsafe {
        match stop {
            Stop::Trap(error) => {
                let text = trap_text(&error);
                if !quiet {
                    crate::foreign::fail(context, site, text);
                }
            }
            // The call takes exactly the steps its bound had left, and one
            // more raises `OutOfTime` at the call site (MEM-30).
            Stop::OutOfTime => {
                let left = steps::remaining(&*context).expect("a bound is current");
                let _ = steps::take_many(context, left + 1, site);
            }
            Stop::Capacity(capacity) if !quiet => {
                causes::record_capacity(&mut *context, capacity, site)
            }
            Stop::Capacity(_) => {}
        }
    }
    1
}

/// One step per 16 KiB, or part, of each loan copied in, and again of each
/// exclusive loan copied back (FFI-13 Steps).
///
/// # Safety
/// As [`loan_total`].
unsafe fn copy_steps(types: &[u8], arguments: *const Argument) -> u64 {
    let mut steps = 0;
    for (i, &ty) in types.iter().enumerate() {
        if is_loan(ty) {
            // SAFETY: as above.
            let chunks = unsafe { (*arguments.add(i))[1] }.div_ceil(16 << 10);
            steps += chunks * if ty == abi::CONTAINED_ARG_MUT { 2 } else { 1 };
        }
    }
    steps
}

/// The contained call. Zero on success, with the result written; one on
/// failure, with the cause recorded unless `quiet`.
///
/// # Safety
/// As `hirsel_core_contained_call`.
unsafe fn call(
    context: *mut Context,
    call: &Call,
    arguments: *mut Argument,
    result: *mut u64,
    site: u64,
    quiet: bool,
) -> u8 {
    // SAFETY: the generated slot pins the run, descriptor and operands.
    unsafe {
        let run = &*(*context).foreign;
        let module = &*call.module;
        let key = call.module as usize;
        let types = types(call);
        let mut operands = Operands {
            table: &raw const run.handles,
            arguments,
            parameters: call.parameters,
            count: call.count,
            result: call.new_kind.as_ref(),
            release: call.release,
            instance: 0,
        };
        let status = operands.prepare(context, site);
        if status != 0 {
            return if quiet {
                1
            } else {
                crate::foreign::refuse(context, site, status)
            };
        }
        let refuse = |text: &'static [u8]| {
            if !quiet {
                crate::foreign::fail(context, site, text);
            }
            1
        };
        let Some(total) = loan_total(types, arguments) else {
            operands.rollback();
            return refuse(b"contained loans exceed linear memory");
        };
        if !steps::take_many(context, copy_steps(types, arguments), site) {
            operands.rollback();
            return 1;
        }
        let mut live = match (*context).contained.take(key) {
            Some(live) => live,
            None => match instantiate(context, module, site) {
                Ok(live) => live,
                Err(stop) => return stopped(context, operands, None, stop, site, quiet),
            },
        };
        operands.instance = live.id;
        let area = match lend(context, &mut live, module, types, arguments, total, site) {
            Ok(Ok(area)) => area,
            Ok(Err(text)) => {
                operands.rollback();
                (*context).contained.0.push((key, live));
                return refuse(text);
            }
            Err(stop) => return stopped(context, operands, Some(live), stop, site, quiet),
        };
        operands.issue();
        match enter(context, &mut live, call, types, arguments, area, site) {
            Ok(bits) => {
                (*context).contained.0.push((key, live));
                let outcome = operands.deliver(bits);
                if outcome.status != 0 {
                    return if quiet {
                        1
                    } else {
                        crate::foreign::refuse(context, site, outcome.status)
                    };
                }
                result.write(outcome.value);
                0
            }
            Err(stop) => stopped(context, operands, Some(live), stop, site, quiet),
        }
    }
}

/// FFI-6's mechanical release of a live contained handle, after its
/// resource's finalizer. Its result and any failure are discarded.
///
/// # Safety
/// The current finalizer exclusively owns its resource and context.
pub(crate) unsafe fn release(context: *mut Context, release: &Call, ordinal: u64) {
    debug_assert_eq!(release.count, 1);
    let mut argument: Argument = [ordinal, 0];
    let mut result = 0;
    // SAFETY: the sole argument and result live through the call.
    unsafe {
        call(
            context,
            release,
            &raw mut argument,
            &raw mut result,
            0,
            true,
        )
    };
}

/// FFI-13: when its task ends, after its finalizers, the task's instances
/// are dropped and their handles consumed with no release.
///
/// # Safety
/// The ending task owns `context`.
pub(crate) unsafe fn end_task(context: *mut Context) {
    // SAFETY: as above; a task with an instance runs with a foreign run.
    unsafe {
        let lives = std::mem::take(&mut (*context).contained.0);
        for (_, live) in lives {
            (*(*context).foreign).handles.consume(live.id);
        }
    }
}

/// The Hirsel-side entry to a contained call, FFI-12's protocol over
/// FFI-6's handle table.
///
/// # Safety
/// Context belongs to the current foreign-enabled run. The checked
/// descriptor and its rows are static. The argument words and loans
/// remain live and pinned through the call. The result word is an
/// exclusive destination, written only on success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_contained_call(
    context: *mut Context,
    descriptor: *const Call,
    arguments: *mut Argument,
    result: *mut u64,
    site: u64,
) -> u8 {
    // SAFETY: as above.
    unsafe {
        crate::process::deny_describe_capability(context);
        call(context, &*descriptor, arguments, result, site, false)
    }
}

/// A contained kind's `release` thunk, never entered: its handles are
/// released through [`release`].
///
/// # Safety
/// None; it aborts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_contained_thunk(_: *mut u64) -> u64 {
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_contained::wasmtime::OutOfMemory;

    #[test]
    #[ignore = "isolated host-memory grow refusal, invoked by the named parent"]
    fn memory_grow_host_oom_child() {
        let mut state = State::default();
        let _ = state.memory_grow_failed(Error::from(OutOfMemory::new(64 << 10)));
        panic!("memory_grow_failed returned on OutOfMemory");
    }

    #[test]
    fn memory_grow_failed_stops_on_host_memory_only() {
        let mut state = State::default();
        assert!(
            state
                .memory_grow_failed(Error::msg("Memory maximum size exceeded"))
                .is_ok()
        );
        let output = crate::test_process::command()
            .args([
                "--ignored",
                "--exact",
                "host::linux::contained::tests::memory_grow_host_oom_child",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(102), "{output:?}");
        assert_eq!(output.stderr, b"run stop: HostMemory\n");
    }
}
