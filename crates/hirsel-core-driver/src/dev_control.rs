use super::{Error, POLL, STOP, fallback_sources, source_state, sources, stopped};
use crate::operator_cli::{
    finite::{Envelope, Failure},
    wait::Wait,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RECEIPTS: usize = 64;
const MESSAGE_BYTES: u64 = 8192;
const IO_WAIT: Duration = Duration::from_secs(2);

pub(super) fn edit(paths: &[PathBuf]) -> String {
    let mut hash = Sha256::new();
    for (path, metadata) in paths.iter().zip(source_state(paths)) {
        let bytes = path.as_os_str().as_encoded_bytes();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        hash.update(serde_json::to_vec(&metadata).unwrap());
    }
    hirsel_core_ids::hex::encode(&hash.finalize())
}

#[derive(Clone)]
pub(super) struct State(Arc<Mutex<Session>>);

struct Session {
    id: String,
    counter: u64,
    active: Option<Value>,
    receipts: VecDeque<Value>,
    stopping: bool,
}

impl State {
    pub(super) fn new() -> Self {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(Arc::new(Mutex::new(Session {
            id: format!("{}-{}", std::process::id(), started.as_nanos()),
            counter: 0,
            active: None,
            receipts: VecDeque::new(),
            stopping: false,
        })))
    }

    pub(super) fn active(&self, active: Option<Value>) {
        self.0.lock().unwrap().active = active;
    }

    pub(super) fn begin(&self, edit: String) -> io::Result<u64> {
        let mut state = self.0.lock().unwrap();
        let sequence = state
            .counter
            .checked_add(1)
            .ok_or_else(|| io::Error::other("dev edit sequence exhausted"))?;
        state.counter = sequence;
        let receipt = json!({"attempted":{"edit":edit,"sequence":sequence.to_string()},"active":state.active,"superseded":null,"state":"Building"});
        if state.receipts.len() == RECEIPTS {
            state.receipts.pop_front();
        }
        state.receipts.push_back(receipt);
        Ok(sequence)
    }

    pub(super) fn finish(&self, outcome: &str, superseded: Option<Value>) -> Value {
        let mut state = self.0.lock().unwrap();
        let active = json!(state.active);
        let receipt = state.receipts.back_mut().expect("finish follows begin");
        receipt["active"] = active;
        receipt["superseded"] = json!(superseded);
        receipt["state"] = json!(outcome);
        receipt.clone()
    }

    pub(super) fn snapshot(&self) -> Value {
        let state = self.0.lock().unwrap();
        summary(&state, state.receipts.back())
    }
}

fn summary(state: &Session, receipt: Option<&Value>) -> Value {
    json!({
        "attempted":receipt.map(|r| &r["attempted"]),
        "active":state.active,"superseded":receipt.map(|r| &r["superseded"]),
        "state":receipt.map(|r| &r["state"]),"stopping":state.stopping
    })
}

fn socket_path(server: &Path) -> io::Result<PathBuf> {
    let identity = fs::metadata(server)?;
    let digest = Sha256::digest(server.as_os_str().as_encoded_bytes());
    let path = crate::host::runtime_directory("hirsel")?;
    Ok(path.join(format!(
        "dev-{:x}-{:x}-{}.sock",
        identity.dev(),
        identity.ino(),
        hirsel_core_ids::hex::encode(&digest[..8])
    )))
}

fn at_socket<T: Send>(
    path: &Path,
    open: impl FnOnce(&Path) -> io::Result<T> + Send,
) -> io::Result<T> {
    let parent = std::fs::File::open(path.parent().expect("dev runtime directory"))?;
    crate::resident::with_socket_path(
        &parent,
        Path::new(path.file_name().expect("dev socket name")),
        open,
    )
}

pub(super) struct Server {
    shutdown: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    path: PathBuf,
}

impl Server {
    pub(super) fn start(project: &Path, state: State) -> Result<Self, Error> {
        let path = socket_path(project)?;
        // The project's dev lock is held throughout this server's lifetime.
        match fs::remove_file(&path) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let listener = at_socket(&path, |p| UnixListener::bind(p))?;
        let mut server = Self {
            shutdown: Arc::new(AtomicBool::new(false)),
            worker: None,
            path,
        };
        fs::set_permissions(&server.path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let shutdown = Arc::clone(&server.shutdown);
        server.worker = Some(thread::Builder::new().spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve(stream, &state);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                    Err(_) => break,
                }
            }
        })?);
        Ok(server)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

fn serve(mut peer: UnixStream, state: &State) -> io::Result<()> {
    if crate::host::peer_uid(&peer)? != crate::operator::operator_uid() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // Darwin inherits O_NONBLOCK from the listener. Read through the
    // client's half-close under the existing timeout.
    peer.set_nonblocking(false)?;
    crate::host::socket_option(peer.set_read_timeout(Some(IO_WAIT)))?;
    crate::host::socket_option(peer.set_write_timeout(Some(IO_WAIT)))?;
    let mut request = Vec::new();
    (&mut peer).take(MESSAGE_BYTES).read_to_end(&mut request)?;
    let mut state = state.0.lock().unwrap();
    let mut reply = match request.as_slice() {
        b"status" => summary(&state, state.receipts.back()),
        b"stop" => {
            state.stopping = true;
            STOP.store(true, Ordering::Relaxed);
            summary(&state, state.receipts.back())
        }
        bytes if bytes.len() == 64 => {
            let receipt = state.receipts.iter().rev().find(|entry| {
                entry["attempted"]["edit"]
                    .as_str()
                    .is_some_and(|edit| edit.as_bytes() == bytes)
            });
            receipt.cloned().unwrap_or_else(|| summary(&state, None))
        }
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    };
    reply["stopping"] = json!(state.stopping || stopped());
    reply["session"] = json!(state.id);
    drop(state);
    serde_json::to_writer(&mut peer, &reply)?;
    Ok(())
}

fn query(socket: &Path, request: &[u8]) -> Result<Option<Value>, Failure> {
    let mut stream = match at_socket(socket, |p| UnixStream::connect(p)) {
        Ok(stream) => stream,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(Failure::io(e)),
    };
    if crate::host::peer_uid(&stream).map_err(Failure::io)? != crate::operator::operator_uid() {
        return Err(Failure::input("dev", "dev socket has a different peer UID"));
    }
    crate::host::socket_option(stream.set_read_timeout(Some(IO_WAIT))).map_err(Failure::io)?;
    crate::host::socket_option(stream.set_write_timeout(Some(IO_WAIT))).map_err(Failure::io)?;
    let exchange = || -> io::Result<Vec<u8>> {
        stream.write_all(request)?;
        stream.shutdown(std::net::Shutdown::Write)?;
        let mut bytes = Vec::new();
        stream.take(MESSAGE_BYTES).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    match exchange() {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(Failure::damaged),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::NotConnected
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(Failure::io(error)),
    }
}

fn project(directory: Option<&str>, env: &mut Envelope<'_>) -> Result<PathBuf, Failure> {
    let project =
        fs::canonicalize(directory.unwrap_or(".")).map_err(|e| Failure::input("directory", e))?;
    env.target = json!({"kind":"dev","directory":project});
    let socket = socket_path(&project).map_err(Failure::io)?;
    Ok(socket)
}

pub(crate) fn status(
    directory: Option<&str>,
    ready: bool,
    env: &mut Envelope<'_>,
) -> Result<(), Failure> {
    let socket = project(directory, env)?;
    let reply = query(&socket, b"status")?
        .ok_or_else(|| Failure::blocked("Unavailable", "no live dev session"))?;
    let active = !reply["active"].is_null() && reply["stopping"] == false;
    let failed = reply["state"] == "Failed" || reply["stopping"] == true;
    env.result = reply;
    if ready && !active {
        if failed {
            return Err(Failure::negative(
                "Unready",
                "the dev session has no serving release",
            ));
        }
        return Err(Failure::blocked(
            "Unproved",
            "the dev session has no serving proof yet",
        ));
    }
    Ok(())
}

pub(crate) fn run(req: crate::cli::Request<crate::cli::DevControlOption>) -> i32 {
    let mut env = Envelope::read(req.command.name);
    env.contract = "wait";
    let action = || -> Result<(), Failure> {
        let socket = project(req.positional.as_deref(), &mut env)?;
        let wait = Wait::start(None)?;
        if req.command.name == "dev stop" {
            let reply = query(&socket, b"stop")?
                .ok_or_else(|| Failure::blocked("Unavailable", "no live dev session"))?;
            env.result = reply;
            while query(&socket, b"status")?
                .is_some_and(|current| current["session"] == env.result["session"])
            {
                wait.poll()?;
            }
            env.outcome = json!({"kind":"Stopped","effect":"committed"});
            return Ok(());
        }
        let directory = Path::new(req.positional.as_deref().unwrap_or("."))
            .canonicalize()
            .map_err(Failure::io)?;
        let capture =
            || edit(&sources(&directory).unwrap_or_else(|_| fallback_sources(&directory)));
        let wanted = capture();
        loop {
            let reply = query(&socket, wanted.as_bytes())?.ok_or_else(|| {
                Failure::blocked(
                    "Unavailable",
                    "dev session ended before the selected edit finished",
                )
            })?;
            let completed = reply["attempted"]["edit"] == wanted && reply["state"] != "Building";
            let failed = reply["state"] == "Failed";
            let interrupted = reply["state"] == "Interrupted" || reply["stopping"] == true;
            env.result = reply;
            if completed && !interrupted {
                if failed {
                    return Err(Failure::negative(
                        "EditFailed",
                        "the selected edit did not activate",
                    ));
                }
                return Ok(());
            }
            if interrupted {
                return Err(Failure::blocked(
                    "Interrupted",
                    "the dev session is stopping",
                ));
            }
            if env.result["attempted"].is_null() && capture() != wanted {
                env.result["attempted"] = json!({"edit":wanted});
                return Err(Failure::blocked(
                    "Superseded",
                    "the selected edit changed before it was observed",
                ));
            }
            wait.poll()?;
        }
    };
    let mut action = action;
    if let Err(failure) = action() {
        env.fail(failure);
    }
    env.render(!req.options.is_empty())
}

pub(super) fn record(kind: &str, attempt: Option<u64>, payload: Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|v| v.as_millis().to_string());
    let record = json!({"v":1,"kind":kind,"subject":{"kind":"dev"},"operation":attempt.map(|v| v.to_string()),"cursor":null,"observed_at":observed_at,"payload":payload});
    serde_json::to_writer(&mut stdout, &record)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
