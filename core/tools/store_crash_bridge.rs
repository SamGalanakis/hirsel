//! Test-only C adapter for the existing W2 crash provider.
//! No store rules live here; the compiled Hirsel program checks recovered rows.
use std::{collections::BTreeMap, ffi::c_void, slice};

#[allow(dead_code)]
#[path = "../../crates/hirsel-core-rt/src/test_provider.rs"]
mod test_provider;
use test_provider::crash::{FileSystem, Op, Snapshot};

const ORPHAN_END_SYNC_FAILURE: u8 = 6;
/// runner_crash.c's poison-segment: fail the second armed segment sync.
const SECOND_SEGMENT_SYNC_FAILURE: u8 = 7;
const OPEN_MANIFEST_SYNC_FAILURE: u8 = 8;
const SECOND_SEGMENT_SHORT_WRITE: u8 = 9;
/// follower-poison (BL5 part 2): fail the 64th armed group write, ENOSPC.
const POISON_GROUP: u8 = 12;
#[path = "store_crash_late.rs"]
mod late;
#[path = "store_crash_reach.rs"]
mod reach;
#[path = "store_crash_sync.rs"]
mod sync;
#[path = "store_crash_window.rs"]
mod window;
use late::LateWriteSchedule;
use reach::Eio;
#[path = "store_crash_staged.rs"]
mod staged;
use staged::{ForkObservation, StagedCommit, StagedCut, fork_frame_start};
use window::WindowObservation;

#[repr(C)]
#[derive(Default)]
pub struct Error {
    tag: u8,
    code: u32,
}
#[repr(C)]
#[derive(Default)]
pub struct Outcome {
    value: u64,
    error: Error,
    written: u64,
    status: u8,
}
const _: () = assert!(size_of::<Outcome>() == 32);

struct File {
    name: String,
    inode: u64,
    cursor: usize,
    mode: u8,
}
const DURABLE: u8 = 5;
const DIRECT: u8 = 6;
const TRY_LOCK_SHARED: u8 = 172;
const ZERO_AT: u8 = 173;

fn durable(mode: u8) -> bool {
    matches!(mode, DURABLE | DIRECT)
}

struct Bridge {
    fs: FileSystem,
    files: BTreeMap<u64, File>,
    // (inode, open file description) -> exclusive
    locks: BTreeMap<(u64, u64), bool>,
    next_file: u64,
    phase: u64,
    cut: u64,
    damage_seed: Option<u64>,
    trace_cuts: bool,
    events: u64,
    crashed: bool,
    acked: bool,
    floor: u64,
    sync_failure: u8,
    armed: bool,
    failed_syncs: u64,
    split_syncs: u64,
    segment_syncs: u64,
    /// After ARM in the write phase: `.end` opens, END syncs, segment syncs
    /// and other syncs (files and directories).
    armed_io: [u64; 4],
    /// Direct segment group writes after ARM in the write phase.
    armed_groups: u64,
    reader_syncs: [u64; 2],
    reader_segment_reads: u64,
    late: Option<LateWriteSchedule>,
    /// Each direct segment's frame end, after its last completed write.
    ends: BTreeMap<u64, usize>,
    split_at: usize,
    random: u8,
    orphan_end: bool,
    orphan_end_path: Option<String>,
    checkpoint_reopen: bool,
    fork_observation: Option<ForkObservation>,
    read_only: Option<Snapshot>,
    staged: Option<StagedCommit>,
    staged_cut: Option<StagedCut>,
    window: Option<WindowObservation>,
    /// CLOUD #3: the cut's intercepted step, `<k+1> <kind> <role> <prefix>`,
    /// where prefix hashes the k completed write-phase steps.
    receipt: Option<String>,
    prefix: u64,
    /// SQLITE #3: eligible host calls in this phase, counted before effects,
    /// beside the mutation ordinal `events`.
    calls: u64,
    eio: Option<Eio>,
}

impl Bridge {
    fn exists(&self, name: &str) -> bool {
        self.fs.inode(name).is_some()
    }
    fn apply(&mut self, op: Op<'_>) -> bool {
        assert!(
            self.read_only.is_none()
                || matches!(op, Op::Sync(name) if is_segment(name))
                || matches!(op, Op::SyncDir),
            "read-only open issued {op:?}"
        );
        if !self.reach(&op) {
            return false;
        }
        if self.phase == 1 && self.trace_cuts {
            // BL2: where a Direct write first changes the durable image. A
            // group over a tail block starts past a block boundary (72 for a
            // fresh segment's first group, behind its creation HEADER).
            if let Op::Write(name, bytes) = &op
                && self.fs.is_direct(name)
                && let Some(inode) = self.fs.inode(name)
            {
                let durable = self.fs.durable_inode(inode);
                let first = (0..bytes.len()).find(|&at| durable.get(at) != Some(&bytes[at]));
                if let Some(first) = first {
                    eprintln!("w2-direct {} {name} {first}", self.events + 1);
                }
            }
        }
        self.fs.issue(&op);
        if self.phase == 1 {
            self.events += 1;
        }
        true
    }
    fn error(tag: u8) -> Outcome {
        Outcome {
            error: Error {
                tag,
                code: if tag == 5 { 125 } else { 0 },
            },
            ..Outcome::default()
        }
    }
    fn issue(
        &mut self,
        op: u8,
        mode: u8,
        handle: u64,
        path: &[u8],
        data: &mut [u8],
        len: usize,
    ) -> Outcome {
        if self.read_only.is_some() {
            assert!(op != 4, "read-only open tried to lock exclusively");
            assert!(op != 0 || mode == 0, "read-only open opened a writer");
        }
        // A crashed process closes descriptors without changing the filesystem.
        if op == 3 {
            let failure = if self.crashed {
                None
            } else {
                self.eligible(op, handle, path)
            };
            if let Some(file) = self.files.remove(&handle) {
                if !self.late_write_holds(handle) {
                    self.locks.remove(&(file.inode, handle));
                }
            }
            // Even a failed close consumes the descriptor.
            return failure.unwrap_or_default();
        }
        // The out-of-process witness records returned durability even when a
        // different task has just encountered the simulated filesystem cut.
        if op == 12 {
            // BL10, BL9 (iii): reader segment syncs and directory syncs since
            // the last SYNCS, in any phase; with counts, they must match.
            if let Some(expected) = data[..len].strip_prefix(b"SYNCS") {
                if let Some(counts) = expected.strip_prefix(b" ") {
                    let counts: Vec<u64> = std::str::from_utf8(counts)
                        .expect("SYNCS is ASCII")
                        .split(' ')
                        .map(|count| count.parse().expect("SYNCS count"))
                        .collect();
                    assert_eq!(
                        self.reader_syncs[..],
                        counts[..],
                        "reader segment and directory syncs"
                    );
                }
                self.reader_syncs = [0; 2];
                return Outcome {
                    written: len as u64,
                    ..Outcome::default()
                };
            }
            // The late-write cases' new owner acknowledges its own commits.
            if self.phase == 1 || (self.phase == 3 && self.late.is_some()) {
                if &data[..len] == b"DONE" {
                    self.acked = true;
                } else if let Some(raw) = data[..len].strip_prefix(b"STAGE") {
                    if raw[0] == 3 {
                        assert!(self.staged_cut.is_none(), "one staged cut");
                        self.staged_cut = Some(StagedCut::parse(&raw[1..]));
                    } else {
                        assert!(self.staged.is_none(), "one staged COMMIT");
                        self.staged = Some(StagedCommit::parse(raw));
                    }
                } else if &data[..len] == b"ARM" {
                    self.armed = true;
                } else if &data[..len] == b"CHILD RETURNED" {
                    if let Some(fork) = &mut self.fork_observation {
                        fork.child_returned = true;
                    }
                } else if let Some(version) = data[..len].strip_prefix(b"ACK ") {
                    let version: u64 = std::str::from_utf8(version)
                        .expect("ACK is ASCII")
                        .parse()
                        .expect("ACK version");
                    if let Some(fork) = &self.fork_observation {
                        if version == 3 {
                            assert!(fork.synced, "fork returned before its segment sync");
                        }
                    }
                    assert!(
                        version >= self.floor,
                        "durability acknowledgments regressed"
                    );
                    self.floor = version;
                }
            }
            return Outcome {
                written: len as u64,
                ..Outcome::default()
            };
        }
        if op == 13 {
            eprint!("{}", String::from_utf8_lossy(&data[..len]));
            return Outcome {
                written: len as u64,
                ..Outcome::default()
            };
        }
        if self.phase == 3 {
            self.advance_late_write();
        }
        // A late case's power cut ends the filesystem, not the entropy source:
        // the owner's next file operation is the one that fails.
        if self.crashed && !(op == 18 && self.late.is_some()) {
            return Self::error(5);
        }
        if let Some(outcome) = self.eligible(op, handle, path) {
            return outcome;
        }
        if self.phase == 1 && self.armed {
            let slot = match op {
                0 if path.ends_with(b".end") => Some(0),
                17 => Some(3),
                _ => None,
            };
            if let Some(slot) = slot {
                self.armed_io[slot] += 1;
            }
        }
        let name = || {
            std::str::from_utf8(path)
                .expect("Hirsel String path")
                .to_owned()
        };
        match op {
            0 => {
                let name = name();
                let exists = self.exists(&name);
                if !exists && mode == 0 {
                    return Self::error(1);
                }
                if exists && mode == 3 {
                    return Self::error(3);
                }
                if !exists && !self.apply(Op::Create(&name)) {
                    return Self::error(5);
                }
                if (mode == 1 || durable(mode)) && !self.apply(Op::Write(&name, &[])) {
                    return Self::error(5);
                }
                let cursor = if mode == 2 {
                    self.fs.read(&name).unwrap_or(&[]).len()
                } else {
                    0
                };
                let id = self.next_file;
                self.next_file += 1;
                let inode = self.fs.inode(&name).unwrap();
                if mode == DIRECT {
                    self.fs.mark_direct(inode);
                }
                self.files.insert(
                    id,
                    File {
                        name,
                        inode,
                        cursor,
                        mode,
                    },
                );
                Outcome {
                    value: id,
                    ..Outcome::default()
                }
            }
            1 | 37 => {
                let file = self
                    .files
                    .get_mut(&handle)
                    .expect("live provider descriptor");
                self.reader_segment_reads += u64::from(is_segment(&file.name));
                let bytes = self.fs.read_inode(file.inode).unwrap_or(&[]);
                let offset = if op == 37 {
                    let position = u64::from_le_bytes(path.try_into().expect("ReadAt offset"));
                    usize::try_from(position).unwrap_or(usize::MAX)
                } else {
                    file.cursor
                };
                let remaining = bytes.get(offset..).unwrap_or(&[]);
                let count = (data.len() - len).min(remaining.len());
                data[len..len + count].copy_from_slice(&remaining[..count]);
                if op == 1 {
                    file.cursor += count;
                }
                let name = file.name.clone();
                self.land_cut(&name);
                Outcome {
                    value: count as u64,
                    ..Outcome::default()
                }
            }
            47 => {
                let name = self
                    .files
                    .get(&handle)
                    .expect("live stat descriptor")
                    .name
                    .clone();
                let size = self.fs.read(&name).unwrap_or(&[]).len() as u64;
                Outcome {
                    value: size,
                    ..Outcome::default()
                }
            }
            2 | 38 => {
                let file = self.files.get(&handle).expect("live provider descriptor");
                let mode = file.mode;
                let inode = file.inode;
                let name = file.name.clone();
                let cursor = if op == 38 {
                    let position = u64::from_le_bytes(path.try_into().expect("WriteAt offset"));
                    usize::try_from(position).expect("WriteAt offset fits")
                } else {
                    file.cursor
                };
                if mode == DIRECT {
                    assert_eq!(
                        self.locks.get(&(inode, handle)),
                        Some(&true),
                        "direct write without its exclusive fence"
                    );
                    assert_eq!((cursor | len) % 4096, 0);
                }
                // A direct segment write rewrites the block holding the frame
                // end; its new frames start at that end.
                let start = match self.ends.get(&inode) {
                    Some(&end) if mode == DIRECT && (cursor..cursor + len).contains(&end) => end,
                    _ => cursor,
                };
                if self.phase == 1
                    && self.armed
                    && mode == DIRECT
                    && is_segment(&name)
                    && self.hold_late_write(inode, handle, cursor, &data[..len])
                {
                    return Self::error(5);
                }
                if self.phase == 1 && self.armed && mode == DIRECT && is_segment(&name) {
                    self.armed_groups += 1;
                    if self.sync_failure == POISON_GROUP && self.armed_groups == 64 {
                        self.failed_syncs += 1;
                        return Outcome {
                            error: Error { tag: 5, code: 28 },
                            ..Outcome::default()
                        };
                    }
                }
                if self.phase == 1 && is_segment(&name) {
                    let image = self.fs.read_inode(inode).unwrap().to_vec();
                    if let Some(outcome) = self.window_write(&image, cursor, &data[..len]) {
                        return outcome;
                    }
                }
                if self.phase == 1 && is_segment(&name) {
                    if let Some(fork) = &self.fork_observation {
                        if fork.image == 5
                            && fork.synced
                            && data[start - cursor..len].starts_with(&[17, 0])
                            && data[start - cursor..len].get(16..20) == Some(&[1, 0, 0, 0])
                        {
                            assert!(
                                fork.child_returned,
                                "child run did not return before its write"
                            );
                            self.crashed = true;
                            return Self::error(5);
                        }
                    }
                }
                if self.phase == 1
                    && self.armed
                    && self.sync_failure == 4
                    && name.starts_with("j-")
                    && !name.ends_with(".end")
                {
                    self.failed_syncs += 1;
                    assert_eq!(self.failed_syncs, 1, "store retried a failed write");
                    return Self::error(5);
                }
                if self.orphan_end
                    && self.phase == 3
                    && self.sync_failure != ORPHAN_END_SYNC_FAILURE
                    && is_segment(&name)
                {
                    // The second crash keeps the reused segment name and
                    // only the beginning of its new header.
                    self.fs.issue(&Op::Write(&name, &data[..24]));
                    self.fs.issue(&Op::Sync(&name));
                    self.fs.persist_name(&name);
                    self.crashed = true;
                    return Self::error(5);
                }
                let short = self.phase == 1
                    && self.armed
                    && self.sync_failure == SECOND_SEGMENT_SHORT_WRITE
                    && is_segment(&name)
                    && self.segment_syncs == 1;
                let written = if short {
                    self.failed_syncs += 1;
                    assert_eq!(self.failed_syncs, 1, "store retried a short write");
                    // Direct: the ring loop's Other(5) after a lone sector
                    // completion; Durable: a byte-granular short write.
                    let prefix = if mode == DIRECT { 512 } else { 7 };
                    assert!(len > prefix);
                    prefix
                } else {
                    len
                };
                if self.phase == 1 && self.armed && self.sync_failure == 5 && is_segment(&name) {
                    self.split_at = start;
                }
                let append = op == 2 && mode == 2
                    || cursor != 0
                        && self
                            .fs
                            .read(&name)
                            .is_some_and(|bytes| cursor == bytes.len());
                let applied = if append {
                    self.apply(Op::Append(&name, &data[..written]))
                } else {
                    let mut bytes = self.fs.read(&name).unwrap_or(&[]).to_vec();
                    bytes.resize(bytes.len().max(cursor + written), 0);
                    bytes[cursor..cursor + written].copy_from_slice(&data[..written]);
                    self.apply(Op::Write(&name, &bytes))
                };
                if !applied {
                    return Self::error(5);
                }
                if durable(mode) && !short {
                    if name.starts_with("j-") {
                        // An O_DSYNC journal write is its write and its sync.
                        let synced = self.sync_file(handle);
                        if synced.error.tag != 0 {
                            return synced;
                        }
                        let end = window::frames_end(&data[..len], start - cursor);
                        self.ends.insert(inode, cursor + end);
                    } else {
                        // A successful O_DSYNC write persists its bytes at completion.
                        self.count_sync(&name);
                        self.fs.sync_inode(inode);
                    }
                }
                if op == 2 {
                    self.files.get_mut(&handle).unwrap().cursor += written;
                }
                Outcome {
                    written: written as u64,
                    ..if short {
                        Self::error(5)
                    } else {
                        Outcome::default()
                    }
                }
            }
            4 | TRY_LOCK_SHARED => {
                let name = self.files[&handle].name.clone();
                self.land_staged(&name);
                let file = self.files.get(&handle).expect("live lock descriptor");
                let exclusive = op == 4;
                let key = (file.inode, handle);
                let free = !self.locks.iter().any(|(&other, &held_exclusive)| {
                    other.0 == key.0 && other != key && (exclusive || held_exclusive)
                });
                if free {
                    self.locks.insert(key, exclusive);
                }
                Outcome {
                    value: u64::from(free),
                    ..Outcome::default()
                }
            }
            5 => {
                let file = self.files.get(&handle).expect("live lock descriptor");
                self.locks.remove(&(file.inode, handle));
                Outcome::default()
            }
            ZERO_AT => {
                let file = &self.files[&handle];
                let inode = file.inode;
                let mode = file.mode;
                let name = file.name.clone();
                let at = u64::from_le_bytes(path[..8].try_into().unwrap()) as usize;
                let count = u64::from_le_bytes(path[8..].try_into().unwrap()) as usize;
                if mode == DIRECT {
                    assert_eq!(
                        self.locks.get(&(inode, handle)),
                        Some(&true),
                        "direct zero without its exclusive fence"
                    );
                    assert_eq!((at | count) % 4096, 0);
                }
                if self.phase == 1
                    && is_segment(&name)
                    && let Some(outcome) = self.window_zero(at, count)
                {
                    return outcome;
                }
                let mut bytes = self.fs.read_inode(inode).unwrap().to_vec();
                bytes.resize(bytes.len().max(at + count), 0);
                bytes[at..at + count].fill(0);
                if !self.apply(Op::Write(&name, &bytes)) {
                    return Self::error(5);
                }
                if mode == DIRECT && is_segment(&name) {
                    return self.sync_file(handle);
                }
                if durable(mode) {
                    self.count_sync(&name);
                    self.fs.sync_inode(inode);
                }
                Outcome::default()
            }
            8 => {
                let name = name();
                if self.checkpoint_reopen && self.phase == 3 {
                    assert_ne!(
                        self.sync_failure, OPEN_MANIFEST_SYNC_FAILURE,
                        "open tried to remove a file before its failed directory sync"
                    );
                }
                if !self.exists(&name) {
                    return Self::error(1);
                }
                if self.apply(Op::Remove(&name)) {
                    if self.checkpoint_reopen && self.phase == 3 && name.starts_with("p-") {
                        self.fs.persist_removal(&name);
                        self.crashed = true;
                    }
                    if self.phase == 3
                        && self.sync_failure == ORPHAN_END_SYNC_FAILURE
                        && self.failed_syncs == 0
                        && self.orphan_end_path.as_deref() == Some(name.as_str())
                    {
                        self.armed = true;
                    }
                    Outcome::default()
                } else {
                    Self::error(5)
                }
            }
            10 => Outcome {
                value: self.phase,
                ..Outcome::default()
            },
            11 => Outcome {
                value: self.events,
                ..Outcome::default()
            },
            14 => {
                let split = path.iter().position(|&b| b == 0).expect("rename delimiter");
                let from = std::str::from_utf8(&path[..split]).unwrap();
                let to = std::str::from_utf8(&path[split + 1..]).unwrap();
                if !self.exists(from) {
                    return Self::error(1);
                }
                if self.apply(Op::Rename(from, to)) {
                    if self.checkpoint_reopen && self.phase == 1 && to == "MANIFEST" {
                        self.crashed = true;
                    }
                    Outcome::default()
                } else {
                    Self::error(5)
                }
            }
            16 => self.sync_file(handle),
            17 => {
                self.reader_syncs[1] += 1;
                if self.checkpoint_reopen
                    && self.phase == 3
                    && self.sync_failure == OPEN_MANIFEST_SYNC_FAILURE
                {
                    self.failed_syncs += 1;
                    return Self::error(5);
                }
                if self.phase == 3 && self.sync_failure == ORPHAN_END_SYNC_FAILURE && self.armed {
                    self.armed = false;
                    self.failed_syncs += 1;
                    return Self::error(5);
                }
                if self.apply(Op::SyncDir) {
                    Outcome::default()
                } else {
                    Self::error(5)
                }
            }
            124 => Outcome {
                value: 1,
                ..Outcome::default()
            },
            18 => {
                for byte in &mut data[len..] {
                    *byte = self.random;
                    self.random = self.random.wrapping_add(1);
                }
                Outcome {
                    written: (data.len() - len) as u64,
                    ..Outcome::default()
                }
            }
            _ => panic!("unexpected provider operation {op}"),
        }
    }
}

fn is_segment(name: &str) -> bool {
    name.starts_with("j-") && !name.ends_with(".end")
}

#[unsafe(no_mangle)]
pub extern "C" fn store_crash_new(cut: u64) -> *mut c_void {
    Box::into_raw(Box::new(Bridge {
        fs: FileSystem::default(),
        files: BTreeMap::new(),
        locks: BTreeMap::new(),
        next_file: 1,
        phase: 0,
        cut,
        damage_seed: None,
        trace_cuts: cut == u64::MAX && std::env::var_os("HIRSEL_W2_TRACE_CUTS").is_some(),
        events: 0,
        crashed: false,
        acked: false,
        floor: 0,
        sync_failure: 0,
        armed: false,
        failed_syncs: 0,
        split_syncs: 0,
        segment_syncs: 0,
        armed_io: [0; 4],
        armed_groups: 0,
        reader_syncs: [0; 2],
        reader_segment_reads: 0,
        late: None,
        ends: BTreeMap::new(),
        split_at: 0,
        random: 1,
        orphan_end: false,
        orphan_end_path: None,
        checkpoint_reopen: false,
        fork_observation: None,
        read_only: None,
        staged: None,
        staged_cut: None,
        window: None,
        receipt: None,
        prefix: 0xcbf29ce484222325,
        calls: 0,
        eio: None,
    }))
    .cast()
}

/// # Safety
/// `p` is the live pointer from `store_crash_new` before any provider calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_damage_seed(p: *mut c_void, seed: u64) {
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    assert!(bridge.cut != u64::MAX);
    bridge.damage_seed = Some(seed);
}

/// # Safety
/// `p` is the live, exclusively accessed pointer from `store_crash_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_manifest_inode(p: *mut c_void) -> u64 {
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    bridge
        .fs
        .inode("MANIFEST")
        .expect("read-only fixture has a MANIFEST")
}

/// # Safety
/// `p` is the live pointer from `store_crash_new` before any provider calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_journal_fork(p: *mut c_void, image: u8) {
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    bridge.fork_observation = Some(ForkObservation {
        image,
        ..ForkObservation::default()
    });
}

/// Whether a request syncs its file: a sync, or a write to a Durable or
/// Direct descriptor, which carries its sync (§3.3b).
///
/// # Safety
/// `p` is the live pointer from store_crash_new, read between callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_syncs(p: *const c_void, op: u8, handle: u64) -> u8 {
    let bridge = unsafe { &*p.cast::<Bridge>() };
    let file = bridge.files.get(&handle);
    u8::from(op == 16 || (matches!(op, 2 | 38) && file.is_some_and(|file| durable(file.mode))))
}

/// # Safety
/// `p` is the live pointer from store_crash_new, exclusively accessed by the
/// serialized Ready provider callbacks. No callback runs during phase changes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_phase(p: *mut c_void, phase: u64) {
    // SAFETY: the C run wrapper owns p through its three synchronous runs.
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    assert!(bridge.files.is_empty(), "run leaked provider file handles");
    if phase >= 2 {
        // The first crash in L25(b) is process-only. The next opener sees
        // the pending rename and must make it durable before any removal.
        if phase == 3 && bridge.late.is_some() {
            assert!(bridge.crashed);
            assert!(bridge.late.as_ref().unwrap().pending.is_some());
        } else if let Some(seed) = bridge.damage_seed.take() {
            assert_eq!(bridge.events, bridge.cut, "damage cut was not reached");
            for (image, candidates) in bridge.fs.restart_damaged(bridge.cut, seed) {
                eprintln!("w2-image {image:?}");
                if let Some(count) = candidates {
                    eprintln!("w2-candidates {count}");
                }
            }
        } else if phase != 3 || !bridge.checkpoint_reopen {
            bridge.fs.restart();
        }
        if phase != 3 || bridge.late.is_none() {
            bridge.locks.clear();
        }
        // The read phase starts after a power cut: a group write still in
        // flight is lost with the page cache.
        if phase == 2 {
            if let Some(late) = &mut bridge.late {
                late.pending = None;
            }
        }
        bridge.crashed = false;
    }
    bridge.phase = phase;
    bridge.calls = 0;
}

/// # Safety
/// `p` meets store_crash_phase's lifetime and serialization contract. Path and
/// data point to the runtime's disjoint request buffers for this callback.
/// Their lengths fit those allocations. A null pointer is permitted at zero
/// length. Nothing borrowed is retained after this function returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_op(
    p: *mut c_void,
    op: u8,
    mode: u8,
    handle: u64,
    path: *const u8,
    path_len: usize,
    data: *mut u8,
    len: usize,
    capacity: usize,
) -> Outcome {
    // SAFETY: pointers and exclusive callback access are guaranteed above.
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    let path = if path_len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(path, path_len) }
    };
    // Capacity zero can still borrow static input bytes, as specified by the
    // host ABI. The provider only writes the spare space of Read/Random.
    if op == 2 || op == 38 || op == 12 || op == 13 {
        let source = if len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(data, len) }
        };
        let mut owned = source.to_vec();
        bridge.issue(op, mode, handle, path, &mut owned, len)
    } else {
        let bytes = if capacity == 0 {
            &mut []
        } else {
            unsafe { slice::from_raw_parts_mut(data, capacity) }
        };
        bridge.issue(op, mode, handle, path, bytes, len)
    }
}

/// # Safety
/// `p` is live and exclusively owned by the wrapper before the first run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_orphan_end(p: *mut c_void) {
    // SAFETY: the wrapper configures the bridge before starting any callbacks.
    unsafe { (*p.cast::<Bridge>()).orphan_end = true };
}

/// # Safety
/// `p` is live, and no provider callback is active.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_events(p: *mut c_void) -> u64 {
    // SAFETY: the wrapper reads between synchronous runs.
    unsafe { (*p.cast::<Bridge>()).events }
}

/// # Safety
/// `p` is live, and no provider callback is active.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_acked(p: *mut c_void) -> u8 {
    // SAFETY: the wrapper reads between synchronous runs.
    u8::from(unsafe { (*p.cast::<Bridge>()).acked })
}

/// # Safety
/// `p` is live, and no provider callback is active.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_expect_read_only(p: *mut c_void) {
    // SAFETY: the C wrapper calls this once, after the simulated restart.
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    bridge.read_only = Some(bridge.fs.snapshot());
    bridge.reader_syncs = [0; 2];
    bridge.reader_segment_reads = 0;
}

/// # Safety
/// `p` was returned by store_crash_new and has not been freed. All runs and
/// callbacks have ended, and no pointer into the bridge was exposed to C.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_free(p: *mut c_void) {
    // SAFETY: ownership returns to Rust exactly once, after callback quiescence.
    let bridge = unsafe { Box::from_raw(p.cast::<Bridge>()) };
    assert!(
        bridge.files.is_empty(),
        "final run leaked provider file handles"
    );
    if let Some(staged) = &bridge.staged {
        assert_eq!(
            staged.takes_until_commit, 0,
            "the staged COMMIT was never applied"
        );
    }
    if let Some(cut) = &bridge.staged_cut {
        assert!(cut.landed, "the staged cut was never applied");
    }
    if let Some(before) = &bridge.read_only {
        assert_eq!(*before, bridge.fs.snapshot(), "read-only directory changed");
        // BL13: a free fence means the writer's writes completed, not that
        // they are durable. The reader syncs the terminal segment and the
        // directory before it trusts what it read.
        if bridge.trace_cuts {
            eprintln!(
                "w2-reader reads {} syncs {:?}",
                bridge.reader_segment_reads, bridge.reader_syncs
            );
        }
        assert!(
            bridge.reader_segment_reads == 0 || bridge.reader_syncs.iter().all(|&count| count > 0),
            "read-only capture read the journal without its segment or directory sync: {:?}",
            bridge.reader_syncs
        );
    }
    drop(bridge);
}

/// # Safety
/// `p` is the live bridge; only the C wrapper accesses it between serialized runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_floor(p: *mut c_void) -> u64 {
    // SAFETY: the wrapper owns p and no provider callback is active here.
    unsafe { (*p.cast::<Bridge>()).floor }
}

/// # Safety
/// `p` is live and quiescent. `kind` is 0 (off), 1 (segment), 2 (END),
/// 3 (pack), 4 (segment write), 5 (byte split), 6 (orphan END recovery), or
/// 7 (second segment sync), 8 (O1 open barrier), 9 (second segment short write),
/// 10 (window chunk), 11 (window sync), or 12 (the 64th group, ENOSPC).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_sync_failure(p: *mut c_void, kind: u8) {
    assert!(kind <= POISON_GROUP);
    // SAFETY: the C wrapper configures the bridge before starting a run.
    unsafe { (*p.cast::<Bridge>()).sync_failure = kind };
}

/// # Safety
/// `p` is live and quiescent, as for store_crash_floor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_failed_syncs(p: *mut c_void) -> u64 {
    // SAFETY: called only between the wrapper's synchronous runs.
    unsafe { (*p.cast::<Bridge>()).failed_syncs }
}

/// # Safety
/// `p` is a live bridge, exclusively owned by the serialized parent callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_crashed(p: *mut c_void) -> u8 {
    // SAFETY: the parent calls this between provider operations.
    u8::from(unsafe { (*p.cast::<Bridge>()).crashed })
}

/// # Safety
/// `p` is live and exclusive. Every process using its descriptors has exited,
/// and all provider callbacks and RPCs have ended.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_process_exit(p: *mut c_void) {
    // SAFETY: only the parent owns the bridge after both children are reaped.
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    assert!(bridge.crashed);
    bridge.files.clear();
    bridge.locks.clear();
}

/// Report a crash observed by the out-of-process signal witness.
/// # Safety
/// As for `store_crash_process_exit`. The child was reaped after a signal.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_signal_exit(p: *mut c_void) {
    // SAFETY: the wrapper reaped the child and owns the bridge.
    unsafe {
        (*p.cast::<Bridge>()).crashed = true;
        store_crash_process_exit(p);
    }
}

/// # Safety
/// `p` is live and exclusive before the first run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_checkpoint_reopen(p: *mut c_void) {
    // SAFETY: the wrapper owns p exclusively before invoking any provider.
    unsafe { (*p.cast::<Bridge>()).checkpoint_reopen = true };
}

/// # Safety
/// `p` is live and quiescent; `out` points to four writable u64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_armed_io(p: *const c_void, out: *mut u64) {
    // SAFETY: the wrapper reads between synchronous runs into its own array.
    let io = unsafe { (*p.cast::<Bridge>()).armed_io };
    unsafe { std::ptr::copy_nonoverlapping(io.as_ptr(), out, 4) };
}
