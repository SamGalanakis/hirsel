//! Startup refusals: what stops a run before main, and their `not started: ` lines.
use super::{DirectoryOperations, OsProvider, View, bytes, io};
use hirsel_core_ids::launch::{FIRST_INHERITED_FD, INHERITED_SLOT};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

pub(crate) enum StartupError {
    Io(io::Error),
    /// A directory grant that could not be opened: its path as configured,
    /// and the path it resolves to when that is relative.
    Directory(std::path::PathBuf, io::Error),
    /// Every grant main's parameters need and the run cannot give, in
    /// parameter order: the run names them all at once.
    Missing(Vec<Missing>),
    NetRules,
    UnixProbe(io::Error),
    EnvPatterns,
    EnvSecretOverlap(String),
    UnsupportedHostOpenat2(io::Error),
    Exec(String),
    /// A process argument that is not UTF-8, by its index in the process
    /// argv: the program name is 0 and private options are counted.
    NonUtf8Arg(usize),
}
/// One parameter of main that the run cannot grant, by its index.
#[derive(Clone, Copy)]
pub(crate) enum Missing {
    /// The run's configuration has no grant for it.
    Grant(usize),
    /// Its Secret grant has no readable nonempty source.
    Secret(usize),
}
impl From<io::Error> for StartupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl StartupError {
    pub(super) fn into_io(self) -> io::Error {
        match self {
            Self::Io(error)
            | Self::Directory(_, error)
            | Self::UnsupportedHostOpenat2(error)
            | Self::UnixProbe(error) => error,
            Self::Exec(reason) => io::Error::other(reason),
            Self::NonUtf8Arg(_) => io::Error::from_raw_os_error(libc::EILSEQ),
            Self::Missing(_) | Self::NetRules | Self::EnvPatterns | Self::EnvSecretOverlap(_) => {
                io::Error::from_raw_os_error(libc::EACCES)
            }
        }
    }
}

pub(crate) unsafe fn report_startup(error: StartupError, stderr: i32, names: *const View) {
    let mut line = b"not started: ".to_vec();
    match error {
        StartupError::UnixProbe(error) => {
            line.extend_from_slice(format!("{error}\n").as_bytes());
        }
        StartupError::NetRules => {
            line.extend_from_slice(b"Net requires --grant ");
            line.extend_from_slice(hirsel_core_net::RULE_SYNTAX.as_bytes());
            line.push(b'\n');
        }
        StartupError::EnvPatterns => {
            line.extend_from_slice(hirsel_core_net::EnvPatterns::SYNTAX.as_bytes());
            line.push(b'\n');
        }
        StartupError::EnvSecretOverlap(name) => {
            line.extend_from_slice(
                format!("Env grant covers Secret source variable {name:?}\n").as_bytes(),
            );
        }
        StartupError::Missing(missing) => {
            // One line per missing grant, each with the `not started: ` prefix.
            line.clear();
            for missing in missing {
                let (index, reason): (_, &[u8]) = match missing {
                    Missing::Grant(index) => (index, b"is not in the run's configuration"),
                    Missing::Secret(index) => (index, b"[secret] has no readable nonempty source"),
                };
                line.extend_from_slice(b"not started: grant `");
                // SAFETY: emitter supplies one valid UTF-8 name per main parameter.
                line.extend_from_slice(unsafe { bytes(*names.add(index)) });
                line.extend_from_slice(b"` ");
                line.extend_from_slice(reason);
                line.push(b'\n');
            }
        }
        StartupError::UnsupportedHostOpenat2(_) => {
            line.extend_from_slice(
                b"Dir requires openat2 (Linux 5.6 or later, permitted by the sandbox)\n",
            );
        }
        StartupError::Exec(reason) => {
            line.extend_from_slice(reason.as_bytes());
            line.push(b'\n');
        }
        StartupError::NonUtf8Arg(n) => {
            line.extend_from_slice(format!("argument {n} is not valid UTF-8\n").as_bytes());
        }
        StartupError::Io(error) => {
            line.extend_from_slice(
                format!("host grants could not be configured: {error}\n").as_bytes(),
            );
        }
        StartupError::Directory(path, error) => {
            line.extend_from_slice(b"directory grant `");
            line.extend_from_slice(path.as_os_str().as_bytes());
            line.push(b'`');
            if path.is_relative()
                && let Ok(cwd) = std::env::current_dir()
            {
                line.extend_from_slice(b" (");
                line.extend_from_slice(cwd.join(path).as_os_str().as_bytes());
                line.push(b')');
            }
            line.extend_from_slice(format!(": {error}\n").as_bytes());
        }
    }
    // SAFETY: the configured diagnostic fd and line live through this write.
    unsafe { crate::os::diagnostic(stderr, line.as_ptr(), line.len()) };
}
/// Own the descriptor a launch passed for an inherited slot, and keep it from
/// the run's children. A slot the launch did not pass is EBADF.
pub(crate) fn take_inherited(fd: i32) -> io::Result<OwnedFd> {
    // SAFETY: fcntl only marks the descriptor; ownership is taken once it is known open.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the launch transferred this slot's descriptor to the run.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
/// Open the directory grant `index`, configured as `slot`: a path, or
/// [`INHERITED_SLOT`] for the directory the launch passed at its descriptor.
pub(super) fn grant_directory(
    os: &mut OsProvider,
    operations: &DirectoryOperations,
    slot: &[u8],
    index: usize,
) -> Result<u64, StartupError> {
    if slot == INHERITED_SLOT.as_bytes() {
        let fd = FIRST_INHERITED_FD + index as i32;
        return take_inherited(fd)
            .and_then(|dir| (operations.adopt)(os, dir))
            .map_err(|error| StartupError::Directory(format!("/dev/fd/{fd}").into(), error));
    }
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(slot));
    (operations.grant)(os, path).map_err(|error| StartupError::Directory(path.to_owned(), error))
}
