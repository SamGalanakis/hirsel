//! Streamed admission (docs/spec/launch.md "The launcher", "Publish and assemble"): each
//! attempt runs the code-admission verifier on a worker thread over a socket pair, so the
//! state loop keeps serving child ends and fences while an upload or an assembly runs.
//! The verifier, its reservations and the assembly path are those of local admission; only
//! the byte transport is the session's `AdmitData` chunks. Submitted source is first
//! published by `hirsel publish` in a child process under the host's caches, and then
//! admitted as that publication.
use super::objects::Records;
use super::session::{Approvals, Attempt, Failure, Reader, Summary, Wire, Writer};
use super::{Conn, Epoll};
use crate::code_admission::{
    AdmissionError, AdmissionLimits, AdmissionStore, AdmittedRelease, ControlConnection,
    NativeSemantics, ObjectKey, ObjectKind, send_release,
};
use crate::code_store::CodeStore;
use crate::native::recipe_build::foreign_grant_lines;
use crate::{AssemblyRecord, Link, NativeOptions, Publication};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::decode::DecodeBudget;
use hirsel_core_ir::release_recipe::Recipe;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The launcher's admission bounds: the recipe and object limits of one release.
pub(crate) fn limits() -> AdmissionLimits {
    AdmissionLimits {
        recipe_bytes: 16 << 20,
        closure_objects: 196_608,
        object_bytes: 512 << 20,
        decoded_bytes: 2 << 30,
        members: 65_536,
        edges: 4 << 20,
        nesting: 512,
        outstanding: 196_608,
        timeout: Some(Duration::from_secs(20)),
    }
}

/// The bytes the code store holds for recipe `root`, checked against its name.
pub(crate) fn read_recipe(code: &CodeStore, root: ContentHash, max: usize) -> io::Result<Recipe> {
    let max = max as u64;
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
    Ok(Recipe::decode_retained(
        &bytes,
        &mut DecodeBudget::unlimited(),
    )?)
}

/// Whether `new` holds a grant, or a grant value, that `old` does not.
pub(crate) fn grants_added(old: &Recipe, new: &Recipe) -> bool {
    new.launch.grants.iter().any(|grant| {
        !old.launch.grants.iter().any(|current| {
            current.kind == grant.kind
                && current.policy == grant.policy
                && grant.values.iter().all(|v| current.values.contains(v))
        })
    })
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
            publish: Mutex::new(()),
        })
    }

    /// This admission, compiling submitted source with `compiler`.
    #[cfg(test)]
    pub(crate) fn compile_with(mut self, compiler: Compiler) -> Self {
        self.compiler = compiler;
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
    /// The worker publishes this source into the code store, which then sends it.
    Tree(Tree),
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
    if tree.package != "." && !plain(&tree.package) {
        return Err(invalid(
            "the package directory is not a plain relative path",
        ));
    }
    if tree.files.len() > TREE_FILES {
        return Err(invalid("too many files"));
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
        epoll: &Epoll,
        id: u64,
        attempt: Attempt,
        source: Source,
        approvals: Approvals,
        tell: impl Fn(Note) + Send + 'static,
    ) -> io::Result<Self> {
        let (ours, theirs) = UnixStream::pair()?;
        let (go, gone) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (conn, attested, local) = match source {
            Source::Attested(root) => (Some(Conn::new(epoll, ours)?), Some(root), None),
            Source::Publication(root) => (None, None, Some((Ok(root), ours))),
            Source::Tree(tree) => (None, None, Some((Err(tree), ours))),
        };
        let cancelled = cancel.clone();
        std::thread::Builder::new()
            .name("hirsel-admit".into())
            .spawn(move || {
                let publication = match local {
                    None => None,
                    Some((Ok(root), ours)) => Some((root, ours)),
                    Some((Err(tree), ours)) => match publish(&admission, &tree, &cancelled) {
                        None => return,
                        Some(Ok(root)) => Some((root, ours)),
                        Some(Err(why)) => return tell(Note::Failed(id, attempt, why)),
                    },
                };
                let note = work(
                    &admission,
                    id,
                    attempt,
                    theirs,
                    attested,
                    publication,
                    &approvals,
                    &gone,
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

fn failure(error: AdmissionError) -> Failure {
    match error {
        AdmissionError::RecordingForeign(reason) | AdmissionError::Refused(reason) => {
            Failure::Denied(reason)
        }
        error => Failure::Invalid(error.to_string()),
    }
}

/// Submitted source (`launcher.Tree`): a workspace's files, the package directory to
/// publish within it, and the `--grant` and `--dir` values its recipe binds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tree {
    pub(crate) package: String,
    pub(crate) grants: Vec<String>,
    pub(crate) dirs: Vec<String>,
    pub(crate) files: Vec<(String, Vec<u8>)>,
}

/// The most codec bytes a tree may take (`launcher.TREE_BYTES`).
pub(crate) const TREE_BYTES: usize = 4 << 20;

impl Wire for Tree {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.package.as_bytes());
        w.strings(&self.grants);
        w.strings(&self.dirs);
        w.len(self.files.len());
        for (path, bytes) in &self.files {
            w.bytes(path.as_bytes());
            w.bytes(bytes);
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let package = r.string()?;
        let grants = r.strings()?;
        let dirs = r.strings()?;
        let files = (0..r.count(8)?)
            .map(|_| Ok((r.string()?, r.bytes()?)))
            .collect::<io::Result<_>>()?;
        Ok(Self {
            package,
            grants,
            dirs,
            files,
        })
    }
}

/// The most diagnostic bytes a refused compile reports; the rest is cut and marked.
const DIAGNOSTIC_BYTES: usize = 64 << 10;

/// Publish `tree` into the launcher's code store with `hirsel publish`, run on a fresh copy
/// of the tree under the host's compiler and package stores, and return its recipe root.
/// The child inherits the launcher's environment, so the toolchain's memory limit
/// (`HIRSEL_MEMORY_LIMIT`) bounds it. `None` is a cancel, which kills the child.
fn publish(
    ad: &Admission,
    tree: &Tree,
    cancel: &AtomicBool,
) -> Option<Result<ContentHash, Failure>> {
    let unrun =
        |e: &dyn std::fmt::Display| Some(Err(Failure::Invalid(format!("publish source: {e}"))));
    let dir = match tempfile::tempdir_in(ad.state.join("source")) {
        Ok(dir) => dir,
        Err(e) => return unrun(&e),
    };
    for (path, bytes) in &tree.files {
        let path = dir.path().join(path);
        let written = fs::create_dir_all(path.parent().expect("a file under the tree"))
            .and_then(|()| fs::File::create_new(&path))
            .and_then(|mut file| file.write_all(bytes));
        if let Err(e) = written {
            return unrun(&e);
        }
    }
    let path = |name: &str| ad.state.join(name).to_string_lossy().into_owned();
    let mut args: Vec<String> = vec![
        "publish".into(),
        tree.package.clone(),
        "--frozen".into(),
        "--store".into(),
        path("compiler"),
        "--package-store".into(),
        path("packages"),
        "--code-store".into(),
        path("code"),
    ];
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
    command
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return unrun(&e),
    };
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            if let Some(mut pipe) = pipe {
                let mut chunk = [0; 8192];
                let mut cut = false;
                while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                    let room = DIAGNOSTIC_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                    cut |= n > room;
                }
                if cut {
                    kept.extend_from_slice(b"\n[diagnostics cut at 64 KiB]\n");
                }
            }
            kept
        })
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return unrun(&e),
        }
    };
    let stdout = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    Some(match status.code() {
        Some(0) => stdout
            .split_whitespace()
            .last()
            .and_then(|root| crate::native::recipe_build::parse_root(root).ok())
            .ok_or_else(|| Failure::Invalid(format!("publish source printed no recipe: {stdout}"))),
        Some(hirsel_guard::EXIT_STATUS) => Err(Failure::Exhausted(stderr)),
        Some(_) => Err(Failure::Uncompiled(stderr)),
        None => Err(Failure::Uncompiled(format!(
            "the compiler ended by {status}\n{stderr}"
        ))),
    })
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
    approvals: &Approvals,
    go: &mpsc::Receiver<()>,
    tell: &impl Fn(Note),
) -> Option<Note> {
    let fail = |why| Some(Note::Failed(id, attempt, why));
    let start = Instant::now();
    let foreign: BTreeSet<String> = approvals.foreign.iter().cloned().collect();
    let exec: BTreeSet<String> = approvals.exec.iter().cloned().collect();
    let workdir = PathBuf::from(&approvals.workdir);
    let mut connection = ControlConnection::local(peer);
    let admitted = std::thread::scope(|scope| {
        let sender = publication.map(|(root, mut local)| {
            scope.spawn(move || send_release(&mut local, &ad.code, root, &ad.limits))
        });
        let mut admitted = ad.store.admit_for_runtime(
            &mut connection,
            &ad.limits,
            &NativeSemantics,
            approvals.recording,
            Some(&foreign),
            Some((&exec, &workdir)),
            attested,
        );
        match &admitted {
            Err(AdmissionError::RecordingForeign(reason) | AdmissionError::Refused(reason)) => {
                let _ = connection.refuse_before_need(reason);
            }
            Ok(_) if attested.is_some() => {
                let left = ad
                    .limits
                    .timeout
                    .unwrap_or_default()
                    .saturating_sub(start.elapsed());
                if let Err(error) = connection.finished(left) {
                    admitted = Err(error);
                }
            }
            _ => {}
        }
        drop(connection);
        let sent = sender.map(|sender| sender.join());
        match (admitted, sent) {
            (Err(error), _) => Err(failure(error)),
            (Ok(_), Some(Err(_))) => Err(Failure::Invalid("publication sender failed".into())),
            (Ok(_), Some(Ok(Err(error)))) => Err(failure(error)),
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
        Ok(summary) => summary,
        Err(why) => return fail(why),
    };
    let mut display = deploy_foreign_lines(recipe, root);
    display.push_str(&std::mem::take(&mut admitted.exec_lines));
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

fn summary(ad: &Admission, recipe: &Recipe, old: Option<&[u8]>) -> Result<Summary, Failure> {
    let mut summary = Summary {
        root_limit: recipe.launch.root_limit,
        target: recipe.launch.target.clone(),
        n_place: recipe.launch.n_place,
        ..Summary::default()
    };
    if let Some(old) = old {
        let key = <[u8; 32]>::try_from(old)
            .map_err(|_| Failure::Invalid("the old assembly name is not 32 bytes".into()))?;
        let old = ad
            .records
            .assembly(ContentHash(key))
            .and_then(|record| read_recipe(&ad.code, record.recipe, ad.limits.recipe_bytes))
            .map_err(|e| Failure::Invalid(format!("old assembly: {e}")))?;
        summary.old_root_limit = old.launch.root_limit;
        summary.grants_added = grants_added(&old, recipe);
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
    let installed = crate::installation::Installed::discover(&std::env::current_exe()?)?
        .ok_or("launcher assembly requires an installed toolchain")?;
    let runtime = if recording {
        installed.recording_runtime(&launch.target)
    } else {
        installed.runtime(&launch.target)
    }
    .ok_or("missing installed runtime")?;
    let link = installed
        .link(&launch.target)
        .ok_or("missing installed linker")?;
    let staging = tempfile::tempdir_in(ad.state.join("assemble"))?;
    let mut native = NativeOptions::development(
        launch.target.clone(),
        runtime,
        Link::Installed(link),
        ad.state.join("build-cache"),
        staging.path().join("image"),
    );
    native.optimize = launch.backend == hirsel_core_ir::release_recipe::Backend::Llvm;
    native.recording = recording;
    let product = publication
        .assemble_with(&native, Default::default(), None, None)
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
