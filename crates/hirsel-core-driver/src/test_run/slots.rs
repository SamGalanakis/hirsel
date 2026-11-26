//! Directory and Exec-workdir grants passed to a test as inherited descriptors.
use crate::NativeOptions;
use hirsel_core_ids::launch::{FIRST_INHERITED_FD, INHERITED_SLOT};
use serde::{Deserialize, Serialize};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The absolute directories and Exec workdirs a test launch grants, each at
/// its descriptor. Each is sealed into the test executable as
/// [`INHERITED_SLOT`] and passed at its slot's descriptor, so the executable
/// names no place and the same sources build the same test wherever they are.
/// A relative directory stays a path: it names the run's private directory.
/// The driver that builds the test chooses the slots; the process that runs
/// it opens them ([`InheritedSlots::open`]).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct InheritedSlots(Vec<(PathBuf, i32)>);

impl InheritedSlots {
    /// A directory that cannot be opened here stays a path, so the test's own
    /// start refuses it as any launch's does.
    pub(crate) fn take(options: &mut NativeOptions) -> Self {
        let mut slots = Vec::new();
        let mut inherit = |path: &Path, slot: usize| {
            (path.is_absolute() && open_directory(path).is_ok()).then(|| {
                slots.push((path.to_owned(), FIRST_INHERITED_FD + slot as i32));
            })
        };
        for (slot, directory) in options.directories.iter_mut().enumerate() {
            if inherit(Path::new(directory), slot).is_some() {
                *directory = INHERITED_SLOT.to_owned();
            }
        }
        let first = options.directories.len();
        for (slot, grant) in options.exec_grants.iter_mut().enumerate() {
            if let Some(workdir) = &mut grant.workdir
                && inherit(workdir, first + slot).is_some()
            {
                *workdir = PathBuf::from(INHERITED_SLOT);
            }
        }
        Self(slots)
    }

    /// Open each slot's directory and give `command`'s child each at its
    /// descriptor.
    pub(crate) fn pass(&self, command: &mut Command) -> io::Result<()> {
        use std::os::unix::process::CommandExt;
        if self.0.is_empty() {
            return Ok(());
        }
        // Above every target, so moving one slot never overwrites another's source.
        // Slots sealed as paths still take their numbers, so the targets can be sparse.
        let above = self
            .0
            .iter()
            .map(|(_, target)| target.saturating_add(1))
            .max()
            .unwrap_or(FIRST_INHERITED_FD);
        let fds = self
            .0
            .iter()
            .map(|(path, target)| {
                let directory = open_directory(path).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("directory grant {}: {error}", path.display()),
                    )
                })?;
                Ok((duplicate_above(directory.as_fd(), above)?, *target))
            })
            .collect::<io::Result<Vec<_>>>()?;
        // SAFETY: the child only calls dup2, which is async-signal-safe, on
        // descriptors this closure owns until the command is dropped.
        unsafe {
            command.pre_exec(move || {
                for (fd, target) in &fds {
                    if libc::dup2(fd.as_raw_fd(), *target) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        Ok(())
    }
}

fn open_directory(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
}

/// `fd` duplicated at the lowest free descriptor not below `floor`.
fn duplicate_above(fd: BorrowedFd<'_>, floor: i32) -> io::Result<OwnedFd> {
    // SAFETY: fcntl duplicates a borrowed open descriptor into a fresh one.
    let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, floor) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_DUPFD_CLOEXEC returned a descriptor no one else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
