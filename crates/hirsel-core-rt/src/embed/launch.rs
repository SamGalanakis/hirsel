//! Embedded place launch: in-process `Process` stop provider, `EmbedOsProcess`,
//! `Dialer` capture handoff, startup timeout enforcement, and worker lifecycle.
use super::{
    EmbedProgram,
    grants::{OpenedDir, ResolvedGrants, resolve_embedded_grants},
};
use crate::{
    entry::{
        ARGS, CLOCK, DIR, EXEC, FOREIGN, Grant, NET, PROCESS, RANDOM, SECRET, STDERR, STDIN,
        STDOUT, TASKS,
    },
    os::{OsProvider, hirsel_core_os_directory_ops},
    process::{Config, View},
    provider::{CANCELLED, Outcome, Provider, Request, STEP_PENDING, Step, Wait, deliver},
    run_stop::{EndReason, Execution, ExecutionScope},
};
use hirsel_core_ids::launch::Metadata;
use std::{
    collections::BTreeSet,
    os::fd::OwnedFd,
    ptr,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// In-process Process stop provider (no SIGINT/SIGTERM signal handlers!)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StopState {
    stopped: bool,
    tokens: BTreeSet<u64>,
}

pub(crate) struct EmbedStopProvider {
    state: Arc<Mutex<StopState>>,
    provider: Provider,
}

// SAFETY: `provider.state` points to the pinned `Arc<Mutex<StopState>>` inside this struct.
unsafe impl Send for EmbedStopProvider {}
unsafe impl Sync for EmbedStopProvider {}

impl EmbedStopProvider {
    pub(crate) fn new() -> Box<Self> {
        let state = Arc::new(Mutex::new(StopState::default()));
        let ptr = Arc::as_ptr(&state).cast_mut().cast();
        Box::new(Self {
            state,
            provider: Provider {
                state: ptr,
                wait: embed_stop_wait,
                issue: embed_stop_issue,
                resume: embed_stop_issue,
                abort: embed_stop_abort,
                deliver,
            },
        })
    }

    pub(crate) fn provider_ptr(&self) -> *const Provider {
        &self.provider
    }

    pub(crate) fn trigger_stop(&self) {
        let tokens = {
            let mut g = self.state.lock().unwrap();
            if g.stopped {
                return;
            }
            g.stopped = true;
            std::mem::take(&mut g.tokens)
        };
        for token in tokens {
            crate::tasks::hirsel_core_ready(token);
        }
    }
}

unsafe extern "C" fn embed_stop_wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
    Wait::default()
}

unsafe extern "C" fn embed_stop_issue(state: *mut u8, _: *mut Request, token: u64) -> Step {
    // SAFETY: `EmbedStopProvider` pins `Mutex<StopState>` for the whole generation.
    let mut g = unsafe { &*state.cast::<Mutex<StopState>>() }
        .lock()
        .unwrap();
    if g.stopped {
        Step::default()
    } else {
        g.tokens.insert(token);
        Step {
            kind: STEP_PENDING,
            ..Step::default()
        }
    }
}

unsafe extern "C" fn embed_stop_abort(
    state: *mut u8,
    _: *mut Request,
    token: u64,
    _: u8,
) -> Outcome {
    // SAFETY: `EmbedStopProvider` pins `Mutex<StopState>` for the whole generation.
    unsafe { &*state.cast::<Mutex<StopState>>() }
        .lock()
        .unwrap()
        .tokens
        .remove(&token);
    CANCELLED
}

// ---------------------------------------------------------------------------
// Dialer capture hook (called by `units_lang.rs::hirsel_core_conn_new`)
// ---------------------------------------------------------------------------

struct DialCaptureSlot {
    armed: bool,
    captured: Option<crate::units::ledger::conn::Client>,
}

static DIAL_CAPTURE: Mutex<DialCaptureSlot> = Mutex::new(DialCaptureSlot {
    armed: false,
    captured: None,
});

pub(crate) fn arm_dialer_capture() {
    let mut g = DIAL_CAPTURE.lock().unwrap();
    g.armed = true;
    g.captured = None;
}

pub fn maybe_capture_dialer(client: &crate::units::ledger::conn::Client) {
    let mut g = DIAL_CAPTURE.lock().unwrap();
    if g.armed {
        g.armed = false;
        g.captured = Some(client.clone_client());
    }
}

pub(crate) fn take_captured_dialer() -> Option<crate::units::ledger::conn::Client> {
    let mut g = DIAL_CAPTURE.lock().unwrap();
    g.armed = false;
    g.captured.take()
}

// ---------------------------------------------------------------------------
// Embedded process state & lifecycle startup channel
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) enum LaunchStatus {
    Starting,
    Ready,
    Refused(String),
    Ended { code: u32, reason: String },
}

pub(crate) struct LifecycleBridge {
    state: Mutex<LaunchStatus>,
    cv: Condvar,
    diag: Mutex<Vec<u8>>,
    on_spontaneous_end: Mutex<Option<Box<dyn FnOnce(u32, Vec<u8>) + Send>>>,
    host_stopping: AtomicBool,
}

impl LifecycleBridge {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(LaunchStatus::Starting),
            cv: Condvar::new(),
            diag: Mutex::new(Vec::new()),
            on_spontaneous_end: Mutex::new(None),
            host_stopping: AtomicBool::new(false),
        })
    }

    pub(crate) fn set_spontaneous_end_hook(&self, hook: Box<dyn FnOnce(u32, Vec<u8>) + Send>) {
        *self.on_spontaneous_end.lock().unwrap() = Some(hook);
    }

    fn on_emit(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match bytes[0] {
            1 => {
                arm_dialer_capture();
            }
            2 => {
                let mut g = self.state.lock().unwrap();
                if matches!(*g, LaunchStatus::Starting) {
                    *g = LaunchStatus::Ready;
                    self.cv.notify_all();
                }
            }
            3 => {
                let text = String::from_utf8_lossy(&bytes[1..]).into_owned();
                let mut g = self.state.lock().unwrap();
                if matches!(*g, LaunchStatus::Starting) {
                    *g = LaunchStatus::Refused(text);
                    self.cv.notify_all();
                }
            }
            4 if bytes.len() >= 17 => {
                let requested = u64::from_le_bytes(bytes[1..9].try_into().unwrap());
                let available = u64::from_le_bytes(bytes[9..17].try_into().unwrap());
                let reason = format!(
                    "PlaceCapacity(Room {{ requested: {requested}, available: {available} }})"
                );
                let mut g = self.state.lock().unwrap();
                if matches!(*g, LaunchStatus::Starting) {
                    *g = LaunchStatus::Refused(reason);
                    self.cv.notify_all();
                }
            }
            _ => {}
        }
    }

    fn append_diag(&self, bytes: &[u8]) {
        let mut g = self.diag.lock().unwrap();
        if g.len() < 8192 {
            let take = bytes.len().min(8192 - g.len());
            g.extend_from_slice(&bytes[..take]);
        }
    }

    fn diag_string(&self) -> String {
        String::from_utf8_lossy(&self.diag.lock().unwrap())
            .trim()
            .to_owned()
    }
}

struct EmbedProcessState {
    sources: *const crate::abi_sites::SourceTable,
    os: Option<Box<OsProvider>>,
    _provider: Box<Provider>,
    _stop: Box<EmbedStopProvider>,
    _metadata: Box<[Grant]>,
    _net_rules: Option<crate::net::RulesStorage>,
    secrets: Vec<crate::secret::Granted>,
    /// The Exec grants, whose children the embedder's helper holds. Closing
    /// them ends the helper, which kills and reaps every child first.
    exec: Box<[crate::exec::Grant]>,
    grants: Vec<u64>,
    _launch: Box<Metadata>,
    diagnostic_buf: Vec<u8>,
    bridge: Arc<LifecycleBridge>,
    config: Config,
}

// SAFETY: `EmbedProcessState` is constructed on the start thread and moved once to the worker thread.
unsafe impl Send for EmbedProcessState {}

unsafe extern "C" fn embed_close_grants(state: *mut u8) {
    // SAFETY: worker run has ended; closes descriptors and zeroes secrets.
    unsafe {
        let proc = &mut *state.cast::<EmbedProcessState>();
        proc.secrets.clear();
        drop(std::mem::take(&mut proc.exec));
        drop(proc.os.take());
    }
}

unsafe extern "C" fn embed_diagnostic(state: *mut u8, data: *const u8, len: usize) {
    // SAFETY: borrowed bytes live through this callback.
    unsafe {
        let proc = &*state.cast::<EmbedProcessState>();
        let slice = if len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(data, len)
        };
        proc.bridge.append_diag(slice);
    }
}

unsafe extern "C" fn embed_describe(state: *mut u8, cause: *const crate::causes::Cause) -> View {
    // SAFETY: `EmbedProcessState` pins `diagnostic_buf` until the next call.
    unsafe {
        let proc = &mut *state.cast::<EmbedProcessState>();
        proc.diagnostic_buf = crate::abi_sites::format_cause(&*cause, proc.sources).into_bytes();
        View {
            data: proc.diagnostic_buf.as_ptr(),
            len: proc.diagnostic_buf.len(),
        }
    }
}

unsafe extern "C" fn embed_emit(state: *mut u8, data: *const u8, len: usize) -> bool {
    // SAFETY: borrowed bytes live through this callback.
    unsafe {
        let proc = &*state.cast::<EmbedProcessState>();
        let slice = if len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(data, len)
        };
        proc.bridge.on_emit(slice);
        true
    }
}

fn build_embed_process(
    program: &EmbedProgram,
    digest: Option<[u8; 32]>,
    n_place: u64,
    resolved: ResolvedGrants,
    exec: Box<[crate::exec::Grant]>,
    bridge: Arc<LifecycleBridge>,
) -> Result<Box<EmbedProcessState>, String> {
    let mut os = Box::new(OsProvider::from_streams(None, None));
    let dir_ops = &hirsel_core_os_directory_ops;

    let mut dir_handles = Vec::with_capacity(1 + resolved.extra_dirs.len());
    let adopt_dir = |os: &mut OsProvider, d: OpenedDir| -> Result<u64, String> {
        let path_disp = d.path.display().to_string();
        let fd: OwnedFd = d.fd;
        (dir_ops.adopt)(os, fd).map_err(|e| {
            format!(
                "DirUnavailable({path_disp}, {})",
                e.raw_os_error().unwrap_or(libc::EIO)
            )
        })
    };
    dir_handles.push(adopt_dir(&mut os, resolved.store_dir)?);
    for extra in resolved.extra_dirs {
        dir_handles.push(adopt_dir(&mut os, extra)?);
    }
    // Probe openat2 / directory support on store dir (`None` uses real `OsProvider::probe_directory`).
    let probe = program.probe_dir.unwrap_or(OsProvider::probe_directory);
    probe(&os, dir_handles[0]).map_err(|e| {
        format!(
            "DirUnavailable(probe, {})",
            e.raw_os_error().unwrap_or(libc::EIO)
        )
    })?;

    let net_rules = if resolved.grant_mask & (1 << NET) != 0 {
        let storage = if resolved.net_rules.is_empty() {
            crate::net::RulesStorage::empty()
        } else {
            let parsed = hirsel_core_net::NetGrant::parse(&resolved.net_rules)
                .map_err(|_| "NetRules".to_owned())?;
            crate::net::RulesStorage::new(parsed.rules)
        };
        Some(storage)
    } else {
        None
    };

    let mut secrets = Vec::with_capacity(resolved.secrets.len());
    for (idx, s) in resolved.secrets.iter().enumerate() {
        let src = format!("file:{}", s.path.display());
        let granted = (crate::secret::hirsel_core_os_secret_ops.read)(
            [s.param.as_bytes(), src.as_bytes(), s.policy.as_bytes()],
            idx as u64,
        )
        .ok_or_else(|| format!("SecretSource(Secret:{}={src})", s.param))?;
        secrets.push(granted);
    }

    let provider = Box::new(os.provider_with(Some(dir_ops)));
    let provider_ptr: *const Provider = &*provider;
    let stop_provider = EmbedStopProvider::new();
    let stop_ptr = stop_provider.provider_ptr();

    let mut launch = Metadata::in_process_embed(digest, program.target, n_place);
    if let Some(rev) = program.revision {
        launch = launch.with_launch_identity(Some(rev), true);
    }
    let launch = Box::new(launch);

    let mut metadata = Vec::new();
    for secret in &secrets {
        metadata.push(Grant {
            kind: SECRET,
            provider: provider_ptr,
            handle: (secret as *const crate::secret::Granted) as u64,
            terminal: false,
        });
    }
    for handle in dir_handles {
        metadata.push(Grant {
            kind: DIR,
            provider: provider_ptr,
            handle,
            terminal: false,
        });
    }
    for kind in NET..=TASKS {
        if resolved.grant_mask & (1 << kind) != 0 {
            metadata.push(Grant {
                kind,
                provider: if kind == PROCESS {
                    stop_ptr
                } else {
                    provider_ptr
                },
                handle: if kind == NET {
                    net_rules.as_ref().map_or(0, |r| r.handle())
                } else {
                    0
                },
                terminal: false,
            });
        }
    }
    metadata.extend(exec.iter().map(|grant| Grant {
        kind: EXEC,
        provider: provider_ptr,
        handle: std::ptr::from_ref(grant) as u64,
        terminal: false,
    }));
    if resolved.has_foreign {
        for r in program.parameters.iter().filter(|r| r.kind == FOREIGN) {
            metadata.push(Grant {
                kind: FOREIGN,
                provider: ptr::null(),
                handle: r.index as u64,
                terminal: false,
            });
        }
    }
    metadata.push(Grant {
        kind: ARGS,
        provider: provider_ptr,
        handle: 0,
        terminal: false,
    });
    let metadata = metadata.into_boxed_slice();

    let config = Config {
        root_limit: program.root_limit,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: provider_ptr,
        state: ptr::null_mut(),
        diagnostic: embed_diagnostic,
        describe: Some(embed_describe),
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: -1,
        capabilities: metadata.as_ptr(),
        capabilities_len: metadata.len(),
        close_grants: Some(embed_close_grants),
        launch: &*launch,
        describe_output: Some(embed_emit),
    };

    let mut state = Box::new(EmbedProcessState {
        sources: program.sources,
        os: Some(os),
        _provider: provider,
        _stop: stop_provider,
        _metadata: metadata,
        _net_rules: net_rules,
        secrets,
        exec,
        grants: Vec::with_capacity(program.parameters.len()),
        _launch: launch,
        diagnostic_buf: Vec::new(),
        bridge,
        config,
    });

    for req in program.parameters {
        // SAFETY: `state.config` has pinned `capabilities`.
        let h =
            unsafe { crate::entry::hirsel_core_entry_grant(&state.config, req.kind, req.index) };
        if h == 0 {
            return Err(format!(
                "GrantMissing(kind={}, index={})",
                req.kind, req.index
            ));
        }
        state.grants.push(h);
    }
    state.config.grants = state.grants.as_ptr();
    state.config.grants_len = state.grants.len();
    state.config.state = (&mut *state as *mut EmbedProcessState).cast();
    Ok(state)
}

/// Running worker handle for one embedded place generation.
pub(crate) struct LaunchedPlace {
    generation_end: Arc<Execution>,
    stop_trigger: Arc<Mutex<StopState>>,
    bridge: Arc<LifecycleBridge>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LaunchedPlace {
    /// Step 4 of `start`: validate ISA, grants, descriptors, arm fault handler,
    /// spawn place worker, and wait up to `start_timeout_ms` for Ready + Dialer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        program: &EmbedProgram,
        parameter_names: &[&str],
        digest: Option<[u8; 32]>,
        n_place: u64,
        start_timeout_ms: u32,
        grants: &[u8],
        dir_bases: &[u8],
        store_dir: &[u8],
        exec_helper: &[u8],
    ) -> Result<(Self, crate::units::ledger::conn::Client), String> {
        // 1. Baseline-safe ISA check (EMB-3: never reads env, never calls exit).
        crate::isa::init_embedded().map_err(|s| s.to_owned())?;

        // 2. Grants, EMB-7 target checks, dir_bases, SecretSource, descriptor StoreOverlapsDir.
        let resolved = resolve_embedded_grants(
            program.target,
            store_dir,
            dir_bases,
            grants,
            program.parameters,
            parameter_names,
        )?;
        let generation_end = Arc::new(Execution::contained());
        let exec = crate::host::embed_exec(
            &resolved.exec_grants,
            exec_helper,
            start_timeout_ms,
            &generation_end,
        )?;

        // 3. Arm embedded fault handler (EMB-3: verifies no replacement).
        crate::fiber::arm_embedded().map_err(|e| match e {
            crate::fiber::ArmError::FaultHandlerReplaced => "FaultHandlerReplaced".to_owned(),
            crate::fiber::ArmError::SignalStack => "SignalStack".to_owned(),
        })?;

        let bridge = LifecycleBridge::new();
        let proc_state =
            build_embed_process(program, digest, n_place, resolved, exec, bridge.clone())?;
        let stop_trigger = proc_state._stop.state.clone();
        let gen_for_worker = generation_end.clone();
        let bridge_for_worker = bridge.clone();
        let entry = program.entry;
        let foreign_waits = program.foreign_waits;

        let worker = thread::Builder::new()
            .name("hirsel-embed-place".into())
            .spawn(move || {
                // Install a per-worker alternate signal stack so fiber faults never
                // touch any host thread's signal stack (EMB-3).
                let _alt = match crate::fiber::Alternate::install() {
                    Ok(a) => a,
                    Err(stop) => {
                        let hook = {
                            let mut g = bridge_for_worker.state.lock().unwrap();
                            *g = LaunchStatus::Refused(stop.name().to_owned());
                            bridge_for_worker.cv.notify_all();
                            None::<Box<dyn FnOnce(u32, Vec<u8>) + Send>>
                        };
                        drop(hook);
                        drop(proc_state);
                        return;
                    }
                };
                // SAFETY: `gen_for_worker` and `proc_state` live for the entire worker thread.
                let res = unsafe {
                    let _scope = ExecutionScope::enter(&gen_for_worker);
                    if foreign_waits {
                        crate::process::run_config_contained_with_foreign(
                            &proc_state.config,
                            entry,
                            true,
                            &gen_for_worker,
                        )
                    } else {
                        crate::process::run_config_contained(
                            &proc_state.config,
                            entry,
                            &gen_for_worker,
                        )
                    }
                };
                drop(proc_state);
                let host_stopping = bridge_for_worker.host_stopping.load(Ordering::Acquire);
                let (code, reason) = match res {
                    Ok(0) => {
                        if host_stopping {
                            (super::abi::HIRSEL_END_STOPPED, String::new())
                        } else {
                            let diag = bridge_for_worker.diag_string();
                            let msg = if diag.is_empty() {
                                "place exited unexpectedly".to_owned()
                            } else {
                                diag
                            };
                            (super::abi::HIRSEL_END_PLACE_FAILED, msg)
                        }
                    }
                    Ok(_) => {
                        let diag = bridge_for_worker.diag_string();
                        let msg = if diag.is_empty() {
                            "place failed".to_owned()
                        } else {
                            diag
                        };
                        (super::abi::HIRSEL_END_PLACE_FAILED, msg)
                    }
                    Err(EndReason::RunStop(stop)) => {
                        (super::abi::HIRSEL_END_RUN_STOP, stop.name().to_owned())
                    }
                    Err(EndReason::HostStop) => (super::abi::HIRSEL_END_STOPPED, String::new()),
                };

                let hook = {
                    let mut g = bridge_for_worker.state.lock().unwrap();
                    match *g {
                        LaunchStatus::Starting => {
                            *g = LaunchStatus::Refused(if reason.is_empty() {
                                "place stopped before Ready".to_owned()
                            } else {
                                reason.clone()
                            });
                            bridge_for_worker.cv.notify_all();
                            None
                        }
                        LaunchStatus::Refused(_) => {
                            bridge_for_worker.cv.notify_all();
                            None
                        }
                        LaunchStatus::Ready => {
                            *g = LaunchStatus::Ended {
                                code,
                                reason: reason.clone(),
                            };
                            bridge_for_worker.cv.notify_all();
                            if !host_stopping {
                                bridge_for_worker.on_spontaneous_end.lock().unwrap().take()
                            } else {
                                None
                            }
                        }
                        LaunchStatus::Ended { .. } => None,
                    }
                };
                if let Some(cb) = hook {
                    cb(code, reason.into_bytes());
                }
            })
            .map_err(|e| format!("WorkerSpawn({e})"))?;

        let launched = Self {
            generation_end,
            stop_trigger,
            bridge: bridge.clone(),
            worker: Mutex::new(Some(worker)),
        };

        // Wait up to `start_timeout_ms` for Ready or Refused.
        let deadline = Instant::now() + Duration::from_millis(u64::from(start_timeout_ms));
        let status = {
            let mut g = bridge.state.lock().unwrap();
            loop {
                match &*g {
                    LaunchStatus::Ready => break Ok(()),
                    LaunchStatus::Refused(reason) => break Err(reason.clone()),
                    LaunchStatus::Ended { reason, .. } => break Err(reason.clone()),
                    LaunchStatus::Starting => {
                        let now = Instant::now();
                        if now >= deadline {
                            break Err("PlaceTimedOut".to_owned());
                        }
                        let (next, _) = bridge.cv.wait_timeout(g, deadline - now).unwrap();
                        g = next;
                    }
                }
            }
        };

        match status {
            Ok(()) => {
                if let Some(dialer) = take_captured_dialer() {
                    Ok((launched, dialer))
                } else {
                    launched.force_stop_and_join();
                    Err("DialerMissing".to_owned())
                }
            }
            Err(reason) => {
                launched.force_stop_and_join();
                let _ = take_captured_dialer();
                Err(reason)
            }
        }
    }

    pub(crate) fn set_spontaneous_end_hook(&self, hook: Box<dyn FnOnce(u32, Vec<u8>) + Send>) {
        self.bridge.set_spontaneous_end_hook(hook);
    }

    /// Trigger graceful place shutdown (via `Process` stop) so in-flight group
    /// commits finish and the store closes cleanly before joining.
    pub(crate) fn stop_gracefully_and_join(&self) {
        self.bridge.host_stopping.store(true, Ordering::Release);
        {
            let tokens = {
                let mut g = self.stop_trigger.lock().unwrap();
                g.stopped = true;
                std::mem::take(&mut g.tokens)
            };
            for token in tokens {
                crate::tasks::hirsel_core_ready(token);
            }
        }
        if let Some(join) = self.worker.lock().unwrap().take() {
            let _ = join.join();
        }
    }

    /// Force-stop a stuck or timed-out generation (stops pure Hirsel loops via
    /// `EMBED_QUANTUM` and wakes parked waiters) and join the worker thread.
    pub(crate) fn force_stop_and_join(&self) {
        self.bridge.host_stopping.store(true, Ordering::Release);
        self.generation_end.request_host_stop();
        {
            let tokens = {
                let mut g = self.stop_trigger.lock().unwrap();
                g.stopped = true;
                std::mem::take(&mut g.tokens)
            };
            for token in tokens {
                crate::tasks::hirsel_core_ready(token);
            }
        }
        crate::tasks::wake_all_waiters();
        if let Some(join) = self.worker.lock().unwrap().take() {
            let _ = join.join();
        }
    }

    pub(crate) fn join_only(&self) {
        if let Some(join) = self.worker.lock().unwrap().take() {
            let _ = join.join();
        }
    }
}
