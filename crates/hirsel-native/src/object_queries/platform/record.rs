//! The platform record: a later process reuses one full platform capture
//! while nothing that capture read or searched has changed (native rerun
//! design §15). One rule for every build profile: a record answers only when
//! the capture witnessed its complete selection; any other capture is cold.
//!
//! The record is a LOCAL cache that detects accidents; it is not a security
//! boundary. Files of the user's own account (the packaged toolchain, this
//! private cache) are trusted to that account. Full verification is the cold
//! capture: it runs the capture children and identifies every tool, kit and
//! input byte. It runs on every miss, under `HIRSEL_PLATFORM_CAPTURE=cold`
//! (which reads no record) and when a record has expired.
//!
//! A record lives beside the content it names, in the local product store
//! the selection reads (`platform-v1` under that store's root): two homes
//! never answer from each other's records, and a record cannot outlive or
//! precede its content's store. A capture made outside any request (a
//! program record's lookup) publishes its content before its record. A store
//! without a registration keeps no records.
//!
//! Complete selection (R1, R2). The linker reports its own search
//! (`--search-trace`: every query through its input filesystem, found or
//! absent, and the final `-L` list) and the loader probe reports its search
//! the same way. A hit compares the stamps of exactly these paths, taken by
//! the capture that issued the record:
//! - every directory and file in each guarded tree, recursively: the linker
//!   package, the kit and each runtime library-path directory. A directory's
//!   stamp changes when an immediate entry is added, removed or renamed, so a
//!   new file at any depth of a tree (a preferred DSO in a nested hwcaps
//!   directory, say) changes the stamp of the deepest directory that already
//!   existed, and that directory is in the record;
//! - each other file the capture read by name (the runtime archive, its
//!   startup order, foreign inputs, loader, auditor, files a linker script
//!   names, a static emulator). Nothing found these by a search;
//! - each directory a search looked in, and each candidate it found absent
//!   (by the nearest existing ancestor of an absent directory).
//!
//! It also re-resolves every selected spelling, so a symlink hop that now
//! leads elsewhere is a miss. Foreign-input selection searches system
//! directories, but it runs in every process and its result is in the key,
//! so system directories need no guard. A capture is not recorded when a
//! search could not be traced, when the loader found an object outside the
//! guarded trees, when an object carries DT_RPATH or DT_RUNPATH, when the
//! guard exceeds `GUARD_ENTRIES`, when the emulator is dynamic, when a
//! guarded stamp changed between the stamps bracketing the capture or lies
//! within the two-second racy margin, or when a guarded file is on a
//! filesystem outside the admitted list.
//!
//! Scope (R4). Admitted filesystems: ext2/3/4, xfs, btrfs, tmpfs, f2fs, zfs
//! and bcachefs (not overlayfs, ramfs, NFS, FUSE or any other). A record
//! answers only under the registration epoch that issued it, on the same
//! machine id and `boot_id`, with every guarded device and the record
//! directory still showing the same (device, statx mount id, `f_fsid`) and
//! the record directory the same inode: a remount or a restored snapshot
//! misses, and an identity that cannot be read refuses or misses.
//!
//! Authentication (R5). The file is the hex MAC, under the registration's
//! key and bound to its epoch, of every byte that follows: format, key,
//! scope, spellings, guard (searched directories and absent candidates
//! included), both issue clocks, platform reference and identity. Any other
//! format has no such line and misses.
//!
//! Expiry (R6). A fixed day from the capture's start that no hit renews,
//! measured on realtime and CLOCK_BOOTTIME, which must agree within two
//! seconds; a clock earlier than the issue misses.
//!
//! Not promised: raw-disk corruption, a metadata-preserving rollback (bytes
//! restored with their full stamps, ctime included), and privileged or
//! deliberate same-user tampering with the guarded files or this cache.
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
const FORMAT: u64 = 3;
#[cfg(test)]
thread_local! {
    static RECORD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// A record older than this gets a full verification; no hit renews it.
const PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// The most a record's realtime and boottime ages may disagree.
const CLOCK_SKEW: Duration = Duration::from_secs(2);
/// Directory and file stamps in one record, across every guarded tree.
const GUARD_ENTRIES: usize = 4096;
/// statfs `f_type`s of local filesystems that advance ctime on every change
/// and report a stable identity: ext2/3/4, xfs, btrfs, tmpfs, f2fs, zfs and
/// bcachefs. overlayfs (whose lower layers change under it) and ramfs are
/// not admitted; neither are NFS, FUSE or any other.
const LOCAL_FILESYSTEMS: [i64; 7] = [
    0xef53,
    0x5846_5342,
    0x9123_683e,
    0x0102_1994,
    0xf2f5_2010,
    0x2fc1_2fc1,
    0xca45_1a4e,
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
    /// The one platform policy of every build profile: Record, unless
    /// `HIRSEL_PLATFORM_CAPTURE=cold` selects full verification (tools/land
    /// and conformance gates set it).
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
        self.select_in(store, &records(store), mode)
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
        self.recapture_in(store, &records(store), reason)
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
        self.recover_in(store, &records(store), recorded, failure)
    }

    fn recover_in(
        &self,
        store: &dyn ResultStore,
        records: &Records,
        recorded: ContentHash,
        failure: &str,
    ) -> Result<CapturedPlatform, String> {
        let fresh = self.recapture_in(store, records, failure)?;
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
        records: &Records,
        reason: &str,
    ) -> Result<CapturedPlatform, String> {
        crate::runner::count_platform_record(&format!(
            "mismatch: {reason}; record deleted and recaptured"
        ));
        if let Ok(place) = records {
            let _ = std::fs::remove_file(place.directory.join(name(&self.key()?)));
        }
        Ok(self.select_in(store, records, CaptureMode::Cold)?.platform)
    }

    fn select_in(
        &self,
        store: &dyn ResultStore,
        records: &Records,
        mode: CaptureMode,
    ) -> Result<Selected, String> {
        let name = name(&self.key()?);
        let (reason, recorded) = match mode {
            CaptureMode::Cold => ("full verification required".to_string(), None),
            CaptureMode::Record => match records
                .as_ref()
                .map_err(|reason| Miss::from(reason.clone()))
                .and_then(|place| self.lookup(store, place, &name))
            {
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
        let (platform, issued) = self.capture(store, records, &name)?;
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
        place: &RecordPlace,
        name: &str,
    ) -> Result<CapturedPlatform, Miss> {
        let directory = crate::runtime_cache::private_directory(&place.directory)
            .map_err(|error| Miss::from(format!("private cache: {error}")))?;
        #[cfg(test)]
        RECORD_READS.update(|count| count + 1);
        let file = match crate::runtime_cache::private_file(&place.directory.join(name)) {
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
        // Rule R5: the MAC under the registration's key covers every byte
        // after its line; any other format has no such line and misses here.
        let (mac, body) = text
            .split_once('\n')
            .ok_or_else(|| Miss::from("record malformed: no MAC".to_string()))?;
        if mac != hex(&place.authority.platform_record_mac(body.as_bytes())) {
            return Err("record MAC differs".to_string().into());
        }
        let record: LocalRecord = hirsel_codec::from_text(body)
            .map_err(|error| Miss::from(format!("record malformed: {}", error.message)))?;
        if record.format != FORMAT || record.key != name {
            return Err("record format or key differs".to_string().into());
        }
        if record.epoch != place.authority.epoch() {
            return Err("record from another registration".to_string().into());
        }
        if record.machine != machine().map_err(Miss::from)? {
            return Err("record from another machine".to_string().into());
        }
        // Rule R6: a record lives within one boot, for a fixed day from its
        // issue that no hit renews, measured on both clocks.
        if record.boot != place.clock.boot {
            return Err("record from another boot".to_string().into());
        }
        let real = place
            .clock
            .realtime
            .duration_since(UNIX_EPOCH + Duration::from_nanos(record.issued_realtime))
            .map_err(|_| Miss::from("realtime is before the record's issue".to_string()))?;
        let boot = place
            .clock
            .boottime
            .checked_sub(Duration::from_nanos(record.issued_boottime))
            .ok_or_else(|| Miss::from("boottime is before the record's issue".to_string()))?;
        if real.abs_diff(boot) > CLOCK_SKEW {
            return Err("realtime and boottime ages disagree".to_string().into());
        }
        if real.max(boot) >= PERIOD {
            return Err(Miss {
                reason: "record older than a day".into(),
                recorded: Some(record.identity),
            });
        }
        // Rule R4: the same mounts of the same filesystems, and this cache
        // directory itself, not a copy or a restored snapshot of it.
        let guarded = record
            .guard
            .iter()
            .map(|(path, stamp)| Ok((*stamp.first().ok_or("empty stamp")?, PathBuf::from(path))))
            .collect::<Result<Vec<_>, &str>>()
            .map_err(|error| Miss::from(error.to_string()))?;
        if record.devices != devices(&guarded, &directory).map_err(Miss::from)? {
            return Err("record from another mount, filesystem or cache"
                .to_string()
                .into());
        }
        for (spelled, canonical) in &record.spellings {
            if std::fs::canonicalize(spelled).ok().as_deref() != Some(Path::new(canonical)) {
                return Err(format!("{spelled} now resolves elsewhere").into());
            }
        }
        if let Some(path) = changed(&record.guard, &record.absent) {
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
    /// cannot be witnessed or its content is not durable. The capture's own
    /// failure is the build's.
    ///
    /// The capture's records are published before the record that names
    /// them: a capture made outside any request (a program record's lookup)
    /// would otherwise leave them pending in a handle no request finishes,
    /// and the next process would find the record but not its content.
    fn capture(
        &self,
        store: &dyn ResultStore,
        records: &Records,
        name: &str,
    ) -> Result<(CapturedPlatform, Result<(), String>), String> {
        let publication = store.local_product_store().map(|store| store.request());
        let captured = self.capture_unpublished(store, records, name);
        // A nested request publishes with its outer one; a record issued
        // under it whose content is then lost is a checked miss
        // ("store lacks the recorded platform"), never a wrong answer.
        let published = publication.map_or(Ok(()), |publication| {
            publication
                .finish()
                .map_err(|error| format!("platform content not published: {error}"))
        });
        let (platform, record) = captured?;
        let issued = record.and_then(|record| {
            published?;
            let place = records.as_ref().map_err(Clone::clone)?;
            publish(&place.directory, name, &record)
        });
        Ok((platform, issued))
    }

    fn capture_unpublished(
        &self,
        store: &dyn ResultStore,
        records: &Records,
        name: &str,
    ) -> Result<(CapturedPlatform, Result<String, String>), String> {
        let selection = Selection::resolve(self);
        let before = selection
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|selection| selection.guard(&[], &[]));
        let linker = FrozenLinker::snapshot(store, &self.linker)?;
        let (kit, kit_searches) =
            FrozenLinkKit::snapshot_traced(store, &self.kit, &self.target, linker, self.limits)?;
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
        let record = selection.and_then(|selection| {
            let place = records.as_ref().map_err(Clone::clone)?;
            selection.issue(store, kit, &kit_searches, &platform, &before?, place, name)
        });
        // Only issuance reads what the loader searched; the build keeps the
        // platform without it.
        platform.search = RuntimeSearch::default();
        Ok((platform, record))
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

/// The first guarded path whose current stamp is not the recorded one, or
/// recorded absent path that is no longer absent.
fn changed<'a>(guard: &'a [(String, Vec<u64>)], absent: &'a [String]) -> Option<&'a str> {
    guard
        .iter()
        .find_map(|(path, recorded)| {
            let current = std::fs::metadata(path).map(|meta| stamp(&meta));
            (!current.is_ok_and(|current| same(&current, recorded))).then_some(path.as_str())
        })
        .or_else(|| {
            absent
                .iter()
                .find(|path| !is_absent(Path::new(path.as_str())))
                .map(String::as_str)
        })
}

/// Absent as the linker and loader see it: following symlinks, not found.
/// Any other answer (found, or a permission error) is not absent.
fn is_absent(path: &Path) -> bool {
    std::fs::metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
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
    for (path, earlier) in &before.stamps {
        if !after
            .stamps
            .get(path)
            .is_some_and(|later| same(later, earlier))
        {
            return Err(format!("{} changed during the capture", path.display()));
        }
    }
    for (path, stamp) in &after.stamps {
        if !stable(stamp, started) {
            return Err(format!("{} changed too recently", path.display()));
        }
    }
    Ok(())
}

/// Path stamps, dirs and files alike, and the candidates a search found
/// absent.
struct Guard {
    stamps: BTreeMap<PathBuf, Stamp>,
    absent: BTreeSet<PathBuf>,
}

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
        // Discovery runs through the emulator with a cleared environment
        // (runtime/discovery.rs), so a static one is witnessed whole by its
        // stamp. A script or dynamic emulator reads files nothing here
        // names: its capture is cold.
        if let Some(emulator) = &spec.emulator {
            let emulator = canonical(emulator)?;
            if crate::object_queries::runtime::is_static_executable(&emulator) != Ok(true) {
                return Err(format!(
                    "emulator {} is not a static executable",
                    emulator.display()
                ));
            }
            files.push(emulator);
        }
        selection.trees = trees;
        selection.ancestors = ancestors;
        selection.files = files;
        Ok(selection)
    }

    /// Stamps of every guarded path, plus `extra` files read by name, and
    /// what `searches` looked for: each candidate it found (stamped), each it
    /// found absent (which must stay absent), and each searched directory
    /// (stamped, or the nearest existing ancestor of an absent one).
    fn guard(&self, extra: &[PathBuf], searches: &[&SearchTrace]) -> Result<Guard, String> {
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
        let mut absent = BTreeSet::new();
        for search in searches {
            for (path, found) in &search.queries {
                if *found {
                    add(path, &mut guard)?;
                } else if is_absent(path) {
                    absent.insert(path.clone());
                } else {
                    return Err(format!("{} appeared during the capture", path.display()));
                }
            }
            for directory in &search.searched {
                if is_absent(directory) {
                    let existing = directory
                        .ancestors()
                        .skip(1)
                        .find(|ancestor| ancestor.exists())
                        .ok_or("a searched directory has no existing ancestor")?;
                    add(existing, &mut guard)?;
                    absent.insert(directory.clone());
                } else {
                    add(directory, &mut guard)?;
                }
            }
        }
        if guard.len() + absent.len() > GUARD_ENTRIES {
            return Err(format!("guarded paths exceed {GUARD_ENTRIES}"));
        }
        Ok(Guard {
            stamps: guard,
            absent,
        })
    }

    /// The record of a cold capture whose every read is witnessed.
    #[allow(clippy::too_many_arguments)]
    fn issue(
        &self,
        store: &dyn ResultStore,
        kit: FrozenLinkKit,
        kit_searches: &[Traced],
        platform: &CapturedPlatform,
        before: &Guard,
        place: &RecordPlace,
        name: &str,
    ) -> Result<String, String> {
        let started = place.clock.realtime;
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
        // Every linker search (the kit's links and the runtime probe) must
        // have been traced; the loader's search is its candidate list.
        let mut searches = Vec::new();
        for traced in kit_searches.iter().chain(platform.search.linker.as_ref()) {
            searches.push(
                traced
                    .as_ref()
                    .map_err(|error| format!("untraceable linker search: {error}"))?,
            );
        }
        if platform.search.linker.is_none() {
            return Err("untraceable linker search: the runtime probe kept none".into());
        }
        let opened = platform.search.opened.iter().collect::<BTreeSet<_>>();
        let loader = SearchTrace {
            queries: platform
                .search
                .candidates
                .iter()
                .map(|path| (path.clone(), opened.contains(path) || !is_absent(path)))
                .collect(),
            searched: Vec::new(),
        };
        searches.push(&loader);
        let after = self.guard(&read, &searches)?;
        witnessed(before, &after, started)?;
        // The kit capture holds the bytes its linker read; they must be the
        // bytes behind the witnessed name.
        for (path, content) in &kit_files {
            let mut file = std::fs::File::open(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let witnessed = after.stamps.get(path).ok_or("unwitnessed kit input")?;
            if stamp(&file.metadata().map_err(error)?) != *witnessed
                || !file_matches_blob(&blob_record(store, *content)?, &mut file)?
            {
                return Err(format!(
                    "{} differs from what the kit capture read",
                    path.display()
                ));
            }
        }
        let directory = crate::runtime_cache::private_directory(&place.directory)
            .map_err(|error| error.to_string())?;
        let guarded = after
            .stamps
            .iter()
            .map(|(path, stamp)| (stamp[0], path.clone()))
            .collect::<Vec<_>>();
        let devices = devices(&guarded, &directory)?;
        let text = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("non-UTF8 platform path {}", path.display()))
        };
        let record = LocalRecord {
            format: FORMAT,
            key: name.to_owned(),
            epoch: place.authority.epoch(),
            machine: machine()?,
            boot: place.clock.boot.clone(),
            devices,
            issued_realtime: nanos(
                started
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| e.to_string())?,
            )?,
            issued_boottime: nanos(place.clock.boottime)?,
            spellings: self
                .spellings
                .iter()
                .map(|(spelled, canonical)| Ok((text(spelled)?, text(canonical)?)))
                .collect::<Result<_, String>>()?,
            guard: after
                .stamps
                .iter()
                .map(|(path, stamp)| Ok((text(path)?, stamp.to_vec())))
                .collect::<Result<_, String>>()?,
            absent: after
                .absent
                .iter()
                .map(|path| text(path))
                .collect::<Result<_, String>>()?,
            identity: platform.identity,
            platform: platform.store(store)?.into(),
        };
        let body = hirsel_codec::to_text(&record);
        let mac = hex(&place.authority.platform_record_mac(body.as_bytes()));
        Ok(format!("{mac}\n{body}"))
    }
}

/// This machine's ID; empty on a host without one (a build sandbox), which
/// the boot ID and the per-device identities still scope.
fn machine() -> Result<String, String> {
    match std::fs::read_to_string("/etc/machine-id") {
        Ok(machine) => Ok(machine.trim().to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("/etc/machine-id: {error}")),
    }
}

/// Rule R4's per-device scope, in device order: for each device a guarded
/// path is on, and for the record directory, (device, statx mount ID,
/// `f_fsid`), followed by the directory's own inode. A device on a
/// filesystem outside the local allowlist, or whose mount ID the kernel does
/// not report, has no identity a record could be scoped by: Err.
fn devices(guarded: &[(u64, PathBuf)], directory: &std::fs::File) -> Result<Vec<u64>, String> {
    let meta = directory.metadata().map_err(error)?;
    let fd = format!(
        "/proc/self/fd/{}",
        std::os::fd::AsRawFd::as_raw_fd(directory)
    );
    let mut paths = BTreeMap::new();
    for (device, path) in guarded {
        paths.entry(*device).or_insert_with(|| path.clone());
    }
    paths.insert(meta.dev(), PathBuf::from(fd));
    let mut words = Vec::with_capacity(paths.len() * 3 + 1);
    for (device, path) in paths {
        let name =
            std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: `name` is a NUL-terminated path and `status` is a valid,
        // writable statfs buffer for the duration of the call.
        let mut status: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(name.as_ptr(), &mut status) } != 0 {
            return Err(format!(
                "{}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        #[allow(clippy::unnecessary_cast)]
        let kind = status.f_type as i64;
        if !LOCAL_FILESYSTEMS.contains(&kind) {
            return Err(format!(
                "{} is on filesystem {kind:#x}, outside the local allowlist",
                path.display()
            ));
        }
        // SAFETY: as above, with a statvfs buffer.
        let mut volume: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(name.as_ptr(), &mut volume) } != 0 {
            return Err(format!(
                "{}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: as above, with a statx buffer; the path is absolute.
        let mut extended: libc::statx = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::statx(
                libc::AT_FDCWD,
                name.as_ptr(),
                0,
                libc::STATX_MNT_ID,
                &mut extended,
            )
        } != 0
        {
            return Err(format!(
                "{}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        if extended.stx_mask & libc::STATX_MNT_ID == 0 {
            return Err(format!(
                "{}: the kernel reports no mount ID",
                path.display()
            ));
        }
        #[allow(clippy::unnecessary_cast)]
        words.extend([device, extended.stx_mnt_id, volume.f_fsid as u64]);
    }
    words.push(meta.ino());
    Ok(words)
}

/// The record's MAC, as lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn nanos(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_nanos()).map_err(|e| e.to_string())
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

/// Where and under what a selection keeps platform records, or why it
/// keeps none (and every selection through it captures cold).
type Records = Result<RecordPlace, String>;

/// A store's platform records: `platform-v1` under its local product store's
/// root, beside the content they name, so a record never outlives, precedes
/// or crosses into a home other than its content's; MAC'd under the store's
/// registration authority (design §3's key); checked against this boot's
/// clocks.
#[derive(Clone)]
struct RecordPlace {
    directory: PathBuf,
    authority: hirsel_validate::programs::ProgramAuthority,
    clock: Clock,
}

/// The clocks a record is issued and checked against.
#[derive(Clone)]
struct Clock {
    realtime: SystemTime,
    boottime: Duration,
    /// `/proc/sys/kernel/random/boot_id`: boottime counts within this boot.
    boot: String,
}
impl Clock {
    fn now() -> Result<Self, String> {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `time` is a valid, writable timespec for the call.
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } != 0 {
            return Err(format!("boottime: {}", std::io::Error::last_os_error()));
        }
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|error| format!("boot ID: {error}"))?;
        Ok(Self {
            realtime: SystemTime::now(),
            boottime: Duration::new(time.tv_sec as u64, time.tv_nsec as u32),
            boot: boot.trim().to_owned(),
        })
    }
}

#[cfg(test)]
impl Clock {
    /// This clock `elapsed` later in the same boot.
    fn after(&self, elapsed: Duration) -> Self {
        Self {
            realtime: self.realtime + elapsed,
            boottime: self.boottime + elapsed,
            boot: self.boot.clone(),
        }
    }
}

fn records(store: &dyn ResultStore) -> Records {
    let directory = store
        .local_product_store()
        .ok_or("the store keeps no local records")?
        .root()
        .join("platform-v1");
    let authority = store
        .program_authority()
        .ok_or("the store has no registration to key records")?;
    Ok(RecordPlace {
        directory,
        authority,
        clock: Clock::now()?,
    })
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
    /// Rule R4's scope: the registration's authority epoch, the machine,
    /// the boot, and `devices`' per-device identities.
    epoch: ContentHash,
    machine: String,
    boot: String,
    devices: Vec<u64>,
    /// Rule R6: when the capture that issued it started, on both clocks.
    issued_realtime: u64,
    issued_boottime: u64,
    spellings: Vec<(String, String)>,
    guard: Vec<(String, Vec<u64>)>,
    /// Candidates a search found absent; each must still be absent.
    absent: Vec<String>,
    identity: ContentHash,
    platform: WireRef,
}
snapshot_struct!(LocalRecord {
    format,
    key,
    epoch,
    machine,
    boot,
    devices,
    issued_realtime,
    issued_boottime,
    spellings,
    guard,
    absent,
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
