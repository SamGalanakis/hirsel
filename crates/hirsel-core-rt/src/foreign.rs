//! The common native boundary for the C thunks emitted by the linker.
use crate::tasks;

pub mod contained;
pub(crate) mod handles;
pub(crate) mod helpers;

use crate::{Context, provider::Outcome};
pub use handles::{Argument, Kind, Parameter};

/// A C thunk takes an initialized array of argument words and returns the
/// result bits. Its caller retains custody of the words and every loan.
pub type Thunk = unsafe extern "C" fn(*mut u64) -> u64;

/// A zero-byte loan still passes a stable nonnull address.
#[unsafe(no_mangle)]
pub static hirsel_core_foreign_empty: u8 = 0;

/// FFI-5 result tags shared with the linker's thunks.
pub use crate::abi_layout::{FOREIGN_RESULT_F32 as RESULT_F32, FOREIGN_RESULT_F64 as RESULT_F64};

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
    unsafe {
        tasks::on_host_stack(
            true,
            #[cfg(feature = "prof")]
            0,
            || invoke(thunk, args, result),
        )
    }
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
        tasks::before_provider_entry();
        if tasks::check_cancel(context, site) != 0 {
            return 1;
        }
        let run = &*(*context).foreign;
        let outcome = run.helpers.as_ref().unwrap().call(
            context,
            Invocation {
                thunk,
                args,
                result_tag,
                handles: None,
            },
            true,
            site,
        );
        if outcome.status == 0 {
            result.write(outcome.value);
        }
        outcome.status
    }
}

/// One checked foreign call's link-time descriptor, with one parameter row
/// per packed argument.
#[repr(C)]
pub struct Call {
    pub thunk: Thunk,
    pub parameters: *const Parameter,
    pub count: usize,
    pub new_kind: *const Kind,
    pub result_tag: u8,
    pub waits: bool,
    pub release: bool,
}

const _: () = {
    use crate::abi_layout::*;
    use std::mem::offset_of;
    assert!(size_of::<Kind>() == FOREIGN_KIND_BYTES);
    assert!(offset_of!(Kind, release) == FOREIGN_KIND_RELEASE as usize);
    assert!(offset_of!(Kind, waits) == FOREIGN_KIND_WAITS);
    assert!(offset_of!(Kind, contained) == FOREIGN_KIND_CONTAINED as usize);
    assert!(size_of::<Call>() == FOREIGN_CALL_BYTES);
    assert!(offset_of!(Call, thunk) == FOREIGN_CALL_THUNK as usize);
    assert!(offset_of!(Call, parameters) == FOREIGN_CALL_PARAMETERS as usize);
    assert!(offset_of!(Call, count) == FOREIGN_CALL_COUNT);
    assert!(offset_of!(Call, new_kind) == FOREIGN_CALL_NEW as usize);
    assert!(offset_of!(Call, result_tag) == FOREIGN_CALL_RESULT);
    assert!(offset_of!(Call, waits) == FOREIGN_CALL_WAITS);
    assert!(offset_of!(Call, release) == FOREIGN_CALL_RELEASE);
};

pub(crate) struct Run {
    pub(crate) handles: handles::Handles,
    helpers: Option<helpers::Helpers>,
}

impl Run {
    pub(crate) fn new(waits: bool) -> std::io::Result<Self> {
        Ok(Self {
            handles: handles::Handles::default(),
            helpers: waits.then(helpers::Helpers::start).transpose()?,
        })
    }

    /// # Safety
    /// This run's context remains live. The scheduler has joined all tasks
    /// and every issued call was delivered before this orderly exit.
    pub(crate) unsafe fn finish(&self, context: *mut Context) {
        while let Some((kind, mut argument)) = self.handles.pop() {
            // FFI-13: a contained handle's instance has gone with its task.
            if kind.is_contained() {
                continue;
            }
            let call = Invocation {
                thunk: kind.release,
                args: argument.as_mut_ptr(),
                result_tag: 0,
                handles: None,
            };
            // SAFETY: the removed table row transfers its release obligation
            // to us, and the call cannot outlive these two argument words.
            unsafe { self.perform(context, call, kind.waits, false, 0) };
        }
    }

    /// # Safety
    /// The current task (or orderly run end) pins the call through delivery.
    unsafe fn perform(
        &self,
        context: *mut Context,
        call: Invocation,
        waits: bool,
        cancellable: bool,
        site: u64,
    ) -> Outcome {
        // SAFETY: perform transfers the unique issued call to the chosen stack.
        unsafe {
            if waits {
                self.helpers
                    .as_ref()
                    .unwrap()
                    .call(context, call, cancellable, site)
            } else {
                tasks::on_host_stack(
                    true,
                    #[cfg(feature = "prof")]
                    site,
                    || call.issue(),
                )
            }
        }
    }

    /// # Safety
    /// The current finalizer exclusively owns its resource and context.
    unsafe fn cleanup_handle(&self, context: *mut Context, ordinal: u64) {
        let Some(kind) = self.handles.kind(ordinal) else {
            return;
        };
        if kind.is_contained() {
            if !self.handles.consume_orphan(ordinal) {
                // SAFETY: as for this function; the release descriptor is static.
                unsafe { contained::release(context, &*kind.contained, ordinal) };
            }
            return;
        }
        let mut argument = [ordinal, 0];
        let parameters = [Parameter {
            kind: std::ptr::from_ref(kind),
            dir: false,
        }];
        let handles = handles::Operands {
            table: &raw const self.handles,
            arguments: &raw mut argument,
            parameters: parameters.as_ptr(),
            count: 1,
            result: None,
            release: true,
            instance: 0,
        };
        // SAFETY: table preflight recognizes a racing use or explicit release.
        // Keep the sole argument and kind table pinned through the cleanup.
        unsafe {
            if handles.prepare(context, 0) == 0 {
                self.perform(
                    context,
                    Invocation {
                        thunk: kind.release,
                        args: argument.as_mut_ptr(),
                        handles: Some(handles),
                        result_tag: 0,
                    },
                    kind.waits,
                    false,
                    0,
                );
            }
        }
    }
}

/// Release the foreign owners that remain in a resource after its finalizer.
///
/// # Safety
/// The descriptor is the exact layout's foreign cleanup paths, and the
/// exclusive payload stays pinned until cleanup completes. Context belongs
/// to its task, under the finalizer's shield.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_foreign_cleanup(
    context: *mut Context,
    payload: *const u8,
    paths: *const u64,
    len: usize,
) {
    // SAFETY: the current task pins the table or the run has no foreign owners.
    let Some(run) = (unsafe { (*context).foreign.as_ref() }) else {
        return;
    };
    // SAFETY: this is the emitted immutable cleanup descriptor.
    let paths = unsafe { std::slice::from_raw_parts(paths, len) };
    let mut before = None;
    loop {
        let ordinal = hirsel_core_layout::next_foreign_handle(paths, before, |offset, width| {
            // SAFETY: each active path names an initialized tag or ordinal.
            unsafe {
                let ptr = payload.add(offset as usize);
                match width {
                    1 => u64::from(ptr.read()),
                    2 => u64::from(ptr.cast::<u16>().read_unaligned()),
                    4 => u64::from(ptr.cast::<u32>().read_unaligned()),
                    8 => ptr.cast::<u64>().read_unaligned(),
                    _ => unreachable!("foreign cleanup tag width"),
                }
            }
        });
        if ordinal == 0 {
            break;
        }
        // SAFETY: emitter pins the released resource under its finalizer shield.
        unsafe { run.cleanup_handle(context, ordinal) };
        before = Some(ordinal);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Invocation {
    thunk: Thunk,
    args: *mut u64,
    result_tag: u8,
    handles: Option<handles::Operands>,
}

impl Invocation {
    /// # Safety
    /// On a host or helper stack. This is the sole issue of a prepared call.
    pub(crate) unsafe fn issue(self) -> Outcome {
        // SAFETY: the task pins loaned storage and the compiler's ABI frame.
        unsafe {
            if let Some(handles) = self.handles {
                handles.issue();
            }
            let bits = invoke(self.thunk, self.args, self.result_tag);
            self.handles.map_or_else(
                || Outcome {
                    value: bits,
                    ..Outcome::default()
                },
                |handles| handles.deliver(bits),
            )
        }
    }

    /// # Safety
    /// The sole cancellation of a prepared call, before issue.
    pub(crate) unsafe fn rollback(self) {
        if let Some(handles) = self.handles {
            // SAFETY: the winning cancellation owns the prepared arguments.
            unsafe { handles.rollback() };
        }
    }
}

/// The shared Hirsel-side entry to the FFI-5 to FFI-7 protocol.
///
/// # Safety
/// Context belongs to the current foreign-enabled run. The checked descriptor
/// and its parameter table are pinned through the call. The kind descriptors are
/// static. The argument words and loans remain live and pinned through
/// delivery. The thunk follows the native contract. The result word is an
/// exclusive destination, written only on success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_foreign_call(
    context: *mut Context,
    call: *const Call,
    arguments: *mut Argument,
    result: *mut u64,
    site: u64,
) -> u8 {
    // SAFETY: the generated bridge pins the run, descriptor and operands.
    unsafe {
        crate::process::deny_describe_capability(context);
        let call = &*call;
        let cancellable = call.waits && !call.release;
        if cancellable && tasks::check_cancel(context, site) != 0 {
            return 1;
        }
        let run = &*(*context).foreign;
        let handles = handles::Operands {
            table: &raw const run.handles,
            arguments,
            parameters: call.parameters,
            count: call.count,
            result: call.new_kind.as_ref(),
            release: call.release,
            instance: 0,
        };
        let status = handles.prepare(context, site);
        if status != 0 {
            return refuse(context, site, status);
        }
        let invocation = Invocation {
            thunk: call.thunk,
            args: arguments.cast(),
            result_tag: call.result_tag,
            handles: Some(handles),
        };
        let outcome = run.perform(context, invocation, call.waits, cancellable, site);
        if outcome.status == 0 {
            result.write(outcome.value);
        } else if outcome.status != 1 {
            return refuse(context, site, outcome.status);
        }
        outcome.status
    }
}

/// # Safety
/// The current task owns its prepaid cause cell.
pub(crate) unsafe fn refuse(context: *mut Context, site: u64, status: u8) -> u8 {
    let text: &[u8] = match status {
        handles::RELEASED => b"foreign handle released",
        handles::IN_USE => b"foreign handle in use",
        handles::WRONG_KIND => b"foreign handle of another kind",
        handles::ALREADY_HELD => b"foreign pointer already held",
        handles::NEEDS_HOST_DIR => b"foreign call needs a host Dir",
        handles::DISCARDED => b"foreign instance discarded",
        _ => unreachable!("foreign handle refusal"),
    };
    // SAFETY: the literal is static.
    unsafe { fail(context, site, text) };
    1
}

/// Record `Fail(text)` at `site`.
///
/// # Safety
/// The current task owns its prepaid cause cell.
pub(crate) unsafe fn fail(context: *mut Context, site: u64, text: &'static [u8]) {
    // SAFETY: the literal lends a permanent String carrier to the cause.
    unsafe {
        crate::causes::record(
            &mut *context,
            crate::causes::Cause::fail(
                crate::causes::StringMessage {
                    data: text.as_ptr().cast_mut(),
                    len: text.len() as u64,
                    capacity: 0,
                },
                site,
            ),
        );
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
