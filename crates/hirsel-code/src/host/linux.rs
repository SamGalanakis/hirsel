//! Linux: a whole RX text band and a loader-only RW view, and `membarrier` to make every
//! running thread of the process see new code (PACK-4).

use super::Access;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

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
fn reserve_bands(bytes: usize) -> io::Result<usize> {
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

/// Map the fixed-size file beside data for execution and at an independent VA for writes.
/// Both views exclude fork children before either can write or execute.
pub(crate) fn reserve(bytes: usize) -> io::Result<(usize, usize)> {
    const MFD_EXEC: libc::c_uint = 0x10;
    let flags = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
    // SAFETY: the name is terminated and successful descriptors are adopted exactly once.
    let fd = unsafe {
        let mut fd = libc::memfd_create(c"hirsel-text".as_ptr(), flags | MFD_EXEC);
        if fd < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
            fd = libc::memfd_create(c"hirsel-text".as_ptr(), flags);
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedFd::from_raw_fd(fd)
    };
    let fd = fd.as_raw_fd();
    // SAFETY: the fd is owned and the length fits off_t.
    unsafe {
        done(libc::ftruncate(fd, bytes as libc::off_t).into())?;
        done(
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW,
            )
            .into(),
        )?;
    }
    let base = reserve_bands(2 * bytes)?;
    let mut writer = std::ptr::null_mut();
    // SAFETY: MAP_FIXED replaces only our own reservation. Both aliases start inaccessible.
    let mapped = (|| unsafe {
        let rx = libc::mmap(
            base as *mut _,
            bytes,
            libc::PROT_NONE,
            libc::MAP_SHARED | libc::MAP_FIXED,
            fd,
            0,
        );
        if rx == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        writer = libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_NONE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        if writer == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        for at in [rx, writer] {
            for advice in [libc::MADV_DONTFORK, libc::MADV_DONTDUMP] {
                done(libc::madvise(at, bytes, advice).into())?;
            }
        }
        // READ_IMPLIES_EXEC must be ruled out before requesting a writable mapping.
        done(libc::mprotect(writer, bytes, libc::PROT_READ).into())?;
        if map_permissions(writer as usize)? != "r--s" {
            return Err(io::ErrorKind::Unsupported.into());
        }
        done(libc::mprotect(rx, bytes, libc::PROT_READ | libc::PROT_EXEC).into())?;
        done(libc::mprotect(writer, bytes, libc::PROT_READ | libc::PROT_WRITE).into())?;
        done(
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SEAL,
            )
            .into(),
        )?;
        if mappings(base, writer as usize, bytes) != Some((2, 0)) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        #[cfg(target_arch = "aarch64")]
        check_cache_access(fd)?;
        Ok((base, writer as usize))
    })();
    if mapped.is_err() {
        release(base, 2 * bytes);
        if !writer.is_null() && writer != libc::MAP_FAILED {
            release(writer as usize, bytes);
        }
    }
    mapped
}

pub(crate) fn release(at: usize, bytes: usize) {
    // SAFETY: the caller reserved exactly this range and nothing uses it now.
    unsafe { libc::munmap(at as *mut libc::c_void, bytes) };
}

pub(crate) fn protect(at: usize, bytes: usize, access: Access) -> io::Result<()> {
    let protection = match access {
        Access::None => libc::PROT_NONE,
        Access::Write => libc::PROT_READ | libc::PROT_WRITE,
        Access::Read => libc::PROT_READ,
    };
    // SAFETY: the range is inside the caller's reservation.
    done(unsafe { libc::mprotect(at as *mut libc::c_void, bytes, protection) }.into())
}

/// Give the pages of a range back; it reads as zero when next mapped writable.
pub(crate) fn discard(at: usize, bytes: usize) {
    // SAFETY: the range is inside the caller's reservation and holds no live image.
    unsafe { libc::madvise(at as *mut libc::c_void, bytes, libc::MADV_DONTNEED) };
}

pub(crate) fn write(work: &mut dyn FnMut()) {
    work()
}

#[cfg(target_arch = "x86_64")]
fn synchronize(_writer: usize, _rx: usize, _bytes: usize) {}

#[cfg(target_arch = "aarch64")]
fn cache_type() -> usize {
    let control;
    // SAFETY: Linux provides the system-safe CTR_EL0 value to userspace.
    unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) control) };
    control
}

/// IC IVAU can fault on RX with FEAT_CMOW. Probe that permission once on a disposable
/// RX view. A fork child inherits neither slab view; it never returns into Rust.
#[cfg(target_arch = "aarch64")]
fn check_cache_access(fd: libc::c_int) -> io::Result<()> {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if cache_type() & (1 << 29) != 0 {
        return Ok(()); // DIC dispenses with IC IVAU.
    }
    let supported = SUPPORTED.get_or_init(|| {
        // SAFETY: the child uses only async-signal-safe operations and immediately exits.
        unsafe {
            let length = page();
            let pid = libc::fork();
            if pid == 0 {
                libc::signal(libc::SIGSEGV, libc::SIG_DFL);
                libc::signal(libc::SIGBUS, libc::SIG_DFL);
                libc::signal(libc::SIGILL, libc::SIG_DFL);
                libc::setrlimit(
                    libc::RLIMIT_CORE,
                    &libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    },
                );
                let at = libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ | libc::PROT_EXEC,
                    libc::MAP_SHARED,
                    fd,
                    0,
                );
                if at == libc::MAP_FAILED {
                    libc::_exit(1);
                }
                std::arch::asm!("ic ivau, {}", "dsb ish", "isb", in(reg) at);
                libc::_exit(0);
            }
            let mut status = 0;
            if pid <= 0 {
                return false;
            }
            loop {
                if libc::waitpid(pid, &mut status, 0) == pid {
                    return libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
                }
                if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return false;
                }
            }
        }
    });
    if *supported {
        Ok(())
    } else {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(any(target_arch = "aarch64", test))]
fn lines(at: usize, bytes: usize, stride: usize) -> impl Iterator<Item = usize> {
    ((at & !(stride - 1))..at + bytes).step_by(stride)
}

#[cfg(any(target_arch = "aarch64", test))]
fn cache_lines(
    writer: usize,
    rx: usize,
    bytes: usize,
    control: usize,
) -> (impl Iterator<Item = usize>, impl Iterator<Item = usize>) {
    (
        lines(writer, bytes, 4 << ((control >> 16) & 15)),
        lines(rx, bytes, 4 << (control & 15)),
    )
}

#[cfg(test)]
pub(crate) fn check_alias_lines() {
    let (clean, invalidate) = cache_lines(0x102c, 0x802c, 96, (4 << 16) | 3);
    assert_eq!(clean.collect::<Vec<_>>(), [0x1000, 0x1040, 0x1080]);
    assert_eq!(
        invalidate.collect::<Vec<_>>(),
        [0x8020, 0x8040, 0x8060, 0x8080]
    );
}

#[cfg(target_arch = "aarch64")]
fn synchronize(writer: usize, rx: usize, bytes: usize) {
    use std::arch::asm;
    let control = cache_type();
    let (clean, invalidate) = cache_lines(writer, rx, bytes, control);
    // SAFETY: disjoint aliases of the same normal cacheable memfd; CTR gives the
    // independent minimum strides. Linux broadcasts IC IVAU to the shareability domain.
    unsafe {
        if control & (1 << 28) == 0 {
            for line in clean {
                asm!("dc cvau, {}", in(reg) line);
            }
        }
        asm!("dsb ish");
        if control & (1 << 29) == 0 {
            for line in invalidate {
                asm!("ic ivau, {}", in(reg) line);
            }
        }
        asm!("dsb ish", "isb");
    }
}

/// Complete publication or retirement after the final store.
pub(crate) fn publish(writer: usize, rx: usize, bytes: usize) -> io::Result<()> {
    synchronize(writer, rx, bytes);
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

/// Read actual file identity, offsets, permissions and extents of both code views.
pub(crate) fn mappings(rx: usize, rw: usize, bytes: usize) -> Option<(usize, usize)> {
    if bytes == 0 {
        return Some((0, 0));
    }
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    let mut views = [0; 2];
    let mut identity = None;
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let (start, end) = fields.next()?.split_once('-')?;
        let start = usize::from_str_radix(start, 16).ok()?;
        let end = usize::from_str_radix(end, 16).ok()?;
        for (i, (base, perms)) in [(rx, "r-xs"), (rw, "rw-s")].into_iter().enumerate() {
            if start >= base + bytes || end <= base {
                continue;
            }
            if start != base + views[i]
                || end > base + bytes
                || fields.next()? != perms
                || usize::from_str_radix(fields.next()?, 16).ok()? != start - base
            {
                return None;
            }
            let backing = (fields.next()?, fields.next()?);
            if backing.1 == "0" || identity.is_some_and(|id| id != backing) {
                return None;
            }
            identity = Some(backing);
            views[i] += end - start;
            break;
        }
    }
    if views != [bytes; 2] {
        return None;
    }
    let count = maps
        .lines()
        .filter(|line| {
            let mut f = line.split_whitespace().skip(3);
            f.next().zip(f.next()) == identity
        })
        .count();
    Some((count, 0))
}

fn map_permissions(at: usize) -> io::Result<String> {
    for line in std::fs::read_to_string("/proc/self/maps")?.lines() {
        let Some((range, perms)) = line.split_once(' ') else {
            continue;
        };
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        if let (Ok(start), Ok(end)) = (
            usize::from_str_radix(start, 16),
            usize::from_str_radix(end, 16),
        ) && (start..end).contains(&at)
        {
            return Ok(perms[..4].to_owned());
        }
    }
    Err(io::ErrorKind::NotFound.into())
}

#[cfg(test)]
pub(crate) fn protection(at: usize) -> String {
    map_permissions(at).unwrap()
}

/// Whether this process can have a slab; the laws of one run only where it can.
#[cfg(test)]
pub(crate) fn slab() -> bool {
    true
}

/// The protection letters of a published text run and of one being filled, and the
/// signal a jump into a recycled run raises.
#[cfg(test)]
pub(crate) const LIVE: &str = "r-xs";
#[cfg(test)]
pub(crate) const OPEN: &str = "r-xs";
#[cfg(test)]
pub(crate) const FAULT: i32 = if cfg!(target_arch = "x86_64") {
    libc::SIGTRAP
} else {
    libc::SIGILL
};

/// The faulting code address a signal handler was given.
#[cfg(test)]
pub(crate) fn pc(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) -> usize {
    #[cfg(target_arch = "x86_64")]
    if signal == libc::SIGTRAP {
        // SAFETY: hardware INT3 saves RIP immediately after its one-byte instruction.
        return unsafe {
            (*context.cast::<libc::ucontext_t>()).uc_mcontext.gregs[libc::REG_RIP as usize] as usize
                - 1
        };
    }
    let _ = (signal, context);
    // SAFETY: a synchronous SIGILL reports its instruction address.
    unsafe { (*info).si_addr() as usize }
}
