use crate::{
    cli::{Invocation, OptionId},
    launcher::{
        dev::{self as launcher, DevChild, Open, StoreId},
        session::{HostReply, InstallRefusal, PlaceReady},
    },
    package_workspace::Workspace,
    sdk::{Describe, Reader, Schema},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    net::{SocketAddr, TcpListener},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[path = "dev_directory.rs"]
mod directory;
#[path = "dev_session.rs"]
mod session;
#[path = "dev_transport.rs"]
mod transport;

type Error = Box<dyn std::error::Error>;

#[derive(Debug)]
struct HeldStore;

impl std::fmt::Display for HeldStore {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("store is Held by another process")
    }
}

impl std::error::Error for HeldStore {}
const POLL: Duration = Duration::from_millis(100);
const STOP_GRACE: Duration = Duration::from_secs(5);
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct ChildProcess(Child);

impl ChildProcess {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let parent = std::process::id();
        // SAFETY: these syscalls only set this child's parent-death signal and
        // inspect its parent, and run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() as u32 != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
        Ok(Self(command.spawn()?))
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            if STOP.load(Ordering::Relaxed) {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            thread::sleep(POLL);
        }
    }

    fn stop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        // SAFETY: this is a child owned by the supervisor that has not been reaped.
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if !matches!(self.0.try_wait(), Ok(None)) {
                return;
            }
            thread::sleep(POLL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Options {
    server: PathBuf,
    listen: SocketAddr,
    workdir: PathBuf,
    build: Vec<OsString>,
    arguments: Vec<String>,
    store: Option<PathBuf>,
    fresh: bool,
    n_place: u64,
}

impl Options {
    fn read(invocation: Invocation) -> Result<Self, Error> {
        let mut options = Self {
            server: fs::canonicalize(invocation.positional.as_deref().unwrap_or("."))?,
            listen: ([127, 0, 0, 1], 7700).into(),
            workdir: std::env::current_dir()?,
            build: Vec::new(),
            arguments: invocation.arguments,
            store: None,
            fresh: false,
            n_place: hirsel_core_ids::launch::DEFAULT_N_PLACE,
        };
        let mut limit = false;
        let mut net = false;
        for (id, value) in invocation.options {
            let flag = match id {
                OptionId::Listen => {
                    options.listen = value.ok_or("missing listen address")?.parse()?;
                    continue;
                }
                OptionId::Store => {
                    options.store = value.map(PathBuf::from);
                    continue;
                }
                OptionId::Fresh => {
                    options.fresh = true;
                    continue;
                }
                OptionId::NPlace => {
                    options.n_place = value.ok_or("missing n_place")?.parse()?;
                    if options.n_place == 0 {
                        return Err("--n-place must be positive".into());
                    }
                    continue;
                }
                OptionId::Limit => {
                    limit = true;
                    "--limit"
                }
                OptionId::Grant => {
                    net |= value
                        .as_deref()
                        .is_some_and(|grant| grant.starts_with("Net"));
                    "--grant"
                }
                OptionId::Dir => "--dir",
                OptionId::Backend => "--backend",
                OptionId::Libraries => "--libraries",
                OptionId::Runtime => "--runtime",
                OptionId::SourceCore => "--source-core",
                OptionId::SourceStd => "--source-std",
                _ => return Err("unsupported dev option".into()),
            };
            options.build.push(flag.into());
            options.build.extend(value.map(OsString::from));
        }
        if options.fresh && options.store.is_some() {
            return Err(
                "--fresh starts a dev-owned store; it cannot be combined with --store".into(),
            );
        }
        if options.arguments.iter().any(|arg| {
            arg == "--hirsel-install"
                || arg.starts_with("--hirsel-activation=")
                || arg.starts_with("--hirsel-held")
        }) {
            return Err("dev chooses the store activation; remove the launch selection from the program arguments".into());
        }
        if !options.listen.ip().is_loopback() {
            return Err("dev listens on a loopback address".into());
        }
        if !limit {
            options
                .build
                .extend(["--limit".into(), "1610612736".into()]);
        }
        for grant in ["Clock", "Random", "Tasks", "Process", "Stderr"] {
            options.build.extend(["--grant".into(), grant.into()]);
        }
        if !net {
            options
                .build
                .extend(["--grant".into(), "Net=Listen 127.0.0.1/32 0-65535".into()]);
        }
        Ok(options)
    }

    fn build(&self, directory: &directory::Directory, output: &Path) -> Result<bool, Error> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("build")
            .arg(&self.server)
            .arg("--store")
            .arg(directory.root.join("compiler"))
            .arg("--dir")
            .arg(&directory.store)
            .args(&self.build)
            .arg("--output")
            .arg(output);
        // The public listening address is the first stdout line.
        #[cfg(unix)]
        {
            use std::os::fd::AsFd;
            command.stdout(Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?));
        }
        Ok(ChildProcess::spawn(&mut command)?.wait()?.success())
    }
}

fn sources(server: &Path) -> Result<Vec<PathBuf>, Error> {
    let workspace = Workspace::discover(server)?.ok_or("dev requires a Hirsel package")?;
    let mut files = vec![workspace.directory.join(crate::package_dir::MANIFEST)];
    for member in workspace.members.values() {
        files.push(member.manifest_path.clone());
        let directory = workspace.directory.join(&member.directory);
        files.extend(member.content.files.keys().map(|path| directory.join(path)));
        files.extend(member.tests.keys().map(|path| directory.join(path)));
        for foreign in member.content.foreign.values() {
            files.extend(
                foreign
                    .archives
                    .values()
                    .map(|archive| directory.join(&archive.path)),
            );
        }
    }
    let canonical: Vec<_> = files
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    files.extend(canonical);
    files.sort();
    files.dedup();
    Ok(files)
}

fn source_state(paths: &[PathBuf]) -> Vec<Option<[i64; 4]>> {
    use std::os::unix::fs::MetadataExt;
    paths
        .iter()
        .map(|path| {
            fs::metadata(path).ok().map(|info| {
                [
                    info.ino() as i64,
                    info.size() as i64,
                    info.mtime(),
                    info.mtime_nsec(),
                ]
            })
        })
        .collect()
}

struct Connections(Vec<thread::JoinHandle<()>>);

impl Connections {
    fn reap(&mut self) {
        let mut i = 0;
        while i < self.0.len() {
            if self.0[i].is_finished() {
                let _ = self.0.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
    }
}

impl Drop for Connections {
    fn drop(&mut self) {
        STOP.store(true, Ordering::Relaxed);
        for join in self.0.drain(..) {
            let _ = join.join();
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct ImageRef {
    number: u64,
    identity: [u8; 32],
}

struct Image {
    reference: ImageRef,
    describe: Arc<Describe>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct Pending {
    old: Option<ImageRef>,
    predecessor: [u8; 32],
    new: ImageRef,
    store: StoreId,
    installed: Option<u64>,
}

struct Serving {
    image: Image,
    child: DevChild,
    proof: PlaceReady,
}

struct CurrentStore {
    id: StoreId,
    binding: [u8; 32],
    claimed: bool,
}

impl CurrentStore {
    fn read(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader { bytes, at: 0 };
        let id = (r.word()?, r.word()?);
        let binding = r
            .bytes()?
            .try_into()
            .map_err(|_| "store reply has no release identity")?;
        if r.count()? != 1 {
            return Err("store reply has no registration allowance".into());
        }
        let _ = r.word()?;
        let _ = r.bytes()?; // Prepared release witness.
        if r.byte()? > 1 {
            return Err("invalid store witness".into());
        }
        let claimed = match r.count()? {
            0 => false,
            1 => {
                let _ = r.word()?;
                if r.bytes()?.len() != 16 {
                    return Err("invalid store claim holder".into());
                }
                true
            }
            _ => return Err("invalid store claim".into()),
        };
        r.done()?;
        Ok(Self {
            id,
            binding,
            claimed,
        })
    }
}

fn describe(path: &Path, n_place: u64, number: u64) -> Result<Image, Error> {
    let response = crate::sdk_command::describe(path, n_place, Duration::from_secs(10))?;
    let (describe, identity) = Describe::read_with_identity(&response)?;
    Ok(Image {
        reference: ImageRef { number, identity },
        describe: Arc::new(describe),
    })
}

fn stopped() -> bool {
    STOP.load(Ordering::Relaxed)
}

fn digest_text(digest: &[u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

struct Supervisor<'a> {
    options: &'a Options,
    directory: &'a directory::Directory,
    listener: TcpListener,
    public: SocketAddr,
    current: session::Current,
    active: Option<Serving>,
    locator: directory::Locator,
    announced: bool,
}

impl Supervisor<'_> {
    fn open(
        &self,
        image: &Image,
        store: Option<StoreId>,
        version: Option<u64>,
    ) -> Result<Open, Error> {
        self.directory.verify_path()?;
        let executable = self.directory.image(image.reference.number);
        let command = launcher::command(
            &executable,
            self.options.n_place,
            store,
            None,
            &self.options.arguments,
        );
        Ok(launcher::open(
            command,
            &self.options.workdir,
            &self.listener,
            &self.directory.root.join("scratch"),
            self.directory.store_fd(),
            version,
            stopped,
        )?)
    }

    fn serve(&mut self, image: Image, child: DevChild, proof: PlaceReady) -> Result<(), Error> {
        let backing = if proof.version == 0 {
            "temporary"
        } else if CurrentStore::read(&proof.read).is_ok_and(|read| read.claimed) {
            "objects@1"
        } else {
            "local directory"
        };
        self.directory.sync_image(image.reference.number)?;
        self.locator.active = Some(image.reference);
        self.locator.pending = None;
        self.directory.save(&self.locator)?;
        let number = image.reference.number;
        *self.current.lock().unwrap() = Some(Arc::new(session::Release {
            generation: number,
            describe: Arc::clone(&image.describe),
        }));
        eprintln!(
            "hirsel dev: serving generation {number}, activation {}, store {} ({backing}), allowance {}",
            proof.version,
            self.directory.store.display(),
            self.options.n_place,
        );
        self.active = Some(Serving {
            image,
            child,
            proof,
        });
        if !self.announced {
            println!("hirsel dev listening on {}", self.public);
            io::stdout().flush()?;
            self.announced = true;
        }
        Ok(())
    }

    fn stop_place(&mut self) {
        *self.current.lock().unwrap() = None;
        drop(self.active.take());
    }

    fn retained(&self, image: ImageRef) -> Result<Image, Error> {
        let executable = self.directory.image(image.number);
        let found = describe(&executable, self.options.n_place, image.number)?;
        if found.reference.identity != image.identity {
            return Err("retained executable has another release identity".into());
        }
        Ok(found)
    }

    fn broken(&self, cause: &str, detail: impl std::fmt::Display) {
        if let Some(active) = &self.active {
            eprintln!(
                "hirsel dev: {cause}; generation {} still serving: {detail}",
                active.image.reference.number
            );
        } else {
            eprintln!("hirsel dev: {cause}: {detail}");
        }
    }

    fn pending(&self, pending: Pending, detail: impl std::fmt::Display) {
        eprintln!(
            "hirsel dev: recovery pending: NEW {}, OLD {}: {detail}",
            digest_text(&pending.new.identity),
            digest_text(&pending.predecessor)
        );
    }

    fn reap(&mut self) -> io::Result<()> {
        if let Some(active) = self.active.as_mut()
            && let Some(status) = active.child.exited()?
        {
            self.stop_place();
            self.broken("place crashed", format_args!("{status:?}"));
        }
        Ok(())
    }

    fn try_reopen(
        &mut self,
        image: Image,
        store: Option<StoreId>,
        version: Option<u64>,
    ) -> Result<Result<(), String>, Error> {
        let result = self.open(&image, store, version);
        match result {
            Ok(Open::Ready(child, proof)) => {
                self.serve(image, child, proof)?;
                Ok(Ok(()))
            }
            other => Ok(Err(open_problem(other))),
        }
    }

    fn recover(&mut self, pending: Pending, refusal: Option<&InstallRefusal>) -> Result<(), Error> {
        let mut detail = String::new();
        if refusal.is_none() {
            let result = match self.retained(pending.new) {
                Ok(image) => self.try_reopen(image, Some(pending.store), pending.installed)?,
                Err(error) => Err(error.to_string()),
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) => detail = format!("NEW: {error}"),
            }
            if pending.installed.is_some() {
                self.broken("start failed", &detail);
                self.pending(pending, detail);
                return Ok(());
            }
        }
        if let Some(refusal) = refusal {
            detail = format!("migration refused: {refusal:?}");
        }
        if let Some(old) = pending.old {
            let result = match self.retained(old) {
                Ok(image) => self.try_reopen(image, Some(pending.store), None)?,
                Err(error) => Err(error.to_string()),
            };
            match result {
                Ok(()) => {
                    self.broken("migration failed; OLD reopened", detail);
                    return Ok(());
                }
                Err(error) => detail.push_str(&format!("; OLD: {error}")),
            }
        }
        self.pending(pending, detail);
        Ok(())
    }

    fn migrate(
        &mut self,
        candidate: Image,
        current: CurrentStore,
        old: Option<ImageRef>,
    ) -> Result<(), Error> {
        let pending = Pending {
            old,
            predecessor: current.binding,
            new: candidate.reference,
            store: current.id,
            installed: None,
        };
        self.directory.sync_image(candidate.reference.number)?;
        self.locator.pending = Some(pending);
        self.directory.save(&self.locator)?;
        self.stop_place();
        if stopped() {
            return Ok(());
        }
        self.directory.verify_path()?;
        let executable = self.directory.image(candidate.reference.number);
        let command = launcher::command(
            &executable,
            self.options.n_place,
            Some(current.id),
            Some(current.binding),
            &self.options.arguments,
        );
        let reply = launcher::install(
            command,
            &self.options.workdir,
            &self.directory.root.join("scratch"),
            self.directory.store_fd(),
            stopped,
        );
        if stopped() {
            return Ok(());
        }
        match reply {
            Ok(Some(HostReply::Installed(version, false))) => {
                eprintln!("hirsel dev: installed activation {version}");
                let pending = Pending {
                    installed: Some(version),
                    ..pending
                };
                self.locator.pending = Some(pending);
                self.directory.save(&self.locator)?;
                if let Err(error) = self.try_reopen(candidate, Some(current.id), Some(version))? {
                    self.broken("start failed", &error);
                    self.pending(pending, error);
                }
            }
            Ok(Some(HostReply::InstallRefused(refusal))) => {
                eprintln!("hirsel dev: migration refused: {refusal:?}");
                self.recover(pending, Some(&refusal))?;
            }
            result => {
                if let Err(error) = result {
                    eprintln!("hirsel dev: install outcome uncertain: {error}");
                }
                self.recover(pending, None)?;
            }
        }
        Ok(())
    }

    fn activate(&mut self, candidate: Image) -> Result<(), Error> {
        self.reap()?;
        if let Some(active) = &self.active {
            let old = active.image.reference;
            let id = (active.proof.store_hi, active.proof.store_lo);
            let version = active.proof.version;
            if active.image.describe.digest == candidate.describe.digest {
                self.directory.discard(candidate.reference.number)?;
                eprintln!("hirsel dev: unchanged");
                return Ok(());
            }
            if old.identity != candidate.reference.identity && version != 0 {
                return self.migrate(
                    candidate,
                    CurrentStore {
                        id,
                        binding: old.identity,
                        claimed: false,
                    },
                    Some(old),
                );
            }
            let store = (version != 0).then_some(id);
            self.stop_place();
            if let Err(error) = self.try_reopen(candidate, store, Some(version))? {
                self.broken("start failed", error);
                let recovered = match self.retained(old) {
                    Ok(old) => self.try_reopen(old, store, Some(version))?,
                    Err(error) => Err(error.to_string()),
                };
                if let Err(error) = recovered {
                    self.broken("previous place could not reopen", error);
                }
            }
            return Ok(());
        }

        match self.open(&candidate, None, None) {
            Ok(Open::Ready(child, proof)) => self.serve(candidate, child, proof),
            Ok(Open::Refused(HostReply::Current(_, read))) => match CurrentStore::read(&read) {
                Ok(current) if current.binding != candidate.reference.identity => {
                    let old = self
                        .locator
                        .active
                        .filter(|old| old.identity == current.binding);
                    self.migrate(candidate, current, old)
                }
                Ok(_) => {
                    self.broken(
                        "identity refusal",
                        "candidate identity already matches the store",
                    );
                    Ok(())
                }
                Err(error) => {
                    self.broken("identity refusal", error);
                    Ok(())
                }
            },
            Ok(Open::Refused(HostReply::Refused(reason)))
                if reason == "Held" && !self.announced =>
            {
                Err(HeldStore.into())
            }
            other => {
                self.broken("start failed", open_problem(other));
                Ok(())
            }
        }
    }

    fn build(&mut self, files: &[PathBuf]) -> Result<(), Error> {
        if let Err(error) = self.directory.verify(files) {
            self.broken("store refused", error);
            return Ok(());
        }
        if let Some(pending) = self.locator.pending {
            return self.recover(pending, None);
        }
        let (number, path) = self.directory.allocate()?;
        if !self.options.build(self.directory, &path)? {
            self.directory.discard(number)?;
            self.reap()?;
            self.broken("build failed", "the compiler refused the package");
            return Ok(());
        }
        if stopped() {
            return Ok(());
        }
        match describe(&path, self.options.n_place, number) {
            Ok(candidate) => self.activate(candidate),
            Err(error) => {
                self.directory.discard(number)?;
                self.broken("identity refusal", error);
                Ok(())
            }
        }
    }
}

fn open_problem(result: Result<Open, Error>) -> String {
    match result {
        Ok(Open::Unproved(detail)) => detail,
        Ok(Open::Refused(reply)) => format!("{reply:?}"),
        Ok(Open::Ready(..)) => unreachable!("ready result is handled at its caller"),
        Err(error) => error.to_string(),
    }
}

struct Proxy(thread::JoinHandle<io::Result<()>>);

impl Proxy {
    fn start(
        public: TcpListener,
        address: SocketAddr,
        current: session::Current,
    ) -> Result<Self, Error> {
        let codec = Arc::new(Schema::read(crate::sdk_command::FRAME_SCHEMA)?);
        Ok(Self(thread::Builder::new().spawn(move || {
            let mut connections = Connections(Vec::new());
            while !stopped() {
                match public.accept() {
                    Ok((stream, _)) => {
                        let current = Arc::clone(&current);
                        let codec = Arc::clone(&codec);
                        connections.0.push(thread::Builder::new().spawn(move || {
                            if let Err(problem) = session::serve(stream, address, current, codec) {
                                eprintln!("hirsel dev session: {problem}");
                            }
                        })?);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                    Err(error) => return Err(error),
                }
                connections.reap();
            }
            Ok(())
        })?))
    }

    fn stop(self) -> Result<(), Error> {
        STOP.store(true, Ordering::Relaxed);
        self.0.join().map_err(|_| "dev proxy thread panicked")??;
        Ok(())
    }
}

pub(in crate::request) fn run(invocation: Invocation) -> Result<i32, Error> {
    let options = Options::read(invocation)?;
    if crate::resident::serving() {
        return Err("dev must run in the terminal process".into());
    }
    // SAFETY: the handler only writes an atomic flag.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let discovery = sources(&options.server);
    let directory = directory::Directory::open(
        &options.server,
        options.store.as_deref(),
        options.fresh,
        discovery.as_deref().ok(),
    )?;
    let locator = directory.load()?;
    let public = TcpListener::bind(options.listen)?;
    public.set_nonblocking(true)?;
    let address = public.local_addr()?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let current: session::Current = Arc::new(Mutex::new(None));
    let proxy = Proxy::start(public, listener.local_addr()?, Arc::clone(&current))?;
    let mut dev = Supervisor {
        options: &options,
        directory: &directory,
        listener,
        public: address,
        current,
        active: None,
        locator,
        announced: false,
    };
    let result = watch(&mut dev, discovery);
    dev.stop_place();
    let interrupted = stopped();
    let proxy_result = proxy.stop();
    match (result, proxy_result) {
        (Ok(()), Ok(())) => Ok(0),
        (Err(_), _) if interrupted => Ok(0),
        (Err(error), _) if error.is::<HeldStore>() => Err(error),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("hirsel dev: {error}");
            Ok(1)
        }
    }
}

fn watch(dev: &mut Supervisor<'_>, discovery: Result<Vec<PathBuf>, Error>) -> Result<(), Error> {
    let fallback: Vec<_> = dev
        .options
        .server
        .ancestors()
        .map(|path| path.join(crate::package_dir::MANIFEST))
        .collect();
    let mut files = fallback.clone();
    let mut next = Some(discovery);
    let mut seen = source_state(&files);
    while !stopped() {
        dev.reap()?;
        if let Some(discovery) = next.take() {
            match discovery {
                Ok(found) => {
                    files = found;
                    seen = source_state(&files);
                    dev.build(&files)?;
                }
                Err(error) => {
                    dev.broken("discovery failed", error);
                    let mut watched = files.clone();
                    watched.extend(fallback.iter().cloned());
                    watched.sort();
                    watched.dedup();
                    files = watched;
                    seen = source_state(&files);
                }
            }
        }
        if source_state(&files) != seen {
            next = Some(sources(&dev.options.server));
        }
        thread::sleep(POLL);
    }
    Ok(())
}
