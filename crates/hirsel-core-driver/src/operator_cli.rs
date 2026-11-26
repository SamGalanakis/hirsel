//! The operator's commands (launch.md "Starting the host" and "The operator socket"):
//! `hirsel host` runs the launcher with the installed platform host, `hirsel register` and
//! `hirsel deploy` drive one exchange each on `STATE/operator.sock`, and `hirsel replay
//! --state` resolves a recording's pin through the host or, with no host, the offline pin
//! reader.
use crate::cli::{
    CompilerOption, DeployOption, HostOption, LauncherOption, RegisterOption, Request,
};
use crate::code_store::CodeStore;
use crate::launcher::session::{Attempt, Grants, Group};
use crate::native::recipe_build::{foreign_grant_refusal, parse_root};
use crate::operator::{
    self, Answer, Ask, Class, Fates, Forward, Front, Pause, Phase, Record, Registration, Report,
};
use crate::reachability::foreign_recording_refusal;
use hirsel_core_ids::ContentHash;
use hirsel_core_ids::hex;
use hirsel_core_ir::release_recipe::Recipe;
use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod conversation;
#[cfg(test)]
mod laws;
mod preview;
mod pump;
mod source;

use conversation::Conversation;
pub(crate) use preview::decide;
use preview::{approve_grant, decision_json, held_fate, preview_json};
use pump::Pump;
use source::{Source, submission};

type Error = Box<dyn std::error::Error>;

/// The exit status of an outcome the operator cannot know yet: retry with the same request.
const UNAVAILABLE: i32 = 75;
/// How long replay waits for a running host's pin answer.
const PIN_WAIT: Duration = Duration::from_secs(5);
/// The admission bytes per `Data` frame.
const CHUNK: usize = 256 << 10;

pub(crate) fn installed(name: &str) -> Result<PathBuf, Error> {
    let executable = crate::installation::compiler_executable()?;
    let installation = crate::installation::Installed::discover(&executable)?
        .ok_or("hirsel is not running from an installation")?;
    Ok(installation.libexec(name))
}

/// Run the launcher in the foreground with this installation's platform host
/// (launch.md "Starting the host").
pub(crate) fn host(invocation: Request<HostOption>) -> Result<std::convert::Infallible, Error> {
    if invocation.positional.is_some() || !invocation.arguments.is_empty() {
        return Err(
            "host takes no recipe or arguments; register groups with hirsel register".into(),
        );
    }
    let mut state = None;
    let mut max_places = false;
    let mut lifecycle = None;
    let mut options = Vec::new();
    for option in invocation.options {
        let forwarded = match option {
            HostOption::HostState(value) => {
                state = Some(PathBuf::from(value));
                continue;
            }
            HostOption::MaxPlaces(value) => {
                max_places = true;
                LauncherOption::MaxPlaces(value)
            }
            HostOption::Lifecycle(value) => {
                lifecycle = Some(crate::launcher::parse_lifecycle(&value)?);
                LauncherOption::Lifecycle(value)
            }
            HostOption::IdleMs(value) => LauncherOption::IdleMs(value),
            HostOption::Admission(value) => LauncherOption::Admission(value),
        };
        options.push(forwarded);
    }
    let state = state.ok_or("host requires --state DIR")?;
    if !max_places {
        return Err("host requires --max-places N".into());
    }
    crate::launcher::resolve_lifecycle(lifecycle)?;
    let path = |name: &str| state.join(name).to_string_lossy().into_owned();
    options.extend([
        LauncherOption::HostState(state.to_string_lossy().into_owned()),
        LauncherOption::Control(path(crate::product::bundled::LAUNCHER_SOCKET)),
        LauncherOption::OperatorListen(path(crate::product::bundled::OPERATOR_SOCKET)),
    ]);
    crate::launcher::run(Request {
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
    limits: crate::code_admission::AdmissionLimits,
}

impl Upload {
    fn open(root: Option<&str>, store: Option<PathBuf>, state: &Path) -> Result<Self, Error> {
        let root = parse_root(root.ok_or("expected a recipe hash")?)?;
        let store = CodeStore::new(store.ok_or("requires --code-store DIR")?);
        let limits = match crate::launcher::admit::read_envelope(state) {
            Ok(limits) => limits,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(NoHost(format!(
                    "no host has published admission bounds in {}",
                    state.display()
                ))
                .into());
            }
            Err(error) => return Err(error.into()),
        };
        let recipe = crate::launcher::read_recipe(&store, root, Some(limits.recipe_bytes))?;
        Ok(Self {
            store,
            root,
            recipe,
            limits,
        })
    }
}

fn request_id(text: Option<String>) -> Result<Vec<u8>, Error> {
    match text {
        Some(text) => hex::decode(&text)
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
pub(crate) fn register(invocation: Request<RegisterOption>) -> Result<i32, Error> {
    or_unavailable("register", register_on(invocation))
}

struct Register {
    state: Option<PathBuf>,
    inputs: Submission,
    name: Option<String>,
    listen: Option<String>,
    tenant: String,
    trusted: bool,
    memory: Option<u64>,
    record: Option<PathBuf>,
    front: Option<Front>,
    retention_days: u64,
    packages: Vec<[u8; 32]>,
    request_id: Option<String>,
    arguments: Vec<String>,
}

enum RegisterPath {
    Store(PathBuf),
    Workdir(PathBuf),
}

impl Register {
    fn read(
        invocation: Request<RegisterOption>,
        paths: &mut Vec<RegisterPath>,
    ) -> Result<Self, Error> {
        let mut request = Self {
            state: None,
            inputs: Submission {
                positional: invocation.positional,
                ..Submission::default()
            },
            name: None,
            listen: None,
            tenant: "operator".into(),
            trusted: false,
            memory: None,
            record: None,
            front: None,
            retention_days: 30,
            packages: Vec::new(),
            request_id: None,
            arguments: invocation.arguments,
        };
        use RegisterOption as O;
        for option in invocation.options {
            match option {
                O::HostState(text) => request.state = Some(PathBuf::from(text)),
                O::GroupName(text) => request.name = Some(text),
                O::Tenant(text) => request.tenant = text,
                O::Listen(text) => request.listen = Some(text),
                O::FrontHttp(ref text) | O::FrontStream(ref text) => {
                    let forward = if matches!(option, O::FrontHttp(_)) {
                        Forward::Http
                    } else {
                        Forward::Stream
                    };
                    let front = request.front.get_or_insert_with(|| Front {
                        names: Vec::new(),
                        forward,
                    });
                    if front.forward != forward {
                        return Err(
                            "a group cannot use both --front-http and --front-stream".into()
                        );
                    }
                    front.names.push(text.clone());
                }
                O::GroupStore(text) => paths.push(RegisterPath::Store(text.into())),
                O::CodeStore(text) => request.inputs.code = Some(PathBuf::from(text)),
                O::Workdir(text) => paths.push(RegisterPath::Workdir(text.into())),
                O::Trusted => request.trusted = true,
                O::HostMemory(text) => request.memory = Some(text.parse()?),
                O::Record(text) => request.record = Some(PathBuf::from(text)),
                O::TapeRetention(text) => {
                    request.retention_days = text
                        .parse::<u64>()
                        .ok()
                        .filter(|&n| n != 0)
                        .ok_or("invalid tape retention")?
                }
                O::Grant(text) => request.inputs.grants.push(text),
                O::RequestId(text) => request.request_id = Some(text),
                O::SourceDir(text) => request.inputs.source = Some(PathBuf::from(text)),
                O::PackageRoot(text) => request.packages.push(
                    crate::package_content::Root::parse(&text)
                        .ok_or("--package-root takes sha256:HEX")?
                        .0,
                ),
            }
        }
        Ok(request)
    }

    fn registration(
        self,
        workdir: PathBuf,
        store: Option<PathBuf>,
        record: Option<PathBuf>,
    ) -> Result<Registration, Error> {
        Ok(Registration {
            name: self.name.ok_or("register requires --group NAME")?,
            tenant: self.tenant,
            workdir: workdir.to_string_lossy().into_owned(),
            trusted: self.trusted,
            listen: self.listen.ok_or("register requires --listen ADDR")?,
            store: store
                .ok_or("register requires --store DIR")?
                .to_string_lossy()
                .into_owned(),
            memory: self.memory,
            args: self.arguments,
            record: record.map(|dir| Record {
                dir: dir.to_string_lossy().into_owned(),
                retention_s: self.retention_days.saturating_mul(86400),
            }),
            front: self.front,
            packages: self.packages,
        })
    }
}

fn register_on(invocation: Request<RegisterOption>) -> Result<i32, Error> {
    let mut workdir = std::env::current_dir()?;
    let mut paths = Vec::new();
    let request = Register::read(invocation, &mut paths);
    let mut store = None;
    for step in paths {
        match step {
            RegisterPath::Store(path) => store = Some(std::path::absolute(path)?),
            RegisterPath::Workdir(path) => workdir = path.canonicalize()?,
        }
    }
    let mut request = request?;
    let state = request
        .state
        .take()
        .ok_or("register requires --state DIR")?;
    let (input, upload, source, approvals) = std::mem::take(&mut request.inputs).open(&state)?;
    if let Some(upload) = &upload {
        if let Some(reason) = approvals.check(&upload.recipe) {
            eprintln!("register: refused: {reason}");
            return Ok(2);
        }
        if request.record.is_some()
            && let Some(reason) = foreign_recording_refusal(&upload.recipe)
        {
            eprintln!("register: refused: {reason}");
            return Ok(2);
        }
    }
    // The recordings directory is made only for a registration the CLI does not refuse.
    let record = request
        .record
        .take()
        .map(crate::launcher::private_dir)
        .transpose()?;
    let id = request.request_id.take();
    let registration = request.registration(workdir, store, record)?;
    let d = request_id(id)?;
    eprintln!("request {}", hex::encode(&d));
    let ask = Ask::Register(
        d.clone(),
        Box::new(registration),
        input,
        approvals.foreign.clone(),
        approvals.exec.clone(),
    );
    let mut exchange = Conversation::new(&state, d, "register", upload, approvals);
    exchange.source = source;
    exchange.run(ask)
}

/// `hirsel deploy R --state S --group NAME --code-store DIR [--request-id HEX]
/// [--approve-grant +LINE|-LINE ...] [--grant ...]` or `hirsel deploy --state S --status HEX`.
/// A request that awaits any other decision is decided with `hirsel decide`.
pub(crate) fn deploy(invocation: Request<DeployOption>) -> Result<i32, Error> {
    or_unavailable("deploy", deploy_on(invocation))
}

#[derive(Default)]
struct Submission {
    positional: Option<String>,
    code: Option<PathBuf>,
    source: Option<PathBuf>,
    grants: Vec<String>,
}

impl Submission {
    fn open(
        self,
        state: &Path,
    ) -> Result<(operator::Input, Option<Upload>, Option<Source>, Approvals), Error> {
        submission(
            self.source,
            self.positional.as_deref(),
            self.code,
            self.grants,
            state,
        )
    }
}

enum Deploy {
    Status(Vec<u8>),
    Submit {
        inputs: Submission,
        name: Option<String>,
        request_id: Option<String>,
        decision: Grants,
        held: Fates,
    },
}

impl Deploy {
    fn read(invocation: Request<DeployOption>) -> Result<(PathBuf, Self), Error> {
        let (mut state, mut code, mut name, mut d, mut status) = (None, None, None, None, None);
        let mut decision = Grants::default();
        let (mut lines, mut source_dir) = (Vec::new(), None);
        let mut held = Fates::default();
        let mut held_given = false;
        use DeployOption as O;
        for option in invocation.options {
            match option {
                O::HostState(text) => state = Some(PathBuf::from(text)),
                O::CodeStore(text) => code = Some(PathBuf::from(text)),
                O::GroupName(text) => name = Some(text),
                O::RequestId(text) => d = Some(text),
                O::RequestStatus(text) => status = Some(text),
                O::ApproveGrant(text) => approve_grant(&mut decision, &text)?,
                O::Grant(text) => lines.push(text),
                O::SourceDir(text) => source_dir = Some(PathBuf::from(text)),
                O::HeldAll(ref text) | O::HeldEntry(ref text) => {
                    held_fate(&mut held, matches!(option, O::HeldAll(_)), text)?;
                    held_given = true;
                }
            }
        }
        let state = state.ok_or("deploy requires --state DIR")?;
        if !invocation.arguments.is_empty() {
            return Err(
                "deploy takes no program arguments; a group's arguments are registered".into(),
            );
        }
        let has_decision = decision != Grants::default();
        let submission_options = code.is_some()
            || name.is_some()
            || !lines.is_empty()
            || source_dir.is_some()
            || held_given;
        if let Some(text) = status {
            if invocation.positional.is_some() || d.is_some() || submission_options || has_decision
            {
                return Err("deploy --status takes only --state".into());
            }
            let d = hex::decode(&text)
                .filter(|d| d.len() == 16)
                .ok_or("--status takes 32 hex digits")?;
            return Ok((state, Self::Status(d)));
        }
        sort_grants(&mut decision);
        let inputs = Submission {
            positional: invocation.positional,
            code,
            source: source_dir,
            grants: lines,
        };
        Ok((
            state,
            Self::Submit {
                inputs,
                name,
                request_id: d,
                decision,
                held,
            },
        ))
    }
}

fn deploy_on(invocation: Request<DeployOption>) -> Result<i32, Error> {
    let (state, request) = Deploy::read(invocation)?;
    let (inputs, name, d, decision, held) = match request {
        Deploy::Status(d) => return status_of(&state, &d),
        Deploy::Submit {
            inputs,
            name,
            request_id,
            decision,
            held,
        } => (inputs, name, request_id, decision, held),
    };
    let (input, upload, source, approvals) = inputs.open(&state)?;
    if let Some(upload) = &upload
        && let Some(reason) = approvals.check(&upload.recipe)
    {
        eprintln!("deploy: refused: {reason}");
        return Ok(2);
    }
    let given = d.is_some();
    let d = request_id(d)?;
    eprintln!("request {}", hex::encode(&d));
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
        input.into(),
        approvals.foreign.clone(),
        approvals.exec.clone(),
        held,
    );
    let mut exchange = Conversation::new(&state, d, "deploy", upload, approvals);
    exchange.decision = Some(decision);
    exchange.source = source;
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
        None => eprintln!("{command}: outcome unknown: request {}", hex::encode(d)),
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
    hex::encode(&a.bytes())
}

fn state_json(d: &[u8], group: &Group, phase: &Phase) -> serde_json::Value {
    let mut out = serde_json::json!({
        "request": hex::encode(d),
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
            "phase": "installed", "activation": v, "assembly": hex::encode(assembly), "report": report_json(report),
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
        .arg(hex::encode(&id))
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

struct Replay {
    state: Option<PathBuf>,
    tape: PathBuf,
    diagnose: bool,
    unit: Option<String>,
    outputs: Option<PathBuf>,
}

impl Replay {
    fn read(invocation: Request<CompilerOption>) -> Result<Self, Error> {
        if invocation.positional.is_some() || !invocation.arguments.is_empty() {
            return Err(
                "host recording replay takes its program and arguments from the pin".into(),
            );
        }
        let (mut state, mut tape, mut diagnose, mut unit) = (None, None, false, None);
        let mut outputs = None;
        use CompilerOption as O;
        for option in invocation.options {
            match option {
                O::HostState(value) => state = Some(PathBuf::from(value)),
                O::Tape(value) => tape = Some(PathBuf::from(value)),
                O::Diagnose => diagnose = true,
                O::ReplayOutputs(value) => outputs = Some(PathBuf::from(value)),
                O::ReplayUnit(value) => unit = Some(value),
                _ => return Err("unexpected option for host recording replay".into()),
            }
        }
        Ok(Self {
            state,
            tape: tape.ok_or("replay requires --tape TAPE")?,
            diagnose,
            unit,
            outputs,
        })
    }
}

/// `hirsel replay --state S --tape TAPE [--diagnose] [--unit NAME]`: run a
/// recording's retained image, or replay one unit's stream alone.
pub(crate) fn replay(invocation: Request<CompilerOption>) -> Result<i32, Error> {
    let Replay {
        state,
        tape,
        diagnose,
        unit,
        outputs,
    } = Replay::read(invocation)?;
    let tape = tape.canonicalize()?;
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
    let image = pinned_image(&state, &objects, assembly, &launch);
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
    let mut command = Command::new(crate::host::fd_path(&image)?);
    command
        .arg("--hirsel-replay-recording")
        .arg(if diagnose { "diagnose" } else { "same" })
        .arg(tape)
        .args(unit);
    let status = crate::tape_cli::outputs::execute(&mut command, outputs.as_deref())?;
    if status.success() {
        println!("Sealed");
    }
    Ok(status.code().unwrap_or(1))
}

fn pinned_image(
    state: &Path,
    objects: &crate::launcher::objects::Records,
    pinned: ContentHash,
    launch: &[u8],
) -> io::Result<Option<std::fs::File>> {
    let assembly = objects.assembly(pinned)?;
    let recipe =
        crate::launcher::read_recipe(&CodeStore::new(state.join("code")), assembly.recipe, None)?;
    if let Some(reason) = foreign_recording_refusal(&recipe) {
        eprintln!("replay: refused: {reason}");
        return Ok(None);
    }
    check_pin_digest(launch_digest(launch), assembly.digest)?;
    objects.open_image(assembly.digest).map(Some)
}

fn check_pin_digest(recorded: Option<ContentHash>, pinned: ContentHash) -> io::Result<()> {
    if recorded == Some(pinned) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recording image differs from its pin",
        ))
    }
}

fn launch_digest(snapshot: &[u8]) -> Option<ContentHash> {
    let mut cursor = hirsel_tape_format::Cursor::new(snapshot);
    for _ in 0..cursor.varint().ok()? {
        cursor.bytes().ok()?;
    }
    Some(ContentHash(cursor.bytes().ok()?.try_into().ok()?))
}
