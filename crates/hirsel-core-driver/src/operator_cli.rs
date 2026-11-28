//! The operator's commands (launch.md "Starting the host" and "The operator socket"):
//! `hirsel host` runs the launcher with the installed platform host, `hirsel op decide` and
//! `hirsel deploy` drive one exchange each on `STATE/operator.sock`, and `hirsel tape replay
//! --state` resolves a recording's pin through the host or, with no host, the offline pin
//! reader.
use crate::cli::{CompilerOption, HostOption, LauncherOption, Request};
use crate::code_store::CodeStore;
use crate::launcher::session::{Attempt, Group};
use crate::native::recipe_build::{foreign_grant_refusal, parse_root};
use crate::operator::{self, Answer, Ask, Class, Pause, Phase, Report};
use hirsel_core_ids::ContentHash;
use hirsel_core_ids::hex;
use hirsel_core_ir::release_recipe::Recipe;
use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub(crate) mod adopt;
pub(crate) mod archive;
mod archive_decision;
mod bucket;
mod config;
mod context;
mod decision;
pub(crate) mod pitr;
pub(crate) mod pitr_decision;
mod receipt;
pub(crate) mod retire;
pub(crate) mod store_process;
mod submit;
pub(crate) use decision::decide;
pub(crate) use submit::deploy;
mod conversation;
pub(crate) mod finite;
#[path = "operator_cli/observe.rs"]
pub(crate) mod observe;
pub(crate) use context::operation_id;
#[cfg(test)]
mod grants_law;
#[cfg(test)]
mod laws;
mod preview;
mod pump;
#[cfg(test)]
mod slice2_law;
mod source;
pub(crate) mod wait;
pub(crate) use wait::{Observer, duration as wait_duration};

use preview::{decision_json, preview_json};
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
            "host takes no recipe or arguments; create groups with hirsel deploy --create".into(),
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

/// The Foreign and Exec approvals requested by the submitted source or recipe, sorted and unique.
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
        return Err("attested recipes fix their own bindings; the group may list only Foreign and Exec approvals".into());
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

fn class_name(class: Class) -> &'static str {
    match class {
        Class::Protocol => "protocol",
        Class::Denied => "denied",
        Class::UnknownRequest => "unknown request",
        Class::UnknownGroup => "unknown group",
        Class::UnknownActivation => "unknown activation",
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
        Phase::AwaitStore(_) => serde_json::json!({"phase":"awaiting child acceptance"}),
        Phase::Retiring => serde_json::json!({"phase":"retiring"}),
        Phase::Retired => serde_json::json!({"phase":"retired"}),
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
                O::Json => (),
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

/// Run a recording's retained image, or replay one unit's stream alone.
pub(crate) fn replay(invocation: Request<CompilerOption>) -> Result<i32, Error> {
    use finite::{Envelope, Failure};
    use serde_json::json;
    let machine = invocation.options.contains(&CompilerOption::Json);
    let mut env = Envelope::read("tape replay");
    env.contract = "execute";
    let action = || -> Result<(), Failure> {
        let Replay {
            state,
            tape,
            diagnose,
            unit,
            outputs,
        } = Replay::read(invocation).map_err(|e| Failure::input("arguments", e))?;
        let tape = tape.canonicalize().map_err(|e| Failure::input("tape", e))?;
        let state =
            state.ok_or_else(|| Failure::input("state", "select the host's state directory"))?;
        let (tape_id, launch) = crate::tape_cli::run_route(&tape)
            .map_err(|e| Failure::negative("TapeUnreadable", &e.to_string()))?;
        let gone =
            || Failure::negative("ImageGone", "the recording's retained image is unavailable");
        let assembly = match resolve_pin(&state, tape_id)
            .map_err(|e| Failure::blocked("PinUnavailable", &e.to_string()))?
        {
            Pin::Found(assembly) => assembly,
            Pin::Gone => return Err(gone()),
            Pin::Unavailable(why) => return Err(Failure::blocked("PinUnavailable", &why)),
        };
        let objects = crate::launcher::objects::Records::read_only(&state.join("objects"))
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    gone()
                } else {
                    Failure::io(e)
                }
            })?;
        let image = match pinned_image(&state, &objects, assembly, &launch) {
            Ok(Some(file)) => file,
            Ok(None) => return Err(Failure::input("image", "the pinned recipe refuses replay")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(gone()),
            Err(error) => return Err(Failure::input("image", error)),
        };
        let mut command = Command::new(crate::host::fd_path(&image).map_err(Failure::io)?);
        command
            .arg("--hirsel-replay-recording")
            .arg(if diagnose { "diagnose" } else { "same" })
            .arg(tape)
            .args(unit);
        if machine {
            command.stdout(std::process::Stdio::null());
        }
        let status = crate::tape_cli::outputs::execute(&mut command, outputs.as_deref())
            .map_err(|e| Failure::input("execution", e))?;
        use std::os::unix::process::ExitStatusExt;
        env.result = json!({"assembly":assembly.to_string(),"guest":{"exit":status.code(),"signal":status.signal()}});
        if !status.success() {
            return Err(Failure::negative(
                "ReplayRefused",
                "the pinned image did not seal the replay",
            ));
        }
        env.outcome = json!({"kind":"Sealed","effect":"none"});
        Ok(())
    };
    if let Err(error) = action() {
        env.fail(error);
    }
    if !machine && env.exit == 0 {
        println!("Sealed");
        Ok(0)
    } else {
        Ok(env.render(machine))
    }
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
    if let Some(reason) = crate::reachability::foreign_recording_refusal(&recipe) {
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
