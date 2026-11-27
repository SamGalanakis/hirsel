//! Darwin: the kernel holds a worker's caps. It has no parent-death signal, so a worker
//! whose supervisor is gone runs to its processor cap.

fn limit(resource: libc::c_int, soft: u64, hard: u64) -> std::io::Result<()> {
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
    limit(libc::RLIMIT_FSIZE, file, file)
}
