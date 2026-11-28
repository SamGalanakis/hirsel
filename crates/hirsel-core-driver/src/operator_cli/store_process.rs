//! Pre-opened source and destination grants for the installed Store helper.
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Command, Stdio},
};

pub(crate) fn command(
    helper: &str,
    source: &File,
    destination: &File,
) -> Result<Command, Box<dyn std::error::Error>> {
    let mut command = Command::new(super::installed(helper)?);
    command.stdin(Stdio::null());
    let fds = [source.as_raw_fd(), destination.as_raw_fd()];
    // SAFETY: Only async-signal-safe descriptor operations run between fork and exec.
    // Stage both before placing either on the potentially overlapping grant descriptors.
    unsafe {
        command.pre_exec(move || {
            let a = libc::fcntl(fds[0], libc::F_DUPFD_CLOEXEC, 10);
            let b = libc::fcntl(fds[1], libc::F_DUPFD_CLOEXEC, 10);
            if a < 0 || b < 0 || libc::dup2(a, 3) < 0 || libc::dup2(b, 4) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}
