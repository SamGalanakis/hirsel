//! Host limits used for scheduler startup admission.
use std::io;

pub(crate) const HAS_IO_URING: bool = cfg!(target_os = "linux");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostEnvelope {
    pub(crate) page_bytes: usize,
    pub(crate) allowed_cpus: usize,
    pub(crate) memory_limit: Option<u64>,
    pub(crate) address_space_limit: Option<u64>,
    pub(crate) thread_limit: Option<usize>,
    pub(crate) memlock_limit: Option<u64>,
}

impl HostEnvelope {
    pub(crate) fn observe() -> io::Result<Self> {
        // SAFETY: sysconf needs no storage or initialized process state.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_bytes = usize::try_from(page)
            .ok()
            .filter(|p| p.is_power_of_two() && (4096..=65536).contains(p))
            .ok_or_else(|| io::Error::other(format!("unsupported host page size {page}")))?;
        let rlimit = |resource| {
            let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            // SAFETY: getrlimit fills the structure on success.
            let limit = unsafe {
                if libc::getrlimit(resource, limit.as_mut_ptr()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                limit.assume_init().rlim_cur
            };
            Ok((limit != libc::RLIM_INFINITY).then_some(limit))
        };
        let address_space_limit = rlimit(libc::RLIMIT_AS)?;
        let mut memory_limit = min_limit(rlimit(libc::RLIMIT_DATA)?, address_space_limit);
        let mut thread_limit = rlimit(libc::RLIMIT_NPROC)?.and_then(|n| usize::try_from(n).ok());
        let memlock_limit = if cfg!(target_os = "linux") {
            rlimit(libc::RLIMIT_MEMLOCK)?
        } else {
            None
        };
        if let Some(limits) = cgroup_limits() {
            memory_limit = min_limit(memory_limit, limits.0);
            thread_limit = min_limit(thread_limit, limits.1);
        }
        Ok(Self {
            page_bytes,
            allowed_cpus: std::thread::available_parallelism().map_or(1, usize::from),
            memory_limit,
            address_space_limit,
            thread_limit,
            memlock_limit,
        })
    }

    #[cfg(test)]
    pub(crate) const UNLIMITED: Self = Self {
        page_bytes: 4096,
        allowed_cpus: 1,
        memory_limit: None,
        address_space_limit: None,
        thread_limit: None,
        memlock_limit: None,
    };
}

pub(crate) fn min_limit<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(target_os = "linux")]
fn cgroup_limits() -> Option<(Option<u64>, Option<usize>)> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let membership = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let directory = hirsel_core_ids::cgroup::resolve(&mountinfo, &membership).ok()?;
    let mut memory = None;
    let mut threads = None;
    // An ancestor can be tighter than the leaf. "max" has no numeric value.
    for path in directory.ancestors() {
        let read = |name| std::fs::read_to_string(path.join(name)).ok();
        if let Some(maximum) = read("memory.max").and_then(|s| s.trim().parse().ok()) {
            memory = min_limit(memory, Some(maximum));
        }
        if let Some(maximum) = read("pids.max").and_then(|s| s.trim().parse::<usize>().ok())
            && let Some(current) = read("pids.current").and_then(|s| s.trim().parse().ok())
        {
            threads = min_limit(threads, Some(maximum.saturating_sub(current)));
        }
    }
    Some((memory, threads))
}

#[cfg(not(target_os = "linux"))]
fn cgroup_limits() -> Option<(Option<u64>, Option<usize>)> {
    None
}
