//! Linux: the kernel holds a worker's caps and ends a worker whose supervisor is gone.

fn limit(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: setrlimit reads one record.
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Run in the worker between `fork` and `exec`: `cpu` seconds of processor time, no
/// file longer than `file` bytes, no core dump.
pub(crate) fn confine(cpu: u64, file: u64) -> std::io::Result<()> {
    limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
    limit(libc::RLIMIT_CORE, 0, 0)?;
    limit(libc::RLIMIT_FSIZE, file, file)?;
    // SAFETY: prctl with this option reads its one integer argument.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
