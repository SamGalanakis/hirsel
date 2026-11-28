//! The Linux host provider.
pub(crate) mod clock;
pub(crate) mod contained;
pub mod exec;
pub(crate) mod file;
pub(crate) mod path;
#[cfg(feature = "prof")]
pub(crate) mod prof_sampler;
pub(crate) mod stat;
pub(crate) mod sync;
pub(crate) mod sys;
pub(crate) mod unix;
pub(crate) mod warm;

#[cfg(all(test, target_arch = "x86_64"))]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::X86_64Linux;
#[cfg(all(test, target_arch = "aarch64"))]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Linux;

pub(crate) fn literal_file() -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: memfd_create returns a fresh descriptor on success.
    let fd = unsafe { libc::memfd_create(c"hirsel-literals".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(test)]
mod literal_tests;
