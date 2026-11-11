//! `hirsel serve`: a runner host for `remote.tls@1` (docs/spec/runners.md RUN-13). It
//! listens where its host configuration says, admits each TLS client as the caller its
//! certificate (or its absence) names, checks every arriving call (RUN-8) against the
//! callee entries its program defines, keeps each caller's call streams exactly once
//! ([`hirsel_remote_host::host::Host`]), and runs each admitted call as its own callee run on
//! this host, recorded to its own tape when the configuration records callees (RUN-10).
//!
//! Connections run on their own threads; the callee runs run one at a time on the calling
//! thread, which owns the program. A cancellation a caller forwards reaches the running
//! callee through its [`ExternalCancel`](hirsel_host_io::ExternalCancel).
//!
//! A host whose configuration names a `place` is a `db@1` place (runners.md `db@1`): its
//! head runs on its own thread from startup, and each transact or query call that passes
//! the host's checks reaches the head's inbox through a [`PlaceHandle`]; the head's ends go
//! back to the caller through the host.
use crate::{HostLaunch, ToolError};
use hirsel_host_io::{CancelHandle, ExternalCancel, Hosted, PlaceFeed, PlaceHandle};
use hirsel_remote::frame::Call;
use hirsel_remote::transport::{Command, Commands, Event, HostTls, Listener};
use hirsel_remote::wire::Wire;
use hirsel_remote_host::host::{
    ABANDONED, Action, Arrival, Caller, Host, Limits, Place, Refusal, RunId, SessionId, Start,
};
use hirsel_run_config::host::HostConfig;
use hirsel_run_schema::place::{PlaceMessage, ServedCall};
use hirsel_run_schema::{CallForm, CallOutcome, CalleeFacts, CalleeOutcome, CodeReach, IoGrants};
use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

const DENIED: u32 = 0;
const VERSION_MISMATCH: u32 = 1;
const REJECTED: u32 = 4;

/// The callee entries this host serves, and its place policy.
struct Served {
    entries: BTreeMap<String, (CalleeFacts, CallForm)>,
    /// `admit.safe@1`: a callee that reaches an extern or `Unsafe` operation is refused.
    safe_only: bool,
    /// This host is a `db@1` place: it has a head to hand transact and query calls to.
    place: bool,
}
impl Served {
    /// Check 4's allowlists and code policy, shared by every call form.
    fn permitted(&self, caller: &Caller, call: &Call) -> bool {
        let listed = |allowlist: &Option<Vec<String>>| {
            allowlist
                .as_ref()
                .is_none_or(|paths| paths.contains(&call.path))
        };
        let safe = !self.safe_only
            || self
                .entries
                .get(&call.path)
                .is_none_or(|(facts, _)| facts.reach == CodeReach::Safe);
        listed(&caller.definitions) && listed(&call.definitions) && safe
    }
}
impl Place for Served {
    /// RUN-8 checks 3 and 4, for a caller the session already admitted (check 2).
    fn admit(&mut self, caller: &Caller, call: &Call) -> Result<IoGrants, Refusal> {
        let refuse = |reason, missing| Err(Refusal { reason, missing });
        let Some((facts, form)) = self.entries.get(&call.path) else {
            return refuse(VERSION_MISMATCH, vec![call.code]);
        };
        if facts.code != call.code || facts.fingerprint != call.fingerprint {
            return refuse(VERSION_MISMATCH, vec![call.code]);
        }
        if *form != call.form || !self.permitted(caller, call) {
            return refuse(DENIED, vec![]);
        }
        match hirsel_run_schema::narrow(&caller.grants, &call.grants) {
            Some(grants) => Ok(grants),
            None => refuse(DENIED, vec![]),
        }
    }
    /// The host's share of check 4 at a place; the head checks the entries it serves.
    fn admit_served(&mut self, caller: &Caller, call: &Call) -> Result<(), Refusal> {
        if self.place
            && self.permitted(caller, call)
            && hirsel_run_schema::narrow(&caller.grants, &call.grants).is_some()
        {
            Ok(())
        } else {
            Err(Refusal {
                reason: DENIED,
                missing: vec![],
            })
        }
    }
}

struct Shared {
    host: Host,
    place: Served,
    sessions: BTreeMap<SessionId, Commands>,
    cancels: BTreeMap<RunId, CancelHandle>,
    /// The place's head inbox, once the head runs.
    head: Option<Head>,
    next_session: SessionId,
    work: Sender<Work>,
}
/// What the callee thread does next.
enum Work {
    Start(Start, ExternalCancel),
    /// The host stopped on a failed journal append: the process exits to recover.
    Stop,
}
impl Shared {
    fn dispatch(&mut self, actions: Vec<Action>) {
        self.act(actions);
        if self.host.failure().is_some() {
            let _ = self.work.send(Work::Stop);
        }
    }
    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send(session, frame) => {
                    if let Some(connection) = self.sessions.get(&session) {
                        let _ = connection.send(Command::Send(frame));
                    }
                }
                Action::Close(session) => {
                    if let Some(connection) = self.sessions.get(&session) {
                        let _ = connection.send(Command::Close);
                    }
                }
                Action::Start(start) => match ExternalCancel::pair() {
                    Ok((cancel, handle)) => {
                        self.cancels.insert(start.run, handle);
                        let _ = self.work.send(Work::Start(start, cancel));
                    }
                    Err(error) => {
                        eprintln!("hirsel serve: a callee's cancellation channel: {error}");
                        let actions =
                            self.host
                                .ended(start.run, CallOutcome::InDoubt(ABANDONED), None);
                        self.act(actions);
                    }
                },
                Action::Cancel(run, _) => {
                    if let Some(handle) = self.cancels.get(&run) {
                        handle.cancel();
                    }
                }
                Action::Arrive(arrival) => match &mut self.head {
                    Some(head) => head.arrive(*arrival),
                    // Served calls are admitted only at a place, whose head is set first.
                    None => {
                        let actions = self
                            .host
                            .arrival_ended(arrival.id, CallOutcome::NotRun(DENIED));
                        self.act(actions);
                    }
                },
            }
        }
    }
}

/// The place's head, as the host hands it arrivals: an interpreted head's inbox, or the
/// host's end of a native head's place link.
enum Head {
    Interpreted(PlaceHandle),
    Native(UnixStream),
}
impl Head {
    /// Hand the head `arrival`. A native head that is gone reads nothing more; its process
    /// ending closes the place.
    fn arrive(&mut self, arrival: Arrival) {
        match self {
            Self::Interpreted(head) => head.arrive(arrival.id, arrived(arrival)),
            Self::Native(link) => {
                let id = arrival.id;
                let _ = PlaceMessage::Arrive(id, served(arrival)).write(link);
            }
        }
    }
}

/// A served call as a native head's `arrival.request` states it.
fn served(arrival: Arrival) -> ServedCall {
    let call = arrival.call;
    ServedCall {
        form: call.form,
        path: call.path,
        fingerprint: call.fingerprint,
        code: call.code,
        caller: arrival.caller,
        stream: arrival.stream,
        epoch: call.epoch,
        position: call.position,
        digest: arrival.digest,
        acknowledged: call.acknowledged,
        level: call.level,
        argument: call.argument,
    }
}

/// A served call as the head's `arrival.request` states it.
fn arrived(arrival: Arrival) -> hirsel_interp::ArrivedCall {
    let call = arrival.call;
    hirsel_interp::ArrivedCall {
        form: call.form,
        path: call.path,
        fingerprint: call.fingerprint,
        code: call.code,
        caller: arrival.caller,
        stream: arrival.stream,
        epoch: call.epoch,
        position: call.position,
        digest: arrival.digest,
        acknowledged: call.acknowledged,
        level: call.level,
        argument: call.argument,
    }
}

fn host_error(what: &str, error: impl std::fmt::Display) -> ToolError {
    ToolError::Runtime(format!("hirsel serve: {what}: {error}"))
}
fn read(path: &str, what: &str) -> Result<Vec<u8>, ToolError> {
    std::fs::read(path).map_err(|e| host_error(what, format!("{path}: {e}")))
}

/// Serve `program`'s callee entries under the host configuration at `config` until the
/// process ends, or until a failed call journal append stops the host: then it returns the
/// failure, so the process exits and its restart recovers from the journal. `ready` is told the listening endpoint once connections are accepted.
pub fn serve(
    program: &Path,
    config: &Path,
    native: Option<hirsel_native::Backend>,
    policy: crate::session::Policy,
    ready: &mut dyn FnMut(&str),
) -> Result<(), ToolError> {
    let bytes = std::fs::read(config).map_err(|e| {
        host_error(
            "the host configuration",
            format!("{}: {e}", config.display()),
        )
    })?;
    let config =
        HostConfig::validate(&bytes).map_err(|e| host_error("the host configuration", e))?;
    // The callee runs run one at a time on this thread, which owns the program.
    if config.runs() != 1 {
        return Err(host_error(
            "host.runs",
            "only 1 callee run at a time is supported",
        ));
    }
    let mut session = crate::session::Session::open_resident(program, policy);
    let initial = session.request(crate::requests::CompileOperation::Tir { definition: None })?;
    let crate::requests::CompileResult::Tir(input) = &initial.result else {
        return Err(host_error(
            "compiler",
            "TIR request returned another operation",
        ));
    };
    crate::tir_execution::refuse_foreign_interpretation(input)?;
    let entries = input.entries.clone();
    let runtime = input.runtime.clone();
    let wire = Arc::new(
        hirsel_host_io::remote_wire(&runtime, config.frame_bytes())
            .map_err(|e| host_error("the frame codec", e))?,
    );
    let fresh: [u8; 16] = hirsel_rt::io::RandomSource::System
        .draw(16)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| host_error("the incarnation", "the secure generator is unavailable"))?;
    let journal: Box<dyn hirsel_remote_host::journal::Journal> = match &config.journal {
        None => Box::new(hirsel_remote_host::journal::NoJournal::new(fresh)),
        Some(entry) => Box::new(
            hirsel_remote_host::journal::DirJournal::open(
                Path::new(entry.path.as_deref().unwrap_or_default()),
                fresh,
            )
            .map_err(|e| host_error("the call journal", e))?,
        ),
    };
    let host = Host::new(
        journal,
        Limits {
            runs: config.runs(),
            queue: config.queue(),
            streams: config.streams(),
            unacknowledged: config.unacknowledged(),
        },
    )
    .map_err(|e| host_error("the call journal", e))?;
    let clients = match &config.clients {
        Some(roots) => Some(read(&roots.pem, "host.clients")?),
        None => None,
    };
    let tls = Arc::new(
        HostTls::new(
            &read(&config.identity.chain, "host.identity")?,
            &read(&config.identity.key, "host.identity")?,
            clients.as_deref(),
        )
        .map_err(|e| host_error("host.identity", e))?,
    );
    let listener = Listener::bind(&config.listen).map_err(|e| host_error("host.listen", e))?;
    let (work, next) = channel();
    let shared = Arc::new(Mutex::new(Shared {
        host,
        place: Served {
            entries: entries.clone(),
            safe_only: config.admission.is_some(),
            place: config.place.is_some(),
        },
        sessions: BTreeMap::new(),
        cancels: BTreeMap::new(),
        head: None,
        next_session: 1,
        work,
    }));
    let config = Arc::new(config);
    if config.place.is_some() {
        match native {
            None => start_head(program, policy, fresh, &config, &shared)?,
            Some(backend) => start_native_head(program, policy, backend, fresh, &config, &shared)?,
        }
    }
    {
        let (shared, config, wire) = (shared.clone(), config.clone(), wire.clone());
        std::thread::Builder::new()
            .name("runner host".into())
            .spawn(move || accept(listener, tls, shared, config, wire))
            .map_err(|e| host_error("the listener", e))?;
    }
    ready(&match &config.listen {
        hirsel_run_schema::Endpoint::Unix(path) => path.clone(),
        hirsel_run_schema::Endpoint::Tcp { address, service } => format!("{address}:{service}"),
    });
    let stopped = |shared: &Shared| {
        shared
            .host
            .failure()
            .map(|failure| host_error("the call journal", failure))
    };
    for work in next {
        let Work::Start(start, cancel) = work else {
            break;
        };
        // A stopped host acts on nothing more, not even a callee it queued before it stopped.
        if let Some(error) = stopped(&shared.lock().unwrap()) {
            return Err(error);
        }
        let run = start.run;
        let (end, callee) = run_callee(&mut session, &entries, &config, &start, cancel);
        let mut shared = shared.lock().unwrap();
        shared.cancels.remove(&run);
        let actions = shared.host.ended(run, end, callee);
        shared.dispatch(actions);
    }
    stopped(&shared.lock().unwrap()).map_or(Ok(()), Err)
}

/// Start the place's head on its own thread: the host hands it the served calls it admits
/// and sends the ends it reports; once it ends, later served calls are unreachable.
fn start_head(
    program: &Path,
    policy: crate::session::Policy,
    instance: [u8; 16],
    config: &Arc<HostConfig>,
    shared: &Arc<Mutex<Shared>>,
) -> Result<(), ToolError> {
    let ends = shared.clone();
    let (feed, handle) = PlaceFeed::pair(Box::new(move |id, end| {
        let mut shared = ends.lock().unwrap();
        let actions = shared.host.arrival_ended(id, end);
        shared.dispatch(actions);
    }))
    .map_err(|e| host_error("the place's inbox", e))?;
    shared.lock().unwrap().head = Some(Head::Interpreted(handle));
    let (program, config, shared) = (program.to_path_buf(), config.clone(), shared.clone());
    std::thread::Builder::new()
        .name("place head".into())
        .spawn(move || {
            if let Err(error) = run_head(&program, policy, instance, &config, feed) {
                eprintln!("hirsel serve: the place's head: {error:?}");
            }
            let mut shared = shared.lock().unwrap();
            let actions = shared.host.head_ended();
            shared.dispatch(actions);
        })
        .map_err(|e| host_error("the place's head", e))?;
    Ok(())
}

/// Run the place's head entry `fn(rt: Io, a: ()) -> Result[(), E]` under the place's
/// grants, with its inbox fed by `feed`, until it ends.
fn run_head(
    program: &Path,
    policy: crate::session::Policy,
    instance: [u8; 16],
    config: &HostConfig,
    feed: PlaceFeed,
) -> Result<(), ToolError> {
    let place = config.place.as_ref().expect("a place host");
    let definition =
        crate::def_path(&place.head).ok_or_else(|| host_error("host.place.head", &place.head))?;
    // The head runs beside the callee runs, so it holds its own compiler session.
    let mut session = crate::session::Session::open_resident(program, policy);
    let selected = session.request(crate::requests::CompileOperation::Tir {
        definition: Some(definition),
    })?;
    let crate::requests::CompileResult::Tir(input) = &selected.result else {
        return Err(host_error("host.place.head", "not a runnable entry"));
    };
    let run = head_record(input, &place.head, place.grants.clone(), instance)?
        .validated()
        .map_err(|e| host_error("host.place", e))?;
    let tape = config
        .tapes
        .as_ref()
        .map(|dir| PathBuf::from(dir).join("head.tape"));
    let launch = HostLaunch::Callee {
        run: &run,
        record: tape.as_deref(),
    };
    let selection = crate::launch_binding::LaunchSelection::read(launch)?;
    let (outcome, _) = crate::tir_execution::run_tir_on_host(
        input,
        launch,
        selection,
        None,
        None,
        Some(Hosted::Head(feed)),
        false,
    )?;
    eprintln!("hirsel serve: the place's head ended: {outcome:?}");
    Ok(())
}

/// The head's run record: a call of its `fn(rt: Io, a: ())` entry with `()` under `grants`,
/// its seeds derived from the host's `instance`.
fn head_record(
    input: &hirsel_requests::TirExecution,
    head: &str,
    grants: IoGrants,
    instance: [u8; 16],
) -> Result<hirsel_run_config::RunConfig, ToolError> {
    let (_, schema) = crate::call_schemas(&input.signature, &input.runtime)
        .map_err(|e| host_error("host.place.head", format!("{e:?}")))?;
    let argument = hirsel_interp::library::runners::encode(&schema, &hirsel_interp::Value::Unit)
        .map_err(|e| host_error("host.place.head", format!("{e:?}")))?;
    let cause = hirsel_run_schema::Cause {
        instance,
        ordinal: 0,
        digest: [0; 32],
    };
    let key = hirsel_run_schema::call_key(&instance, 0);
    let seed = |name: &str| hirsel_run_schema::callee_seed(&key, name);
    let mut record = hirsel_run_config::RunConfig::empty(
        input.revision.digest(),
        head.to_owned(),
        hirsel_run_schema::LaunchKind::Call,
    );
    record.call = Some(hirsel_run_schema::CallRecord::new(
        CallForm::Call,
        &cause,
        &argument,
    ));
    record.inputs.hash_seed = seed("hash");
    record.inputs.random_seed = seed("random");
    record.inputs.test_seed = seed("test");
    record.io = Some(grants);
    Ok(record)
}

/// Start the place's head natively on its own thread: it builds the head as a bound call
/// executable and runs it as a tasks.free@1 call run linked to this host. The host writes it
/// each served call it admits and takes back each end; once the head's process exits and
/// its last ends are read, later served calls are unreachable.
fn start_native_head(
    program: &Path,
    policy: crate::session::Policy,
    backend: hirsel_native::Backend,
    instance: [u8; 16],
    config: &Arc<HostConfig>,
    shared: &Arc<Mutex<Shared>>,
) -> Result<(), ToolError> {
    // A native head's arrivals are not on its tape (determinism.md): it records nothing.
    if config.tapes.is_some() {
        return Err(host_error(
            "host.tapes",
            "a native place's head does not record its arrivals",
        ));
    }
    let (host_end, head_end) = UnixStream::pair().map_err(|e| host_error("the place's link", e))?;
    let mut ends = host_end
        .try_clone()
        .map_err(|e| host_error("the place's link", e))?;
    shared.lock().unwrap().head = Some(Head::Native(host_end));
    let reader = {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("place link".into())
            .spawn(move || {
                while let Ok(Some(PlaceMessage::End(id, end))) = PlaceMessage::read(&mut ends) {
                    let mut shared = shared.lock().unwrap();
                    let actions = shared.host.arrival_ended(id, end);
                    shared.dispatch(actions);
                }
            })
            .map_err(|e| host_error("the place's link", e))?
    };
    let (program, config, shared) = (program.to_path_buf(), config.clone(), shared.clone());
    std::thread::Builder::new()
        .name("place head".into())
        .spawn(move || {
            if let Err(error) =
                run_native_head(&program, policy, backend, instance, &config, &head_end)
            {
                eprintln!("hirsel serve: the place's head: {error:?}");
            }
            // The head's process has exited: once the link's last ends are in, the place
            // serves nothing more.
            drop(head_end);
            let _ = reader.join();
            let mut shared = shared.lock().unwrap();
            let actions = shared.host.head_ended();
            shared.dispatch(actions);
        })
        .map_err(|e| host_error("the place's head", e))?;
    Ok(())
}

/// Build and run the place's head natively until it ends, linked to the host by `link`.
fn run_native_head(
    program: &Path,
    policy: crate::session::Policy,
    backend: hirsel_native::Backend,
    instance: [u8; 16],
    config: &HostConfig,
    link: &UnixStream,
) -> Result<(), ToolError> {
    use std::os::fd::AsFd;
    let place = config.place.as_ref().expect("a place host");
    let definition =
        crate::def_path(&place.head).ok_or_else(|| host_error("host.place.head", &place.head))?;
    let mut session = crate::session::Session::open_resident(program, policy);
    let selected = session.request(crate::requests::CompileOperation::Tir {
        definition: Some(definition),
    })?;
    let crate::requests::CompileResult::Tir(input) = &selected.result else {
        return Err(host_error("host.place.head", "not a runnable entry"));
    };
    // Native places run under tasks.free@1 (determinism.md).
    let mut grants = place.grants.clone();
    grants.tasks = Some("tasks.free@1".into());
    let run = head_record(input, &place.head, grants, instance)?
        .validated()
        .map_err(|e| host_error("host.place", e))?;
    drop(selected);
    drop(session);
    let launch = HostLaunch::Callee {
        run: &run,
        record: None,
    };
    let outcome = crate::native::run_place_head(program, launch, backend, policy, link.as_fd())?
        .map_err(|e| host_error("the place's head", e))?;
    eprintln!("hirsel serve: the place's head ended: {outcome:?}");
    Ok(())
}

/// Accept connections until the listener fails; each runs on its own thread.
fn accept(
    listener: Listener,
    tls: Arc<HostTls>,
    shared: Arc<Mutex<Shared>>,
    config: Arc<HostConfig>,
    wire: Arc<Wire>,
) {
    loop {
        let socket = match listener.accept() {
            Ok(socket) => socket,
            Err(error) => {
                eprintln!("hirsel serve: accept: {error}");
                continue;
            }
        };
        let (tls, shared, config, wire) =
            (tls.clone(), shared.clone(), config.clone(), wire.clone());
        let _ = std::thread::Builder::new()
            .name("runner session".into())
            .spawn(move || {
                // A failed handshake is the client's: it never becomes a session.
                let Ok((tls, peer)) = tls.accept(socket) else {
                    return;
                };
                let caller = config
                    .caller(peer.as_ref().map(|p| &p.digest), |name| {
                        peer.as_ref().is_some_and(|p| p.names(name))
                    })
                    .map(|entry| Caller {
                        name: entry.name.clone(),
                        grants: entry.grants.clone(),
                        definitions: entry.definitions.clone(),
                    });
                let (commands, queue) = hirsel_remote::transport::queue();
                let session = {
                    let mut shared = shared.lock().unwrap();
                    let session = shared.next_session;
                    shared.next_session += 1;
                    shared.sessions.insert(session, commands);
                    shared.host.connected(session, caller);
                    session
                };
                let digests = wire.clone();
                hirsel_remote::transport::serve(tls, wire, queue, |event| {
                    let mut guard = shared.lock().unwrap();
                    let shared = &mut *guard;
                    let actions = match event {
                        Event::Frame(frame) => {
                            shared
                                .host
                                .frame(session, frame, &mut shared.place, &*digests)
                        }
                        Event::Closed => {
                            shared.sessions.remove(&session);
                            shared.host.disconnected(session)
                        }
                    };
                    shared.dispatch(actions);
                    true
                });
            });
    }
}

/// Run one admitted call as its own callee run (RUN-10): its outcome, and the callee run's
/// instance when it ran.
fn run_callee(
    session: &mut crate::session::Session,
    entries: &BTreeMap<String, (CalleeFacts, CallForm)>,
    config: &HostConfig,
    start: &Start,
    cancel: ExternalCancel,
) -> (CallOutcome, Option<[u8; 16]>) {
    let call = &start.call;
    let Some((facts, _)) = entries.get(&call.path) else {
        return (CallOutcome::NotRun(VERSION_MISMATCH), None);
    };
    let selected = crate::def_path(&call.path).and_then(|definition| {
        session
            .request_applied(crate::requests::CompileOperation::Tir {
                definition: Some(definition),
            })
            .ok()
    });
    let Some(crate::requests::CompileResult::Tir(input)) =
        selected.as_ref().map(|selected| &selected.result)
    else {
        return (CallOutcome::NotRun(REJECTED), None);
    };
    // Check 8 uses the selected immutable callee's parameter schema.
    let decodes = crate::call_schemas(&input.signature, &input.runtime)
        .ok()
        .and_then(|(_, argument)| {
            hirsel_interp::library::runners::decode(&argument, &call.argument, &input.runtime).ok()
        });
    if decodes.is_none() {
        return (CallOutcome::NotRun(REJECTED), None);
    }
    let key = hirsel_run_schema::call_key(&start.instance, call.ordinal);
    let cause = hirsel_run_schema::Cause {
        instance: start.instance,
        ordinal: call.ordinal,
        digest: start.digest,
    };
    let seed = |name: &str| hirsel_run_schema::callee_seed(&key, name);
    let mut record = hirsel_run_config::RunConfig::empty(
        facts.program,
        call.path.clone(),
        hirsel_run_schema::LaunchKind::Call,
    );
    record.call = Some(hirsel_run_schema::CallRecord::new(
        call.form,
        &cause,
        &call.argument,
    ));
    record.inputs.hash_seed = seed("hash");
    record.inputs.random_seed = seed("random");
    record.inputs.test_seed = seed("test");
    record.io = (call.form == CallForm::Call).then(|| start.grants.clone());
    record.admission = config
        .admission
        .as_ref()
        .map(|place| hirsel_run_schema::Admission {
            not_before: None,
            expires: None,
            revocation: None,
            code: place.code.clone(),
        });
    let callee = hirsel_run_schema::callee_instance(&key);
    let run = match record.validated() {
        Ok(run) => run,
        Err(error) => {
            eprintln!("hirsel serve: {}: callee admission: {error}", call.path);
            return (CallOutcome::NotRun(REJECTED), None);
        }
    };
    // Each callee run records to its own tape, named by its call key.
    let tape = config
        .tapes
        .as_ref()
        .map(|dir| PathBuf::from(dir).join(format!("{}.tape", hirsel_run_schema::hex(&key))));
    let launch = HostLaunch::Callee {
        run: &run,
        record: tape.as_deref(),
    };
    let result = crate::launch_binding::LaunchSelection::read(launch).and_then(|selection| {
        crate::tir_execution::run_tir_on_host(
            input,
            launch,
            selection,
            None,
            None,
            Some(Hosted::Callee(cancel)),
            false,
        )
    });
    match result {
        Ok((_, Some(CalleeOutcome::Ok(bytes)))) => (CallOutcome::Ok(bytes), Some(callee)),
        Ok((_, Some(CalleeOutcome::Err(bytes)))) => (CallOutcome::Failed(bytes), Some(callee)),
        Ok((_, Some(CalleeOutcome::Failed(bytes)))) => (CallOutcome::Faulted(bytes), Some(callee)),
        // A result beyond the codec's budgets cannot cross.
        Ok((hirsel_run_schema::RunOutcome::Returned(_), None)) => {
            (CallOutcome::NotRun(REJECTED), Some(callee))
        }
        Ok((outcome, None)) => {
            eprintln!(
                "hirsel serve: {}: the callee ended without an outcome: {outcome:?}",
                call.path
            );
            (CallOutcome::InDoubt(ABANDONED), Some(callee))
        }
        Err(error) => {
            eprintln!("hirsel serve: {}: {error:?}", call.path);
            (CallOutcome::InDoubt(ABANDONED), Some(callee))
        }
    }
}
