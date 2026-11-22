//! Bounded describe and preview child requests.
use std::{
    fmt,
    io::{self, Read},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

// Value codec: DescribeError tag u32 + Room.requested u64 + Room.available u64.
pub const PLACE_CAPACITY_ERR_LEN: u64 = 4 + 8 + 8;

#[derive(Debug)]
pub enum DescribeError {
    PlaceTimedOut,
    PlaceError(Vec<u8>),
    FailedExit(Option<i32>),
    ResponseTooLarge,
    Io(io::Error),
}
impl fmt::Display for DescribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlaceTimedOut => f.write_str("PlaceTimedOut"),
            Self::PlaceError(_) => f.write_str("place registration failed"),
            Self::FailedExit(status) => write!(f, "describe child failed: {status:?}"),
            Self::ResponseTooLarge => f.write_str("describe response exceeds n_place"),
            Self::Io(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for DescribeError {}
impl From<io::Error> for DescribeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct Running {
    child: Child,
    reaped: bool,
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Capture one Describe value, or the status-2 error codec. No output files are
/// written. Captured stdout is bounded by n_place or the fixed capacity error. The deadline covers the
/// complete child lifetime, including a full pipe and exit after stdout closes.
pub fn describe_child(
    executable: &Path,
    n_place: u64,
    timeout: Duration,
) -> Result<Vec<u8>, DescribeError> {
    child(executable, n_place, timeout, None, None, &[])
}

pub fn preview_child(
    executable: &Path,
    n_place: u64,
    timeout: Duration,
    old: &[u8],
) -> Result<Vec<u8>, DescribeError> {
    preview_child_at(executable, n_place, timeout, old, None, &[])
}

pub(crate) fn preview_child_at(
    executable: &Path,
    n_place: u64,
    timeout: Duration,
    old: &[u8],
    workdir: Option<&Path>,
    exec: &[String],
) -> Result<Vec<u8>, DescribeError> {
    if old.len() as u64 > n_place {
        return Err(DescribeError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "preview input exceeds n_place",
        )));
    }
    child(executable, n_place, timeout, Some(old), workdir, exec)
}

fn child(
    executable: &Path,
    n_place: u64,
    timeout: Duration,
    old: Option<&[u8]>,
    workdir: Option<&Path>,
    exec: &[String],
) -> Result<Vec<u8>, DescribeError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "place timeout is too large"))?;
    let limit = usize::try_from(n_place.max(PLACE_CAPACITY_ERR_LEN)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "n_place does not fit this host",
        )
    })?;
    let (kind, stdin) = if let Some(old) = old {
        use std::io::{Seek, Write};
        let mut input = tempfile::tempfile()?;
        input.write_all(old)?;
        input.rewind()?;
        ("--hirsel-preview", Stdio::from(input))
    } else {
        ("--hirsel-describe", Stdio::null())
    };
    let mut command = Command::new(std::path::absolute(executable)?);
    if let Some(workdir) = workdir {
        command.current_dir(workdir);
    }
    command
        .args(exec)
        .arg(kind)
        .arg(format!("--hirsel-n-place={n_place}"))
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .env_remove("LISTEN_PID")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_FDNAMES")
        .env_remove("LISTEN_PIDFDID");
    if std::env::var_os("LISTEN_FDS").is_some() {
        // SAFETY: fcntl changes only the fd flags in the forked preview child.
        // The CLI normally sealed this fd at startup. This also covers the
        // library entry and hosts without close_range.
        unsafe {
            command.pre_exec(|| {
                if libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EBADF) {
                        return Err(error);
                    }
                }
                Ok(())
            });
        }
    }
    let mut owner = Running {
        child: command.spawn()?,
        reaped: false,
    };
    let mut stdout = owner.child.stdout.take().expect("piped child stdout");
    let fd = stdout.as_raw_fd();
    // SAFETY: this owned pipe remains live through polling; only its flags change.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut output = Vec::new();
    let mut status = None;
    let mut eof = false;
    let mut scratch = [0; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err(DescribeError::PlaceTimedOut);
        }
        if !eof {
            match stdout.read(&mut scratch) {
                Ok(0) => eof = true,
                Ok(count) => {
                    if count > limit.saturating_sub(output.len()) {
                        return Err(DescribeError::ResponseTooLarge);
                    }
                    output
                        .try_reserve(count)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    output.extend_from_slice(&scratch[..count]);
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(error) => return Err(error.into()),
            }
        }
        if status.is_none() {
            status = owner.child.try_wait()?;
            owner.reaped = status.is_some();
        }
        if eof && let Some(status) = status {
            return match status.code() {
                Some(0) => Ok(output),
                Some(2) => Err(DescribeError::PlaceError(output)),
                code => Err(DescribeError::FailedExit(code)),
            };
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(10))
            .as_millis() as i32;
        let mut poll = libc::pollfd {
            fd: if eof { -1 } else { fd },
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll borrows one initialized descriptor entry, at most 10 ms.
        if unsafe { libc::poll(&mut poll, 1, wait) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn script(root: &Path, body: &str) -> std::path::PathBuf {
        let path = root.join("describe-child");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    #[test]
    fn place_run_bounded_kills_and_reaps_after_stdout_closes() {
        let root = tempfile::tempdir().unwrap();
        let child = script(
            root.path(),
            "echo $$ > \"$0.pid\"\nexec 1>&-\nexec python3 -c 'import time; time.sleep(2)'",
        );
        let start = Instant::now();
        assert!(matches!(
            describe_child(&child, 4096, Duration::from_millis(100)),
            Err(DescribeError::PlaceTimedOut)
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
        let pid = std::fs::read_to_string(root.path().join("describe-child.pid")).unwrap();
        assert!(
            !Path::new("/proc").join(pid.trim()).exists(),
            "child was not reaped"
        );
    }
    #[test]
    fn place_capacity_error_is_intact_at_zero_and_one() {
        let root = tempfile::tempdir().unwrap();
        let mut error = vec![0; PLACE_CAPACITY_ERR_LEN as usize];
        error[4..12].copy_from_slice(&8u64.to_le_bytes());
        let escaped = error
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect::<String>();
        let child = script(root.path(), &format!("printf '{escaped}'; exit 2"));
        for n_place in [0, 1] {
            match describe_child(&child, n_place, Duration::from_secs(1)) {
                Err(DescribeError::PlaceError(bytes)) => assert_eq!(bytes, error),
                other => panic!("n_place={n_place}: {other:?}"),
            }
        }
    }
    #[test]
    fn describe_child_drains_a_pipe_and_preserves_error_bytes() {
        let root = tempfile::tempdir().unwrap();
        let child = script(root.path(), "printf '%s' \"$1,$2\"; exit 2");
        match describe_child(&child, 100, Duration::from_secs(1)) {
            Err(DescribeError::PlaceError(bytes)) => {
                assert_eq!(bytes, b"--hirsel-describe,--hirsel-n-place=100")
            }
            other => panic!("{other:?}"),
        }
        let child = script(root.path(), "printf 'more than twenty bytes of output'");
        assert!(matches!(
            describe_child(&child, 1, Duration::from_secs(1)),
            Err(DescribeError::ResponseTooLarge)
        ));
    }
}
