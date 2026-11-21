use std::fs::{self, File};
use std::io::{self, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

pub(super) struct Place {
    child: Child,
    control: UnixStream,
    pidfile: Option<PathBuf>,
}

pub(super) struct Children(PathBuf);

pub(super) struct Launch<'a> {
    pub(super) executable: &'a Path,
    pub(super) workdir: &'a Path,
    pub(super) scratch: &'a Path,
    pub(super) listener: &'a TcpListener,
    pub(super) activation: u64,
    pub(super) n_place: u64,
    pub(super) args: &'a [String],
    pub(super) recording: Option<&'a super::recording::Recording>,
}

impl Children {
    pub(super) fn open(path: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&path)?;
        let children = Self(path);
        for file in fs::read_dir(&children.0)? {
            let file = file?;
            let name = file.file_name();
            let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
                continue;
            };
            let expected = fs::read_to_string(file.path())?;
            wait_previous(pid, &expected)?;
            fs::remove_file(file.path())?;
        }
        File::open(&children.0)?.sync_all()?;
        Ok(children)
    }

    fn record(&self, pid: u32) -> io::Result<Option<PathBuf>> {
        let identity = match process_identity(pid) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let path = self.0.join(pid.to_string());
        let mut file = tempfile::NamedTempFile::new_in(&self.0)?;
        file.write_all(identity.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&path)?;
        File::open(&self.0)?.sync_all()?;
        Ok(Some(path))
    }
}

fn process_identity(pid: u32) -> io::Result<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, tail) = stat.rsplit_once(')').ok_or(io::ErrorKind::InvalidData)?;
    let started = tail
        .split_whitespace()
        .nth(19)
        .ok_or(io::ErrorKind::InvalidData)?;
    let comm = fs::read_to_string(format!("/proc/{pid}/comm"))?;
    Ok(format!("{started}\n{comm}"))
}

fn wait_previous(pid: u32, expected: &str) -> io::Result<()> {
    // SAFETY: pidfd_open pins a kernel process identity, with no signal.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
    if raw < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        };
    }
    // SAFETY: successful pidfd_open returns a fresh owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    match process_identity(pid) {
        Ok(current) if current == expected => (),
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let mut waiting = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let now = Instant::now();
        if now >= end {
            return Err(io::ErrorKind::TimedOut.into());
        }
        // SAFETY: the local poll record is live and holds one owned pidfd.
        let ready =
            unsafe { libc::poll(&raw mut waiting, 1, (end - now).as_millis().max(1) as i32) };
        if ready > 0 {
            return Ok(());
        }
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
}

impl Place {
    pub(super) fn start(children: &Children, launch: Launch<'_>) -> io::Result<Self> {
        let Launch {
            executable,
            workdir,
            scratch,
            listener,
            activation,
            n_place,
            args,
            recording,
        } = launch;
        let (control, peer) = UnixStream::pair()?;
        let listen_fd = outside_launch_slots(listener.as_raw_fd())?;
        let control_fd = outside_launch_slots(peer.as_raw_fd())?;
        // The shell sets LISTEN_PID to its own PID and exec preserves it.
        // Image names, launch fields and application arguments are argv.
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "LISTEN_PID=$$ LISTEN_FDS=1 exec \"$@\"",
                "hirsel-place",
            ])
            .arg(executable)
            .arg(format!("--hirsel-n-place={n_place}"))
            .arg(format!("--hirsel-activation={activation}"))
            .args(args)
            .current_dir(workdir)
            .env("HIRSEL_CONTROL_FD", "4")
            .env("HIRSEL_SCRATCH", scratch)
            .env_remove("LISTEN_FDNAMES")
            .env_remove("LISTEN_PIDFDID")
            .env_remove("HIRSEL_RECORD")
            .env_remove("HIRSEL_RECORD_ID");
        if let Some(recording) = recording {
            recording.attach(&mut command);
        }
        arm(&mut command);
        // SAFETY: dup2 installs two owned descriptors in the forked child, and
        // the source descriptors avoid both destination slots.
        unsafe {
            command.pre_exec(move || {
                inherit(listen_fd.as_raw_fd(), 3)?;
                inherit(control_fd.as_raw_fd(), 4)
            });
        }
        let child = command.spawn()?;
        let mut place = Self {
            child,
            control,
            pidfile: None,
        };
        place.pidfile = children.record(place.child.id())?;
        Ok(place)
    }

    pub(super) fn control(&mut self) -> &mut UnixStream {
        &mut self.control
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub(super) fn wait_retired(&mut self, timeout: Duration) -> io::Result<()> {
        let end = Instant::now() + timeout;
        while self.try_wait()?.is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(path) = &self.pidfile {
            let _ = fs::remove_file(path);
        }
    }
}

pub(super) fn prepare(mut command: Command, timeout: Duration) -> io::Result<ExitStatus> {
    arm(&mut command);
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(io::ErrorKind::InvalidInput)?;
    let mut child = command.spawn()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            result => {
                let _ = child.kill();
                child.wait()?;
                return Err(result
                    .err()
                    .unwrap_or_else(|| io::ErrorKind::TimedOut.into()));
            }
        }
    }
}

fn arm(command: &mut Command) {
    let parent = std::process::id() as libc::pid_t;
    command.env("HIRSEL_HOST_PID", parent.to_string());
    // SAFETY: the closure performs only system calls and returns errno.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

fn outside_launch_slots(source: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl transfers ownership of a fresh descriptor.
    let fd = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 5) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

unsafe fn inherit(source: RawFd, destination: RawFd) -> io::Result<()> {
    // SAFETY: the caller supplies its own open source and a child launch slot.
    if unsafe { libc::dup2(source, destination) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
