//! The driver's Linux host mechanisms.
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::Command,
};

/// Keep the checked image across an atomic replacement of its path.
pub(crate) fn release_command(file: &File, executable: &Path) -> Command {
    let mut command = Command::new(format!("/proc/self/fd/{}", file.as_raw_fd()));
    command.arg0(executable);
    command
}

/// Create `to` as a copy of `from`: a reflink (`FICLONE`) where the file
/// system shares extents, else a byte copy into the same new inode.
pub(crate) fn clone_file(from: &mut File, to: &Path) -> io::Result<()> {
    let mut to = File::create_new(to)?;
    // SAFETY: FICLONE borrows both descriptors for the call.
    if unsafe { libc::ioctl(to.as_raw_fd(), libc::FICLONE, from.as_raw_fd()) } != 0 {
        io::copy(from, &mut to)?;
    }
    Ok(())
}
