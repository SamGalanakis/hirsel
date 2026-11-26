//! FFI-12 to FFI-14 on Linux: the contained engine, its null imports and
//! the calling task's instances.
use crate::Context;
use crate::abi_layout as abi;
use crate::foreign::contained::{Call, Module};
use crate::foreign::handles::{Argument, Operands};
use crate::tasks;
use hirsel_core_contained::load::ImageEngine;
use hirsel_core_contained::wasmtime::{
    Caller, Error, Extern, Instance, InstancePre, Linker, Memory, Store, Trap, Val,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

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
    store: Store<()>,
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

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn engine() -> &'static ImageEngine {
    static ENGINE: OnceLock<ImageEngine> = OnceLock::new();
    // The runtime's own signal handlers are installed before any task
    // runs, so Wasmtime's chain to them (FFI-15 Signals).
    ENGINE.get_or_init(|| ImageEngine::new().unwrap_or_else(|_| std::process::abort()))
}

const WASI: &str = "wasi_snapshot_preview1";

/// FFI-12's null imports, each with its preview-1 core type.
fn linker() -> &'static Linker<()> {
    static LINKER: OnceLock<Linker<()>> = OnceLock::new();
    LINKER.get_or_init(|| {
        let mut linker = Linker::new(engine().engine());
        define(&mut linker).unwrap_or_else(|_| std::process::abort());
        linker
    })
}

fn define(linker: &mut Linker<()>) -> Result<(), Error> {
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
fn sizes(mut caller: Caller<'_, ()>, count: i32, size: i32) -> Result<i32, Error> {
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

fn prepared(module: &'static Module) -> InstancePre<()> {
    static PREPARED: OnceLock<Mutex<HashMap<usize, InstancePre<()>>>> = OnceLock::new();
    let mut prepared = PREPARED.get_or_init(Default::default).lock().unwrap();
    prepared
        .entry(std::ptr::from_ref(module) as usize)
        .or_insert_with(|| {
            // SAFETY: the linker placed the unmodified artifact at a 64 KiB
            // boundary of the image's read-execute segment (FFI-15).
            let artifact = unsafe { std::slice::from_raw_parts(module.artifact, module.len) };
            let loaded =
                unsafe { engine().load(artifact) }.unwrap_or_else(|_| std::process::abort());
            linker()
                .instantiate_pre(&loaded)
                .unwrap_or_else(|_| std::process::abort())
        })
        .clone()
}

/// Drive a Wasmtime future that never waits on anything outside it.
fn complete<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
}

/// FFI-13: instantiation runs `_initialize`. A trap makes no instance.
fn instantiate(module: &'static Module) -> Result<Box<Live>, Error> {
    let mut store = Store::new(engine().engine(), ());
    store.set_fuel(u64::MAX)?;
    let instance = complete(prepared(module).instantiate_async(&mut store))?;
    if let Some(initialize) = instance.get_func(&mut store, "_initialize") {
        complete(initialize.call_async(&mut store, &[], &mut []))?;
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

/// FFI-14's closed trap map. Any other ending is a failed runtime invariant.
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
    live: &mut Live,
    module: &Module,
    types: &[u8],
    arguments: *const Argument,
    total: u64,
) -> Result<Result<u32, &'static [u8]>, Error> {
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
    complete(loan_area.call_async(&mut live.store, &[Val::I32(total as u32 as i32)], &mut area))?;
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
    live: &mut Live,
    call: &Call,
    types: &[u8],
    arguments: *const Argument,
    area: u32,
) -> Result<u64, Error> {
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
    complete(function.call_async(&mut live.store, &params, results))?;
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

/// FFI-13: discard the instance, orphaning its handles, and fail the call.
///
/// # Safety
/// The current task owns its prepaid cause cell.
unsafe fn trapped(
    context: *mut Context,
    operands: Operands,
    live: Option<Box<Live>>,
    error: &Error,
    site: u64,
    quiet: bool,
) -> u8 {
    let text = trap_text(error);
    // SAFETY: the pinned run owns the table.
    let table = unsafe { &*operands.table };
    match live {
        Some(live) => table.orphan(live.id, operands.result.is_some()),
        // SAFETY: the call was prepared and never issued.
        None => unsafe { operands.rollback() },
    }
    if !quiet {
        // SAFETY: as above.
        unsafe { crate::foreign::fail(context, site, text) };
    }
    1
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
        let mut live = match (*context).contained.take(key) {
            Some(live) => live,
            None => match host(site, || instantiate(module)) {
                Ok(live) => live,
                Err(error) => return trapped(context, operands, None, &error, site, quiet),
            },
        };
        operands.instance = live.id;
        live.store
            .set_fuel(u64::MAX)
            .unwrap_or_else(|_| std::process::abort());
        let area = match host(site, || lend(&mut live, module, types, arguments, total)) {
            Ok(Ok(area)) => area,
            Ok(Err(text)) => {
                operands.rollback();
                (*context).contained.0.push((key, live));
                return refuse(text);
            }
            Err(error) => return trapped(context, operands, Some(live), &error, site, quiet),
        };
        operands.issue();
        match host(site, || enter(&mut live, call, types, arguments, area)) {
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
            Err(error) => trapped(context, operands, Some(live), &error, site, quiet),
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
