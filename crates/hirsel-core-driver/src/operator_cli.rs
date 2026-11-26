//! The operator's commands (launch.md "Starting the host" and "The operator socket"):
//! `hirsel host` runs the launcher with the installed platform host, `hirsel register` and
//! `hirsel deploy` drive one exchange each on `STATE/operator.sock`, and `hirsel replay
//! --state` resolves a recording's pin through the host or, with no host, the offline pin
//! reader.
use crate::cli::{Invocation, OptionId as Opt};
use crate::code_admission::{AdmissionError, ObjectKey, ObjectKind, send_release};
use crate::code_store::CodeStore;
use crate::launcher::session::{Attempt, Grants, Group, Input};
use crate::native::recipe_build::{foreign_grant_refusal, foreign_recording_refusal, parse_root};
use crate::operator::{
    self, Answer, Ask, Class, DecisionStage, Fates, Forward, Front, Pause, Phase, Record,
    Registration, Report,
};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::release_recipe::Recipe;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(test)]
mod laws;
mod preview;

use preview::decide;
use preview::{approve_grant, decision_json, held_fate, preview_json};

type Error = Box<dyn std::error::Error>;

/// The exit status of an outcome the operator cannot know yet: retry with the same request.
const UNAVAILABLE: i32 = 75;
/// How long replay waits for a running host's pin answer.
const PIN_WAIT: Duration = Duration::from_secs(5);
/// How long a deploy keeps resending a request the host answers `Busy`.
const BUSY_WAIT: Duration = Duration::from_secs(60);
/// The admission bytes per `Data` frame.
const CHUNK: usize = 256 << 10;

fn hex(bytes: &[u8]) -> String {
    crate::call::hex(bytes)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

pub(crate) fn installed(name: &str) -> Result<PathBuf, Error> {
    let executable = std::env::current_exe()?;
    let installation = crate::installation::Installed::discover(&executable)?
        .ok_or("hirsel is not running from an installation")?;
    Ok(installation.libexec(name))
}

/// `hirsel host --state S [--max-places N] [--idle-ms M]`: the launcher in
/// the foreground with this installation's platform host (launch.md "Starting the host").
pub(crate) fn host(invocation: Invocation) -> Result<std::convert::Infallible, Error> {
    if invocation.positional.is_some() || !invocation.arguments.is_empty() {
        return Err(
            "host takes no recipe or arguments; register groups with hirsel register".into(),
        );
    }
    let mut state = None;
    let mut options = Vec::new();
    for (option, value) in invocation.options {
        match option {
            Opt::HostState => state = value.map(PathBuf::from),
            Opt::MaxPlaces | Opt::IdleMs => options.push((option, value)),
            _ => return Err("unexpected host option".into()),
        }
    }
    let state = state.ok_or("host requires --state DIR")?;
    let path = |name: &str| Some(state.join(name).to_string_lossy().into_owned());
    options.extend([
        (Opt::HostState, Some(state.to_string_lossy().into_owned())),
        (Opt::Control, path("launcher.sock")),
        (Opt::OperatorListen, path("operator.sock")),
    ]);
    // With no installation the launcher reports the missing platform host (exit 74).
    if let Ok(program) = installed("platform_main") {
        options.push((
            Opt::PlatformHost,
            Some(program.to_string_lossy().into_owned()),
        ));
    }
    crate::launcher::run(Invocation {
        command: invocation.command,
        options,
        positional: None,
        arguments: Vec::new(),
    })
}

/// The Foreign and Exec approvals the operator gave with `--grant`, sorted and unique.
struct Approvals {
    foreign: Vec<String>,
    exec: Vec<String>,
}

impl Approvals {
    fn check(&self, recipe: &Recipe) -> Option<String> {
        let foreign: BTreeSet<String> = self.foreign.iter().cloned().collect();
        foreign_grant_refusal(recipe, &foreign).or_else(|| {
            crate::exec_launch::refuse_missing(recipe, &self.exec.iter().cloned().collect())
        })
    }
}

fn grant(grants: &mut crate::package_dir::Grants, value: &str) -> Result<(), Error> {
    if !(value.starts_with("Foreign=") || value.starts_with("Exec=")) {
        return Err("--grant takes only Foreign and Exec approvals here".into());
    }
    grants.grant(value)
}

fn approvals(grants: crate::package_dir::Grants) -> Approvals {
    let exec: BTreeSet<String> = grants
        .exec
        .iter()
        .map(|binding| binding.request.text())
        .collect();
    Approvals {
        foreign: grants.foreign.into_iter().collect(),
        exec: exec.into_iter().collect(),
    }
}

/// The recipe a deploy or registration uploads from the operator's code store.
struct Upload {
    store: CodeStore,
    root: ContentHash,
    recipe: Recipe,
}

impl Upload {
    fn open(root: Option<&str>, store: Option<PathBuf>) -> Result<Self, Error> {
        let root = parse_root(root.ok_or("expected a recipe hash")?)?;
        let store = CodeStore::new(store.ok_or("requires --code-store DIR")?);
        let recipe =
            crate::launcher::read_recipe(&store, root, crate::launcher::limits().recipe_bytes)?;
        Ok(Self {
            store,
            root,
            recipe,
        })
    }
}

fn request_id(text: Option<String>) -> Result<Vec<u8>, Error> {
    match text {
        Some(text) => unhex(&text)
            .filter(|d| d.len() == 16)
            .ok_or_else(|| "--request-id takes 32 hex digits".into()),
        None => {
            let mut d = vec![0; 16];
            std::fs::File::open("/dev/urandom")?.read_exact(&mut d)?;
            Ok(d)
        }
    }
}

/// `hirsel register R --state S --group NAME --listen ADDR --store DIR --code-store DIR
/// [options] [-- arguments]`: add a group and deploy its first release.
pub(crate) fn register(invocation: Invocation) -> Result<i32, Error> {
    or_unavailable("register", register_on(invocation))
}

fn register_on(invocation: Invocation) -> Result<i32, Error> {
    let (mut state, mut store_dir, mut code, mut name, mut listen) = (None, None, None, None, None);
    let mut tenant = String::from("operator");
    let mut workdir = std::env::current_dir()?;
    let mut trusted = false;
    let mut memory = None;
    let (mut record, mut front) = (None, None);
    let mut retention_days = 30u64;
    let mut grants = crate::package_dir::Grants::default();
    let mut d = None;
    for (option, value) in invocation.options {
        let text = value.unwrap_or_default();
        match option {
            Opt::HostState => state = Some(PathBuf::from(text)),
            Opt::GroupName => name = Some(text),
            Opt::Tenant => tenant = text,
            Opt::Listen => listen = Some(text),
            Opt::FrontHttp | Opt::FrontStream => {
                let forward = if option == Opt::FrontHttp {
                    Forward::Http
                } else {
                    Forward::Stream
                };
                let front = front.get_or_insert_with(|| Front {
                    names: Vec::new(),
                    forward,
                });
                if front.forward != forward {
                    return Err("a group cannot use both --front-http and --front-stream".into());
                }
                front.names.push(text);
            }
            Opt::GroupStore => store_dir = Some(std::path::absolute(text)?),
            Opt::CodeStore => code = Some(PathBuf::from(text)),
            Opt::Workdir => workdir = PathBuf::from(text).canonicalize()?,
            Opt::Trusted => trusted = true,
            Opt::HostMemory => memory = Some(text.parse()?),
            Opt::Record => record = Some(PathBuf::from(text)),
            Opt::TapeRetention => {
                retention_days = text
                    .parse::<u64>()
                    .ok()
                    .filter(|&n| n != 0)
                    .ok_or("invalid tape retention")?
            }
            Opt::Grant => grant(&mut grants, &text)?,
            Opt::RequestId => d = Some(text),
            _ => return Err("unexpected register option".into()),
        }
    }
    let state = state.ok_or("register requires --state DIR")?;
    let upload = Upload::open(invocation.positional.as_deref(), code)?;
    let approvals = approvals(grants);
    if let Some(reason) = approvals.check(&upload.recipe) {
        eprintln!("register: refused: {reason}");
        return Ok(2);
    }
    if record.is_some()
        && let Some(reason) = foreign_recording_refusal(&upload.recipe)
    {
        eprintln!("register: refused: {reason}");
        return Ok(2);
    }
    // The recordings directory is made only for a registration the CLI does not refuse.
    let record = record.map(crate::launcher::private_dir).transpose()?;
    let registration = Registration {
        name: name.ok_or("register requires --group NAME")?,
        tenant,
        workdir: workdir.to_string_lossy().into_owned(),
        trusted,
        listen: listen.ok_or("register requires --listen ADDR")?,
        store: store_dir
            .ok_or("register requires --store DIR")?
            .to_string_lossy()
            .into_owned(),
        memory,
        args: invocation.arguments,
        record: record.map(|dir| Record {
            dir: dir.to_string_lossy().into_owned(),
            retention_s: retention_days.saturating_mul(86400),
        }),
        front,
    };
    let d = request_id(d)?;
    eprintln!("request {}", hex(&d));
    let ask = Ask::Register(
        d.clone(),
        Box::new(registration),
        Input::Attested(upload.root.0.to_vec()),
        approvals.foreign.clone(),
        approvals.exec.clone(),
    );
    Exchange::new(&state, d, "register", Some(upload), approvals).run(ask)
}

/// `hirsel deploy` or `hirsel decide`.
pub(crate) fn run(command: &str, invocation: Invocation) -> Result<crate::request::Outcome, Error> {
    let code = if command == "decide" {
        decide(invocation)?
    } else {
        deploy(invocation)?
    };
    Ok(crate::request::Outcome::Exit(code))
}

/// `hirsel deploy R --state S --group NAME --code-store DIR [--request-id HEX]
/// [--approve-grant +LINE|-LINE ...] [--grant ...]` or `hirsel deploy --state S --status HEX`.
/// A request that awaits any other decision is decided with `hirsel decide`.
pub(crate) fn deploy(invocation: Invocation) -> Result<i32, Error> {
    or_unavailable("deploy", deploy_on(invocation))
}

fn deploy_on(invocation: Invocation) -> Result<i32, Error> {
    let (mut state, mut code, mut name, mut d, mut status) = (None, None, None, None, None);
    let mut decision = Grants::default();
    let mut grants = crate::package_dir::Grants::default();
    let mut held = Fates::default();
    let mut held_given = false;
    for (option, value) in invocation.options {
        let text = value.unwrap_or_default();
        match option {
            Opt::HostState => state = Some(PathBuf::from(text)),
            Opt::CodeStore => code = Some(PathBuf::from(text)),
            Opt::GroupName => name = Some(text),
            Opt::RequestId => d = Some(text),
            Opt::RequestStatus => status = Some(text),
            Opt::ApproveGrant => approve_grant(&mut decision, &text)?,
            Opt::Grant => grant(&mut grants, &text)?,
            Opt::HeldAll | Opt::HeldEntry => {
                held_fate(&mut held, option == Opt::HeldAll, &text)?;
                held_given = true;
            }
            _ => return Err("unexpected deploy option".into()),
        }
    }
    let state = state.ok_or("deploy requires --state DIR")?;
    if !invocation.arguments.is_empty() {
        return Err("deploy takes no program arguments; a group's arguments are registered".into());
    }
    let has_decision = decision != Grants::default();
    let submission_options = code.is_some()
        || name.is_some()
        || !grants.foreign.is_empty()
        || !grants.exec.is_empty()
        || held_given;
    if let Some(text) = status {
        if invocation.positional.is_some() || d.is_some() || submission_options || has_decision {
            return Err("deploy --status takes only --state".into());
        }
        let d = unhex(&text)
            .filter(|d| d.len() == 16)
            .ok_or("--status takes 32 hex digits")?;
        return status_of(&state, &d);
    }
    sort_grants(&mut decision);
    let upload = Upload::open(invocation.positional.as_deref(), code)?;
    let approvals = approvals(grants);
    if let Some(reason) = approvals.check(&upload.recipe) {
        eprintln!("deploy: refused: {reason}");
        return Ok(2);
    }
    let given = d.is_some();
    let d = request_id(d)?;
    eprintln!("request {}", hex(&d));
    // A resubmitted request keeps the group it was first sent to, even if the name was
    // reused since.
    let mut group = None;
    if given {
        match one(&state, &Ask::Status(d.clone()))? {
            Some(Answer::State(_, g, _, _)) => group = Some(g),
            Some(Answer::Error(Class::UnknownRequest, _)) => (),
            other => return Ok(unexpected("deploy", &d, other)),
        }
    }
    let group = match group {
        Some(g) => g,
        None => {
            let name = name.ok_or("deploy requires --group NAME")?;
            match one(&state, &Ask::Lookup(name))? {
                Some(Answer::Group(g)) => g,
                Some(Answer::Error(class, detail)) => {
                    eprintln!("deploy: {}: {detail}", class_name(class));
                    return Ok(1);
                }
                other => return Ok(unexpected("deploy", &d, other)),
            }
        }
    };
    let ask = Ask::Begin(
        d.clone(),
        group,
        Input::Attested(upload.root.0.to_vec()),
        approvals.foreign.clone(),
        approvals.exec.clone(),
        held,
    );
    let mut exchange = Exchange::new(&state, d, "deploy", Some(upload), approvals);
    exchange.decision = Some(decision);
    exchange.run(ask)
}

/// One request and its one reply, on a fresh connection.
fn one(state: &Path, ask: &Ask) -> Result<Option<Answer>, Error> {
    let mut connection = connect(state)?;
    connection.send(ask)?;
    Ok(connection.recv()?)
}

/// No host listens on the state directory's operator socket; the same request may succeed
/// once one does.
#[derive(Debug)]
struct NoHost(String);

impl std::fmt::Display for NoHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoHost {}

fn connect(state: &Path) -> Result<operator::Connection, Error> {
    operator::connect(state).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => NoHost(format!(
            "no host is listening at {}: {error}",
            state.display()
        ))
        .into(),
        _ => error.into(),
    })
}

/// A command's exit status, with no listening host reported as temporarily unavailable.
/// A grant diff in the host's canonical order: each side sorted with no repeats.
fn sort_grants(diff: &mut Grants) {
    diff.added.sort();
    diff.added.dedup();
    diff.removed.sort();
    diff.removed.dedup();
}

fn or_unavailable(command: &str, result: Result<i32, Error>) -> Result<i32, Error> {
    match result {
        Err(error) if error.is::<NoHost>() => {
            eprintln!("{command}: temporarily unavailable: {error}");
            Ok(UNAVAILABLE)
        }
        other => other,
    }
}

fn unexpected(command: &str, d: &[u8], answer: Option<Answer>) -> i32 {
    match answer {
        None => eprintln!("{command}: outcome unknown: request {}", hex(d)),
        Some(other) => eprintln!("{command}: unexpected reply {other:?}"),
    }
    UNAVAILABLE
}

fn status_of(state: &Path, d: &[u8]) -> Result<i32, Error> {
    match one(state, &Ask::Status(d.to_vec()))? {
        Some(Answer::State(d, group, _, phase)) => {
            println!("{}", state_json(&d, &group, &phase));
            Ok(0)
        }
        Some(Answer::Error(class, detail)) => {
            eprintln!("deploy: {}: {detail}", class_name(class));
            Ok(1)
        }
        other => Ok(unexpected("deploy", d, other)),
    }
}

fn class_name(class: Class) -> &'static str {
    match class {
        Class::Protocol => "protocol",
        Class::Denied => "denied",
        Class::UnknownRequest => "unknown request",
        Class::UnknownGroup => "unknown group",
        Class::Conflict => "conflict",
        Class::Busy => "busy",
        Class::Full => "full",
        Class::Invalid => "invalid",
        Class::Unassembled => "unassembled",
        Class::Store => "store",
        Class::Uncompiled => "uncompiled",
        Class::NotInstalledAfterRecovery => "not installed after recovery",
        Class::Exhausted => "exhausted",
        Class::Red => "red",
    }
}

fn report_json(report: &Report) -> serde_json::Value {
    let pause = match &report.pause {
        Pause::NotStarted => serde_json::json!({"state": "not started"}),
        Pause::AwaitWrite => serde_json::json!({"state": "awaiting a write"}),
        Pause::Measured(ns) => serde_json::json!({"state": "measured", "ns": ns}),
        Pause::MeasurementLost => serde_json::json!({"state": "measurement lost"}),
        Pause::StartFailed(why) => serde_json::json!({"state": "start failed", "detail": why}),
    };
    serde_json::json!({
        "fetch_ns": report.fetch_ns,
        "assemble_ns": report.assemble_ns,
        "pause": pause,
        "preview": preview_json(&report.preview),
    })
}

fn attempt_hex(a: &Attempt) -> String {
    hex(&a.bytes())
}

fn state_json(d: &[u8], group: &Group, phase: &Phase) -> serde_json::Value {
    let mut out = serde_json::json!({
        "request": hex(d),
        "group": group.name,
        "incarnation": group.incarnation,
    });
    let fields = match phase {
        Phase::AwaitInput(a) => {
            serde_json::json!({"phase": "awaiting input", "attempt": attempt_hex(a)})
        }
        Phase::Admitting(a) => serde_json::json!({"phase": "admitting", "attempt": attempt_hex(a)}),
        Phase::AwaitConfirm(a) => {
            serde_json::json!({"phase": "awaiting confirmation", "attempt": attempt_hex(a)})
        }
        Phase::Assembling(a) => {
            serde_json::json!({"phase": "assembling", "attempt": attempt_hex(a)})
        }
        Phase::Preparing(a) => serde_json::json!({"phase": "preparing", "attempt": attempt_hex(a)}),
        Phase::Pending(a, v) => {
            serde_json::json!({"phase": "pending", "attempt": attempt_hex(a), "activation": v})
        }
        Phase::Undecided(a, v, why) => {
            serde_json::json!({"phase": "reconciling", "attempt": attempt_hex(a), "activation": v, "reason": why})
        }
        Phase::Installed(v, assembly, report) => serde_json::json!({
            "phase": "installed", "activation": v, "assembly": hex(assembly), "report": report_json(report),
        }),
        Phase::Failed(class, detail, report) => serde_json::json!({
            "phase": "failed", "class": class_name(*class), "detail": detail, "report": report_json(report),
        }),
        Phase::AwaitDecision(decision) => {
            serde_json::json!({"phase": "awaiting a decision", "decision": decision_json(decision)})
        }
    };
    for (key, value) in fields.as_object().unwrap() {
        out[key] = value.clone();
    }
    out
}

/// A Foreign or Exec policy refusal, which exits 2 like a refusal the CLI finds itself.
fn policy_refusal(detail: &str) -> bool {
    detail.starts_with("FFI-8: ")
        || detail.starts_with("Exec[")
        || detail.starts_with("Exec grants")
}

/// The local end of an upload: `send_release` writes the admission bytes into one end of a
/// socket pair, a pump relays them to the host as `Data`, and the host's `Need` parts are
/// written back into the pair once the last one arrives.
struct Pump {
    peer: UnixStream,
    stopped: Arc<AtomicBool>,
    sender: std::thread::JoinHandle<Result<(), AdmissionError>>,
    relay: std::thread::JoinHandle<()>,
    needed: Vec<ObjectKey>,
}

impl Pump {
    fn start(
        upload: &Upload,
        d: &[u8],
        a: Attempt,
        out: Arc<Mutex<operator::Connection>>,
    ) -> io::Result<Self> {
        let (peer, mut local) = UnixStream::pair()?;
        let store = upload.store.clone();
        let root = upload.root;
        let sender = std::thread::spawn(move || {
            send_release(&mut local, &store, root, &crate::launcher::limits())
        });
        let stopped = Arc::new(AtomicBool::new(false));
        let (mut input, stop, d) = (peer.try_clone()?, stopped.clone(), d.to_vec());
        let relay = std::thread::spawn(move || {
            let mut chunk = vec![0; CHUNK];
            loop {
                let n = match input.read(&mut chunk) {
                    Ok(n) => n,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                };
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let ask = if n == 0 {
                    Ask::End(d.clone(), a)
                } else {
                    Ask::Data(d.clone(), a, chunk[..n].to_vec())
                };
                if out.lock().unwrap().send(&ask).is_err() || n == 0 {
                    return;
                }
            }
        });
        Ok(Self {
            peer,
            stopped,
            sender,
            relay,
            needed: Vec::new(),
        })
    }

    /// One `Need` part; after the last, the whole set goes to the sender as its Need.
    fn need(&mut self, keys: Vec<ObjectKey>, last: bool) -> io::Result<()> {
        self.needed.extend(keys);
        if !last {
            return Ok(());
        }
        let mut frame = (self.needed.len() as u64).to_le_bytes().to_vec();
        for key in self.needed.drain(..) {
            frame.push(match key.kind {
                ObjectKind::Group => 1,
                ObjectKind::Body => 2,
                ObjectKind::Annex => 3,
                ObjectKind::ForeignArchive => 4,
                ObjectKind::Recipe => return Err(io::ErrorKind::InvalidData.into()),
            });
            frame.extend_from_slice(&key.hash.0);
        }
        self.peer.write_all(&frame)
    }

    fn stop(self) -> Result<(), AdmissionError> {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.peer.shutdown(std::net::Shutdown::Both);
        let sent = self.sender.join().unwrap_or(Ok(()));
        let _ = self.relay.join();
        sent
    }
}

/// One deploy or registration: its request, the upload, the display and the outcome.
struct Exchange<'a> {
    state: &'a Path,
    d: Vec<u8>,
    command: &'static str,
    upload: Option<Upload>,
    approvals: Approvals,
    /// The grant diff the operator approves, sent as a Decide when the request awaits an
    /// Admission-stage decision on exactly it.
    decision: Option<Grants>,
}

impl<'a> Exchange<'a> {
    fn new(
        state: &'a Path,
        d: Vec<u8>,
        command: &'static str,
        upload: Option<Upload>,
        approvals: Approvals,
    ) -> Self {
        Self {
            state,
            d,
            command,
            upload,
            approvals,
            decision: None,
        }
    }

    fn run(self, ask: Ask) -> Result<i32, Error> {
        let started = std::time::Instant::now();
        loop {
            match self.attach(&ask)? {
                Some(code) => return Ok(code),
                None if started.elapsed() < BUSY_WAIT => {
                    std::thread::sleep(Duration::from_millis(250))
                }
                None => {
                    eprintln!(
                        "{}: the host stayed busy; resend request {}",
                        self.command,
                        hex(&self.d)
                    );
                    return Ok(UNAVAILABLE);
                }
            }
        }
    }

    /// One attachment to the request: its exit status, or None when the host is busy and
    /// the same request should be sent again.
    fn attach(&self, ask: &Ask) -> Result<Option<i32>, Error> {
        let mut reader = connect(self.state)?;
        let out = Arc::new(Mutex::new(reader.try_clone()?));
        out.lock().unwrap().send(ask)?;
        let mut pump: Option<Pump> = None;
        let mut decided = matches!(ask, Ask::Decide(..));
        let lost = |command: &str, d: &[u8]| {
            eprintln!("{command}: outcome unknown: request {}", hex(d));
            Ok(Some(UNAVAILABLE))
        };
        let result = loop {
            let answer = match reader.recv() {
                Ok(Some(answer)) => answer,
                Ok(None) | Err(_) => break lost(self.command, &self.d),
            };
            match answer {
                Answer::State(_, _, _, Phase::AwaitInput(a)) if pump.is_none() => {
                    let Some(upload) = &self.upload else {
                        break Err(format!(
                            "request {} uploads its input; resend it with `hirsel deploy R --request-id HEX`",
                            hex(&self.d)
                        )
                        .into());
                    };
                    pump = Some(Pump::start(upload, &self.d, a, out.clone())?);
                }
                Answer::Need(_, _, keys, last) => {
                    if let Some(p) = &mut pump
                        && p.need(keys, last).is_err()
                    {
                        break Err("the local upload ended before the host's Need".into());
                    }
                }
                Answer::Display(_, a, root, text) => {
                    if let Err(why) = self.check_display(&root, &text) {
                        break Err(why);
                    }
                    let mut stdout = io::stdout().lock();
                    stdout.write_all(text.as_bytes())?;
                    stdout.flush()?;
                    let hash = Sha256::digest(text.as_bytes()).to_vec();
                    if out
                        .lock()
                        .unwrap()
                        .send(&Ask::Confirm(self.d.clone(), a, hash))
                        .is_err()
                    {
                        break lost(self.command, &self.d);
                    }
                }
                Answer::State(d, group, _, phase) => match &phase {
                    Phase::AwaitDecision(held) => {
                        let grant_only = held.stage == DecisionStage::Admission
                            && !held.shows_held()
                            && self.decision.as_ref() == Some(&held.grants);
                        if grant_only && !decided {
                            decided = true;
                            let ask =
                                Ask::Decide(self.d.clone(), held.clone(), Fates::default(), true);
                            if out.lock().unwrap().send(&ask).is_err() {
                                break lost(self.command, &self.d);
                            }
                            continue;
                        }
                        eprintln!(
                            "{}: request {} awaits the operator's decision; decide it with `hirsel decide --state DIR --request-id {}`",
                            self.command,
                            hex(&self.d),
                            hex(&self.d)
                        );
                        println!("{}", state_json(&d, &group, &phase));
                        break Ok(Some(2));
                    }
                    Phase::Undecided(..) => {
                        println!("{}", state_json(&d, &group, &phase));
                        break Ok(Some(UNAVAILABLE));
                    }
                    Phase::Installed(_, _, report) => {
                        println!("{}", state_json(&d, &group, &phase));
                        break Ok(Some(match report.pause {
                            Pause::StartFailed(_) => 1,
                            _ => 0,
                        }));
                    }
                    Phase::Failed(class, detail, _) => {
                        eprintln!("{}: {}: {detail}", self.command, class_name(*class));
                        println!("{}", state_json(&d, &group, &phase));
                        let refused = *class == Class::Denied && policy_refusal(detail);
                        break Ok(Some(if refused { 2 } else { 1 }));
                    }
                    _ => (),
                },
                Answer::Error(Class::Busy, _) => break Ok(None),
                Answer::Error(class, detail) => {
                    eprintln!("{}: {}: {detail}", self.command, class_name(class));
                    break Ok(Some(1));
                }
                other => break Err(format!("unexpected reply {other:?}").into()),
            }
        };
        reader.shutdown();
        if let Some(p) = pump {
            let _ = p.stop();
        }
        result
    }

    /// The display must name exactly the submitted recipe's Foreign bindings, and Exec
    /// lines only when the operator approved Exec requests.
    fn check_display(&self, root: &[u8], text: &str) -> Result<(), Error> {
        let Some(upload) = &self.upload else {
            return Ok(());
        };
        if root != upload.root.0 {
            return Err("the host displayed approvals for another recipe".into());
        }
        let foreign = crate::launcher::deploy_foreign_lines(&upload.recipe, upload.root);
        let exec = text
            .strip_prefix(&foreign)
            .ok_or("the host reported different Foreign bindings")?;
        if !exec.is_empty() && self.approvals.exec.is_empty() {
            return Err("the host displayed Exec approvals the operator did not request".into());
        }
        Ok(())
    }
}

/// Where a recording's pin stands.
enum Pin {
    Found(ContentHash),
    Gone,
    /// The host holds its state but did not answer; the same replay may succeed later.
    Unavailable(String),
}

/// Ask the running host, or with none, read the catalog offline under both locks.
fn resolve_pin(state: &Path, id: [u8; 16]) -> Result<Pin, Error> {
    match operator::connect(state) {
        Ok(mut connection) => {
            connection.set_read_timeout(Some(PIN_WAIT))?;
            connection.send(&Ask::Resolve(id.to_vec()))?;
            return match connection.recv() {
                Ok(Some(Answer::Pin(operator::Resolved::Found(assembly)))) => {
                    Ok(Pin::Found(ContentHash(
                        assembly
                            .try_into()
                            .map_err(|_| "pin names a malformed assembly")?,
                    )))
                }
                Ok(Some(Answer::Pin(operator::Resolved::Gone))) => Ok(Pin::Gone),
                Ok(Some(Answer::Error(class, detail))) => Err(format!(
                    "the host could not read the pin: {}: {detail}",
                    class_name(class)
                )
                .into()),
                Ok(Some(other)) => Err(format!("unexpected reply {other:?}").into()),
                Ok(None) => Ok(Pin::Unavailable("the host closed the connection".into())),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(Pin::Unavailable("the host did not answer".into()))
                }
                Err(error) => Err(error.into()),
            };
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => return Err(error.into()),
    }
    // No host listens. Hold the launcher lock, so none starts, and the catalog's LOCK, so no
    // writer runs, while the reader replays the journal (STORE-SPEC §11.19).
    let held = |path: PathBuf| -> Result<Option<std::fs::File>, Error> {
        let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    };
    let Some(_launcher) = held(state.join("launcher.lock"))? else {
        return Ok(Pin::Unavailable(
            "another launcher holds the state directory".into(),
        ));
    };
    let Some(_catalog) = held(state.join("catalog").join("LOCK"))? else {
        return Ok(Pin::Unavailable("a writer holds the catalog".into()));
    };
    let output = Command::new(installed("pin_reader")?)
        .arg(hex(&id))
        .current_dir(state)
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "the pin reader failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    match text.trim_end().split_once(' ') {
        _ if text == "gone\n" => Ok(Pin::Gone),
        Some(("found", assembly)) => Ok(Pin::Found(parse_root(assembly)?)),
        _ => Err(format!("the pin reader printed {text:?}").into()),
    }
}

/// `hirsel replay --state S --tape TAPE [--diagnose]`: run a recording's retained image.
pub(crate) fn replay(invocation: Invocation) -> Result<i32, Error> {
    if invocation.positional.is_some() || !invocation.arguments.is_empty() {
        return Err("host recording replay takes its program and arguments from the pin".into());
    }
    let (mut state, mut tape, mut diagnose) = (None, None, false);
    for (option, value) in invocation.options {
        match option {
            Opt::HostState => state = value.map(PathBuf::from),
            Opt::Tape => tape = value.map(PathBuf::from),
            Opt::Diagnose => diagnose = true,
            _ => return Err("unexpected option for host recording replay".into()),
        }
    }
    let tape = tape.ok_or("replay requires --tape TAPE")?.canonicalize()?;
    let state = state.ok_or("replay requires --state DIR")?;
    let (tape_id, launch) = crate::tape_cli::run_route(&tape)?;
    let assembly = match resolve_pin(&state, tape_id)? {
        Pin::Found(assembly) => assembly,
        Pin::Gone => {
            eprintln!("Unknown(ImageGone)");
            return Ok(3);
        }
        Pin::Unavailable(why) => {
            eprintln!("replay: temporarily unavailable: {why}");
            return Ok(UNAVAILABLE);
        }
    };
    let objects = crate::launcher::objects::Records::read_only(&state.join("objects"))?;
    let image = (|| -> io::Result<_> {
        let assembly = objects.assembly(assembly)?;
        let recipe = crate::launcher::read_recipe(
            &CodeStore::new(state.join("code")),
            assembly.recipe,
            usize::MAX,
        )?;
        if let Some(reason) = foreign_recording_refusal(&recipe) {
            eprintln!("replay: refused: {reason}");
            return Ok(None);
        }
        if launch_digest(&launch) != Some(assembly.digest) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording image differs from its pin",
            ));
        }
        objects.open_image(assembly.digest).map(Some)
    })();
    let image = match image {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(2),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("Unknown(ImageGone)");
            return Ok(3);
        }
        Err(error) => return Err(error.into()),
    };
    // Keep the verified file open across exec, even if collection removes its name.
    let status = Command::new(format!("/proc/self/fd/{}", image.as_raw_fd()))
        .arg("--hirsel-replay-recording")
        .arg(if diagnose { "diagnose" } else { "same" })
        .arg(tape)
        .status()?;
    if status.success() {
        println!("Sealed");
    }
    Ok(status.code().unwrap_or(1))
}

fn launch_digest(snapshot: &[u8]) -> Option<ContentHash> {
    let mut cursor = hirsel_tape_format::Cursor::new(snapshot);
    for _ in 0..cursor.varint().ok()? {
        cursor.bytes().ok()?;
    }
    Some(ContentHash(cursor.bytes().ok()?.try_into().ok()?))
}
