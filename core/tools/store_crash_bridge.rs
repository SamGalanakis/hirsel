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
    cursor: usize,
    mode: u8,
}
struct Bridge {
    fs: FileSystem,
    files: BTreeMap<u64, File>,
    locks: BTreeMap<String, u64>,
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
    split_at: usize,
    random: u8,
    orphan_end: bool,
    orphan_end_path: Option<String>,
    checkpoint_reopen: bool,
    fork_observation: Option<ForkObservation>,
    read_only: Option<Snapshot>,
    staged: Option<StagedCommit>,
}

/// A writer COMMIT the follower law stages through stdout ("STAGE").
/// It lands when the follower's recovery pass stats the segment, the
/// second stat of that name after staging; the witness takes the first.
struct StagedCommit {
    segment: String,
    at: usize,
    frame: Vec<u8>,
    /// With an END image: write and sync both after the stat's size is
    /// taken. Without: write the frame unsynced before the stat returns.
    end: Option<Vec<u8>>,
    stats: u32,
}

impl StagedCommit {
    fn parse(raw: &[u8]) -> Self {
        let boundary = raw[0] == 1;
        let at = u64::from_le_bytes(raw[1..9].try_into().unwrap()) as usize;
        let name_len = raw[9] as usize;
        let segment = std::str::from_utf8(&raw[10..10 + name_len])
            .unwrap()
            .to_owned();
        let rest = &raw[10 + name_len..];
        let frame_len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let frame = rest[4..4 + frame_len].to_vec();
        let end = rest[4 + frame_len..].to_vec();
        assert_eq!(boundary, !end.is_empty(), "STAGE END image");
        StagedCommit {
            segment,
            at,
            frame,
            end: boundary.then_some(end),
            stats: 0,
        }
    }
}

#[derive(Default)]
struct ForkObservation {
    image: u8,
    synced: bool,
    child_returned: bool,
}

// Walk the segment's wire envelopes independently of the store reader.
fn fork_frame_start(frames: &[u8]) -> Option<usize> {
    let mut offset = 0;
    while frames.len() - offset >= 16 {
        let frames = &frames[offset..];
        let kind = u16::from_le_bytes(frames[..2].try_into().unwrap());
        let len = u32::from_le_bytes(frames[4..8].try_into().unwrap()) as usize;
        let payload = frames.get(16..16 + len)?;
        if kind == 0x0013 {
            assert_eq!(frames[2..4], [1, 0]);
            // AP19 and §13.1: id 1, parent 0, version 2, name "beta",
            // then the LE watermark. The fixture uses 2 schema stamps,
            // reserves 7 for the pack-list edit and 14 for the first put.
            assert_eq!(
                payload,
                b"\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x04\0beta\x18\0\0\0\0\0\0\0"
            );
            return Some(offset);
        }
        offset += 16 + len;
    }
    None
}

impl Bridge {
    fn exists(&self, name: &str) -> bool {
        self.fs.inode(name).is_some()
    }
    fn apply(&mut self, op: Op<'_>) -> bool {
        assert!(self.read_only.is_none(), "read-only open issued {op:?}");
        if self.phase == 1 && self.events == self.cut {
            self.crashed = true;
        }
        if self.crashed {
            return false;
        }
        if self.phase == 1 && self.trace_cuts {
            let kind = match &op {
                Op::Create(_) => "create",
                Op::Write(name, bytes)
                    if self
                        .fs
                        .read(name)
                        .is_some_and(|old| old.iter().zip(*bytes).any(|(old, new)| old != new)) =>
                {
                    "overwrite"
                }
                Op::Write(..) => "write",
                Op::Append(..) => "append",
                Op::Sync(_) => "sync",
                Op::Rename(..) => "rename",
                Op::Remove(_) => "unlink",
                Op::SyncDir => "sync-dir",
            };
            eprintln!("w2-cut {} {kind}", self.events + 1);
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
            assert!(op != 4, "read-only open tried to lock");
            assert!(op != 0 || mode == 0, "read-only open opened a writer");
        }
        // A crashed process closes descriptors without changing the filesystem.
        if op == 3 {
            if let Some(file) = self.files.remove(&handle) {
                if self.locks.get(&file.name) == Some(&handle) {
                    self.locks.remove(&file.name);
                }
            }
            return Outcome::default();
        }
        // The out-of-process witness records returned durability even when a
        // different task has just encountered the simulated filesystem cut.
        if op == 12 {
            if self.phase == 1 {
                if &data[..len] == b"DONE" {
                    self.acked = true;
                } else if let Some(raw) = data[..len].strip_prefix(b"STAGE") {
                    assert!(self.staged.is_none(), "one staged COMMIT");
                    self.staged = Some(StagedCommit::parse(raw));
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
        if self.crashed {
            return Self::error(5);
        }
        if self.phase == 1 && self.armed {
            let slot = match op {
                0 if path.ends_with(b".end") => Some(0),
                16 => self.files.get(&handle).map(|file| {
                    if file.name.ends_with(".end") {
                        1
                    } else if is_segment(&file.name) {
                        2
                    } else {
                        3
                    }
                }),
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
                if mode == 1 && !self.apply(Op::Write(&name, &[])) {
                    return Self::error(5);
                }
                let cursor = if mode == 2 {
                    self.fs.read(&name).unwrap_or(&[]).len()
                } else {
                    0
                };
                let id = self.next_file;
                self.next_file += 1;
                self.files.insert(id, File { name, cursor, mode });
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
                let bytes = self.fs.read(&file.name).unwrap_or(&[]);
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
                let due = self.staged.as_mut().is_some_and(|staged| {
                    if staged.segment == name {
                        staged.stats += 1;
                    }
                    staged.segment == name && staged.stats == 2
                });
                let mut size = self.fs.read(&name).unwrap_or(&[]).len() as u64;
                if due {
                    let staged = self.staged.as_ref().unwrap();
                    let (at, frame, end) = (staged.at, staged.frame.clone(), staged.end.clone());
                    let mut bytes = self.fs.read(&name).unwrap().to_vec();
                    assert_eq!(bytes.len(), at, "staged COMMIT starts at the recovered end");
                    bytes.extend_from_slice(&frame);
                    assert!(self.apply(Op::Write(&name, &bytes)));
                    if let Some(end) = end {
                        let end_name = format!("{name}.end");
                        assert!(self.apply(Op::Sync(&name)));
                        assert!(self.apply(Op::Write(&end_name, &end)));
                        assert!(self.apply(Op::Sync(&end_name)));
                    } else {
                        size = bytes.len() as u64;
                    }
                }
                Outcome {
                    value: size,
                    ..Outcome::default()
                }
            }
            2 | 38 => {
                let file = self.files.get(&handle).expect("live provider descriptor");
                let name = file.name.clone();
                let cursor = if op == 38 {
                    let position = u64::from_le_bytes(path.try_into().expect("WriteAt offset"));
                    usize::try_from(position).expect("WriteAt offset fits")
                } else {
                    file.cursor
                };
                if self.phase == 1 && is_segment(&name) {
                    if let Some(fork) = &self.fork_observation {
                        if fork.image == 5
                            && fork.synced
                            && data[..len].starts_with(&[17, 0])
                            && data[..len].get(16..20) == Some(&[1, 0, 0, 0])
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
                    assert!(len > 7);
                    7
                } else {
                    len
                };
                if self.phase == 1 && self.armed && self.sync_failure == 5 && is_segment(&name) {
                    self.split_at = cursor;
                }
                let append = op == 2 && file.mode == 2
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
            4 => {
                let file = self.files.get(&handle).expect("live lock descriptor");
                let free = !self.locks.contains_key(&file.name);
                if free {
                    self.locks.insert(file.name.clone(), handle);
                }
                Outcome {
                    value: u64::from(free),
                    ..Outcome::default()
                }
            }
            5 => {
                let file = self.files.get(&handle).expect("live lock descriptor");
                self.locks.remove(&file.name);
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
            16 => {
                let name = self
                    .files
                    .get(&handle)
                    .expect("live sync descriptor")
                    .name
                    .clone();
                if self.phase == 1
                    && self.armed
                    && self.sync_failure == SECOND_SEGMENT_SHORT_WRITE
                    && is_segment(&name)
                {
                    assert_eq!(self.failed_syncs, 0, "store synced after a short write");
                    self.segment_syncs += 1;
                }
                let matches = match self.sync_failure {
                    1 => name.starts_with("j-") && !name.ends_with(".end"),
                    2 => name.starts_with("j-") && name.ends_with(".end"),
                    3 => name.starts_with("p-"),
                    // The runner's poison run: one frame per group, so the
                    // second armed segment sync carries the call's effect frame.
                    SECOND_SEGMENT_SYNC_FAILURE
                        if self.phase == 1 && self.armed && is_segment(&name) && !self.crashed =>
                    {
                        self.segment_syncs += 1;
                        self.segment_syncs == 2
                    }
                    5 if self.phase == 1 && self.armed && is_segment(&name) && !self.crashed => {
                        self.split_syncs += 1;
                        self.split_syncs == 2
                    }
                    _ => false,
                };
                if self.phase == 1 && self.armed && matches {
                    self.failed_syncs += 1;
                    if self.sync_failure == 5 {
                        // Only the first seven bytes of B reached storage.
                        // A's successful sync is the only durable commit.
                        let torn = self.fs.read(&name).unwrap()[..self.split_at + 7].to_vec();
                        self.fs.issue(&Op::Write(&name, &torn));
                        self.fs.issue(&Op::Sync(&name));
                    }
                    // A retry on this live Db must never succeed.
                    assert_eq!(self.failed_syncs, 1, "store retried a failed sync");
                    return Self::error(5);
                }
                if !self.exists(&name) {
                    // journal-ack holds a sync across other tasks' turns, so a
                    // cut may remove the segment's end file first. A sync of an
                    // unlinked file reaches no live name. A restart that revives
                    // the name sees its last synced bytes, which a crash just
                    // before this sync gives anyway, so this hides no state.
                    return if self.crashed {
                        Self::error(5)
                    } else {
                        Outcome::default()
                    };
                }
                let first_crash = self.orphan_end
                    && self.phase == 1
                    && self.files[&handle].mode == 1
                    && name.starts_with("j-")
                    && name.ends_with(".end");
                let fork_start = (self.phase == 1 && is_segment(&name))
                    .then(|| self.fork_observation.as_ref().filter(|fork| !fork.synced))
                    .flatten()
                    .and_then(|_| fork_frame_start(self.fs.read(&name).unwrap()));
                if let Some(start) = fork_start {
                    let fork = self.fork_observation.as_ref().unwrap();
                    if (2..=4).contains(&fork.image) {
                        // The write was issued, but the Store's sync did not
                        // succeed. W2 may retain a complete or torn write.
                        if fork.image == 3 {
                            let torn = self.fs.read(&name).unwrap()[..start + 7].to_vec();
                            self.fs.issue(&Op::Write(&name, &torn));
                        }
                        self.fs.issue(&Op::Sync(&name));
                        if fork.image == 4 {
                            let end = format!("{name}.end");
                            self.fs.issue(&Op::Write(&end, b"bad END"));
                            self.fs.issue(&Op::Sync(&end));
                        }
                        self.crashed = true;
                        return Self::error(5);
                    }
                }
                if self.apply(Op::Sync(&name)) {
                    if fork_start.is_some() {
                        let fork = self.fork_observation.as_mut().unwrap();
                        fork.synced = true;
                        self.floor = 3;
                        if fork.image == 1 {
                            self.crashed = true;
                            return Self::error(5);
                        }
                    }
                    if first_crash {
                        // Both inode syncs have finished. Only the END name
                        // survives the first crash, before the directory sync.
                        self.fs.persist_name(&name);
                        if self.sync_failure == ORPHAN_END_SYNC_FAILURE {
                            self.orphan_end_path = Some(name);
                        }
                        self.crashed = true;
                    }
                    Outcome::default()
                } else {
                    Self::error(5)
                }
            }
            17 => {
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
        split_at: 0,
        random: 1,
        orphan_end: false,
        orphan_end_path: None,
        checkpoint_reopen: false,
        fork_observation: None,
        read_only: None,
        staged: None,
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
        if let Some(seed) = bridge.damage_seed.take() {
            assert_eq!(bridge.events, bridge.cut, "damage cut was not reached");
            bridge.fs.restart_damaged(bridge.cut, seed);
        } else if phase != 3 || !bridge.checkpoint_reopen {
            bridge.fs.restart();
        }
        bridge.locks.clear();
        bridge.crashed = false;
    }
    bridge.phase = phase;
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
        assert!(staged.stats >= 2, "the staged COMMIT was never applied");
    }
    if let Some(before) = &bridge.read_only {
        assert_eq!(*before, bridge.fs.snapshot(), "read-only directory changed");
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
/// 7 (second segment sync), 8 (O1 open barrier), or 9 (second segment short write).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_sync_failure(p: *mut c_void, kind: u8) {
    assert!(kind <= SECOND_SEGMENT_SHORT_WRITE);
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
