//! Linux: the limit is the soft `RLIMIT_DATA`. The kernel refuses the
//! allocation that crosses it, counts heap, anonymous `mmap` and thread stacks
//! against it, and every child inherits it.
use std::process::Command;

/// The kernel holds the limit: children inherit it and thread stacks count.
#[cfg(test)]
pub(crate) const KERNEL_LIMIT: bool = true;

#[cfg(not(miri))]
fn data_limit() -> libc::rlimit {
    let mut current = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: getrlimit writes one record.
    unsafe { libc::getrlimit(libc::RLIMIT_DATA, &mut current) };
    current
}

#[cfg(miri)]
fn data_limit() -> libc::rlimit {
    libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    }
}

/// Limit the process to `requested` bytes, never above a limit it inherited.
/// The hard limit stays, so [`lift_for_sanitizer`] can restore it in a child.
pub(crate) fn set_limit(requested: u64) {
    #[cfg(miri)]
    {
        let _ = requested;
    }
    #[cfg(not(miri))]
    {
        let current = data_limit();
        let limited = libc::rlimit {
            rlim_cur: requested.min(current.rlim_cur),
            rlim_max: current.rlim_max,
        };
        // SAFETY: setrlimit reads one record.
        unsafe { libc::setrlimit(libc::RLIMIT_DATA, &limited) };
    }
}

/// The limit in force, or `None` when there is none.
pub(crate) fn limit() -> Option<u64> {
    let soft = data_limit().rlim_cur;
    (soft != libc::RLIM_INFINITY).then_some(soft)
}

/// Lift the soft limit to the hard one in `command`'s child alone.
pub(crate) fn lift_for_sanitizer(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: getrlimit and setrlimit are async-signal-safe system calls.
    unsafe {
        command.pre_exec(|| {
            let mut current = data_limit();
            current.rlim_cur = current.rlim_max;
            if libc::setrlimit(libc::RLIMIT_DATA, &current) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// The private writable memory the limit counts (`VmData`), for a thread
/// creation the limit may have refused. It allocates: the caller is not the
/// allocator.
pub(crate) fn data_used() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmData:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib << 10)
}

/// The process's resident peak (`VmHWM`), read without allocating: it tells
/// growth apart from reservations the data limit also counts.
pub(crate) fn resident_peak_kib() -> Option<u64> {
    let mut status = [0u8; 4096];
    // SAFETY: the path is NUL-terminated.
    let fd = unsafe {
        libc::open(
            c"/proc/self/status".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    let mut len = 0;
    while len < status.len() {
        // SAFETY: the buffer's tail is writable for the length passed.
        let n = unsafe { libc::read(fd, status[len..].as_mut_ptr().cast(), status.len() - len) };
        if n <= 0 {
            break;
        }
        len += n as usize;
    }
    // SAFETY: fd is open and owned here.
    unsafe { libc::close(fd) };
    let status = &status[..len];
    let at = status.windows(6).position(|w| w == b"VmHWM:")? + 6;
    let mut kib = None::<u64>;
    for &byte in &status[at..] {
        match byte {
            b' ' | b'\t' if kib.is_none() => {}
            b'0'..=b'9' => kib = Some(kib.unwrap_or(0) * 10 + u64::from(byte - b'0')),
            _ => break,
        }
    }
    kib
}
