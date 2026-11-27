//! Streamed admission (docs/spec/launch.md "The launcher", "Publish and assemble"): each
//! attempt runs the code-admission verifier on a worker thread over a socket pair, so the
//! state loop keeps serving child ends and fences while an upload or an assembly runs.
//! The verifier, its reservations and the assembly path are those of local admission; only
//! the byte transport is the session's `AdmitData` chunks. Submitted source is first
//! published by `hirsel publish` in a child process under the host's caches, and then
//! admitted as that publication.
use super::objects::Records;
use super::session::{Approvals, Attempt, Failure, Grants, Green, Summary, Wire};
use super::{Conn, Readiness};
use crate::code_admission::{
    AdmissionError, AdmissionLimits, AdmissionStore, AdmittedRelease, ControlConnection,
    NativeSemantics, ObjectKey, ObjectKind, send_release_until,
};
use crate::code_store::{CodeStore, PUBLISH_RECIPE_STAGE};
use crate::native::recipe_build::foreign_grant_lines;
use crate::native::recipe_grants::grant_units;
use crate::{AssemblyRecord, Link, NativeOptions, Publication};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::decode::Debit;
use hirsel_core_ir::release_recipe::Recipe;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

type Error = Box<dyn std::error::Error + Send + Sync>;

// Keep decoder stack use bounded independently of the launcher's byte and work grants.
const DECODER_NESTING: usize = 512;

/// The control-only launcher's admission bounds, a test and embedding fixture. A launcher
/// started with a platform host instead uses its declared admission envelope.
pub(crate) fn limits() -> AdmissionLimits {
    AdmissionLimits {
        recipe_bytes: 16 << 20,
        closure_objects: 196_608,
        object_bytes: 512 << 20,
        decoded_bytes: 2 << 30,
        members: 65_536,
        edges: 4 << 20,
        nesting: DECODER_NESTING,
        outstanding: 196_608,
        timeout: None,
    }
}

/// The bytes the code store holds for recipe `root`, checked against its name.
pub(crate) fn read_recipe(
    code: &CodeStore,
    root: ContentHash,
    max: Option<usize>,
) -> io::Result<Recipe> {
    let max = max.map_or(u64::MAX, |max| max as u64);
    let mut bytes = Vec::new();
    code.open(ObjectKind::Recipe, root)?
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max || Recipe::hash(&bytes) != root {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "stored recipe hash or length",
        ));
    }
    Ok(Recipe::decode_retained(&bytes, &Debit::unlimited())?)
}

/// The authority `new`'s grants give that `old`'s do not, and the authority they drop.
pub(crate) fn grant_diff(old: &Recipe, new: &Recipe) -> Grants {
    let (old, new) = (grant_units(old), grant_units(new));
    Grants {
        added: new.difference(&old).cloned().collect(),
        removed: old.difference(&new).cloned().collect(),
    }
}

/// The Foreign confirmation lines a deploy of `recipe` prints before it runs.
pub(crate) fn deploy_foreign_lines(recipe: &Recipe, root: ContentHash) -> String {
    let mut output = String::new();
    for line in foreign_grant_lines(recipe, root) {
        output.push_str(&line);
        output.push('\n');
    }
    output
}

/// How the launcher starts the compiler for submitted source: a `hirsel` command with the
/// given arguments. The launcher runs its own executable, which is `hirsel`.
pub(crate) type Compiler = Arc<dyn Fn(&[String]) -> io::Result<Command> + Send + Sync>;

fn own_compiler() -> Compiler {
    Arc::new(|args| {
        let mut command = Command::new(std::env::current_exe()?);
        command.args(args);
        Ok(command)
    })
}

/// The admission side of the launcher's stores: the code store with its staging, and the
/// assembly records the image store shares with launches.
pub(crate) struct Admission {
    store: AdmissionStore,
    code: CodeStore,
    records: Arc<Records>,
    state: PathBuf,
    limits: AdmissionLimits,
    compiler: Compiler,
    /// The compiler identity a green gate names; `None` reads this executable's.
    toolchain: Option<ContentHash>,
    /// Held while a worker writes an assembly record and tells the loop, and while the
    /// loop collects: a collection never removes a record whose `Assembled` it has not seen.
    pub(super) publish: Mutex<()>,
}

impl Admission {
    pub(crate) fn open(
        state: &Path,
        records: Arc<Records>,
        limits: AdmissionLimits,
        staging: u64,
    ) -> io::Result<Self> {
        // Scratch of attempts a lost launcher left behind.
        for scratch in ["assemble", "source"] {
            let dir = state.join(scratch);
            match fs::remove_dir_all(&dir) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => fs::create_dir(&dir)?,
            }
        }
        let code = state.join("code");
        Ok(Self {
            store: AdmissionStore::open(&code, staging)?,
            code: CodeStore::new(code),
            records,
            state: state.to_owned(),
            limits,
            compiler: own_compiler(),
            toolchain: None,
            publish: Mutex::new(()),
        })
    }

    /// This admission, compiling submitted source with `compiler`.
    #[cfg(test)]
    pub(crate) fn compile_with(mut self, compiler: Compiler) -> Self {
        self.compiler = compiler;
        self
    }

    /// This admission, naming `toolchain` as the compiler identity of a green gate.
    #[cfg(test)]
    pub(crate) fn toolchain(mut self, toolchain: ContentHash) -> Self {
        self.toolchain = Some(toolchain);
        self
    }

    /// Run `action` under the admission store's collection lock, with the code objects
    /// reserved by admissions in flight.
    pub(super) fn under_collection<R>(
        &self,
        action: impl FnOnce(&dyn Fn(ObjectKey) -> bool) -> R,
    ) -> R {
        self.store.under_collection(action)
    }

    /// The most exchange bytes an attested admission can take: the recipe and object
    /// limits, with each object's 41-byte frame and the end tag.
    pub(super) fn input_bound(&self) -> u64 {
        let l = &self.limits;
        (l.recipe_bytes as u64 + 40)
            .saturating_add(l.object_bytes)
            .saturating_add(41 * l.outstanding as u64 + 1)
    }
}

pub(super) enum Note {
    Admitted {
        id: u64,
        attempt: Attempt,
        root: Vec<u8>,
        summary: Summary,
        display: String,
    },
    Assembled {
        id: u64,
        attempt: Attempt,
        key: [u8; 32],
        digest: Vec<u8>,
        fetch: u64,
        assembly: u64,
    },
    Failed(u64, Attempt, Failure),
}

/// Where the worker's recipe comes from.
pub(super) enum Source {
    /// The host streams the exchange; the loop holds this end of the pair.
    Attested(ContentHash),
    /// The launcher's own code store sends it.
    Publication(ContentHash),
    /// The worker publishes this source into the code store, which then sends it. With
    /// the tree's digest, the law gate runs on it first.
    Tree(Tree, Option<[u8; 32]>),
}

#[cfg(test)]
pub(super) fn fixture_deadline_ms() -> u64 {
    let expiry = std::time::SystemTime::now() + Duration::from_secs(600);
    u64::try_from(
        expiry
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// The source a submitted tree's codec `bytes` admit; a gated one carries the SHA-256 of
/// the tree's manifest, the bytes the operator's Begin named it by, which a green gate names.
pub(super) fn source(bytes: &[u8], gated: bool) -> Result<Source, Failure> {
    use sha2::Digest as _;
    let tree = tree(bytes)?;
    let digest = gated.then(|| sha2::Sha256::digest(tree.manifest()).into());
    Ok(Source::Tree(tree, digest))
}

/// Most files in one submitted tree.
const TREE_FILES: usize = 65_536;

/// The submitted tree `bytes` encode, with every path plain and relative: no empty, `.`
/// or `..` component, no repeat, and no file that is also another file's directory.
pub(super) fn tree(bytes: &[u8]) -> Result<Tree, Failure> {
    let invalid = |why: &str| Failure::Invalid(format!("source tree: {why}"));
    if bytes.len() > TREE_BYTES {
        return Err(invalid("over its byte limit"));
    }
    let tree = Tree::decode(bytes).map_err(|_| invalid("malformed"))?;
    let plain = |path: &str| {
        !path.is_empty()
            && path
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\0'))
    };
    // The package is the child's one positional argument, so it never reads as an option.
    if tree.package.starts_with('-') || tree.package != "." && !plain(&tree.package) {
        return Err(invalid(
            "the package directory is not a plain relative path",
        ));
    }
    if tree.files.len() > TREE_FILES {
        return Err(invalid("too many files"));
    }
    if tree
        .files
        .iter()
        .map(|(_, bytes)| bytes.len())
        .sum::<usize>()
        > SOURCE_BYTES
    {
        return Err(invalid("its files are over the content limit"));
    }
    let mut paths = BTreeSet::new();
    for (path, _) in &tree.files {
        if !plain(path) {
            return Err(invalid("a file path is not a plain relative path"));
        }
        if !paths.insert(path.as_str()) {
            return Err(invalid("a file path repeats"));
        }
    }
    // In sorted order a file's descendants follow it, before any later sibling.
    let mut earlier: Option<&str> = None;
    for path in &paths {
        if earlier.is_some_and(|e| path.strip_prefix(e).is_some_and(|r| r.starts_with('/'))) {
            return Err(invalid("a file path is also a directory"));
        }
        earlier = Some(path);
    }
    Ok(tree)
}

/// The loop's handle on one admitting attempt. Dropping it cancels the attempt: the worker
/// sees its exchange end or its `Assemble` channel close, and drops its staging.
pub(super) struct Worker {
    pub(super) id: u64,
    /// The loop's end of an attested exchange.
    pub(super) conn: Option<Conn>,
    pub(super) received: u64,
    pub(super) ending: bool,
    pub(super) shut: bool,
    pub(super) admitted: bool,
    need: NeedReader,
    go: Option<mpsc::Sender<()>>,
    /// Set when the loop drops the attempt; a running compile is killed.
    cancel: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The worker's single Need on the exchange: a u64 count, then that many 33-byte keys.
#[derive(Default)]
struct NeedReader {
    left: Option<u64>,
    keys: Vec<ObjectKey>,
    done: bool,
}

/// Keys per `Need` event, well inside one session frame.
const NEED_BATCH: usize = 1 << 16;

impl Worker {
    /// Begin admitting `attempt` on a new worker thread.
    #[expect(clippy::too_many_arguments, reason = "one worker's whole input")]
    pub(super) fn spawn(
        admission: Arc<Admission>,
        poll: &Readiness,
        id: u64,
        attempt: Attempt,
        source: Source,
        approvals: Approvals,
        deadline: Instant,
        tell: impl Fn(Note) + Send + 'static,
    ) -> io::Result<Self> {
        let (ours, theirs) = UnixStream::pair()?;
        let (go, gone) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (conn, attested, local) = match source {
            Source::Attested(root) => (Some(Conn::new(poll, ours)?), Some(root), None),
            Source::Publication(root) => (None, None, Some((Ok(root), ours))),
            Source::Tree(tree, gate) => (None, None, Some((Err((tree, gate)), ours))),
        };
        let cancelled = cancel.clone();
        std::thread::Builder::new()
            .name("hirsel-admit".into())
            .spawn(move || {
                let mut green = None;
                let publication = match local {
                    None => None,
                    Some((Ok(root), ours)) => Some((root, ours)),
                    Some((Err((tree, gate)), ours)) => {
                        match publish(&admission, &tree, gate, &approvals, &cancelled, deadline) {
                            None => return,
                            Some(Ok((root, gated))) => {
                                green = gated;
                                Some((root, ours))
                            }
                            Some(Err(why)) => return tell(Note::Failed(id, attempt, why)),
                        }
                    }
                };
                let note = work(
                    &admission,
                    id,
                    attempt,
                    theirs,
                    attested,
                    publication,
                    green,
                    &approvals,
                    &gone,
                    deadline,
                    &tell,
                );
                if let Some(note) = note {
                    tell(note);
                }
            })?;
        Ok(Self {
            id,
            conn,
            received: 0,
            ending: false,
            shut: false,
            admitted: false,
            need: NeedReader::default(),
            go: Some(go),
            cancel,
        })
    }

    /// Let the worker assemble its verified release.
    pub(super) fn assemble(&mut self) {
        if let Some(go) = self.go.take() {
            let _ = go.send(());
        }
    }

    /// Move queued input to the worker, close its input after `AdmitEnd`, and read its
    /// Need; answer the `Need` parts now complete.
    pub(super) fn pump(&mut self) -> Vec<(Vec<ObjectKey>, bool)> {
        let Some(conn) = &mut self.conn else {
            return Vec::new();
        };
        conn.flush();
        if self.ending && conn.output.is_empty() && !self.shut {
            let _ = conn.stream.shutdown(std::net::Shutdown::Write);
            self.shut = true;
        }
        conn.fill();
        let mut parts = Vec::new();
        let need = &mut self.need;
        while !need.done {
            let input = &mut conn.input;
            match need.left {
                None => {
                    let Some(count) = input.get(..8) else { break };
                    let count = u64::from_le_bytes(count.try_into().unwrap());
                    input.drain(..8);
                    // u64::MAX is a refusal before Need; the worker's note carries it.
                    need.done = count == u64::MAX;
                    need.left = Some(count);
                }
                Some(0) => {
                    parts.push((std::mem::take(&mut need.keys), true));
                    need.done = true;
                }
                Some(left) => {
                    let Some(frame) = input.get(..33) else { break };
                    let kind = match frame[0] {
                        1 => ObjectKind::Group,
                        2 => ObjectKind::Body,
                        3 => ObjectKind::Annex,
                        4 => ObjectKind::ForeignArchive,
                        5 => ObjectKind::Value,
                        _ => {
                            need.done = true;
                            break;
                        }
                    };
                    let hash = ContentHash(frame[1..].try_into().unwrap());
                    input.drain(..33);
                    need.keys.push(ObjectKey { kind, hash });
                    need.left = Some(left - 1);
                    if left > 1 && need.keys.len() == NEED_BATCH {
                        parts.push((std::mem::take(&mut need.keys), false));
                    }
                }
            }
        }
        if need.done {
            conn.input.clear();
        }
        parts
    }
}

pub(super) fn deadline_from_wall_ms(deadline_ms: u64) -> Option<Instant> {
    let now = Instant::now();
    let wall = SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(deadline_ms))?;
    let left = wall.duration_since(SystemTime::now()).ok()?;
    now.checked_add(left).filter(|deadline| *deadline > now)
}

fn failure(error: AdmissionError) -> Failure {
    match error {
        AdmissionError::RecordingForeign(reason) | AdmissionError::Refused(reason) => {
            Failure::Denied(reason)
        }
        error => Failure::Invalid(error.to_string()),
    }
}

/// A verifier or upload failure inside the exchange, with its wire phase attached.
fn exchange_failure(error: AdmissionError) -> Failure {
    match failure(error) {
        Failure::Invalid(text) => Failure::Invalid(format!("admission exchange: {text}")),
        denied => denied,
    }
}

#[path = "tree.rs"]
mod submitted;
pub(crate) use submitted::Tree;
pub(super) use submitted::lock_roots_allowed;

#[path = "protocol.rs"]
pub(crate) mod protocol;
use protocol::{SOURCE_BYTES, TREE_BYTES};

#[path = "gate.rs"]
pub(crate) mod gate;

#[path = "envelope.rs"]
mod envelope;
pub(crate) use envelope::{launcher_envelope, read_envelope, write_envelope};

/// The most diagnostic bytes a refused compile reports; the rest is cut and marked.
const DIAGNOSTIC_BYTES: usize = 64 << 10;

/// Leave the group leader waitable until the group is killed, so its PID cannot be
/// reused between observing its exit and signalling the remaining processes.
/// A run still going at `deadline` is killed and is a `TimedOut` error.
pub(super) fn wait_for_compile(
    child: &mut Child,
    cancel: &AtomicBool,
    deadline: Option<Instant>,
) -> io::Result<ExitStatus> {
    let pid = child.id() as libc::pid_t;
    let mut late = false;
    while !cancel.load(Ordering::Relaxed) {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            late = true;
            break;
        }
        // SAFETY: waitid writes into a siginfo for our own child without reaping it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            if unsafe { info.si_pid() } != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    // SAFETY: spawn put the unreaped child at the head of its own process group.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let status = child.wait()?;
    if late {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the run passed its deadline",
        ));
    }
    Ok(status)
}

fn source_unit(tree: &Tree) -> String {
    let package = Path::new(&tree.package);
    let manifest = package
        .strip_prefix(".")
        .unwrap_or(package)
        .join("hirsel.toml");
    tree.files
        .iter()
        .find(|(name, _)| Path::new(name) == manifest)
        .and_then(|(_, bytes)| crate::package_manifest::Manifest::parse(bytes, &manifest).ok())
        .and_then(|manifest| manifest.package)
        .map_or_else(
            || format!("package at {:?}", tree.package),
            |package| package.entry.to_string(),
        )
}

/// Publish `tree` into the launcher's code store with `hirsel publish`, run on a fresh copy
/// of the tree under the host's compiler and package stores, and return its recipe root.
/// The recipe stays private until the compiler exits successfully and, when `gate` holds
/// the tree's digest, the law gate is green on the same scratch; the green verdict is
/// returned beside the root. The child inherits the toolchain's memory limit
/// (`HIRSEL_MEMORY_LIMIT`). `None` is a cancelled compile.
fn publish(
    ad: &Admission,
    tree: &Tree,
    gate: Option<[u8; 32]>,
    approvals: &Approvals,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Option<Result<(ContentHash, Option<Green>), Failure>> {
    let unrun =
        |e: &dyn std::fmt::Display| Some(Err(Failure::Invalid(format!("publish source: {e}"))));
    if Instant::now() >= deadline {
        return unrun(&"the admission deadline passed");
    }
    let old = approvals.old.as_deref();
    if let Err(outside) = lock_roots_allowed(tree, &approvals.packages) {
        return Some(Err(outside));
    }
    // The gate's authority check costs no compile, so it comes first.
    if gate.is_some()
        && let Err(denied) = gate::authority(tree)
    {
        return Some(Err(denied));
    }
    let dir = match gate::scratch(&ad.state, tree) {
        Ok(dir) => dir,
        Err(e) => return unrun(&e),
    };
    let source = dir.path().join("tree");
    let path = |name: &str| ad.state.join(name).to_string_lossy().into_owned();
    let mut args: Vec<String> = vec![
        "publish".into(),
        tree.package.clone(),
        crate::package_lock::FROZEN_FLAG.into(),
        "--store".into(),
        path("compiler"),
        "--package-store".into(),
        path("packages"),
        "--code-store".into(),
        path("code"),
    ];
    // Source recompiles the group's program under its current recipe allowances.
    // The verifier and host still check the resulting recipe and group memory.
    if let Some(old) = old {
        let old = match old_recipe(ad, old) {
            Ok(recipe) => recipe,
            Err(why) => return Some(Err(why)),
        };
        args.extend([
            "--limit".into(),
            old.launch.root_limit.to_string(),
            "--n-place".into(),
            old.launch.n_place.to_string(),
        ]);
    }
    for grant in &tree.grants {
        args.extend(["--grant".into(), grant.clone()]);
    }
    for directory in &tree.dirs {
        args.extend(["--dir".into(), directory.clone()]);
    }
    let mut command = match (ad.compiler)(&args) {
        Ok(command) => command,
        Err(e) => return unrun(&e),
    };
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let stage = dir.path().join("code");
    command
        .env(PUBLISH_RECIPE_STAGE, &stage)
        .current_dir(&source)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return unrun(&e),
    };
    let (status, stdout, stderr) =
        match gate::collect(child, cancel, Some(deadline), DIAGNOSTIC_BYTES)? {
            Ok(run) => run,
            Err(e) => return unrun(&e),
        };
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    match status.code() {
        Some(0) => {}
        Some(hirsel_guard::EXIT_STATUS) => {
            return Some(Err(Failure::Exhausted(format!(
                "publish source {}: {stderr}",
                source_unit(tree)
            ))));
        }
        Some(_) => return Some(Err(Failure::Uncompiled(stderr.into_owned()))),
        None => {
            return Some(Err(Failure::Uncompiled(format!(
                "the compiler ended by {status}\n{stderr}"
            ))));
        }
    }
    let Some(root) = stdout
        .split_whitespace()
        .last()
        .and_then(|root| crate::native::recipe_build::parse_root(root).ok())
    else {
        return Some(Err(Failure::Invalid(format!(
            "publish source printed no recipe: {stdout}"
        ))));
    };
    let green = match gate {
        None => None,
        Some(digest) => {
            match gate::gate(&ad.compiler, &ad.state, tree, &source, cancel, deadline)? {
                Err(why) => return Some(Err(why)),
                Ok(gate::Verdict::Red(rows)) => return Some(Err(Failure::Red(rows))),
                Ok(gate::Verdict::Lost) => {
                    return Some(Err(Failure::GateLost(
                        "the compiler's session was lost while the law gate ran".into(),
                    )));
                }
                Ok(gate::Verdict::Green) => {
                    let toolchain = ad.toolchain.unwrap_or_else(crate::reuse::compiler_identity);
                    Some(Green {
                        tree: digest.to_vec(),
                        root: Vec::new(),
                        toolchain: toolchain.0.to_vec(),
                    })
                }
            }
        }
    };
    Some(
        store_source_recipe(ad, stage, root)
            .map_err(|e| Failure::Invalid(format!("publish source: {e}")))
            .map(|root| {
                let green = green.map(|green| Green {
                    root: root.0.to_vec(),
                    ..green
                });
                (root, green)
            }),
    )
}

fn store_source_recipe(
    ad: &Admission,
    stage: PathBuf,
    root: ContentHash,
) -> io::Result<ContentHash> {
    let mut bytes = Vec::new();
    let limit = ad.limits.recipe_bytes;
    CodeStore::new(stage)
        .open(ObjectKind::Recipe, root)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recipe exceeds its byte limit",
        ));
    }
    Ok(ad.code.put(ObjectKind::Recipe, &bytes)?.hash)
}

fn nanos(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// One attempt, from its recipe to its assembly record. A `None` answer is a cancel.
#[expect(clippy::too_many_arguments, reason = "one worker's whole input")]
fn work(
    ad: &Admission,
    id: u64,
    attempt: Attempt,
    peer: UnixStream,
    attested: Option<ContentHash>,
    publication: Option<(ContentHash, UnixStream)>,
    green: Option<Green>,
    approvals: &Approvals,
    go: &mpsc::Receiver<()>,
    deadline: Instant,
    tell: &impl Fn(Note),
) -> Option<Note> {
    let fail = |why| Some(Note::Failed(id, attempt, why));
    let start = Instant::now();
    if start >= deadline {
        return fail(Failure::Invalid(
            "the admission deadline passed before verification".into(),
        ));
    }
    let foreign: BTreeSet<String> = approvals.foreign.iter().cloned().collect();
    let exec: BTreeSet<String> = approvals.exec.iter().cloned().collect();
    let workdir = PathBuf::from(&approvals.workdir);
    let mut connection = ControlConnection::local(peer);
    let admitted = std::thread::scope(|scope| {
        let sender = publication.map(|(root, mut local)| {
            scope
                .spawn(move || send_release_until(&mut local, &ad.code, root, &ad.limits, deadline))
        });
        let mut admitted = ad.store.admit_for_runtime(
            &mut connection,
            &ad.limits,
            deadline,
            &NativeSemantics,
            approvals.recording,
            Some(&foreign),
            Some((&exec, &workdir)),
            attested,
        );
        match &admitted {
            Err(AdmissionError::RecordingForeign(reason) | AdmissionError::Refused(reason)) => {
                let _ = connection.refuse_before_need(reason, deadline);
            }
            Ok(_) if attested.is_some() => {
                if let Err(error) = connection.finished(deadline) {
                    admitted = Err(error);
                }
            }
            _ => {}
        }
        drop(connection);
        let sent = sender.map(|sender| sender.join());
        match (admitted, sent) {
            (Err(error), _) => Err(exchange_failure(error)),
            (Ok(_), Some(Err(_))) => Err(Failure::Invalid(
                "admission exchange: publication sender failed".into(),
            )),
            (Ok(_), Some(Ok(Err(error)))) => Err(exchange_failure(error)),
            (Ok(admitted), _) => Ok(admitted),
        }
    });
    let mut admitted = match admitted {
        Ok(admitted) => admitted,
        Err(why) => return fail(why),
    };
    let fetch = nanos(start);
    let recipe = admitted.release().recipe();
    let root = admitted.release().root();
    let summary = match summary(ad, recipe, approvals.old.as_deref()) {
        Ok(summary) => Summary {
            gate: green,
            ..summary
        },
        Err(why) => return fail(why),
    };
    let mut display = deploy_foreign_lines(recipe, root);
    display.push_str(&std::mem::take(&mut admitted.exec_lines));
    if Instant::now() >= deadline {
        return fail(Failure::Invalid(
            "the admission deadline passed while describing the verified release".into(),
        ));
    }
    tell(Note::Admitted {
        id,
        attempt,
        root: root.0.to_vec(),
        summary,
        display,
    });
    go.recv().ok()?;
    let build = Instant::now();
    match assemble(ad, admitted, approvals.recording, |record| {
        Note::Assembled {
            id,
            attempt,
            key: record.identity.0,
            digest: record.digest.0.to_vec(),
            fetch,
            assembly: nanos(build),
        }
    }) {
        Ok(note) => {
            tell(note.0);
            drop(note.1);
            None
        }
        Err(error) => fail(Failure::Unassembled(error.to_string())),
    }
}

fn old_recipe(ad: &Admission, old: &[u8]) -> Result<Recipe, Failure> {
    let key = <[u8; 32]>::try_from(old)
        .map_err(|_| Failure::Invalid("the old assembly name is not 32 bytes".into()))?;
    ad.records
        .assembly(ContentHash(key))
        .and_then(|record| read_recipe(&ad.code, record.recipe, None))
        .map_err(|e| Failure::Invalid(format!("old assembly: {e}")))
}

fn summary(ad: &Admission, recipe: &Recipe, old: Option<&[u8]>) -> Result<Summary, Failure> {
    let mut summary = Summary {
        root_limit: recipe.launch.root_limit,
        target: recipe.launch.target.clone(),
        n_place: recipe.launch.n_place,
        ..Summary::default()
    };
    if let Some(old) = old {
        let old = old_recipe(ad, old)?;
        summary.old_root_limit = old.launch.root_limit;
        summary.grants = grant_diff(&old, recipe);
    }
    Ok(summary)
}

/// Publish the verified objects and recipe, assemble the image, and write the assembly
/// record ("Durable publication" steps 1-3). The note is built and the record written
/// under the publish lock; the guard ends after the loop has been told.
fn assemble<'a>(
    ad: &'a Admission,
    admitted: AdmittedRelease<'_>,
    recording: bool,
    note: impl FnOnce(&AssemblyRecord) -> Note,
) -> Result<(Note, std::sync::MutexGuard<'a, ()>), Error> {
    let (verified, lease) = admitted.into_parts();
    lease.publish(&ad.code, verified.recipe_bytes())?;
    let publication = Publication::from_verified(verified);
    let launch = &publication.recipe().launch;
    let executable = crate::installation::compiler_executable()?;
    let installed = crate::installation::Installed::discover(&executable)?
        .ok_or("launcher assembly requires an installed toolchain")?;
    let runtime = installed
        .runtime(
            &launch.target,
            crate::installation::Flavour {
                record: recording,
                ..crate::installation::Flavour::PLAIN
            },
        )
        .ok_or("missing installed runtime")?;
    let link = installed
        .link(&launch.target)
        .ok_or("missing installed linker")?;
    let staging = tempfile::tempdir_in(ad.state.join("assemble"))?;
    let mut native = NativeOptions::development(
        launch.target.clone(),
        runtime,
        Link::Installed(link),
        staging.path().join("image"),
    );
    native.optimize = launch.backend == hirsel_core_ir::release_recipe::Backend::Llvm;
    native.recording = recording;
    let product = publication
        .assemble(&native, Default::default())
        .map_err(|e| e.to_string())?;
    let record = product.assembly.ok_or("assembly returned no record")?;
    let image = fs::read(product.executable)?;
    let guard = ad.publish.lock().unwrap_or_else(|e| e.into_inner());
    ad.records.put_assembly(&record, &image)?;
    drop(lease);
    Ok((note(&record), guard))
}

/// The wake pipe's write end: a worker writes one byte after each note; a full pipe
/// already wakes the loop.
pub(super) fn waker(write: std::os::fd::OwnedFd) -> io::Result<Arc<File>> {
    use std::os::fd::AsRawFd;
    // SAFETY: fcntl on an owned descriptor.
    unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    Ok(Arc::new(File::from(write)))
}

pub(super) fn wake(file: &File) {
    let _ = (&*file).write(&[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ids::{ContentHash, DefPath};
    use hirsel_core_ir::Identity;
    use hirsel_core_ir::release_recipe::{Backend, GrantBinding, GrantKind, Launch};

    fn recipe(grants: &[(GrantKind, &[&str])]) -> Recipe {
        Recipe {
            entry: Identity::plain(DefPath::from_full("app@1/main/main").unwrap()),
            address: ContentHash::default(),
            groups: vec![],
            bodies: vec![],
            values: vec![],
            foreign_modules: vec![],
            foreign_archives: vec![],
            toolchain: vec![],
            launch: Launch {
                backend: Backend::Cranelift,
                target: "t".into(),
                root_limit: 1,
                n_place: 1,
                exit_status: false,
                grants: grants
                    .iter()
                    .enumerate()
                    .map(|(ordinal, (kind, values))| GrantBinding {
                        ordinal: ordinal as u32,
                        kind: *kind,
                        values: values.iter().map(|v| (*v).into()).collect(),
                        archive: None,
                        policy: None,
                    })
                    .collect(),
            },
        }
    }

    // The deploy gate's diff (launch.md "Deploy preview") names every authority gained and
    // every one lost, value by value. Hand-derived: NEW adds one Net rule beside OLD's,
    // narrows a Secret's patterns, drops Clock and keeps Dir and Args; so the diff is the
    // new rule and the new Secret binding gained, the old Secret binding and Clock lost.
    #[test]
    fn grant_diff_names_each_authority() {
        let old = recipe(&[
            (GrantKind::Net, &["allow tcp a:1"]),
            (GrantKind::Dir, &["/data"]),
            (GrantKind::Clock, &["Clock"]),
            (GrantKind::Secret, &["vault", "api_*"]),
            (GrantKind::Args, &["Args"]),
        ]);
        let new = recipe(&[
            (GrantKind::Net, &["allow tcp a:1", "allow tcp b:2"]),
            (GrantKind::Dir, &["/data"]),
            (GrantKind::Secret, &["vault", "api_key"]),
            (GrantKind::Args, &["Args"]),
        ]);
        let diff = grant_diff(&old, &new);
        assert_eq!(diff.added, ["Net=allow tcp b:2", "Secret=vault api_key"]);
        assert_eq!(diff.removed, ["Clock", "Secret=vault api_*"]);
        assert_eq!(grant_diff(&new, &new), Grants::default());
    }
}
