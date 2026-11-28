//! Shared Darwin host mechanisms and macOS-only adapters.
pub(crate) mod clock;
pub(crate) mod contained;
#[cfg(target_os = "macos")]
pub mod exec;
pub(crate) mod file;
pub(crate) mod path;
#[cfg(feature = "prof")]
pub(crate) mod prof_sampler;
pub(crate) mod stat;
pub(crate) mod sync;
pub(crate) mod sys;
#[cfg(target_os = "macos")]
pub(crate) mod unix;
pub(crate) mod warm;

#[cfg(test)]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64Darwin;

pub(crate) fn literal_file() -> std::io::Result<std::fs::File> {
    use std::os::fd::BorrowedFd;
    // SAFETY: tmpfile creates an anonymous file. Duplicate its descriptor with
    // close-on-exec before fclose releases the stream's ownership.
    unsafe {
        let stream = libc::tmpfile();
        if stream.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let file = BorrowedFd::borrow_raw(libc::fileno(stream)).try_clone_to_owned();
        libc::fclose(stream);
        file.map(std::fs::File::from)
    }
}
