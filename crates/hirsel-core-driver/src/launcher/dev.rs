use super::*;
use session::{PlaceReady, write_frame};
use std::time::Instant;

const POLL: Duration = Duration::from_millis(20);
const GRACE: Duration = Duration::from_secs(5);
const READY: Duration = Duration::from_secs(10);

pub(crate) type StoreId = (u64, u64);

pub(crate) enum Open {
    Ready(DevChild, PlaceReady),
    Refused(HostReply),
    Unproved(String),
}

pub(crate) struct DevChild {
    child: RunningChild,
    endpoint: Option<Conn>,
}

impl DevChild {
    pub(crate) fn exited(&mut self) -> io::Result<Option<Status>> {
        if let Some(endpoint) = &mut self.endpoint {
            endpoint.fill();
            while endpoint.frame().is_some() {}
        }
        if self.child.status.is_none() && self.child.process.exited() {
            self.child.reap_leader()?;
        }
        Ok(self.child.status)
    }
}

impl Drop for DevChild {
    fn drop(&mut self) {
        let _ = self.child.signal(libc::SIGTERM);
        let deadline = Instant::now() + GRACE;
        while matches!(self.exited(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        let _ = self.child.signal(libc::SIGKILL);
        let _ = self.child.reap_leader();
    }
}

fn spawn_child(
    mut command: Command,
    workdir: &Path,
    listener: Option<&TcpListener>,
    stdout: RawFd,
    scratch: &Path,
    store_fd: RawFd,
) -> io::Result<DevChild> {
    for name in [
        LAUNCH_IMAGE_ENV,
        "HIRSEL_CONTROL_FD",
        "HIRSEL_SCRATCH",
        "HIRSEL_HOST_PID",
        "HIRSEL_RECORD",
        "HIRSEL_RECORD_ID",
        "LISTEN_FDS",
        "LISTEN_PID",
        "LISTEN_FDNAMES",
    ] {
        command.env_remove(name);
    }
    let null = File::open("/dev/null")?;
    let mut slots = vec![
        (above_launch_slots(null.as_raw_fd())?, 0),
        (above_launch_slots(stdout)?, 1),
        (above_launch_slots(store_fd)?, 5),
    ];
    let endpoint = if let Some(listener) = listener {
        let (ours, theirs) = UnixStream::pair()?;
        slots.push((above_launch_slots(listener.as_raw_fd())?, 3));
        slots.push((above_launch_slots(theirs.as_raw_fd())?, 4));
        command
            .env("LISTEN_FDS", "1")
            .env("HIRSEL_CONTROL_FD", "4")
            .env("HIRSEL_HOST_PID", std::process::id().to_string())
            .env("HIRSEL_SCRATCH", scratch);
        Some(Conn::new(&Readiness::new()?, ours)?)
    } else {
        None
    };
    let process = spawn::start(&command, workdir, &slots, None, listener.is_some())?;
    Ok(DevChild {
        child: RunningChild {
            process,
            group: None,
            status: None,
        },
        endpoint,
    })
}

pub(crate) fn command(
    executable: &Path,
    n_place: u64,
    store: Option<StoreId>,
    predecessor: Option<[u8; 32]>,
    arguments: &[String],
) -> Command {
    let mut command = Command::new(executable);
    command.arg(format!("--hirsel-n-place={n_place}"));
    if let Some((hi, lo)) = store {
        command.arg(format!("--hirsel-store-id={hi:016x}{lo:016x}"));
    }
    if let Some(predecessor) = predecessor {
        command.arg(format!(
            "--hirsel-install-predecessor={}",
            hex(&predecessor)
        ));
    }
    command.arg("--hirsel-dev-store").args(arguments);
    command
}

pub(crate) fn open(
    command: Command,
    workdir: &Path,
    listener: &TcpListener,
    scratch: &Path,
    store_fd: RawFd,
    expected: Option<u64>,
    stopped: impl Fn() -> bool,
) -> io::Result<Open> {
    let mut child = spawn_child(
        command,
        workdir,
        Some(listener),
        libc::STDERR_FILENO,
        scratch,
        store_fd,
    )?;
    let deadline = Instant::now() + READY;
    while !stopped() && Instant::now() < deadline {
        let endpoint = child.endpoint.as_mut().expect("serving endpoint");
        endpoint.fill();
        if let Some(frame) = endpoint.frame() {
            return match HostReply::decode(&frame)? {
                HostReply::Opened(ready) if expected.is_none_or(|v| v == ready.version) => {
                    let endpoint = child.endpoint.as_mut().expect("serving endpoint");
                    endpoint.stream.set_nonblocking(false)?;
                    endpoint.stream.set_write_timeout(Some(POLL))?;
                    write_frame(&mut endpoint.stream, &HostCommand::StoreAccepted)?;
                    endpoint.stream.set_nonblocking(true)?;
                    if child.exited()?.is_some() {
                        return Ok(Open::Unproved("place exited after its open reply".into()));
                    }
                    Ok(Open::Ready(child, ready))
                }
                HostReply::Opened(ready) => Ok(Open::Unproved(format!(
                    "opened activation {}, expected {expected:?}",
                    ready.version
                ))),
                reply @ (HostReply::Current(..)
                | HostReply::Refused(_)
                | HostReply::StoreMismatch(..)) => Ok(Open::Refused(reply)),
                reply => Ok(Open::Unproved(format!("unexpected open reply: {reply:?}"))),
            };
        }
        if endpoint.closed {
            return Ok(Open::Unproved("place exited without an open reply".into()));
        }
        std::thread::sleep(POLL);
    }
    Ok(Open::Unproved("place did not send an open reply".into()))
}

pub(crate) fn install(
    command: Command,
    workdir: &Path,
    scratch: &Path,
    store_fd: RawFd,
    stopped: impl Fn() -> bool,
) -> io::Result<Option<HostReply>> {
    // HostReply::PlaceInstallRefused(InstallPredecessor) holds a 32-byte digest.
    const PROOF_BYTES: u64 = 44;
    let mut proof = tempfile::tempfile_in(scratch)?;
    let mut child = spawn_child(command, workdir, None, proof.as_raw_fd(), scratch, store_fd)?;
    while !stopped() {
        if let Some(status) = child.exited()? {
            proof.rewind()?;
            let mut bytes = Vec::new();
            proof.take(PROOF_BYTES + 1).read_to_end(&mut bytes)?;
            return Ok(places::install_reply(&bytes, status, None));
        }
        std::thread::sleep(POLL);
    }
    Ok(None)
}
