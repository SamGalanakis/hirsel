//! The common native boundary for the C thunks emitted by the linker.
use crate::tasks;

pub(crate) mod helpers;

/// A C thunk takes an initialized array of argument words and returns the
/// result bits. Its caller retains custody of the words and every loan.
pub type Thunk = unsafe extern "C" fn(*mut u64) -> u64;

/// A zero-byte loan still passes a stable nonnull address.
#[unsafe(no_mangle)]
pub static hirsel_core_foreign_empty: u8 = 0;

/// FFI-5 result tags shared with the linker's thunks.
pub const RESULT_F32: u8 = 1;
pub const RESULT_F64: u8 = 2;

/// Enter a bounded, non-suspending C call on this worker's host stack.
///
/// # Safety
/// The thunk and its arguments agree on the C ABI and obey the native
/// contract. The argument frame and the initialized ranges it lends remain
/// live, pinned and appropriately borrowed until the thunk returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_foreign_stack(
    thunk: Thunk,
    args: *mut u64,
    result: u8,
) -> u64 {
    // SAFETY: the thunk is synchronous and the caller pins its arguments.
    unsafe { tasks::on_host_stack(true, || invoke(thunk, args, result)) }
}

/// Enter a C call that may wait on this run's helper pool.
///
/// # Safety
/// The context is the running task in a run with helpers. The result word is
/// writable on success. For the thunk and arguments, see hirsel_core_foreign_stack.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_foreign_wait(
    context: *mut crate::Context,
    thunk: Thunk,
    args: *mut u64,
    result_tag: u8,
    result: *mut u64,
    site: u64,
) -> u8 {
    // SAFETY: the caller pins the task and its run configuration through delivery.
    unsafe {
        crate::process::deny_describe_capability(context);
        let _commit = tasks::before_provider_entry();
        if tasks::check_cancel(context, site) != 0 {
            return 1;
        }
        let helpers = &*(*context).foreign_helpers;
        let outcome = helpers.call(context, thunk, args, result_tag, site);
        if outcome.status == 0 {
            result.write(outcome.value);
        }
        outcome.status
    }
}

/// The caller is already on a host or helper stack.
///
/// # Safety
/// As hirsel_core_foreign_stack, with at least 1 MiB of stack available.
pub(crate) unsafe fn invoke(thunk: Thunk, args: *mut u64, result: u8) -> u64 {
    restore_floats();
    // SAFETY: the checked thunk and pinned arguments are the caller's contract.
    let bits = unsafe { thunk(args) };
    restore_floats();
    // Classify the raw bits so a signaling NaN cannot set FP status again.
    match result {
        RESULT_F32 if bits as u32 & 0x7fff_ffff > 0x7f80_0000 => 0x7fc0_0000,
        RESULT_F64 if bits & 0x7fff_ffff_ffff_ffff > 0x7ff0_0000_0000_0000 => 0x7ff8_0000_0000_0000,
        _ => bits,
    }
}

#[cfg(target_arch = "x86_64")]
fn restore_floats() {
    let mxcsr: u32 = 0x1f80;
    // SAFETY: the memory operand is a readable MXCSR word. The defaults mask
    // traps, clear status and flush-to-zero, and select round to nearest.
    unsafe {
        core::arch::asm!(
            "fninit",
            "ldmxcsr [{mxcsr}]",
            mxcsr = in(reg) &mxcsr,
            options(nostack, preserves_flags, readonly),
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn restore_floats() {
    // SAFETY: zero is the AArch64 FPCR/FPSR default and these registers are
    // writable in user mode.
    unsafe {
        core::arch::asm!(
            "msr fpcr, xzr",
            "msr fpsr, xzr",
            options(nomem, nostack, preserves_flags),
        );
    }
}
