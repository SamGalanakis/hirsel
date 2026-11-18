//! Test-only C adapter for the existing W2 crash provider.
//! No store rules live here; the compiled Hirsel program checks recovered rows.
use std::{collections::BTreeMap, ffi::c_void, slice};

#[allow(dead_code)]
#[path = "../../crates/hirsel-core-rt/src/test_provider.rs"]
mod test_provider;
use test_provider::crash::{FileSystem, Op};

const ORPHAN_END_SYNC_FAILURE: u8 = 6;

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
    events: u64,
    crashed: bool,
    acked: bool,
    floor: u64,
    sync_failure: u8,
    armed: bool,
    failed_syncs: u64,
    split_syncs: u64,
    split_at: usize,
    random: u8,
    orphan_end: bool,
    orphan_end_path: Option<String>,
}
impl Bridge {
    fn exists(&self, name: &str) -> bool {
        self.fs.snapshot().names.contains_key(name)
    }
    fn apply(&mut self, op: Op<'_>) -> bool {
        if self.phase == 1 && self.events == self.cut {
            self.crashed = true;
        }
        if self.crashed {
            return false;
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
                } else if &data[..len] == b"ARM" {
                    self.armed = true;
                } else if let Some(version) = data[..len].strip_prefix(b"ACK ") {
                    let version: u64 = std::str::from_utf8(version)
                        .expect("ACK is ASCII")
                        .parse()
                        .expect("ACK version");
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
            1 => {
                let file = self
                    .files
                    .get_mut(&handle)
                    .expect("live provider descriptor");
                let bytes = self.fs.read(&file.name).unwrap_or(&[]);
                let count = (data.len() - len).min(bytes.len().saturating_sub(file.cursor));
                data[len..len + count].copy_from_slice(&bytes[file.cursor..file.cursor + count]);
                file.cursor += count;
                Outcome {
                    value: count as u64,
                    ..Outcome::default()
                }
            }
            2 => {
                let file = self.files.get(&handle).expect("live provider descriptor");
                let name = file.name.clone();
                let cursor = file.cursor;
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
                if self.phase == 1 && self.armed && self.sync_failure == 5 && is_segment(&name) {
                    self.split_at = cursor;
                }
                let append = file.mode == 2;
                let applied = if append {
                    self.apply(Op::Append(&name, &data[..len]))
                } else {
                    let mut bytes = self.fs.read(&name).unwrap_or(&[]).to_vec();
                    bytes.resize(bytes.len().max(cursor + len), 0);
                    bytes[cursor..cursor + len].copy_from_slice(&data[..len]);
                    self.apply(Op::Write(&name, &bytes))
                };
                if !applied {
                    return Self::error(5);
                }
                self.files.get_mut(&handle).unwrap().cursor += len;
                Outcome {
                    written: len as u64,
                    ..Outcome::default()
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
                if !self.exists(&name) {
                    return Self::error(1);
                }
                if self.apply(Op::Remove(&name)) {
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
                let matches = match self.sync_failure {
                    1 => name.starts_with("j-") && !name.ends_with(".end"),
                    2 => name.starts_with("j-") && name.ends_with(".end"),
                    3 => name.starts_with("p-"),
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
                if self.apply(Op::Sync(&name)) {
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
        events: 0,
        crashed: false,
        acked: false,
        floor: 0,
        sync_failure: 0,
        armed: false,
        failed_syncs: 0,
        split_syncs: 0,
        split_at: 0,
        random: 1,
        orphan_end: false,
        orphan_end_path: None,
    }))
    .cast()
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
        bridge.fs.restart();
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
    if op == 2 || op == 12 || op == 13 {
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
/// 3 (pack), 4 (segment write), 5 (byte split), or 6 (orphan END recovery).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_sync_failure(p: *mut c_void, kind: u8) {
    assert!(kind <= ORPHAN_END_SYNC_FAILURE);
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
