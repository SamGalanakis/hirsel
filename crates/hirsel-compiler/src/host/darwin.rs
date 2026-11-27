//! Darwin: kernel resource caps for `Service::compile`, and fail-closed
//! `Unavailable` for `check_library` because XNU does not enforce `RLIMIT_DATA`
//! or `RLIMIT_AS` against `mmap`/`libmalloc` and public Darwin APIs provide no
//! unprivileged OS-level aggregate memory cap (`registry.md` §2).
//! Uses only standard POSIX/libc symbols so `darwin-import-law` stays clean.

use super::ConfinementSpec;
use std::convert::Infallible;
use std::io;
use std::process::Command;

#[cfg(test)]
pub(crate) fn missing_confinement() -> Option<&'static str> {
    Some("macOS host cannot enforce OS aggregate memory cap")
}

#[cfg(test)]
pub(crate) fn disable_cgroup_for_test(_disable: bool) {}

fn limit(resource: libc::c_int, soft: u64, hard: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: setrlimit reads one initialized record.
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Run in the worker between `fork` and `exec`: `cpu` seconds of processor time, no
/// file longer than `file` bytes, no core dump.
pub(crate) fn confine(cpu: u64, file: u64) -> io::Result<()> {
    limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
    limit(libc::RLIMIT_CORE, 0, 0)?;
    limit(libc::RLIMIT_FSIZE, file, file)
}

/// Prepared Darwin confinement for a `check_library` worker.
///
/// Uninhabited because macOS cannot enforce an OS aggregate memory cap without
/// disallowed private APIs (`registry.md` §2); `prepare` always fails closed
/// before spawning.
pub(crate) struct Confinement {
    _uninhabited: Infallible,
}

impl Confinement {
    pub(crate) fn prepare(spec: &ConfinementSpec<'_>) -> io::Result<Self> {
        let _ = spec.memory_bytes;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "macOS host cannot enforce OS aggregate memory cap",
        ))
    }

    pub(crate) fn command(&self, _worker_arg: &str) -> Command {
        match self._uninhabited {}
    }

    pub(crate) fn memory_limit_hit(&self) -> bool {
        match self._uninhabited {}
    }
}

// Darwin refuses before starting a worker.
pub(crate) fn seal_worker() -> std::io::Result<()> {
    Err(std::io::Error::other("compiler confinement unavailable"))
}
