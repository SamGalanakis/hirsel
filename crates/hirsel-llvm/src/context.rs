//! Owned LLVM handles.
//!
//! Each owning wrapper (`Context`, `Module`, `TargetMachine`) owns exactly one LLVM object and
//! disposes of it on drop. A `Module` borrows its `Context`, so the borrow checker orders their
//! disposal and no handle outlives its owner. No raw handle leaves this module, and none is
//! public.

use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::sync::OnceLock;

#[cfg(test)]
use llvm_sys::analysis::{LLVMVerifierFailureAction, LLVMVerifyModule};
use llvm_sys::core::*;
use llvm_sys::error::{LLVMDisposeErrorMessage, LLVMErrorRef, LLVMGetErrorMessage};
use llvm_sys::ir_reader::LLVMParseIRInContext2;
use llvm_sys::prelude::{LLVMBool, LLVMContextRef, LLVMMemoryBufferRef, LLVMModuleRef};
use llvm_sys::target::{
    LLVMDisposeTargetData, LLVMInitializeAArch64AsmPrinter, LLVMInitializeAArch64Target,
    LLVMInitializeAArch64TargetInfo, LLVMInitializeAArch64TargetMC, LLVMInitializeX86AsmParser,
    LLVMInitializeX86AsmPrinter, LLVMInitializeX86Target, LLVMInitializeX86TargetInfo,
    LLVMInitializeX86TargetMC, LLVMSetModuleDataLayout,
};
use llvm_sys::target_machine::{
    LLVMCodeGenFileType, LLVMCodeGenOptLevel, LLVMCodeModel, LLVMCreateTargetDataLayout,
    LLVMCreateTargetMachine, LLVMDisposeTargetMachine, LLVMGetTargetFromTriple, LLVMRelocMode,
    LLVMTargetMachineEmitToMemoryBuffer, LLVMTargetMachineRef, LLVMTargetRef,
};

#[cfg(test)]
mod probe_tests;

/// How far apart emitted functions touch a frame's pages (`stack-probe-size`): one 4 KiB
/// page, the smallest page either target maps and so the smallest guard a task stack has.
const STACK_PROBE_BYTES: usize = 4096;

/// Registers the X86 and AArch64 code generators with LLVM, once per process.
fn initialize_targets() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: target registration has no preconditions; OnceLock runs it once.
        unsafe {
            LLVMInitializeX86TargetInfo();
            LLVMInitializeX86Target();
            LLVMInitializeX86TargetMC();
            LLVMInitializeX86AsmPrinter();
            LLVMInitializeX86AsmParser();
            LLVMInitializeAArch64TargetInfo();
            LLVMInitializeAArch64Target();
            LLVMInitializeAArch64TargetMC();
            LLVMInitializeAArch64AsmPrinter();
        }
    });
}

/// Copies an LLVM-allocated message and frees it. A null message is empty.
///
/// # Safety
/// `message` is null or a string LLVM allocated for the caller to dispose of.
unsafe fn take_message(message: *mut c_char) -> String {
    if message.is_null() {
        return String::new();
    }
    // SAFETY: the caller passes a live NUL-terminated string that it owns.
    let text = unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: as above; ownership ends here.
    unsafe { LLVMDisposeMessage(message) };
    text
}

/// Copies a string LLVM still owns.
///
/// # Safety
/// `text` is null or a live NUL-terminated string.
unsafe fn borrowed_string(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: the caller passes a live NUL-terminated string.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// Interior NULs never occur in the names and pass pipelines this crate builds: symbol names
/// are mangled to `[A-Za-z0-9_$]` and every other string is a literal of this crate.
fn c_string(text: &str) -> CString {
    CString::new(text).expect("LLVM strings built by hirsel-llvm contain no NUL")
}

/// An owned `LLVMContext`.
pub(crate) struct Context {
    raw: LLVMContextRef,
}

impl Context {
    pub(crate) fn new() -> Self {
        // SAFETY: creating a context has no preconditions.
        Self {
            raw: unsafe { LLVMContextCreate() },
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the context is live, and every module borrowing it has been dropped.
        unsafe { LLVMContextDispose(self.raw) };
    }
}

/// An owned `LLVMModule` within a borrowed `Context`.
pub(crate) struct Module<'context> {
    raw: LLVMModuleRef,
    context: &'context Context,
}

impl<'context> Module<'context> {
    /// Parses textual IR. The parser's diagnostic is the error.
    pub(crate) fn parse(
        context: &'context Context,
        name: &str,
        text: &str,
    ) -> Result<Self, String> {
        let name = c_string(name);
        // SAFETY: the range is `text`'s bytes; LLVM copies them and appends a NUL.
        let buffer = unsafe {
            LLVMCreateMemoryBufferWithMemoryRangeCopy(
                text.as_ptr().cast(),
                text.len(),
                name.as_ptr(),
            )
        };
        let mut raw = ptr::null_mut();
        let mut message = ptr::null_mut();
        // SAFETY: the context and buffer are live. The parser borrows the buffer;
        // dispose of our copy after parsing on either outcome (LLVM23 IRReader.h).
        let failed = unsafe {
            let failed = LLVMParseIRInContext2(context.raw, buffer, &mut raw, &mut message);
            LLVMDisposeMemoryBuffer(buffer);
            failed
        };
        // SAFETY: on either outcome the message is null or ours to dispose of.
        let message = unsafe { take_message(message) };
        if failed != 0 || raw.is_null() {
            return Err(message);
        }
        Ok(Self { raw, context })
    }

    /// Sets the module's triple and data layout to the machine's.
    pub(crate) fn set_target(&self, machine: &TargetMachine) {
        let triple = c_string(&machine.triple);
        // SAFETY: the module and machine are live; LLVM copies the triple and the layout,
        // and the target data is disposed of after it is copied.
        unsafe {
            LLVMSetTarget(self.raw, triple.as_ptr());
            let data = LLVMCreateTargetDataLayout(machine.raw);
            LLVMSetModuleDataLayout(self.raw, data);
            LLVMDisposeTargetData(data);
        }
    }

    /// Runs the LLVM verifier. The verifier's report is the error.
    #[cfg(test)]
    pub(crate) fn verify(&self) -> Result<(), String> {
        let mut message = ptr::null_mut();
        // SAFETY: the module is live; the verifier only reads it and writes the message.
        let failed = unsafe {
            LLVMVerifyModule(
                self.raw,
                LLVMVerifierFailureAction::LLVMReturnStatusAction,
                &mut message,
            )
        };
        // SAFETY: the message is null or ours to dispose of.
        let message = unsafe { take_message(message) };
        if failed != 0 { Err(message) } else { Ok(()) }
    }

    /// Runs the shared release pipeline and its native lowering passes.
    pub(crate) fn run_core_passes(&self, machine: &TargetMachine) -> Result<(), String> {
        // SAFETY: the module, its context and the target machine are live for
        // the duration of the bridge calls.
        unsafe {
            let hint = LLVMGetEnumAttributeKindForName(b"inlinehint".as_ptr().cast(), 10);
            let mut function = LLVMGetFirstFunction(self.raw);
            while !function.is_null() {
                if LLVMCountBasicBlocks(function) != 0 {
                    LLVMAddAttributeAtIndex(
                        function,
                        llvm_sys::LLVMAttributeFunctionIndex,
                        LLVMCreateEnumAttribute(self.context.raw, hint, 0),
                    );
                }
                function = LLVMGetNextFunction(function);
            }
            unsafe extern "C" {
                fn hirsel_llvm_inline_snapshot_calls(module: LLVMModuleRef);
                fn hirsel_llvm_optimize_release(
                    module: LLVMModuleRef,
                    machine: LLVMTargetMachineRef,
                    prune: LLVMBool,
                    prepay_steps: LLVMBool,
                ) -> LLVMErrorRef;
                fn hirsel_llvm_specialize_release(
                    module: LLVMModuleRef,
                    machine: LLVMTargetMachineRef,
                ) -> LLVMErrorRef;
                fn hirsel_llvm_cleanup_release(
                    module: LLVMModuleRef,
                    machine: LLVMTargetMachineRef,
                ) -> LLVMErrorRef;
            }
            hirsel_llvm_inline_snapshot_calls(self.raw);
            let mut error = hirsel_llvm_optimize_release(self.raw, machine.raw, 0, 0);
            if error.is_null() {
                unsafe extern "C" {
                    fn hirsel_llvm_scalarize_aggregate_phis(
                        module: LLVMModuleRef,
                        context: LLVMContextRef,
                    );
                }
                unsafe extern "C" {
                    fn hirsel_llvm_promote_step_window(module: LLVMModuleRef);
                    fn hirsel_llvm_guard_copy_loops(
                        module: LLVMModuleRef,
                        machine: LLVMTargetMachineRef,
                    );
                }
                unsafe extern "C" {
                    fn hirsel_llvm_mark_loop_callbacks(module: LLVMModuleRef);
                    fn hirsel_llvm_inline_loop_callbacks(module: LLVMModuleRef);
                    fn hirsel_llvm_inline_recursive_leaves(module: LLVMModuleRef);
                }
                hirsel_llvm_mark_loop_callbacks(self.raw);
                error = hirsel_llvm_specialize_release(self.raw, machine.raw);
                if !error.is_null() {
                    let message = LLVMGetErrorMessage(error);
                    let text = CStr::from_ptr(message).to_string_lossy().into_owned();
                    LLVMDisposeErrorMessage(message);
                    return Err(text);
                }
                hirsel_llvm_inline_loop_callbacks(self.raw);
                hirsel_llvm_inline_recursive_leaves(self.raw);
                hirsel_llvm_guard_copy_loops(self.raw, machine.raw);
                hirsel_llvm_promote_step_window(self.raw);
                hirsel_llvm_scalarize_aggregate_phis(self.raw, self.context.raw);
                error = hirsel_llvm_cleanup_release(self.raw, machine.raw);
            }
            if error.is_null() {
                unsafe extern "C" {
                    fn hirsel_llvm_fixed_width_checks(module: LLVMModuleRef);
                    fn hirsel_llvm_finish_reads(
                        module: LLVMModuleRef,
                        machine: LLVMTargetMachineRef,
                    );
                    fn hirsel_llvm_share_cold_sites(module: LLVMModuleRef);
                    fn hirsel_llvm_lower_x86_index_strides(module: LLVMModuleRef);
                    fn hirsel_llvm_reuse_step_flags(module: LLVMModuleRef);
                    fn hirsel_llvm_share_cold_step_sync(
                        module: LLVMModuleRef,
                        machine: LLVMTargetMachineRef,
                    );
                    fn hirsel_llvm_cold_failure_edges(
                        module: LLVMModuleRef,
                        context: LLVMContextRef,
                    );
                }
                hirsel_llvm_fixed_width_checks(self.raw);
                hirsel_llvm_finish_reads(self.raw, machine.raw);
                hirsel_llvm_lower_x86_index_strides(self.raw);
                hirsel_llvm_reuse_step_flags(self.raw);
                hirsel_llvm_share_cold_sites(self.raw);
                hirsel_llvm_cold_failure_edges(self.raw, self.context.raw);
                hirsel_llvm_share_cold_step_sync(self.raw, machine.raw);
            }
            if error.is_null() {
                return Ok(());
            }
            let message = LLVMGetErrorMessage(error);
            let text = borrowed_string(message);
            LLVMDisposeErrorMessage(message);
            Err(text)
        }
    }

    /// Generates an object file and copies its bytes out of LLVM.
    ///
    /// Every defined function probes its frame [`STACK_PROBE_BYTES`] at a time
    /// (`probe-stack`), so a frame larger than that still faults in its fiber's guard: a stack
    /// overflow is always the runtime's stack stop, never a write past the guard.
    pub(crate) fn emit_object(&self, machine: &TargetMachine) -> Result<Vec<u8>, String> {
        if !machine.optimize {
            unsafe extern "C" {
                fn hirsel_llvm_promote_development_slots(module: LLVMModuleRef);
            }
            // SAFETY: the pass mutates this live module without keeping references.
            unsafe { hirsel_llvm_promote_development_slots(self.raw) };
        }
        let interval = STACK_PROBE_BYTES.to_string();
        // SAFETY: each function belongs to this live module; LLVM copies the strings.
        unsafe {
            let attribute = |name: &str, value: &str| {
                LLVMCreateStringAttribute(
                    self.context.raw,
                    name.as_ptr().cast(),
                    name.len() as u32,
                    value.as_ptr().cast(),
                    value.len() as u32,
                )
            };
            let attributes = [
                attribute("probe-stack", "inline-asm"),
                attribute("stack-probe-size", &interval),
            ];
            let mut function = LLVMGetFirstFunction(self.raw);
            while !function.is_null() {
                if LLVMCountBasicBlocks(function) != 0 {
                    for attribute in attributes {
                        LLVMAddAttributeAtIndex(
                            function,
                            llvm_sys::LLVMAttributeFunctionIndex,
                            attribute,
                        );
                    }
                }
                function = LLVMGetNextFunction(function);
            }
        }
        let mut message = ptr::null_mut();
        let mut buffer: LLVMMemoryBufferRef = ptr::null_mut();
        // SAFETY: the module and machine are live; LLVM writes either an error message or a
        // buffer that we own.
        let failed = unsafe {
            LLVMTargetMachineEmitToMemoryBuffer(
                machine.raw,
                self.raw,
                LLVMCodeGenFileType::LLVMObjectFile,
                &mut message,
                &mut buffer,
            )
        };
        // SAFETY: the message is null or ours to dispose of.
        let message = unsafe { take_message(message) };
        if failed != 0 || buffer.is_null() {
            return Err(message);
        }
        // SAFETY: the buffer is live and owned here; its bytes are copied before disposal.
        unsafe {
            let start = LLVMGetBufferStart(buffer).cast::<u8>();
            let bytes = std::slice::from_raw_parts(start, LLVMGetBufferSize(buffer)).to_vec();
            LLVMDisposeMemoryBuffer(buffer);
            Ok(bytes)
        }
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        // SAFETY: LLVM returns an owned message for this live module.
        unsafe { take_message(LLVMPrintModuleToString(self.raw)) }
    }
}

impl Drop for Module<'_> {
    fn drop(&mut self) {
        // SAFETY: the module is live and owned; its context outlives it.
        unsafe { LLVMDisposeModule(self.raw) };
    }
}
/// An owned `LLVMTargetMachine` for one target.
pub(crate) struct TargetMachine {
    raw: LLVMTargetMachineRef,
    triple: String,
    optimize: bool,
}

impl TargetMachine {
    pub(crate) fn core(target: hirsel_core_layout::Target, optimize: bool) -> Result<Self, String> {
        Self::with_settings(target.triple(), target.cpu(), "", optimize)
    }

    fn with_settings(
        triple: &str,
        cpu: &str,
        features: &str,
        optimize: bool,
    ) -> Result<Self, String> {
        initialize_targets();
        let triple_text = triple;
        let triple = c_string(triple);
        let cpu = c_string(cpu);
        let features = c_string(features);
        let mut llvm_target: LLVMTargetRef = ptr::null_mut();
        let mut message = ptr::null_mut();
        // SAFETY: the triple is NUL-terminated; LLVM writes a target or a message.
        let failed =
            unsafe { LLVMGetTargetFromTriple(triple.as_ptr(), &mut llvm_target, &mut message) };
        // SAFETY: the message is null or ours to dispose of.
        let message = unsafe { take_message(message) };
        if failed != 0 {
            return Err(message);
        }
        let level = if optimize {
            LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive
        } else {
            LLVMCodeGenOptLevel::LLVMCodeGenLevelNone
        };
        // SAFETY: the target is registered and the strings are NUL-terminated; LLVM copies them.
        let raw = unsafe {
            LLVMCreateTargetMachine(
                llvm_target,
                triple.as_ptr(),
                cpu.as_ptr(),
                features.as_ptr(),
                level,
                LLVMRelocMode::LLVMRelocPIC,
                LLVMCodeModel::LLVMCodeModelDefault,
            )
        };
        if raw.is_null() {
            return Err(format!(
                "LLVM could not create a target machine for {}",
                triple_text
            ));
        }
        Ok(Self {
            raw,
            triple: triple_text.into(),
            optimize,
        })
    }
}

impl Drop for TargetMachine {
    fn drop(&mut self) {
        // SAFETY: the machine is live and owned.
        unsafe { LLVMDisposeTargetMachine(self.raw) };
    }
}
