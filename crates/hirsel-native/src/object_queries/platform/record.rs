//! The platform record: a later process reuses one full platform capture
//! while nothing that capture read or searched has changed.
//!
//! The record is a LOCAL cache that detects accidents; it is not a security
//! boundary. Files of the user's own account (the packaged toolchain, this
//! private cache) are trusted to that account. Full verification is the cold
//! capture: it runs the capture children and identifies every tool, kit and
//! input byte. It runs on every miss, under `HIRSEL_PLATFORM_CAPTURE=cold`
//! (which reads no record), on every release build and when a record is older than a day.
//!
//! A hit compares the stamps of exactly these paths, taken by the capture
//! that issued the record:
//! - every directory and file in each guarded tree, recursively: the linker
//!   package, the kit and each runtime library-path directory. A directory's
//!   stamp changes when an immediate entry is added, removed or renamed, so a
//!   new file at any depth of a tree (a preferred DSO in a nested hwcaps
//!   directory, say) changes the stamp of the deepest directory that already
//!   existed, and that directory is in the record;
//! - each other file the capture read by name (the runtime archive, its
//!   startup order, foreign inputs, loader, auditor, files a linker script
//!   names). Nothing found these by a search;
//! - the nearest existing ancestor of an absent library-path directory.
//!
//! It also re-resolves every selected spelling, so a symlink hop that now
//! leads elsewhere is a miss. Foreign-input selection searches system
//! directories, but it runs in every process and its result is in the key,
//! so system directories need no guard. A capture is not recorded when the
//! loader found an object outside the guarded trees, when an object carries
//! DT_RPATH or DT_RUNPATH, when the guarded trees are too large, or when a
//! guarded file is on a filesystem outside a local allowlist.
use super::*;
use crate::file_digest::{Stamp, stable, stamp};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::time::{SystemTime, UNIX_EPOCH};

const PLATFORM: Domain = Domain {
    kind: 0x4f000010,
    schema: 1,
};
const FORMAT: u64 = 1;
#[cfg(test)]
thread_local! {
    static RECORD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// A record older than this gets a full verification.
const PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// Directory and file stamps in one record, across every guarded tree.
const GUARD_ENTRIES: usize = 4096;
/// statfs `f_type`s of local filesystems that advance ctime on every change:
/// ext2/3/4, xfs, btrfs, tmpfs, overlayfs, f2fs, zfs, bcachefs and ramfs.
const LOCAL_FILESYSTEMS: [i64; 9] = [
    0xef53,
    0x5846_5342,
    0x9123_683e,
    0x0102_1994,
    0x794c_7630,
    0xf2f5_2010,
    0x2fc1_2fc1,
    0xca45_1a4e,
    0x8584_58f6,
];

/// Everything one platform capture is selected by, as selected. The key
/// resolves relative paths against the working directory.
#[derive(Clone)]
pub struct PlatformSpec {
    pub target: super::super::super::TargetSpec,
    pub linker: PathBuf,
    pub kit: PathBuf,
    pub runtime_archive: PathBuf,
    pub foreign: Vec<PathBuf>,
    pub loader: PathBuf,
    pub auditor: PathBuf,
    pub library_path: OsString,
    pub emulator: Option<PathBuf>,
    pub timeout: Duration,
    pub events_bytes: u64,
    pub runtime_limits: RuntimeLimits,
    pub limits: CaptureLimits,
    pub debug_tables: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// Reuse a valid record, else capture cold and record the capture.
    Record,
    /// Full verification: read no record, capture cold, record the capture.
    Cold,
}
impl CaptureMode {
    /// `HIRSEL_PLATFORM_CAPTURE=cold` selects full verification. tools/land
    /// and conformance gates set it; release build options require Cold directly.
    pub fn from_environment() -> Result<Self, String> {
        match std::env::var_os("HIRSEL_PLATFORM_CAPTURE") {
            None => Ok(Self::Record),
            Some(value) if value == "cold" => Ok(Self::Cold),
            Some(value) => Err(format!(
                "HIRSEL_PLATFORM_CAPTURE={} is not `cold`",
                value.to_string_lossy()
            )),
        }
    }
}

pub struct Selected {
    pub platform: CapturedPlatform,
    /// The platform came from a record. A later failure it fed must delete
    /// the record and recapture before any output is kept.
    pub hit: bool,
}

/// One selection made before a request that would otherwise make it again:
/// a program record's lookup keys a build by the platform it selects now, and
/// a miss hands that selection to the build. It answers only a selection with
/// the same key (so the same tools, archive, foreign inputs and compiler) and
/// the same capture mode, and only once; any other selection captures for
/// itself.
pub struct HandedPlatform {
    key: Vec<u8>,
    mode: CaptureMode,
    identity: ContentHash,
    selected: std::sync::Mutex<Option<Selected>>,
}
impl HandedPlatform {
    /// The selected platform's identity.
    pub fn identity(&self) -> ContentHash {
        self.identity
    }
}

impl PlatformSpec {
    pub fn select(&self, store: &dyn ResultStore, mode: CaptureMode) -> Result<Selected, String> {
        self.select_in(store, &cache_directory(), mode, SystemTime::now())
    }
    /// Select now, for a later request to take with `select_handed`.
    pub fn hand_off(
        &self,
        store: &dyn ResultStore,
        mode: CaptureMode,
    ) -> Result<HandedPlatform, String> {
        let key = self.key()?;
        let selected = self.select(store, mode)?;
        Ok(HandedPlatform {
            key,
            mode,
            identity: selected.platform.identity,
            selected: std::sync::Mutex::new(Some(selected)),
        })
    }
    /// `handed`'s selection when it was made for this selection and mode and
    /// is still untaken, else a selection of its own.
    pub fn select_handed(
        &self,
        handed: Option<&HandedPlatform>,
        store: &dyn ResultStore,
        mode: CaptureMode,
    ) -> Result<Selected, String> {
        if let Some(handed) = handed
            && handed.mode == mode
            && handed.key == self.key()?
            && let Some(selected) = handed
                .selected
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
        {
            return Ok(selected);
        }
        self.select(store, mode)
    }
    /// After a hit was shown wrong (`reason`) before it fed any output:
    /// delete its record, verify fully and record again.
    pub fn recapture(
        &self,
        store: &dyn ResultStore,
        reason: &str,
    ) -> Result<CapturedPlatform, String> {
        self.recapture_in(store, &cache_directory(), reason, SystemTime::now())
    }
    /// Rule 5 for a failure (`failure`) that a record's platform, with
    /// identity `recorded`, fed: delete the record and verify fully. The
    /// fresh platform comes back only when it is the recorded one; the
    /// failure then lost something the record named (store chunks, say) that
    /// the full capture restored, so the caller may run again. Any other
    /// platform withholds every output the record's platform fed.
    pub fn recover(
        &self,
        store: &dyn ResultStore,
        recorded: ContentHash,
        failure: &str,
    ) -> Result<CapturedPlatform, String> {
        self.recover_in(
            store,
            &cache_directory(),
            recorded,
            failure,
            SystemTime::now(),
        )
    }

    fn recover_in(
        &self,
        store: &dyn ResultStore,
        cache: &Path,
        recorded: ContentHash,
        failure: &str,
        now: SystemTime,
    ) -> Result<CapturedPlatform, String> {
        let fresh = self.recapture_in(store, cache, failure, now)?;
        if fresh.identity != recorded {
            return Err(format!(
                "platform cache mismatch ({failure}): record deleted and recaptured; rerun"
            ));
        }
        Ok(fresh)
    }

    fn recapture_in(
        &self,
        store: &dyn ResultStore,
        cache: &Path,
        reason: &str,
        now: SystemTime,
    ) -> Result<CapturedPlatform, String> {
        crate::runner::count_platform_record(&format!(
            "mismatch: {reason}; record deleted and recaptured"
        ));
        let _ = std::fs::remove_file(cache.join(name(&self.key()?)));
        Ok(self
            .select_in(store, cache, CaptureMode::Cold, now)?
            .platform)
    }

    fn select_in(
        &self,
        store: &dyn ResultStore,
        cache: &Path,
        mode: CaptureMode,
        now: SystemTime,
    ) -> Result<Selected, String> {
        let name = name(&self.key()?);
        let (reason, recorded) = match mode {
            CaptureMode::Cold => ("full verification required".to_string(), None),
            CaptureMode::Record => match self.lookup(store, cache, &name, now) {
                Ok(platform) => {
                    crate::runner::count_platform_record("hit");
                    return Ok(Selected {
                        platform,
                        hit: true,
                    });
                }
                Err(miss) => (miss.reason, miss.recorded),
            },
        };
        crate::runner::count_platform_record(&format!("miss: {reason}"));
        let (platform, issued) = self.capture(store, cache, &name, now)?;
        if let Err(reason) = issued {
            crate::runner::count_platform_record(&format!("not recorded: {reason}"));
        }
        if recorded.is_some_and(|identity| identity != platform.identity) {
            crate::runner::count_platform_record(
                "mismatch: full verification disagreed with the record, which was replaced",
            );
        }
        Ok(Selected {
            platform,
            hit: false,
        })
    }

    /// The versioned, length-delimited selection a record answers.
    fn key(&self) -> Result<Vec<u8>, String> {
        let cwd = std::env::current_dir().map_err(error)?;
        let mut key = Vec::new();
        let mut put = |bytes: &[u8]| {
            key.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            key.extend_from_slice(bytes);
        };
        put(b"hirsel.platform-record.key.v1");
        // A rebuilt compiler may capture differently: its executable's stamp
        // selects other records.
        let compiler = std::fs::metadata(std::env::current_exe().map_err(error)?).map_err(error)?;
        put(&words(&stamp(&compiler)));
        put(&self.target.digest().0);
        put(&[u8::from(self.debug_tables)]);
        put(&words(&[
            self.limits.archive_bytes,
            self.limits.files as u64,
            self.runtime_limits.aliases as u64,
            self.runtime_limits.name_bytes as u64,
            self.events_bytes,
            u64::try_from(self.timeout.as_nanos()).unwrap_or(u64::MAX),
        ]));
        for path in [&self.linker, &self.kit, &self.loader, &self.auditor] {
            put(cwd.join(path).as_os_str().as_bytes());
        }
        match &self.emulator {
            None => put(&[0]),
            Some(emulator) => {
                put(&[1]);
                put(cwd.join(emulator).as_os_str().as_bytes());
            }
        }
        // The archive as selected, not canonical: capture finds startup.order
        // beside this spelling.
        let archive = cwd.join(&self.runtime_archive);
        put(archive.as_os_str().as_bytes());
        let order = archive.with_file_name("startup.order");
        put(&[u8::from(order.try_exists().map_err(error)?)]);
        put(order.as_os_str().as_bytes());
        put(&(self.foreign.len() as u64).to_le_bytes());
        for path in &self.foreign {
            put(cwd.join(path).as_os_str().as_bytes());
        }
        let directories = std::env::split_paths(&self.library_path).collect::<Vec<_>>();
        put(&(directories.len() as u64).to_le_bytes());
        for directory in directories {
            put(cwd.join(directory).as_os_str().as_bytes());
        }
        Ok(key)
    }

    fn lookup(
        &self,
        store: &dyn ResultStore,
        cache: &Path,
        name: &str,
        now: SystemTime,
    ) -> Result<CapturedPlatform, Miss> {
        let directory = crate::runtime_cache::private_directory(cache)
            .map_err(|error| Miss::from(format!("private cache: {error}")))?;
        #[cfg(test)]
        RECORD_READS.update(|count| count + 1);
        let file = match crate::runtime_cache::private_file(&cache.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("no record".to_string().into());
            }
            Err(error) => return Err(format!("record unreadable: {error}").into()),
        };
        let mut text = String::new();
        file.take(16 << 20)
            .read_to_string(&mut text)
            .map_err(|error| Miss::from(format!("record unreadable: {error}")))?;
        let record: LocalRecord = hirsel_codec::from_text(&text)
            .map_err(|error| Miss::from(format!("record malformed: {}", error.message)))?;
        if record.format != FORMAT || record.key != name {
            return Err("record format or key differs".to_string().into());
        }
        if record.scope != scope(&directory).map_err(Miss::from)? {
            return Err("record from another machine or cache".to_string().into());
        }
        let issued = UNIX_EPOCH + Duration::from_secs(record.issued);
        if !now.duration_since(issued).is_ok_and(|age| age < PERIOD) {
            return Err(Miss {
                reason: "record older than a day".into(),
                recorded: Some(record.identity),
            });
        }
        for (spelled, canonical) in &record.spellings {
            if std::fs::canonicalize(spelled).ok().as_deref() != Some(Path::new(canonical)) {
                return Err(format!("{spelled} now resolves elsewhere").into());
            }
        }
        if let Some(path) = changed(&record.guard) {
            return Err(format!("{path} changed").into());
        }
        let platform = CapturedPlatform::stored(store, &self.target, record.platform.into())
            .map_err(|error| Miss::from(format!("store lacks the recorded platform: {error}")))?;
        if platform.identity != record.identity {
            return Err("store names another platform".to_string().into());
        }
        Ok(platform)
    }

    /// The cold capture, then the record of it unless something it read
    /// cannot be witnessed. The capture's own failure is the build's.
    fn capture(
        &self,
        store: &dyn ResultStore,
        cache: &Path,
        name: &str,
        started: SystemTime,
    ) -> Result<(CapturedPlatform, Result<(), String>), String> {
        let selection = Selection::resolve(self);
        let before = selection
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|selection| selection.guard(&[]));
        let linker = FrozenLinker::snapshot(store, &self.linker)?;
        let kit = FrozenLinkKit::snapshot(store, &self.kit, &self.target, linker, self.limits)?;
        let mut platform = CapturedPlatform::capture(
            store,
            &self.target,
            PlatformCaptureInputs {
                linker,
                kit,
                runtime_archive: &self.runtime_archive,
                foreign: &self.foreign,
                runtime: RuntimeSelection {
                    loader: &self.loader,
                    auditor: &self.auditor,
                    library_path: &self.library_path,
                    emulator: self.emulator.as_deref(),
                    timeout: self.timeout,
                    events_bytes: self.events_bytes,
                    limits: self.runtime_limits,
                },
                limits: self.limits,
            },
        )?;
        let issued = selection.and_then(|selection| {
            let record = selection.issue(store, kit, &platform, &before?, cache, name, started)?;
            publish(cache, name, &record)
        });
        // Only issuance reads what the loader searched; the build keeps the
        // platform without it.
        platform.search = RuntimeSearch::default();
        Ok((platform, issued))
    }
}

struct Miss {
    reason: String,
    /// The identity a stale record named, for a periodic comparison.
    recorded: Option<ContentHash>,
}
impl From<String> for Miss {
    fn from(reason: String) -> Self {
        Self {
            reason,
            recorded: None,
        }
    }
}

/// The first guarded path whose current stamp is not the recorded one.
fn changed(guard: &[(String, Vec<u64>)]) -> Option<&str> {
    guard.iter().find_map(|(path, recorded)| {
        let current = std::fs::metadata(path).map(|meta| stamp(&meta));
        (!current.is_ok_and(|current| same(&current, recorded))).then_some(path.as_str())
    })
}

/// Stamps are equal in every field, ctime included: an in-place write that
/// restores mtime and size still advances ctime.
fn same(left: &Stamp, right: &[u64]) -> bool {
    left.as_slice() == right
}

/// Every stamp taken before the capture is unchanged after it, and every
/// stamp is older than the capture's start by the racy-timestamp margin.
///
/// Children read by path: the kit and probe linkers and the loader name the
/// files they open. The parent's stamps before and after a child bracket
/// that read only by path, not by the child's descriptor. That is enough
/// because equal stamps with a ctime older than the capture's start exclude
/// a swap-and-restore: a rename or link that restores a file sets its inode's
/// ctime, as does any write. A read first named after the capture (a file a
/// linker script names) has no earlier stamp; its old ctime alone shows it
/// was not changed while the child read it.
fn witnessed(before: &Guard, after: &Guard, started: SystemTime) -> Result<(), String> {
    for (path, earlier) in &before.0 {
        if !after.0.get(path).is_some_and(|later| same(later, earlier)) {
            return Err(format!("{} changed during the capture", path.display()));
        }
    }
    for (path, stamp) in &after.0 {
        if !stable(stamp, started) {
            return Err(format!("{} changed too recently", path.display()));
        }
    }
    Ok(())
}

/// Path stamps; dirs and files alike.
struct Guard(BTreeMap<PathBuf, Stamp>);

/// The guarded paths a selection resolves to.
#[derive(Default)]
struct Selection {
    /// Each selected spelling (working-directory resolved) and its canonical path.
    spellings: Vec<(PathBuf, PathBuf)>,
    /// Canonical roots guarded recursively.
    trees: Vec<PathBuf>,
    /// Canonical files read by name.
    files: Vec<PathBuf>,
    /// The nearest existing ancestor of each absent library-path directory.
    ancestors: Vec<PathBuf>,
}
impl Selection {
    fn resolve(spec: &PlatformSpec) -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(error)?;
        let mut selection = Self::default();
        let mut canonical = |spelled: &Path| -> Result<PathBuf, String> {
            let spelled = cwd.join(spelled);
            let canonical = std::fs::canonicalize(&spelled)
                .map_err(|error| format!("{}: {error}", spelled.display()))?;
            selection.spellings.push((spelled, canonical.clone()));
            Ok(canonical)
        };
        let mut trees = vec![canonical(&spec.linker)?, canonical(&spec.kit)?];
        let mut ancestors = Vec::new();
        for directory in std::env::split_paths(&spec.library_path) {
            let spelled = cwd.join(directory);
            if spelled.try_exists().map_err(error)? {
                trees.push(canonical(&spelled)?);
            } else {
                let existing = spelled
                    .ancestors()
                    .skip(1)
                    .find(|ancestor| ancestor.exists())
                    .ok_or("a library-path directory has no existing ancestor")?;
                ancestors.push(canonical(existing)?);
            }
        }
        let mut files = vec![
            canonical(&spec.loader)?,
            canonical(&spec.auditor)?,
            canonical(&spec.runtime_archive)?,
        ];
        let order = cwd
            .join(&spec.runtime_archive)
            .with_file_name("startup.order");
        if order.try_exists().map_err(error)? {
            files.push(canonical(&order)?);
        }
        for path in &spec.foreign {
            files.push(canonical(path)?);
        }
        selection.trees = trees;
        selection.ancestors = ancestors;
        selection.files = files;
        Ok(selection)
    }

    /// Stamps of every guarded path, plus `extra` files read by name.
    fn guard(&self, extra: &[PathBuf]) -> Result<Guard, String> {
        let mut guard = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let add = |path: &Path, guard: &mut BTreeMap<PathBuf, Stamp>| {
            let meta =
                std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
            if guard.len() >= GUARD_ENTRIES {
                return Err(format!("guarded paths exceed {GUARD_ENTRIES}"));
            }
            guard.insert(path.to_path_buf(), stamp(&meta));
            Ok::<_, String>(meta)
        };
        for tree in &self.trees {
            let mut pending = vec![tree.clone()];
            while let Some(path) = pending.pop() {
                let meta = add(&path, &mut guard)?;
                if meta.is_dir() && visited.insert((meta.dev(), meta.ino())) {
                    let entries = std::fs::read_dir(&path)
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                    for entry in entries {
                        pending.push(entry.map_err(error)?.path());
                    }
                }
            }
        }
        for path in self.files.iter().chain(&self.ancestors).chain(extra) {
            add(path, &mut guard)?;
        }
        Ok(Guard(guard))
    }

    /// The record of a cold capture whose every read is witnessed.
    #[allow(clippy::too_many_arguments)]
    fn issue(
        &self,
        store: &dyn ResultStore,
        kit: FrozenLinkKit,
        platform: &CapturedPlatform,
        before: &Guard,
        cache: &Path,
        name: &str,
        started: SystemTime,
    ) -> Result<String, String> {
        if platform.search.owner_paths {
            return Err("a runtime object carries DT_RPATH or DT_RUNPATH".into());
        }
        for opened in &platform.search.opened {
            let canonical = std::fs::canonicalize(opened)
                .map_err(|error| format!("{}: {error}", opened.display()))?;
            if !self.trees.iter().any(|tree| canonical.starts_with(tree)) {
                return Err(format!(
                    "the loader found {} outside the selected library path",
                    opened.display()
                ));
            }
        }
        let kit_files = kit.captured_files(store)?;
        let read = platform
            .reads
            .iter()
            .map(|read| read.path.clone())
            .chain(kit_files.iter().map(|(path, _)| path.clone()))
            .collect::<Vec<_>>();
        let after = self.guard(&read)?;
        witnessed(before, &after, started)?;
        // The kit capture holds the bytes its linker read; they must be the
        // bytes behind the witnessed name.
        for (path, content) in &kit_files {
            let mut file = std::fs::File::open(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let witnessed = after.0.get(path).ok_or("unwitnessed kit input")?;
            if stamp(&file.metadata().map_err(error)?) != *witnessed
                || !file_matches_blob(&blob_record(store, *content)?, &mut file)?
            {
                return Err(format!(
                    "{} differs from what the kit capture read",
                    path.display()
                ));
            }
        }
        let directory =
            crate::runtime_cache::private_directory(cache).map_err(|error| error.to_string())?;
        local_filesystems(&after, &directory)?;
        let text = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("non-UTF8 platform path {}", path.display()))
        };
        let record = LocalRecord {
            format: FORMAT,
            key: name.to_owned(),
            scope: scope(&directory)?,
            issued: started
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_secs(),
            spellings: self
                .spellings
                .iter()
                .map(|(spelled, canonical)| Ok((text(spelled)?, text(canonical)?)))
                .collect::<Result<_, String>>()?,
            guard: after
                .0
                .iter()
                .map(|(path, stamp)| Ok((text(path)?, stamp.to_vec())))
                .collect::<Result<_, String>>()?,
            identity: platform.identity,
            platform: platform.store(store)?.into(),
        };
        Ok(hirsel_codec::to_text(&record))
    }
}

/// The machine and this cache directory's inode: a cache copied or
/// restored elsewhere names other records. A host without a machine ID
/// (a build sandbox) is scoped by the cache directory and the guard's own
/// device and inode numbers.
fn scope(directory: &std::fs::File) -> Result<(String, Vec<u64>), String> {
    let machine = match std::fs::read_to_string("/etc/machine-id") {
        Ok(machine) => machine,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("/etc/machine-id: {error}")),
    };
    let meta = directory.metadata().map_err(error)?;
    Ok((machine.trim().to_owned(), vec![meta.dev(), meta.ino()]))
}

fn local_filesystems(guard: &Guard, cache: &std::fs::File) -> Result<(), String> {
    let mut devices = BTreeMap::new();
    for (path, stamp) in &guard.0 {
        devices.entry(stamp[0]).or_insert_with(|| path.clone());
    }
    let cache = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(cache));
    for path in devices
        .values()
        .map(PathBuf::as_path)
        .chain([Path::new(&cache)])
    {
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|error| error.to_string())?;
        // SAFETY: `name` is a NUL-terminated path and `status` is a valid,
        // writable statfs buffer for the duration of the call.
        let mut status: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(name.as_ptr(), &mut status) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        #[allow(clippy::unnecessary_cast)]
        let kind = status.f_type as i64;
        if !LOCAL_FILESYSTEMS.contains(&kind) {
            return Err(format!(
                "{} is on filesystem {kind:#x}, outside the local allowlist",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Write the record beside its name and rename it into place, so a reader
/// sees no record or a complete one; concurrent writers each publish whole.
fn publish(cache: &Path, name: &str, text: &str) -> Result<(), String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temporary = cache.join(format!(
        ".{name}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        std::fs::rename(&temporary, cache.join(name))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(error)
}

fn cache_directory() -> PathBuf {
    std::env::var_os("HIRSEL_NATIVE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(crate::runtime_cache::default_directory)
        .join("platform-v1")
}

fn name(key: &[u8]) -> String {
    Sha256::digest(key)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn words(values: &[u64]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

struct LocalRecord {
    format: u64,
    key: String,
    scope: (String, Vec<u64>),
    issued: u64,
    spellings: Vec<(String, String)>,
    guard: Vec<(String, Vec<u64>)>,
    identity: ContentHash,
    platform: WireRef,
}
snapshot_struct!(LocalRecord {
    format,
    key,
    scope,
    issued,
    spellings,
    guard,
    identity,
    platform
});

struct StoredInput {
    name: String,
    content: WireRef,
}
struct StoredPlatform {
    linker: WireRef,
    kit: WireRef,
    runtime_archive: StoredInput,
    foreign: Vec<StoredInput>,
    runtime: WireRef,
    target: ContentHash,
    identity: ContentHash,
    reads: Vec<StoredInput>,
    captured_inputs: WireRef,
    runtime_path: String,
    startup_order: Option<String>,
    foreign_paths: Vec<String>,
    static_foreign: bool,
}
snapshot_struct!(StoredInput { name, content });
snapshot_struct!(StoredPlatform {
    linker,
    kit,
    runtime_archive,
    foreign,
    runtime,
    target,
    identity,
    reads,
    captured_inputs,
    runtime_path,
    startup_order,
    foreign_paths,
    static_foreign
});

impl CapturedPlatform {
    fn store(&self, store: &dyn ResultStore) -> Result<Reference, String> {
        let text = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("non-UTF8 platform path {}", path.display()))
        };
        let input = |input: &NamedLinkInput| StoredInput {
            name: input.name.clone(),
            content: input.content.into(),
        };
        put_record(
            store,
            PLATFORM,
            &StoredPlatform {
                linker: self.linker.reference().into(),
                kit: self.kit.reference().into(),
                runtime_archive: input(&self.runtime_archive),
                foreign: self.foreign.iter().map(input).collect(),
                runtime: self.runtime.reference().into(),
                target: self.target,
                identity: self.identity,
                reads: self
                    .reads
                    .iter()
                    .map(|read| {
                        Ok(StoredInput {
                            name: text(&read.path)?,
                            content: read.content.into(),
                        })
                    })
                    .collect::<Result<_, String>>()?,
                captured_inputs: self.captured_inputs.reference().into(),
                runtime_path: text(&self.runtime_path)?,
                startup_order: self.startup_order.as_deref().map(text).transpose()?,
                foreign_paths: self
                    .foreign_paths
                    .iter()
                    .map(|path| text(path))
                    .collect::<Result<_, _>>()?,
                static_foreign: self.static_foreign,
            },
        )
    }

    /// The platform a record names, checked by structure: every record it
    /// names is present and each input has its blob record. No file bytes
    /// are read; a later consumer that cannot read them fails, and that
    /// failure deletes the record.
    fn stored(
        store: &dyn ResultStore,
        target: &super::super::super::TargetSpec,
        reference: Reference,
    ) -> Result<Self, String> {
        let stored: StoredPlatform = get_record(store, PLATFORM, reference)?;
        if stored.target != target.digest() {
            return Err("recorded platform has another target".into());
        }
        let input = |input: StoredInput| -> Result<NamedLinkInput, String> {
            let content: Reference = input.content.into();
            blob_record(store, content)?;
            Ok(NamedLinkInput {
                name: input.name,
                content,
            })
        };
        let reads = stored
            .reads
            .into_iter()
            .map(|read| {
                let read = input(read)?;
                Ok(PlatformRead {
                    path: read.name.into(),
                    content: read.content,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            linker: FrozenLinker::stored(store, stored.linker.into())?,
            kit: FrozenLinkKit::stored(store, stored.kit.into(), target)?,
            runtime_archive: input(stored.runtime_archive)?,
            foreign: stored
                .foreign
                .into_iter()
                .map(input)
                .collect::<Result<_, _>>()?,
            runtime: FrozenRuntime::from_reference(
                store,
                stored.runtime.into(),
                target,
                RuntimeLimits {
                    aliases: usize::MAX,
                    name_bytes: usize::MAX,
                },
            )?,
            target: stored.target,
            identity: stored.identity,
            reads,
            captured_inputs: CapturedLink::stored(store, stored.captured_inputs.into())?,
            runtime_path: stored.runtime_path.into(),
            startup_order: stored.startup_order.map(PathBuf::from),
            foreign_paths: stored
                .foreign_paths
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            static_foreign: stored.static_foreign,
            search: RuntimeSearch::default(),
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/native-validation/platform_record.rs"]
mod tests;
