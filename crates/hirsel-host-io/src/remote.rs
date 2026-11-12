//! A host run's `remote.tls@1` runners (conformance/old-spec/runners.md, Providers), as the
//! interpreter holds them: [`hirsel_remote::provider`]'s, with each completion announced by
//! a byte on a socket the run's readiness watches for every parked remote call, so an idle
//! run wakes for it.
use hirsel_remote::frame::Call;
use hirsel_remote::provider::Providers;
use hirsel_remote::wire::Wire;
use hirsel_run_schema::{RunnerCompletion, RunnerEntry};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

/// The run's remote runners, by name, and the socket that announces their completions.
pub struct RemoteRunners {
    providers: Providers,
    wake: UnixStream,
}

impl RemoteRunners {
    /// Open a provider for every `remote.tls@1` runner of `entries`; None when there is none.
    pub fn open(
        entries: &[RunnerEntry],
        instance: [u8; 16],
        wire: impl FnOnce() -> Result<Wire, String>,
    ) -> Result<Option<Self>, String> {
        let (wake, signal) = UnixStream::pair().map_err(|e| format!("io.runners: {e}"))?;
        wake.set_nonblocking(true)
            .map_err(|e| format!("io.runners: {e}"))?;
        let notify = Arc::new(move || {
            let _ = (&signal).write(&[1]);
        });
        Ok(Providers::open(entries, instance, wire, notify)?
            .map(|providers| Self { providers, wake }))
    }
    pub fn serves(&self, runner: &str) -> bool {
        self.providers.serves(runner)
    }
    /// The descriptor a parked remote call's readiness watches.
    pub fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }
    pub fn submit(&self, runner: &str, token: u64, call: Call, digest: [u8; 32]) {
        self.providers.submit(runner, token, call, digest)
    }
    pub fn cancel(&self, runner: &str, token: u64, cause: u32) {
        self.providers.cancel(runner, token, cause)
    }
    /// The completions that arrived since the last take.
    pub fn take(&self) -> Vec<(u64, RunnerCompletion)> {
        let mut drained = [0u8; 64];
        while matches!((&self.wake).read(&mut drained), Ok(n) if n > 0) {}
        self.providers.take()
    }
}

/// The request frame of a sent call: the recorded request and the parts only its digest
/// holds.
pub fn call(
    request: &hirsel_run_schema::RunnerRequest,
    remote: hirsel_interp::library::runners::RemoteRequest,
) -> Call {
    Call {
        epoch: 0,
        position: 0,
        ordinal: remote.ordinal,
        key: request.key,
        form: request.form,
        path: request.path.clone(),
        fingerprint: remote.fingerprint,
        code: remote.code,
        grants: remote.grants,
        definitions: remote.definitions,
        argument: request.argument.clone(),
        acknowledged: 0,
        level: remote.level,
    }
}

/// The frame codec of a run, from its program's own core types.
pub fn wire(runtime: &hirsel_interp::aggregate::RuntimeTypes, limit: u64) -> Result<Wire, String> {
    use hirsel_interp::library::runners::{allowlist_schema, core_schema};
    let schema = |r: Result<_, hirsel_interp::Error>| r.map_err(|e| format!("RunFrame: {e:?}"));
    Wire::new(
        &schema(core_schema(runtime, "RunFrame"))?.schema,
        &schema(core_schema(runtime, "GrantSet"))?.schema,
        &schema(allowlist_schema(runtime))?.schema,
        limit,
    )
    .map_err(|e| format!("RunFrame: {e:?}"))
}

/// A runner host's cancellation of a callee run it serves (RUN-9), as the run sees it: the
/// host's thread sets the flag and writes a byte, which wakes the run's idle wait, and the
/// run cancels its root scope with cause `Parent` at its next scheduling point.
pub struct ExternalCancel {
    flag: Arc<std::sync::atomic::AtomicBool>,
    wake: UnixStream,
    applied: bool,
}
/// The host's side of an [`ExternalCancel`].
#[derive(Clone)]
pub struct CancelHandle {
    flag: Arc<std::sync::atomic::AtomicBool>,
    signal: Arc<UnixStream>,
}
impl ExternalCancel {
    pub fn pair() -> std::io::Result<(Self, CancelHandle)> {
        let (wake, signal) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        signal.set_nonblocking(true)?;
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        Ok((
            Self {
                flag: flag.clone(),
                wake,
                applied: false,
            },
            CancelHandle {
                flag,
                signal: Arc::new(signal),
            },
        ))
    }
    pub fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }
    /// Whether the host cancelled the run since the last look; true once.
    pub fn take(&mut self) -> bool {
        let mut drained = [0u8; 16];
        while matches!((&self.wake).read(&mut drained), Ok(n) if n > 0) {}
        if self.applied || !self.flag.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        self.applied = true;
        true
    }
}
impl CancelHandle {
    pub fn cancel(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::Release);
        let _ = (&*self.signal).write(&[1]);
    }
}

/// How a runner host serves a run: a callee its host can cancel, or a `db@1` place's head.
pub enum Hosted {
    Callee(ExternalCancel),
    Head(PlaceFeed),
}

/// A `db@1` place's inbox as its head run sees it (runners.md `db@1`): the host's thread
/// pushes each arrival that passed its checks and writes a byte, which wakes the head's
/// idle wait; the head's run takes them into its inbox at its next scheduling point and
/// reports each call's end, and the ends of every call it held when it ended, to `ended`.
pub struct PlaceFeed {
    arrivals: Arc<std::sync::Mutex<Vec<(u64, hirsel_interp::ArrivedCall)>>>,
    wake: UnixStream,
    ended: Box<dyn FnMut(u64, hirsel_run_schema::CallOutcome) + Send>,
}
/// The host's side of a [`PlaceFeed`].
#[derive(Clone)]
pub struct PlaceHandle {
    arrivals: Arc<std::sync::Mutex<Vec<(u64, hirsel_interp::ArrivedCall)>>>,
    signal: Arc<UnixStream>,
}
impl PlaceFeed {
    pub fn pair(
        ended: Box<dyn FnMut(u64, hirsel_run_schema::CallOutcome) + Send>,
    ) -> std::io::Result<(Self, PlaceHandle)> {
        let (wake, signal) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        signal.set_nonblocking(true)?;
        let arrivals = Arc::new(std::sync::Mutex::new(Vec::new()));
        Ok((
            Self {
                arrivals: arrivals.clone(),
                wake,
                ended,
            },
            PlaceHandle {
                arrivals,
                signal: Arc::new(signal),
            },
        ))
    }
    pub fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }
    /// The arrivals pushed since the last take, in arrival order.
    pub fn take(&mut self) -> Vec<(u64, hirsel_interp::ArrivedCall)> {
        let mut drained = [0u8; 16];
        while matches!((&self.wake).read(&mut drained), Ok(n) if n > 0) {}
        std::mem::take(&mut *self.arrivals.lock().unwrap())
    }
    pub fn ended(&mut self, id: u64, end: hirsel_run_schema::CallOutcome) {
        (self.ended)(id, end)
    }
}
impl PlaceHandle {
    pub fn arrive(&self, id: u64, call: hirsel_interp::ArrivedCall) {
        self.arrivals.lock().unwrap().push((id, call));
        let _ = (&*self.signal).write(&[1]);
    }
}
