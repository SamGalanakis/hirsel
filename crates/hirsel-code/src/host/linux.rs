//! Linux: anonymous pages whose protection changes, and `membarrier` to make every
//! running thread of the process see new code (PACK-4).

use super::Access;
use std::io;

#[cfg(target_arch = "x86_64")]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::X86_64Linux;
#[cfg(target_arch = "aarch64")]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Linux;

const MEMBARRIER_PRIVATE_EXPEDITED_SYNC_CORE: libc::c_int = 1 << 5;
const MEMBARRIER_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE: libc::c_int = 1 << 6;

fn done(result: libc::c_long) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn page() -> usize {
    // SAFETY: sysconf reads one integer.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

/// Register this process for the barrier `publish` raises. Registering twice is fine.
pub(crate) fn register() -> io::Result<()> {
    // SAFETY: membarrier takes three integers.
    done(unsafe {
        libc::syscall(
            libc::SYS_membarrier,
            MEMBARRIER_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE,
            0,
            0,
        )
    })
}

/// Reserve `bytes` of address space that nothing can touch.
pub(crate) fn reserve(bytes: usize) -> io::Result<usize> {
    // SAFETY: a fresh anonymous mapping at an address the kernel picks.
    let at = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    if at == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    Ok(at as usize)
}

pub(crate) fn release(at: usize, bytes: usize) {
    // SAFETY: the caller reserved exactly this range and nothing uses it now.
    unsafe { libc::munmap(at as *mut libc::c_void, bytes) };
}

pub(crate) fn protect(at: usize, bytes: usize, access: Access) -> io::Result<()> {
    let protection = match access {
        Access::None => libc::PROT_NONE,
        Access::Write | Access::Patch => libc::PROT_READ | libc::PROT_WRITE,
        Access::Read => libc::PROT_READ,
        Access::Execute => libc::PROT_READ | libc::PROT_EXEC,
    };
    // SAFETY: the range is inside the caller's reservation.
    done(unsafe { libc::mprotect(at as *mut libc::c_void, bytes, protection) }.into())
}

/// Give the pages of a range back; it reads as zero when next mapped writable.
pub(crate) fn discard(at: usize, bytes: usize) {
    // SAFETY: the range is inside the caller's reservation and holds no live image.
    unsafe { libc::madvise(at as *mut libc::c_void, bytes, libc::MADV_DONTNEED) };
}

/// Whether a discarded range may still hold its bytes when it is next mapped.
pub(crate) const KEEPS: bool = false;

/// Run `work`, which writes text runs mapped for `Access::Patch`.
pub(crate) fn write(work: &mut dyn FnMut()) {
    work()
}

#[cfg(target_arch = "x86_64")]
fn synchronize(_at: usize, _bytes: usize) {}

/// Instruction cache maintenance over a range (`dc cvau`, `ic ivau`, `isb`).
#[cfg(target_arch = "aarch64")]
fn synchronize(at: usize, bytes: usize) {
    use std::arch::asm;
    let control: u64;
    // SAFETY: reading the cache type register is allowed at this level.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) control) };
    let data = 4usize << ((control >> 16) & 15);
    let code = 4usize << (control & 15);
    let end = at + bytes;
    // SAFETY: the range is mapped readable; these instructions change no memory.
    unsafe {
        let mut line = at & !(data - 1);
        while line < end {
            asm!("dc cvau, {}", in(reg) line);
            line += data;
        }
        asm!("dsb ish");
        let mut line = at & !(code - 1);
        while line < end {
            asm!("ic ivau, {}", in(reg) line);
            line += code;
        }
        asm!("dsb ish", "isb");
    }
}

/// After the text run at `at` became executable: make every core fetch the new bytes.
pub(crate) fn publish(at: usize, bytes: usize) -> io::Result<()> {
    synchronize(at, bytes);
    // SAFETY: membarrier takes three integers.
    done(unsafe {
        libc::syscall(
            libc::SYS_membarrier,
            MEMBARRIER_PRIVATE_EXPEDITED_SYNC_CORE,
            0,
            0,
        )
    })
}

/// The protection letters of the mapping that holds `at`, as the kernel reports them.
#[cfg(test)]
pub(crate) fn protection(at: usize) -> String {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    for line in maps.lines() {
        let (range, rest) = line.split_once(' ').unwrap();
        let (start, end) = range.split_once('-').unwrap();
        let start = usize::from_str_radix(start, 16).unwrap();
        let end = usize::from_str_radix(end, 16).unwrap();
        if (start..end).contains(&at) {
            return rest[..4].to_owned();
        }
    }
    String::new()
}

/// Whether this host has a slab; the laws of one run only where it does.
#[cfg(test)]
pub(crate) const SLAB: bool = true;

/// The protection letters of a published text run and of one being filled, and the
/// signal a jump into a recycled run raises.
#[cfg(test)]
pub(crate) const LIVE: &str = "r-xp";
#[cfg(test)]
pub(crate) const OPEN: &str = "rw-p";
#[cfg(test)]
pub(crate) const FAULT: i32 = libc::SIGSEGV;

/// The faulting code address a signal handler was given.
#[cfg(test)]
pub(crate) fn pc(info: *mut libc::siginfo_t, _context: *mut libc::c_void) -> usize {
    // SAFETY: the kernel gave the handler this record.
    unsafe { (*info).si_addr() as usize }
}
