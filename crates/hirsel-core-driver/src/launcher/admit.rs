//! Streamed admission (docs/spec/launch.md "The launcher", "Publish and assemble"): each
//! attempt runs the code-admission verifier on a worker thread over a socket pair, so the
//! state loop keeps serving child ends and fences while an upload or an assembly runs.
//! The verifier, its reservations and the assembly path are those of local admission; only
//! the byte transport is the session's `AdmitData` chunks.
use super::session::{Approvals, Attempt, Failure, Summary};
use super::{Conn, Epoll};
use crate::code_admission::{
    AdmissionError, AdmissionLimits, AdmissionStore, AdmittedRelease, ControlConnection,
    NativeSemantics, ObjectKey, ObjectKind, send_release,
};
use crate::code_store::CodeStore;
use crate::host::records::Records;
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

/// The admission side of the launcher's stores: the code store with its staging, and the
/// assembly records the image store shares with launches.
pub(crate) struct Admission {
    store: AdmissionStore,
    code: CodeStore,
    records: Arc<Records>,
    state: PathBuf,
    limits: AdmissionLimits,
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
        let assemble = state.join("assemble");
        match fs::remove_dir_all(&assemble) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => fs::create_dir(&assemble)?,
        }
        let code = state.join("code");
        Ok(Self {
            store: AdmissionStore::open(&code, staging)?,
            code: CodeStore::new(code),
            records,
            state: state.to_owned(),
            limits,
            publish: Mutex::new(()),
        })
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
        let (conn, publication) = match source {
            Source::Attested(_) => (Some(Conn::new(epoll, ours)?), None),
            Source::Publication(root) => (None, Some((root, ours))),
        };
        let attested = match source {
            Source::Attested(root) => Some(root),
            Source::Publication(_) => None,
        };
        std::thread::Builder::new()
            .name("hirsel-admit".into())
            .spawn(move || {
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
    let prebuilt = installed.product("std")?.map(|image| {
        image
            .store_root()
            .join(crate::native::std_release::PREBUILT)
    });
    let product = publication
        .assemble_with(&native, Default::default(), None, prebuilt.as_deref())
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
