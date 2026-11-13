//! Shared x86-64/AArch64 stack-switch mechanics, moved from hirsel-rt.
//! References: corosensei src/arch/{x86_64,aarch64}.rs and Boost.Context
//! jump_{x86_64_sysv,arm64_aapcs}_elf_gas.S. Scheduling is caller-owned.
#[cfg(test)]
hirsel_guard::install!();
pub type Entry = unsafe extern "C" fn(*mut u8, *mut u8) -> !;
#[cfg(target_arch = "x86_64")]
pub fn initial_frame(entry: Entry, first: *mut u8, second: *mut u8) -> [u64; 8] {
    // From the stack pointer up: the control words fiber_switch restores (MXCSR's default
    // 0x1F80 and the x87 control word's 0x037F), r15, r14, r13, r12, rbx, rbp, the return
    // address. After the return the stack pointer is 16-aligned, as `fiber_start`'s call needs.
    [
        0x037F_u64 << 32 | 0x1F80,
        0,
        entry as usize as u64,
        second as u64,
        first as u64,
        0,
        0,
        fiber_start as usize as u64,
    ]
}

#[cfg(target_arch = "x86_64")]
macro_rules! x86_switch {
    ($name:ident, $update:literal) => {
        /// # Safety
        /// Save slot belongs to the active stack; target is a live suspended frame.
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name(_save: *mut *mut u8, _to: *mut u8) {
            core::arch::naked_asm!(
                $update,
                "push rbp",
                "push rbx",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "sub rsp, 8",
                "stmxcsr [rsp]",
                "fnstcw [rsp + 4]",
                "mov [rdi], rsp",
                "mov rsp, rsi",
                "ldmxcsr [rsp]",
                "fldcw [rsp + 4]",
                "add rsp, 8",
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop rbx",
                "pop rbp",
                "ret",
            )
        }
    };
}
#[cfg(target_arch = "x86_64")]
x86_switch!(switch, "");
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn fiber_start() {
    core::arch::naked_asm!("mov rdi, r12", "mov rsi, r13", "call r14", "ud2")
}

#[cfg(target_arch = "aarch64")]
pub fn initial_frame(entry: Entry, first: *mut u8, second: *mut u8) -> [u64; 20] {
    // From the stack pointer up: x19 .. x28, x29 (frame pointer), x30 (return address),
    // d8 .. d15. x19, x20 and x21 carry the entry's arguments and the entry.
    let mut words = [0u64; 20];
    words[0] = first as u64;
    words[1] = second as u64;
    words[2] = entry as usize as u64;
    words[11] = fiber_start as usize as u64;
    words
}

/// # Safety
/// Save slot belongs to the active stack; target is a live suspended frame.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
pub unsafe extern "C" fn switch(_save: *mut *mut u8, _to: *mut u8) {
    core::arch::naked_asm!(
        "sub sp, sp, #160",
        "stp x19, x20, [sp, #0]",
        "stp x21, x22, [sp, #16]",
        "stp x23, x24, [sp, #32]",
        "stp x25, x26, [sp, #48]",
        "stp x27, x28, [sp, #64]",
        "stp x29, x30, [sp, #80]",
        "stp d8, d9, [sp, #96]",
        "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]",
        "stp d14, d15, [sp, #144]",
        "mov x2, sp",
        "str x2, [x0]",
        "mov sp, x1",
        "ldp x19, x20, [sp, #0]",
        "ldp x21, x22, [sp, #16]",
        "ldp x23, x24, [sp, #32]",
        "ldp x25, x26, [sp, #48]",
        "ldp x27, x28, [sp, #64]",
        "ldp x29, x30, [sp, #80]",
        "ldp d8, d9, [sp, #96]",
        "ldp d10, d11, [sp, #112]",
        "ldp d12, d13, [sp, #128]",
        "ldp d14, d15, [sp, #144]",
        "add sp, sp, #160",
        "ret",
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn fiber_start() {
    core::arch::naked_asm!("mov x0, x19", "mov x1, x20", "blr x21", "brk #0")
}
