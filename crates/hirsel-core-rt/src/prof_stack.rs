//! Register and stack snapshots. Only the offline profiler interprets CFI.
use crate::prof::{self, Kind};
use std::cell::{Cell, RefCell};

const STACK_BYTES: usize = 32 * 1024;
const REGISTER_BYTES: usize = 4 * size_of::<u64>();

thread_local! {
    static HOST: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    static SNAPSHOT: RefCell<[u8; REGISTER_BYTES + STACK_BYTES]> =
        const { RefCell::new([0; REGISTER_BYTES + STACK_BYTES]) };
}

/// Called before the first task starts on the serial replay thread.
pub(crate) fn prepare() {
    if let Some(bounds) = crate::host::sys::stack_bounds() {
        HOST.set(bounds);
    }
}

#[inline(never)]
pub(crate) fn capture(kind: Kind, words: &[u64]) {
    let pc: usize;
    let sp: usize;
    let fp: usize;
    #[cfg(target_arch = "x86_64")]
    let lr = 0usize;
    #[cfg(target_arch = "aarch64")]
    let lr: usize;
    // SAFETY: this only copies registers, and PC names an instruction in this
    // function, so its unwind row describes this exact stack frame.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        std::arch::asm!(
            "lea {pc}, [rip + 2f]",
            "2:",
            "mov {sp}, rsp",
            "mov {fp}, rbp",
            pc = out(reg) pc, sp = out(reg) sp, fp = out(reg) fp,
            options(nomem, nostack, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        std::arch::asm!(
            "mov {lr}, x30",
            "adr {pc}, 2f",
            "2:",
            "mov {sp}, sp",
            "mov {fp}, x29",
            pc = out(reg) pc, sp = out(reg) sp, fp = out(reg) fp, lr = out(reg) lr,
            options(nomem, nostack, preserves_flags),
        );
    }

    let top = crate::fiber::profile_stack_top(sp).or_else(|| {
        let (base, end) = HOST.get();
        (base <= sp && sp < end).then_some(end)
    });
    let len = top.map_or(0, |end| (end - sp).min(STACK_BYTES));
    SNAPSHOT.with_borrow_mut(|buffer| {
        for (dst, value) in buffer.chunks_exact_mut(8).zip([pc, sp, fp, lr]) {
            dst.copy_from_slice(&(value as u64).to_le_bytes());
        }
        // SAFETY: the active stack is readable from its stack pointer to the
        // bound above. Assembly copies its underlying bytes into the initialized
        // buffer without making a Rust reference to stack padding or spill slots.
        unsafe {
            copy_bytes(buffer[REGISTER_BYTES..].as_mut_ptr(), sp, len);
        }
        prof::append(kind, words, &buffer[..REGISTER_BYTES + len]);
    });
}

#[inline]
unsafe fn copy_bytes(destination: *mut u8, source: usize, len: usize) {
    // SAFETY: the caller supplies live disjoint byte ranges and len can be zero.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        std::arch::asm!(
            "rep movsb",
            inout("rdi") destination => _, inout("rsi") source => _, inout("rcx") len => _,
            options(nostack, preserves_flags),
        );
        #[cfg(target_arch = "aarch64")]
        std::arch::asm!(
            "cbz {n}, 3f",
            "2:",
            "ldrb {byte:w}, [{src}], #1",
            "strb {byte:w}, [{dst}], #1",
            "subs {n}, {n}, #1",
            "b.ne 2b",
            "3:",
            dst = inout(reg) destination => _, src = inout(reg) source => _,
            n = inout(reg) len => _, byte = out(reg) _,
            options(nostack),
        );
    }
}
