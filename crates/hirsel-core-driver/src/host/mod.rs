//! One supervisor owns the listener, serving children and deployment records.
mod child;
mod recording;
mod records;

pub(crate) use recording::replay;

use crate::{
    AssemblyRecord, Link, NativeOptions, Publication,
    cli::{Invocation, OptionId as Opt},
    code_admission::{
        AdmissionLimits, AdmissionStore, AdmittedRelease, ControlConnection, NativeSemantics,
        ObjectKey, ReleaseLease, send_release,
    },
    code_store::{CodeStore, ObjectKind},
    native::recipe_build::parse_root,
    sdk::{Describe, Reader},
};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::{
    decode::DecodeBudget,
    release_recipe::{GrantKind, Recipe},
};
use records::{Active, Attempt, Deploy, Records, State};
use std::{
    collections::BTreeSet,
    fs::{self, DirBuilder, File, Permissions},
    io::{self, Read, Write},
    net::TcpListener,
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

type Error = Box<dyn std::error::Error>;
const START_TIMEOUT: Duration = Duration::from_secs(10);
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(120);

fn limits() -> AdmissionLimits {
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

pub(crate) fn run(invocation: Invocation) -> Result<(), Error> {
    let root = parse_root(
        invocation
            .positional
            .as_deref()
            .ok_or("expected a recipe hash")?,
    )?;
    let mut admission = limits();
    let mut control = None;
    let mut source = None;
    let mut state = None;
    let mut workdir = std::env::current_dir()?;
    let mut scratch = None;
    let mut listen = None;
    let mut recording = None;
    let mut pin_seconds = 30 * 86400;
    let mut added = false;
    let mut max_pause = 2_000;
    let mut memory = 2 << 30;
    let mut staging = 512 << 20;
    let mut retries = 2;
    for (option, value) in invocation.options {
        let value = value.unwrap_or_default();
        match option {
            Opt::Control => control = Some(PathBuf::from(value)),
            Opt::CodeStore => source = Some(CodeStore::new(value)),
            Opt::HostState => state = Some(PathBuf::from(value)),
            Opt::Workdir => workdir = PathBuf::from(value).canonicalize()?,
            Opt::Scratch => scratch = Some(PathBuf::from(value)),
            Opt::Listen => listen = Some(value),
            Opt::Record => recording = Some(private_dir(PathBuf::from(value))?),
            Opt::TapeRetention => {
                pin_seconds = value
                    .parse::<u64>()?
                    .checked_mul(86400)
                    .filter(|&seconds| seconds != 0)
                    .ok_or("invalid tape retention")?;
            }
            Opt::GrantAdded => added = true,
            Opt::MaxPause => max_pause = value.parse()?,
            Opt::MaxRecipeBytes => admission.recipe_bytes = value.parse()?,
            Opt::MaxClosure => {
                admission.closure_objects = value.parse()?;
                admission.outstanding = admission.closure_objects;
            }
            Opt::MaxDecodeBytes => admission.decoded_bytes = value.parse()?,
            Opt::StagingBytes => staging = value.parse()?,
            Opt::AdmitTimeout => admission.timeout = Some(Duration::from_secs(value.parse()?)),
            Opt::HostMemory => memory = value.parse()?,
            Opt::StartRetries => retries = value.parse()?,
            _ => return Err("unexpected host option".into()),
        }
    }
    if max_pause == 0 || admission.timeout == Some(Duration::ZERO) {
        return Err("host time limits must be positive".into());
    }
    let socket = control.ok_or("--control SOCKET is required")?;
    if invocation.command.name == "deploy" {
        if !invocation.arguments.is_empty() {
            return Err("deploy takes no place arguments".into());
        }
        let source = source.ok_or("deploy requires --code-store DIR")?;
        let mut peer = UnixStream::connect(socket)?;
        peer.set_write_timeout(admission.timeout)?;
        peer.write_all(&[u8::from(added)])?;
        send_release(&mut peer, &source, root, &admission)?;
        peer.set_read_timeout(Some(DEPLOY_TIMEOUT))?;
        let report = read_frame(&mut peer, admission.recipe_bytes)?;
        let answer = serde_json::from_slice::<Report>(&report)?;
        println!("{}", serde_json::to_string(&answer)?);
        return if let Some(reason) = answer.refused {
            Err(reason.into())
        } else {
            Ok(())
        };
    }
    let state = private_dir(state.ok_or("--state DIR is required")?)?;
    // SAFETY: the host owns this process. Restrict the socket from its creation.
    unsafe {
        libc::umask(0o077);
    }
    let records = Records::open(&state)?;
    let children = child::Children::open(state.join("children"))?;
    let scratch = match scratch {
        Some(parent) => {
            let key = ContentHash::of_parts(
                "hirsel.host-scratch.v1",
                [state.as_os_str().as_encoded_bytes()],
            );
            private_dir(parent)?.join(crate::call::hex(&key.0))
        }
        None => state.join("scratch"),
    };
    reset_staging(&scratch)?;
    let listener = bind_listener(&listen.ok_or("--listen ADDR is required")?)?;
    let assembly_stage = state.join("assemble");
    reset_staging(&assembly_stage)?;
    let code_root = state.join("code");
    let code = CodeStore::new(&code_root);
    let admission_store = AdmissionStore::open(&code_root, staging)?;
    let host = Host {
        records,
        code,
        admission: admission_store,
        limits: admission,
        workdir,
        scratch,
        state,
        assembly_stage,
        children,
        listener,
        recording,
        pin_seconds,
        max_pause,
        memory,
        retries,
        args: invocation.arguments,
    };
    let admin = bind_control(&socket)?;
    let (mut active, mut live) = host.open_place(root, source.as_ref())?;
    loop {
        match ControlConnection::accept(&admin) {
            Ok(mut operator) => {
                let previous = active.active.version;
                let report = match host.submit(&mut operator, &mut active, &mut live) {
                    Ok((report, replacement)) => {
                        if let Some(replacement) = replacement {
                            live = replacement;
                        }
                        report
                    }
                    Err(error) => {
                        drop(live);
                        (active, live) = host.open_place(root, source.as_ref())?;
                        if active.active.version == previous {
                            Report {
                                refused: Some(error.to_string()),
                                ..Report::default()
                            }
                        } else {
                            Report {
                                activation: Some(active.active.version),
                                ..Report::default()
                            }
                        }
                    }
                };
                let mut peer = operator.into_stream()?;
                peer.set_write_timeout(Some(START_TIMEOUT))?;
                let _ = write_frame(&mut peer, &serde_json::to_vec(&report)?);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if live.try_wait()?.is_some() {
                    (active, live) = host.open_place(root, source.as_ref())?;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
}

fn private_dir(path: PathBuf) -> io::Result<PathBuf> {
    match DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "host directory must be private",
        ));
    }
    path.canonicalize()
}

fn reset_staging(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    fs::create_dir(path)
}

fn bind_control(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::FileTypeExt;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() || UnixStream::connect(path).is_ok() {
            return Err(io::ErrorKind::AddrInUse.into());
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn bind_listener(address: &str) -> io::Result<TcpListener> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match TcpListener::bind(address) {
            Err(error) if error.kind() == io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

struct Host {
    records: Records,
    code: CodeStore,
    admission: AdmissionStore,
    limits: AdmissionLimits,
    workdir: PathBuf,
    scratch: PathBuf,
    state: PathBuf,
    assembly_stage: PathBuf,
    children: child::Children,
    listener: TcpListener,
    recording: Option<PathBuf>,
    pin_seconds: u64,
    max_pause: u64,
    memory: u64,
    retries: u32,
    args: Vec<String>,
}

struct Running {
    active: Active,
    retain_from: u64,
    assembly: AssemblyRecord,
    recipe: Recipe,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Report {
    fetch_ms: u128,
    assemble_ms: u128,
    pause_ms: u128,
    activation: Option<u64>,
    refused: Option<String>,
    preview: Vec<String>,
}

impl Host {
    fn assemble<'a>(
        &self,
        admitted: AdmittedRelease<'a>,
    ) -> Result<(AssemblyRecord, Recipe, ReleaseLease<'a>), Error> {
        let (verified, lease) = admitted.into_parts();
        lease.publish(&self.code, verified.recipe_bytes())?;
        let publication = Publication::from_verified(verified);
        let recipe = publication.recipe().clone();
        let launch = &publication.recipe().launch;
        let installed = crate::installation::Installed::discover(&std::env::current_exe()?)?
            .ok_or("host assembly requires an installed toolchain")?;
        let runtime = if self.recording.is_some() {
            installed.recording_runtime(&launch.target)
        } else {
            installed.runtime(&launch.target)
        }
        .ok_or("missing installed runtime")?;
        let link = installed
            .link(&launch.target)
            .ok_or("missing installed linker")?;
        let staging = tempfile::tempdir_in(&self.assembly_stage)?;
        let output = staging.path().join("image");
        let mut native = NativeOptions::development(
            launch.target.clone(),
            runtime,
            Link::Installed(link),
            self.state.join("build-cache"),
            output,
        );
        native.optimize = launch.backend == hirsel_core_ir::release_recipe::Backend::Llvm;
        native.recording = self.recording.is_some();
        let product = publication.assemble(&native, Default::default())?;
        let record = product.assembly.ok_or("assembly returned no record")?;
        self.records
            .put_assembly(&record, &fs::read(product.executable)?)?;
        Ok((record, recipe, lease))
    }

    fn recipe(&self, root: ContentHash) -> Result<Recipe, Error> {
        let max = self.limits.recipe_bytes as u64;
        let mut bytes = Vec::new();
        self.code
            .open(ObjectKind::Recipe, root)?
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max || Recipe::hash(&bytes) != root {
            return Err("stored recipe hash or length".into());
        }
        Ok(Recipe::decode(&bytes, &mut DecodeBudget::unlimited())?)
    }

    fn initial(
        &self,
        root: ContentHash,
        source: &CodeStore,
    ) -> Result<(AssemblyRecord, Recipe, ReleaseLease<'_>), Error> {
        let (peer, mut local) = UnixStream::pair()?;
        std::thread::scope(|scope| {
            let sender = scope.spawn(move || send_release(&mut local, source, root, &self.limits));
            let mut connection = ControlConnection::local(peer);
            let admitted = self
                .admission
                .admit(&mut connection, &self.limits, &NativeSemantics);
            drop(connection);
            sender
                .join()
                .map_err(|_| "local publication sender failed")??;
            self.assemble(admitted?)
        })
    }

    fn start(
        &self,
        assembly: &AssemblyRecord,
        recipe: &Recipe,
        version: u64,
    ) -> Result<child::Place, Error> {
        if recipe.launch.root_limit > self.memory {
            return Err("Memory".into());
        }
        let recording = self
            .recording
            .as_deref()
            .map(recording::Recording::new)
            .transpose()?;
        let image = self.admission.under_collection(|_| -> io::Result<_> {
            let image = self.records.verify_image(assembly.digest)?;
            if let Some(recording) = &recording {
                let expires = recording::unix_seconds()?
                    .checked_add(self.pin_seconds)
                    .ok_or(io::ErrorKind::InvalidInput)?;
                self.records
                    .keep_pin(recording.id, assembly.identity, expires)?;
            }
            Ok(image)
        })?;
        Ok(child::Place::start(
            &self.children,
            child::Launch {
                executable: &image,
                workdir: &self.workdir,
                scratch: &self.scratch,
                listener: &self.listener,
                activation: version,
                n_place: recipe.launch.n_place,
                args: &self.args,
                recording: recording.as_ref(),
            },
        )?)
    }

    fn open_place(
        &self,
        initial: ContentHash,
        source: Option<&CodeStore>,
    ) -> Result<(Running, child::Place), Error> {
        let active = self.records.active()?;
        let record = self.records.read_deploy()?;
        if let Some(active) = active {
            let mut choices = vec![(active.assembly, active.version)];
            if let Some(install) = &record
                && matches!(
                    install.state,
                    State::Installing | State::Done | State::Failed
                )
            {
                if let Some(next) = install.exported.checked_add(1)
                    && (install.assembly, next) != choices[0]
                {
                    choices.insert(0, (install.assembly, next));
                }
                if let Some(prior) = self
                    .records
                    .indexed(active.store, install.prior_activation)?
                    && !choices.contains(&(prior, install.prior_activation))
                {
                    choices.push((prior, install.prior_activation));
                }
            }
            let mut failures = Vec::new();
            for (key, version) in choices {
                let probe = self.reopen(key, version);
                match probe {
                    Ok((running, place)) if running.active.store == active.store => {
                        self.records.set_active(running.active)?;
                        self.records.reconcile_pending()?;
                        self.collect(running.active.store, running.retain_from)?;
                        return Ok((running, place));
                    }
                    Ok(_) => return Err("recovered a different store".into()),
                    Err(error) => failures.push(format!("{version} {error}")),
                }
            }
            return Err(format!("place recovery failed ({})", failures.join(", ")).into());
        }
        if let Some(boot) = record
            && boot.old == ContentHash([0; 32])
        {
            let assembly = self.records.assembly(boot.assembly)?;
            let recipe = self.recipe(assembly.recipe)?;
            return self.boot(assembly, recipe, true);
        }
        let source = source.ok_or("first launch requires --code-store DIR")?;
        let (assembly, recipe, lease) = self.initial(initial, source)?;
        let attempt = attempt()?;
        self.pending(attempt, assembly.identity)?;
        drop(lease);
        self.records.write_deploy(&Deploy {
            attempt,
            prior_activation: 0,
            exported: 0,
            old: ContentHash([0; 32]),
            new: assembly.digest,
            assembly: assembly.identity,
            state: State::Installing,
        })?;
        self.boot(assembly, recipe, false)
    }

    fn boot(
        &self,
        assembly: AssemblyRecord,
        recipe: Recipe,
        recover: bool,
    ) -> Result<(Running, child::Place), Error> {
        let (found, retain_from, child) = match recover.then(|| self.reopen(assembly.identity, 1)) {
            Some(Ok((running, child))) => (running.active, running.retain_from, child),
            _ => {
                let mut child = self.start(&assembly, &recipe, 0)?;
                let (ready, retain_from) =
                    opened(&mut child, assembly.identity, recipe.launch.n_place)?;
                (ready, retain_from, child)
            }
        };
        self.records.set_active(found)?;
        self.records.reconcile_pending()?;
        self.collect(found.store, retain_from)?;
        Ok((
            Running {
                active: found,
                retain_from,
                assembly,
                recipe,
            },
            child,
        ))
    }

    fn reopen(&self, key: ContentHash, version: u64) -> Result<(Running, child::Place), Error> {
        let assembly = self.records.assembly(key)?;
        let recipe = self.recipe(assembly.recipe)?;
        let mut child = self.start(&assembly, &recipe, version)?;
        let (active, retain_from) = opened(&mut child, key, recipe.launch.n_place)?;
        if active.version != version {
            return Err("ready activation differs from selected activation".into());
        }
        Ok((
            Running {
                active,
                retain_from,
                assembly,
                recipe,
            },
            child,
        ))
    }

    fn preview(
        &self,
        assembly: &AssemblyRecord,
        recipe: &Recipe,
        old: &Running,
        report: &mut Report,
    ) -> Result<Preview, Error> {
        let previous = self.read_preview(&old.assembly, &old.recipe, &0_u32.to_le_bytes())?;
        let mut old_list = Vec::new();
        old_list.extend_from_slice(&previous.description[previous.entries_at..]);
        let preview = self.read_preview(assembly, recipe, &old_list)?;
        let old_entries = &previous.describe.exported;
        if preview.answers.len() != old_entries.len() {
            return Err("preview answer count".into());
        }
        for (entry, answer) in old_entries.iter().zip(&preview.answers) {
            report
                .preview
                .push(format!("{answer} {}", crate::call::hex(&entry.identity)));
        }
        for entry in &preview.describe.exported {
            if !old_entries.iter().any(|prior| {
                prior.identity == entry.identity
                    && prior.args_fp == entry.args_fp
                    && prior.result_fp == entry.result_fp
            }) {
                report
                    .preview
                    .push(format!("new {}", crate::call::hex(&entry.identity)));
            }
        }
        let old_grants = self.grants(&old.recipe);
        let new_grants = self.grants(recipe);
        for grant in &new_grants {
            if grant.starts_with("Dir=") {
                report.preview.push(format!("grant {grant}"));
            }
            if !old_grants.contains(grant) {
                report.preview.push(format!("added {grant}"));
            }
        }
        for grant in &old_grants {
            if !new_grants.contains(grant) {
                report.preview.push(format!("removed {grant}"));
            }
        }
        report
            .preview
            .push(format!("target {}", recipe.launch.target));
        report
            .preview
            .push(format!("n_place {}", recipe.launch.n_place));
        report
            .preview
            .push(format!("root_limit {}", recipe.launch.root_limit));
        report.preview.push("schema: unknown".into());
        Ok(preview)
    }

    fn grants(&self, recipe: &Recipe) -> Vec<String> {
        let mut grants = crate::native::recipe_build::display_grants(recipe);
        grants.retain(|grant| !grant.starts_with("Dir="));
        grants.extend(
            recipe
                .launch
                .grants
                .iter()
                .filter(|binding| binding.kind == GrantKind::Dir)
                .flat_map(|binding| &binding.values)
                .map(|path| format!("Dir={}", self.workdir.join(path).display())),
        );
        grants
    }

    fn read_preview(
        &self,
        assembly: &AssemblyRecord,
        recipe: &Recipe,
        old: &[u8],
    ) -> Result<Preview, Error> {
        let image = self.records.verify_image(assembly.digest)?;
        let bytes = crate::describe::preview_child_at(
            &image,
            recipe.launch.n_place,
            START_TIMEOUT,
            old,
            Some(&self.workdir),
        )?;
        let mut wire = Reader {
            bytes: &bytes,
            at: 0,
        };
        wire.bytes()?;
        wire.name()?;
        wire.word()?;
        let entries_at = wire.at;
        wire.at = 0;
        let describe = Describe::read_from(&mut wire)?;
        let description = bytes[..wire.at].to_vec();
        let release = wire.bytes()?;
        if describe.digest.as_slice() != assembly.digest.0
            || describe.target != recipe.launch.target
            || describe.n_place != recipe.launch.n_place
            || release.len() != 32
        {
            return Err("preview does not describe the selected assembly".into());
        }
        let mut answers = Vec::new();
        for _ in 0..wire.count()? {
            answers.push(match wire.count()? {
                0 => {
                    wire.count()?;
                    "unchanged"
                }
                1 => {
                    wire.count()?;
                    wire.bytes()?;
                    "resolvable"
                }
                2 => "gone",
                _ => return Err("preview entry tag".into()),
            });
        }
        wire.done()?;
        Ok(Preview {
            description,
            entries_at,
            describe,
            release,
            answers,
        })
    }

    fn prepare(
        &self,
        live: &mut child::Place,
        assembly: &AssemblyRecord,
        recipe: &Recipe,
        preview: &Preview,
        record: &mut Deploy,
        retry: bool,
    ) -> Result<(child::Place, ContentHash), Error> {
        let mut export = command_prefix(0, record.attempt)?;
        export.push(u8::from(retry));
        let received = exchange(live, &export, recipe.launch.n_place)?;
        let Reply::Exported(version, name) = received else {
            return Err(received.error());
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err("invalid export name".into());
        }
        let scratch = self.scratch.join(name);
        let cleanup = Scratch(scratch);
        let image = self.records.verify_image(assembly.digest)?;
        let mut command = Command::new(&image);
        command
            .current_dir(&self.workdir)
            .arg(format!("--hirsel-n-place={}", recipe.launch.n_place))
            .arg("--hirsel-prepare")
            .arg(&cleanup.0)
            .arg(crate::call::hex(&record.attempt))
            .arg(crate::call::hex(&assembly.digest.0));
        if !child::prepare(command, DEPLOY_TIMEOUT)?.success() {
            return Err("Prepare".into());
        }
        let mut import = 1_u32.to_le_bytes().to_vec();
        field(&mut import, &preview.description)?;
        field(&mut import, &preview.release)?;
        field(&mut import, &assembly.digest.0)?;
        let received = exchange(live, &import, recipe.launch.n_place)?;
        let Reply::Imported(digest) = received else {
            return Err(received.error());
        };
        record.exported = version;
        let next = version.checked_add(1).ok_or("activation overflow")?;
        let place = self.start(assembly, recipe, next)?;
        record.state = State::Installing;
        self.records.write_deploy(record)?;
        Ok((place, digest))
    }

    fn submit(
        &self,
        operator: &mut ControlConnection,
        old: &mut Running,
        live: &mut child::Place,
    ) -> Result<(Report, Option<child::Place>), Error> {
        let mut report = Report::default();
        let start = Instant::now();
        let added = match operator.deploy_permission(self.limits.timeout.unwrap()) {
            Ok(added) => added,
            Err(error) => {
                let reason = format!("Refused(Admit({error}))");
                let _ = operator.refuse_before_need(&reason);
                report.refused = Some(reason);
                return Ok((report, None));
            }
        };
        let admitted = match self
            .admission
            .admit(operator, &self.limits, &NativeSemantics)
        {
            Ok(admitted) => admitted,
            Err(error) => {
                let reason = format!("Refused(Admit({error}))");
                let _ = operator.refuse_before_need(&reason);
                report.refused = Some(reason);
                return Ok((report, None));
            }
        };
        report.fetch_ms = start.elapsed().as_millis();
        let build = Instant::now();
        let proposed = admitted.release().recipe();
        if old
            .recipe
            .launch
            .root_limit
            .saturating_add(proposed.launch.root_limit)
            > self.memory
        {
            report.refused = Some("Refused(Memory)".into());
            return Ok((report, None));
        }
        if !added && grants_added(&old.recipe, proposed) {
            report.refused = Some("Refused(GrantAdded)".into());
            return Ok((report, None));
        }
        let (assembly, recipe, lease) = match self.assemble(admitted) {
            Ok(built) => built,
            Err(error) => {
                report.refused = Some(format!("Refused(Assembly({error}))"));
                return Ok((report, None));
            }
        };
        report.assemble_ms = build.elapsed().as_millis();
        let mut record = Deploy {
            attempt: attempt()?,
            prior_activation: old.active.version,
            exported: 0,
            old: old.assembly.digest,
            new: assembly.digest,
            assembly: assembly.identity,
            state: State::Preparing,
        };
        self.pending(record.attempt, assembly.identity)?;
        drop(lease);
        let candidate = self.preview(&assembly, &recipe, old, &mut report);
        let preview = match candidate {
            Ok(preview) => preview,
            Err(error) => {
                self.records.clear_pending(record.attempt)?;
                report.refused = Some(format!("Refused(Preview({error}))"));
                return Ok((report, None));
            }
        };
        self.records.write_deploy(&record)?;
        let mut paused = None;
        loop {
            let started = Instant::now();
            let retry = paused.is_some();
            let (mut replacement, prepared) =
                match self.prepare(live, &assembly, &recipe, &preview, &mut record, retry) {
                    Ok(target) => target,
                    Err(error) => {
                        abort(live, record.attempt)?;
                        record.state = State::Refused;
                        self.records.write_deploy(&record)?;
                        self.records.clear_pending(record.attempt)?;
                        self.collect(old.active.store, old.retain_from)?;
                        report.refused = Some(format!("Refused({error})"));
                        return Ok((report, None));
                    }
                };
            let preparation = started.elapsed();
            let pause = *paused.get_or_insert_with(Instant::now);
            let outcome = cutover(live, record.attempt, prepared, self.max_pause, retry)?;
            let version = record
                .exported
                .checked_add(1)
                .ok_or("activation overflow")?;
            match outcome {
                Reply::Installed(at) if at == version => {
                    let active = Active {
                        store: old.active.store,
                        version: at,
                        assembly: assembly.identity,
                    };
                    self.records.set_active(active)?;
                    let mut ready =
                        opened(&mut replacement, assembly.identity, recipe.launch.n_place);
                    for _ in 0..self.retries {
                        if ready.is_ok() {
                            break;
                        }
                        drop(replacement);
                        replacement = self.start(&assembly, &recipe, at)?;
                        ready = opened(&mut replacement, assembly.identity, recipe.launch.n_place);
                    }
                    let (observed, retain_from) = match ready {
                        Ok((observed, retain_from))
                            if observed.store == active.store && observed.version == at =>
                        {
                            (observed, retain_from)
                        }
                        refused => {
                            record.state = State::Failed;
                            self.records.write_deploy(&record)?;
                            let detail = match refused {
                                Ok((found, _)) => {
                                    format!("unexpected store or activation {}", found.version)
                                }
                                Err(error) => error.to_string(),
                            };
                            return Err(format!(
                                "committed activation {at} failed to become ready ({detail})"
                            )
                            .into());
                        }
                    };
                    record.state = State::Done;
                    self.records.write_deploy(&record)?;
                    self.records.clear_pending(record.attempt)?;
                    report.activation = Some(at);
                    report.pause_ms = pause.elapsed().as_nanos().div_ceil(1_000_000);
                    live.wait_retired(START_TIMEOUT)?;
                    self.collect(observed.store, retain_from)?;
                    *old = Running {
                        active: observed,
                        retain_from,
                        assembly,
                        recipe,
                    };
                    return Ok((report, Some(replacement)));
                }
                Reply::Drift if !retry && preparation.as_millis() < u128::from(self.max_pause) => {
                    drop(replacement);
                    let previous = record.attempt;
                    record.attempt = attempt()?;
                    self.pending(record.attempt, assembly.identity)?;
                    record.state = State::Preparing;
                    self.records.write_deploy(&record)?;
                    self.records.clear_pending(previous)?;
                }
                Reply::Refused(reason) => {
                    abort(live, record.attempt)?;
                    record.state = State::Refused;
                    self.records.write_deploy(&record)?;
                    self.records.clear_pending(record.attempt)?;
                    report.refused = Some(format!("Refused({reason})"));
                    return Ok((report, None));
                }
                Reply::Drift => {
                    abort(live, record.attempt)?;
                    record.state = State::Refused;
                    self.records.write_deploy(&record)?;
                    self.records.clear_pending(record.attempt)?;
                    report.refused = Some(format!("Refused(Busy({}))", preparation.as_millis()));
                    return Ok((report, None));
                }
                _ => {
                    return Err("cutover outcome unknown, recover the activation under LOCK".into());
                }
            }
        }
    }

    fn collect(&self, store: Attempt, retain_from: u64) -> Result<(), Error> {
        self.admission.under_collection(|reserved| {
            self.records.retire_activations(store, retain_from)?;
            let assemblies = self.records.roots(recording::unix_seconds()?)?;
            let mut images = BTreeSet::new();
            let mut code = BTreeSet::new();
            for name in &assemblies {
                let assembly = self.records.assembly(*name)?;
                images.insert(assembly.digest);
                code.insert(ObjectKey {
                    kind: ObjectKind::Recipe,
                    hash: assembly.recipe,
                });
                let recipe = self.recipe(assembly.recipe)?;
                for group in recipe.groups {
                    code.insert(ObjectKey {
                        kind: ObjectKind::Group,
                        hash: group.hash,
                    });
                }
                for pair in recipe.bodies {
                    code.insert(ObjectKey {
                        kind: ObjectKind::Body,
                        hash: pair.body.hash,
                    });
                    code.insert(ObjectKey {
                        kind: ObjectKind::Annex,
                        hash: pair.annex.hash,
                    });
                }
            }
            self.records.collect(&assemblies, &images)?;
            self.code.collect(|kind, hash| {
                let key = ObjectKey { kind, hash };
                code.contains(&key) || reserved(key)
            })?;
            Ok(())
        })
    }

    fn pending(&self, attempt: Attempt, assembly: ContentHash) -> io::Result<()> {
        self.admission
            .under_collection(|_| self.records.pending(attempt, assembly))
    }
}

struct Preview {
    description: Vec<u8>,
    entries_at: usize,
    describe: Describe,
    release: Vec<u8>,
    answers: Vec<&'static str>,
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum Reply {
    Opened {
        store: Attempt,
        version: u64,
        retain_from: u64,
    },
    Current(u64),
    Refused(String),
    Exported(u64, String),
    Imported(ContentHash),
    Installed(u64),
    Drift,
    Aborted,
    Uncertain,
}

impl Reply {
    fn read(frame: &[u8]) -> Result<Self, Error> {
        let mut wire = Reader {
            bytes: frame,
            at: 0,
        };
        let reply = match wire.count()? {
            0 => {
                let version = wire.word()?;
                let mut store = [0u8; 16];
                store[..8].copy_from_slice(&wire.word()?.to_le_bytes());
                store[8..].copy_from_slice(&wire.word()?.to_le_bytes());
                if wire.bytes()?.len() != 32 {
                    return Err("ready release digest length".into());
                }
                let retain_from = wire.word()?;
                if retain_from == 0 || retain_from > version {
                    return Err("invalid release retention floor".into());
                }
                Self::Opened {
                    store,
                    version,
                    retain_from,
                }
            }
            1 => {
                let version = wire.word()?;
                wire.bytes()?;
                Self::Current(version)
            }
            2 => Self::Refused(wire.name()?),
            3 => Self::Exported(wire.word()?, wire.name()?),
            4 => Self::Imported(ContentHash(
                wire.bytes()?
                    .try_into()
                    .map_err(|_| "prepared digest length")?,
            )),
            5 => Self::Installed(wire.word()?),
            6 => Self::Drift,
            7 => Self::Aborted,
            8 => Self::Uncertain,
            _ => return Err("place response tag".into()),
        };
        wire.done()?;
        Ok(reply)
    }

    fn error(self) -> Error {
        match self {
            Self::Current(version) => format!("NotCurrent({version})").into(),
            Self::Refused(reason) => reason.into(),
            _ => "unexpected place response".into(),
        }
    }
}

fn opened(
    place: &mut child::Place,
    assembly: ContentHash,
    max: u64,
) -> Result<(Active, u64), Error> {
    place.control().set_read_timeout(Some(START_TIMEOUT))?;
    let frame = read_frame(place.control(), usize::try_from(max)?)?;
    match Reply::read(&frame)? {
        Reply::Opened {
            store,
            version,
            retain_from,
        } => Ok((
            Active {
                store,
                version,
                assembly,
            },
            retain_from,
        )),
        reply => Err(reply.error()),
    }
}

fn cutover(
    place: &mut child::Place,
    attempt: Attempt,
    digest: ContentHash,
    pause: u64,
    retry: bool,
) -> Result<Reply, Error> {
    let mut command = command_prefix(2, attempt)?;
    field(&mut command, &digest.0)?;
    command.extend_from_slice(&pause.to_le_bytes());
    command.push(u8::from(retry));
    exchange(place, &command, 4096)
}

fn abort(place: &mut child::Place, attempt: Attempt) -> Result<(), Error> {
    match exchange(place, &command_prefix(3, attempt)?, 4096)? {
        Reply::Aborted => Ok(()),
        reply => Err(reply.error()),
    }
}

fn exchange(place: &mut child::Place, command: &[u8], max: u64) -> Result<Reply, Error> {
    let peer = place.control();
    peer.set_read_timeout(Some(DEPLOY_TIMEOUT))?;
    peer.set_write_timeout(Some(DEPLOY_TIMEOUT))?;
    write_frame(peer, command)?;
    Reply::read(&read_frame(peer, usize::try_from(max)?)?)
}

fn command_prefix(tag: u32, attempt: Attempt) -> io::Result<Vec<u8>> {
    let mut bytes = tag.to_le_bytes().to_vec();
    field(&mut bytes, &attempt)?;
    Ok(bytes)
}

fn field(out: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
    let length = u32::try_from(value.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn read_frame(peer: &mut UnixStream, max: usize) -> io::Result<Vec<u8>> {
    let mut size = [0; 4];
    peer.read_exact(&mut size)?;
    let length = u32::from_le_bytes(size) as usize;
    if length > max {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut payload = vec![0; length];
    peer.read_exact(&mut payload)?;
    Ok(payload)
}

fn write_frame(peer: &mut UnixStream, value: &[u8]) -> io::Result<()> {
    let size = u32::try_from(value.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    peer.write_all(&size.to_le_bytes())?;
    peer.write_all(value)
}

fn grants_added(old: &Recipe, new: &Recipe) -> bool {
    new.launch.grants.iter().any(|grant| {
        !old.launch.grants.iter().any(|current| {
            current.kind == grant.kind && grant.values.iter().all(|v| current.values.contains(v))
        })
    })
}

fn attempt() -> io::Result<Attempt> {
    let mut id = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut id)?;
    Ok(id)
}
