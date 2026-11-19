//! R4's and R5's target twin (STORE-SPEC v2.13 §2.1–§2.4, §3.1–§3.3, §6 Q3): the memory store's
//! per-commit work (`store_tree.rs`), plus the journal the disk store must run.
//!
//! The writer, per commit (§3.1): the body's put through `run`; the COMMIT frame encoded with the
//! store's exact bytes (kind 0x0011, flags MUST, the xxh3-64 check over the 33-byte binding
//! `identity ‖ role 2 ‖ seq ‖ offset`, head[0..8] and the payload; ops sorted, one per key); the
//! resident check counting the frames sent and not yet acked; publish; then `send(Frame)` on the
//! journal channel of capacity 2 × `group_commits` (128), which waits only when the queue is full.
//! `durable(ticket)` receives acks until the acked sequence reaches it. Past `checkpoint_bytes` of
//! segment (64 MiB) the writer rotates: a new segment with its HEADER (`prev_end` = the old end),
//! sync, its END file at d = 72, sync, `sync_dir`, then `Rotate` on the channel. The checkpoint the
//! store starts at the same point (pack and manifest, R6's work) is not in this twin.
//!
//! The journal thread, per group (§3.3): one `recv`, then `try_recv` while the group is under 64
//! frames and 4 MiB (a frame that would pass 4 MiB waits for the next group); one `write` of the
//! group; `fdatasync`; the ack. Then END when the queue was found empty (idle), after every 64th
//! group, on rotate and on close: the END file opened with truncation, its 96 bytes (the segment's
//! HEADER, cached per segment, and END(d) with its check) written, `fdatasync`. The ack always comes
//! before the END.
//!
//! Workload (store-build-1010, 2026-11-16): one table "accounts" in a fresh directory; commit c
//! (from 0) puts Account {id: c % 16, balance: c, owner "owner000"} at key c % 16 and returns c, so
//! every COMMIT is 93 bytes (§2.4). Before READY: 64 commits, each made durable (the history).
//! - `m4-store-journal R4 <gate> <dir>`: 10,000 commits, each `run` then `durable`. Prints
//!   `DONE 50635000 <ns>` (the sum of c over 64..10064).
//! - `m4-store-journal R5 <gate> <dir>`: commits back to back for 10 s, a `durable` after every 64th.
//!   The clock is read after each `durable`. Prints `DONE <commits> <ns>`.
//!
//! After the gate (both): close (the sender dropped; the journal thread writes the last END and
//! ends), then `IO writes <n> syncs <n> ends <n> groups <n> frames <n> rotates <n>` (the journal
//! thread's segment writes, its syncs including END's, END writes, groups and frames; the writer's
//! rotations). Then every segment is read back and replayed: each HEADER's bytes, each frame's
//! check, the version chain from 1, each segment's length equal to its frames' 93 bytes plus 72,
//! its END's d equal to that length, and the replayed rows equal to the tree's and the workload's.
//! At start, the frame code is checked against store-build-1010's libxxhash 0.8.2 vectors.
#[path = "store_tree.rs"]
#[allow(
    dead_code,
    reason = "shared with store_pcopy.rs; each binary uses its own subset"
)]
mod tree;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tree::{Account, Db, Op, Table, put, run};
use xxhash_rust::xxh3::Xxh3;

const GROUP_COMMITS: usize = 64;
const GROUP_BYTES: usize = 4 * 1024 * 1024;
const CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;

fn le(out: &mut Vec<u8>, x: u64, n: usize) {
    out.extend_from_slice(&x.to_le_bytes()[..n]);
}
fn binding(id: &[u8; 16], role: u8, seq: u64, offset: u64) -> [u8; 33] {
    let mut b = [0u8; 33];
    b[..16].copy_from_slice(id);
    b[16] = role;
    b[17..25].copy_from_slice(&seq.to_le_bytes());
    b[25..].copy_from_slice(&offset.to_le_bytes());
    b
}
/// §2.1: the head and payload in `frame` (the check's 8 bytes zero) get their check.
fn seal(frame: &mut [u8], id: &[u8; 16], role: u8, seq: u64, offset: u64) {
    let mut h = Xxh3::new();
    h.update(&binding(id, role, seq, offset));
    h.update(&frame[..8]);
    h.update(&frame[16..]);
    frame[8..16].copy_from_slice(&h.digest().to_le_bytes());
}
fn head(out: &mut Vec<u8>, kind: u16, len: usize) {
    le(out, u64::from(kind), 2);
    le(out, 1, 2);
    le(out, len as u64, 4);
    le(out, 0, 8);
}
/// §2.3: a file's HEADER frame (72 bytes).
fn header(id: &[u8; 16], role: u8, seq: u64, prev_end: u64) -> Vec<u8> {
    let mut f = Vec::with_capacity(72);
    head(&mut f, 0x0001, 56);
    f.extend_from_slice(b"HIRSTORE");
    le(&mut f, 2, 4);
    f.push(role);
    f.extend_from_slice(&[0, 0, 0]);
    f.extend_from_slice(id);
    le(&mut f, 0, 8);
    le(&mut f, seq, 8);
    le(&mut f, prev_end, 8);
    seal(&mut f, id, role, seq, 0);
    f
}
/// §2.4: an END file's bytes: its HEADER, then END(d) at offset 72.
fn end_file(header: &[u8], id: &[u8; 16], seq: u64, d: u64, out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(header);
    head(out, 0x0012, 8);
    le(out, d, 8);
    seal(&mut out[72..], id, 4, seq, 72);
}
/// §2.4: one COMMIT frame on the main branch, one data section, ops in key order.
fn commit(
    id: &[u8; 16],
    seq: u64,
    offset: u64,
    version: u64,
    at: u64,
    ops: &[Op],
    bytes: usize,
) -> Vec<u8> {
    let mut f = Vec::with_capacity(bytes);
    head(&mut f, 0x0011, bytes - 16);
    le(&mut f, 0, 4);
    le(&mut f, version, 8);
    le(&mut f, at, 8);
    f.push(1);
    f.push(0);
    le(&mut f, ops.len() as u64, 4);
    for op in ops {
        let key = op.key();
        match op {
            Op::Put(r) => {
                f.push(1);
                le(&mut f, key.len() as u64, 4);
                f.extend_from_slice(key);
                le(&mut f, r.value.len() as u64, 4);
                f.extend_from_slice(&r.value);
            }
            Op::Delete(_) => {
                f.push(2);
                le(&mut f, key.len() as u64, 4);
                f.extend_from_slice(key);
            }
        }
    }
    assert_eq!(f.len(), bytes, "the COMMIT's size arithmetic");
    seal(&mut f, id, 2, seq, offset);
    f
}
fn word(n: u64) -> Box<[u8]> {
    Box::from(&n.to_be_bytes()[..])
}
fn table_key(k: u64) -> Box<[u8]> {
    let mut b = 1u32.to_be_bytes().to_vec();
    b.extend_from_slice(&k.to_be_bytes());
    b.into_boxed_slice()
}
/// store-build-1010's vectors (`disk_law_frames`, `law_frames`; checks from libxxhash 0.8.2).
fn vectors() {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&1u64.to_le_bytes());
    id[8..].copy_from_slice(&2u64.to_le_bytes());
    let h: [u8; 72] = [
        1, 0, 1, 0, 56, 0, 0, 0, 83, 182, 203, 170, 91, 139, 51, 224, 72, 73, 82, 83, 84, 79, 82,
        69, 1, 0, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(header(&id, 3, 7, 0), h, "HEADER vector");
    let ops = [
        Op::Delete(table_key(1)),
        Op::Put(std::sync::Arc::new(tree::Entry {
            key: table_key(2),
            value: word(33),
        })),
    ];
    let c: [u8; 88] = [
        17, 0, 1, 0, 72, 0, 0, 0, 113, 81, 236, 174, 179, 213, 128, 52, 0, 0, 0, 0, 1, 0, 0, 0, 0,
        0, 0, 0, 30, 0, 0, 0, 0, 0, 0, 0, 1, 0, 2, 0, 0, 0, 2, 12, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
        0, 0, 0, 1, 1, 12, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 8, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 33,
    ];
    assert_eq!(commit(&id, 1, 72, 1, 30, &ops, 88), c, "COMMIT vector");
}

enum Msg {
    Frame(Vec<u8>, u64),
    Rotate(File, File, Vec<u8>),
}
struct Synced {
    last: u64,
    written: u64,
}
#[derive(Default)]
struct Counts {
    writes: u64,
    syncs: u64,
    ends: u64,
    groups: u64,
    frames: u64,
}
struct Segment {
    file: File,
    end_file: File,
    header: Vec<u8>,
}
fn sync_dir(dir: &Path) {
    File::open(dir).unwrap().sync_all().unwrap();
}
/// §3.2's segment creation: HEADER, sync, END(72), sync, `sync_dir`.
fn new_segment(dir: &Path, id: &[u8; 16], seq: u64, prev_end: u64) -> Segment {
    let header = header(id, 2, seq, prev_end);
    let mut file = File::create(dir.join(format!("j-{seq}"))).unwrap();
    file.write_all(&header).unwrap();
    file.sync_data().unwrap();
    let end_header = self::header(id, 4, seq, 0);
    let mut raw = Vec::with_capacity(96);
    end_file(&end_header, id, seq, 72, &mut raw);
    let mut end = File::create(dir.join(format!("j-{seq}.end"))).unwrap();
    end.write_all(&raw).unwrap();
    end.sync_data().unwrap();
    sync_dir(dir);
    Segment {
        file,
        end_file: end,
        header: end_header,
    }
}

struct JournalThread {
    dir: PathBuf,
    id: [u8; 16],
    seq: u64,
    segment: Segment,
    rx: Receiver<Msg>,
    acks: SyncSender<Synced>,
}
impl JournalThread {
    fn write_end(&mut self, d: u64, raw: &mut Vec<u8>, counts: &mut Counts) {
        end_file(&self.segment.header, &self.id, self.seq, d, raw);
        let path = self.dir.join(format!("j-{}.end", self.seq));
        self.segment.end_file = File::create(path).unwrap();
        self.segment.end_file.write_all(raw).unwrap();
        self.segment.end_file.sync_data().unwrap();
        counts.ends += 1;
        counts.syncs += 1;
    }
    fn run(mut self) -> Counts {
        let mut counts = Counts::default();
        let mut buffer: Vec<u8> = Vec::with_capacity(GROUP_BYTES + 4096);
        let mut end_raw = Vec::with_capacity(96);
        let mut end = 72u64;
        let mut written = 0u64;
        let mut groups = 0u64;
        let mut deferred: Option<Msg> = None;
        loop {
            let mut next = deferred.take().or_else(|| self.rx.recv().ok());
            buffer.clear();
            let (mut frames, mut last) = (0usize, 0u64);
            let (mut idle, mut closing) = (false, false);
            let mut rotating = None;
            loop {
                match next.take() {
                    None => {
                        closing = true;
                        break;
                    }
                    Some(Msg::Frame(raw, number)) => {
                        if frames != 0 && buffer.len() + raw.len() > GROUP_BYTES {
                            deferred = Some(Msg::Frame(raw, number));
                            break;
                        }
                        buffer.extend_from_slice(&raw);
                        end += raw.len() as u64;
                        written += raw.len() as u64;
                        last = number;
                        frames += 1;
                        if frames >= GROUP_COMMITS || buffer.len() >= GROUP_BYTES {
                            break;
                        }
                        match self.rx.try_recv() {
                            Ok(m) => next = Some(m),
                            Err(TryRecvError::Empty) => {
                                idle = true;
                                break;
                            }
                            Err(TryRecvError::Disconnected) => {
                                closing = true;
                                break;
                            }
                        }
                    }
                    Some(Msg::Rotate(file, end_file, header)) => {
                        rotating = Some(Segment {
                            file,
                            end_file,
                            header,
                        });
                        break;
                    }
                }
            }
            if frames != 0 {
                self.segment.file.write_all(&buffer).unwrap();
                counts.writes += 1;
                self.segment.file.sync_data().unwrap();
                counts.syncs += 1;
                counts.frames += frames as u64;
                counts.groups += 1;
                if self.acks.send(Synced { last, written }).is_err() {
                    return counts;
                }
                groups += 1;
            }
            if idle || closing || rotating.is_some() || groups == 64 {
                self.write_end(end, &mut end_raw, &mut counts);
                groups = 0;
            }
            if let Some(s) = rotating {
                self.segment = s;
                self.seq += 1;
                end = 72;
            }
            if closing {
                return counts;
            }
        }
    }
}

struct Journal {
    dir: PathBuf,
    id: [u8; 16],
    tx: Option<SyncSender<Msg>>,
    acks: Receiver<Synced>,
    task: Option<JoinHandle<Counts>>,
    file_seq: u64,
    file_end: u64,
    frame_seq: u64,
    journaled: u64,
    inflight: u64,
    acked_bytes: u64,
    pending: Option<Vec<u8>>,
    rotates: u64,
}
impl Journal {
    fn open(dir: &Path, id: [u8; 16]) -> Self {
        let segment = new_segment(dir, &id, 1, 0);
        let (tx, rx) = sync_channel(2 * GROUP_COMMITS);
        let (ack_tx, acks) = sync_channel(128);
        let thread = JournalThread {
            dir: dir.to_path_buf(),
            id,
            seq: 1,
            segment,
            rx,
            acks: ack_tx,
        };
        let task = std::thread::spawn(move || thread.run());
        Journal {
            dir: dir.to_path_buf(),
            id,
            tx: Some(tx),
            acks,
            task: Some(task),
            file_seq: 1,
            file_end: 72,
            frame_seq: 0,
            journaled: 0,
            inflight: 0,
            acked_bytes: 0,
            pending: None,
            rotates: 0,
        }
    }
    fn ack(&mut self, a: Synced) {
        self.journaled = a.last;
        self.inflight -= a.written - self.acked_bytes;
        self.acked_bytes = a.written;
    }
    fn durable(&mut self, ticket: u64) {
        tree::Journal::poll(self);
        while self.journaled < ticket {
            let a = self.acks.recv().unwrap();
            self.ack(a);
        }
    }
    fn close(&mut self) -> Counts {
        self.durable(self.frame_seq);
        drop(self.tx.take());
        self.task.take().unwrap().join().unwrap()
    }
}
impl tree::Journal for Journal {
    fn poll(&mut self) {
        while let Ok(a) = self.acks.try_recv() {
            self.ack(a);
        }
    }
    fn encode(&mut self, ops: &mut Vec<Op>, version: u64, at: u64, bytes: u64) -> u64 {
        ops.sort_unstable_by(|a, b| a.key().cmp(b.key()));
        let raw = commit(
            &self.id,
            self.file_seq,
            self.file_end,
            version,
            at,
            ops,
            bytes as usize,
        );
        self.pending = Some(raw);
        bytes
    }
    fn inflight(&self) -> u64 {
        self.inflight
    }
    fn send(&mut self, _version: u64) -> u64 {
        let raw = self.pending.take().unwrap();
        self.file_end += raw.len() as u64;
        self.inflight += raw.len() as u64;
        self.frame_seq += 1;
        let tx = self.tx.as_ref().unwrap();
        tx.send(Msg::Frame(raw, self.frame_seq)).unwrap();
        if self.file_end - 72 >= CHECKPOINT_BYTES {
            let s = new_segment(&self.dir, &self.id, self.file_seq + 1, self.file_end);
            tx.send(Msg::Rotate(s.file, s.end_file, s.header)).unwrap();
            self.file_seq += 1;
            self.file_end = 72;
            self.rotates += 1;
        }
        self.frame_seq
    }
}

fn one(db: &mut Db, j: &mut Journal, t: &Table, c: u64) -> u64 {
    let done = run(db, j, |tx| {
        let k = c % 16;
        let a = Account {
            id: k,
            owner: String::from("owner000"),
            balance: i64::try_from(c).unwrap(),
        };
        put(tx, t, k, &a)?;
        Ok(c)
    });
    match done {
        Ok(d) => {
            assert_eq!(d.ticket, c + 1);
            d.value
        }
        Err(e) => panic!("store R4/R5 refused: {e:?}"),
    }
}

/// Replays every segment from 1 to `last`, checking what the header comment lists. Returns the rows.
fn replay(dir: &Path, id: &[u8; 16], last: u64, commits: u64) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut rows = BTreeMap::new();
    let mut version = 0;
    let mut prev_end = 0;
    for seq in 1..=last {
        let mut raw = Vec::new();
        File::open(dir.join(format!("j-{seq}")))
            .unwrap()
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(
            raw[..72],
            header(id, 2, seq, prev_end)[..],
            "j-{seq} HEADER"
        );
        let mut at = 72;
        while at < raw.len() {
            let len = u32::from_le_bytes(raw[at + 4..at + 8].try_into().unwrap()) as usize;
            let mut f = raw[at..at + 16 + len].to_vec();
            f[8..16].fill(0);
            seal(&mut f, id, 2, seq, at as u64);
            assert_eq!(f[..], raw[at..at + 16 + len], "j-{seq} frame at {at}");
            assert_eq!((f[0], f[1], len), (0x11, 0, 77), "a one-key COMMIT");
            let p = &f[16..];
            version += 1;
            assert_eq!(
                u64::from_le_bytes(p[4..12].try_into().unwrap()),
                version,
                "version chain"
            );
            assert_eq!(
                (
                    p[20],
                    p[21],
                    u32::from_le_bytes(p[22..26].try_into().unwrap()),
                    p[26]
                ),
                (1, 0, 1, 1)
            );
            let key = p[31..43].to_vec();
            let value = p[47..77].to_vec();
            rows.insert(key, value);
            at += 16 + len;
        }
        let mut end = Vec::new();
        File::open(dir.join(format!("j-{seq}.end")))
            .unwrap()
            .read_to_end(&mut end)
            .unwrap();
        let mut expected = Vec::new();
        end_file(
            &header(id, 4, seq, 0),
            id,
            seq,
            raw.len() as u64,
            &mut expected,
        );
        assert_eq!(
            end, expected,
            "j-{seq}.end names the segment's whole length"
        );
        prev_end = raw.len() as u64;
    }
    assert_eq!(version, commits);
    rows
}

fn main() {
    vectors();
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    assert!(mode == "R4" || mode == "R5", "modes: R4, R5");
    let mut gate = File::open(args.next().unwrap()).unwrap();
    let dir = PathBuf::from(args.next().unwrap());
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "the directory must be empty"
    );
    let mut id = [0u8; 16];
    File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut id)
        .unwrap();
    let (mut db, t) = tree::open(&[]);
    let mut j = Journal::open(&dir, id);
    let mut c = 0;
    while c < 64 {
        one(&mut db, &mut j, &t, c);
        j.durable(c + 1);
        c += 1;
    }
    println!("READY");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    let start = Instant::now();
    let out = if mode == "R4" {
        let mut sum = 0;
        for _ in 0..10000 {
            sum += one(&mut db, &mut j, &t, c);
            j.durable(c + 1);
            c += 1;
        }
        sum
    } else {
        loop {
            for _ in 0..64 {
                one(&mut db, &mut j, &t, c);
                c += 1;
            }
            j.durable(c);
            if start.elapsed() >= Duration::from_secs(10) {
                break c - 64;
            }
        }
    };
    let elapsed = start.elapsed().as_nanos();
    println!("DONE {out} {elapsed}");
    std::io::stdout().flush().unwrap();
    gate.read_exact(&mut [0]).unwrap();
    let k = j.close();
    println!(
        "IO writes {} syncs {} ends {} groups {} frames {} rotates {}",
        k.writes, k.syncs, k.ends, k.groups, k.frames, j.rotates
    );
    assert_eq!(k.frames, c);
    let replayed = replay(&dir, &id, j.file_seq, c);
    let rows = tree::rows(&db);
    assert_eq!(rows.len(), 16);
    for (r, (key, value)) in rows.iter().zip(&replayed) {
        assert!(*r.key == key[..] && *r.value == value[..]);
    }
    for (k, r) in rows.iter().enumerate() {
        let last = (c - 1) - ((c - 1) % 16 + 16 - k as u64) % 16;
        let a = tree::decode(&r.value);
        assert!(
            a.id == k as u64 && a.balance == i64::try_from(last).unwrap() && a.owner == "owner000"
        );
    }
}
