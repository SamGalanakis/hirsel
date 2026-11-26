mod attach;
mod capture;
mod launch;
mod perf;
mod procfs;

pub use attach::attach;
pub use capture::run;
pub(crate) use perf::monotonic_ns;
pub(crate) use procfs::{KernelSymbols, mapped_file, vdso};

#[cfg(target_arch = "x86_64")]
pub(crate) const TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(target_arch = "aarch64")]
pub(crate) const TARGET: &str = "aarch64-unknown-linux-gnu";
/// The clock of perf records and of the runtime's observations.
pub(crate) const CLOCK: &str = "CLOCK_MONOTONIC";

#[cfg(target_arch = "x86_64")]
pub(crate) fn kernel_address(pc: u64) -> bool {
    // The upper canonical half with five-level paging, except the legacy
    // userspace vsyscall page.
    pc >= 0xff00_0000_0000_0000 && !(0xffff_ffff_ff60_0000..0xffff_ffff_ff60_1000).contains(&pc)
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn kernel_address(pc: u64) -> bool {
    // A 52-bit TTBR1 range starts at the lowest kernel address.
    pc >= 0xfff0_0000_0000_0000
}
