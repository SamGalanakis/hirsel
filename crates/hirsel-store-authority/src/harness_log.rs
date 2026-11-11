//! The crash and level law (see `harness.rs`) run against `authority.log@1` on
//! real files, with the file-system calls an append makes faulted and a host
//! failure modelled.
//!
//! Racing handles claim, append windows of one to three entries of one to four
//! commits at either level, wait for durability, read, and use claims that died; process and
//! host failures strike between steps. The disk seam (`log::disk`) fails
//! writes with nothing written, writes short, fails `fdatasync`, segment
//! creation and the directory sync, and fails truncation. A host failure keeps
//! of each segment its synced bytes and any prefix of the rest, and removes a
//! segment no directory sync named. The ledger is kept apart from the
//! authority:
//!
//! 1. an entry acknowledged durable at a level (an `Advanced` there, a covering
//!    `durable`, a read there, or a claim, which is durable at `host`)
//!    survives every later failure that level survives, with its bytes;
//! 2. no position is acknowledged with two different entries;
//! 3. an entry refused, or fenced behind one that failed, has no effect;
//! 4. a claim that died (`Fenced`, `Undecided`) is `Fenced` at every later use;
//! 5. a claimed history follows, its claim is at its end, and its versions are
//!    the commits counted, not the entries;
//! 6. nothing here damages the history, so nothing is ever refused as corrupt.

use crate::harness::{Rng, append, entry, stays_dead, window};
use crate::log::Log;
use crate::{Advanced, Authority, Claim, Error, Level, Record};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

const HOST: Level = Level(1);

const WRITE: usize = 0;
const SHORT: usize = 1;
const SYNC: usize = 2;
const CREATE: usize = 3;
const DIR_SYNC: usize = 4;
const SET_LEN: usize = 5;

/// The faults the disk seam injects, and what a host failure would keep.
struct Disk {
    rng: Rng,
    /// One call in `odds[kind]` fails that way; 0 never.
    odds: [u64; 6],
    /// Each segment's synced length, by inode.
    synced: HashMap<u64, u64>,
    /// The segments a directory sync named, by inode.
    named: HashSet<u64>,
}

thread_local! {
    static DISK: RefCell<Option<Disk>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Disk) -> R) -> Option<R> {
    DISK.with(|d| d.borrow_mut().as_mut().map(f))
}

fn strikes(d: &mut Disk, kind: usize) -> bool {
    d.odds[kind] != 0 && d.rng.below(d.odds[kind]) == 0
}

fn errno(n: i32) -> io::Error {
    io::Error::from_raw_os_error(n)
}

pub(crate) fn write_at(f: &File, bytes: &[u8], at: u64) -> Option<io::Result<usize>> {
    with(|d| {
        if strikes(d, WRITE) {
            return Err(errno(libc::ENOSPC));
        }
        let n = if bytes.len() > 1 && strikes(d, SHORT) {
            1 + d.rng.below(bytes.len() as u64 - 1) as usize
        } else {
            bytes.len()
        };
        f.write_all_at(&bytes[..n], at).map(|_| n)
    })
}

pub(crate) fn sync_data(f: &File) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, SYNC) {
            return Err(errno(libc::EIO));
        }
        let m = f.metadata()?;
        d.synced.insert(m.ino(), m.len());
        Ok(())
    })
}

pub(crate) fn set_len(f: &File, len: u64) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, SET_LEN) {
            return Err(errno(libc::EIO));
        }
        f.set_len(len)?;
        if let Some(s) = d.synced.get_mut(&f.metadata()?.ino()) {
            *s = (*s).min(len);
        }
        Ok(())
    })
}

pub(crate) fn create_new(path: &Path) -> Option<io::Result<File>> {
    with(|d| {
        if strikes(d, CREATE) {
            return Err(errno(libc::ENOSPC));
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
    })
}

pub(crate) fn sync_dir(dir: &Path) -> Option<io::Result<()>> {
    with(|d| {
        if strikes(d, DIR_SYNC) {
            return Err(errno(libc::EIO));
        }
        for e in fs::read_dir(dir)? {
            d.named.insert(e?.metadata()?.ino());
        }
        Ok(())
    })
}

fn segments(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "seg"))
        .collect();
    out.sort();
    out
}

/// The host fails: each segment keeps its synced bytes and any prefix of the
/// rest, and a segment no directory sync named is gone.
fn fail_host(dir: &Path) {
    with(|d| {
        for path in segments(dir) {
            let m = fs::metadata(&path).unwrap();
            if !d.named.contains(&m.ino()) {
                fs::remove_file(&path).unwrap();
                continue;
            }
            let synced = d.synced.get(&m.ino()).copied().unwrap_or(0).min(m.len());
            let keep = synced + d.rng.below(m.len() - synced + 1);
            OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_len(keep))
                .unwrap();
        }
        d.synced.clear();
        d.named.clear();
        for path in segments(dir) {
            let m = fs::metadata(&path).unwrap();
            d.synced.insert(m.ino(), m.len());
            d.named.insert(m.ino());
        }
    });
}

type Ledger = BTreeMap<u64, (Record, u8)>;

fn owe(ledger: &mut Ledger, p: u64, record: &Record, level: u8) -> Result<(), String> {
    match ledger.get_mut(&p) {
        Some((b, l)) if b == record => {
            *l = (*l).max(level);
            Ok(())
        }
        Some(_) => Err(format!(
            "position {p} acknowledged with two different entries"
        )),
        None => {
            ledger.insert(p, (record.clone(), level));
            Ok(())
        }
    }
}

struct Writer {
    log: Log,
    claim: Option<Claim>,
    dead: Option<Claim>,
}

struct Run {
    dir: tempfile::TempDir,
    segment_max: u64,
    writers: Vec<Writer>,
    reader: Log,
    ledger: Ledger,
    /// Entries that must never be in the history: refused or tried dead.
    never: Vec<(u64, Record)>,
}

impl Run {
    fn open(&self) -> Log {
        let mut log = Log::open(self.dir.path(), Level(0)).unwrap();
        log.segment_max = self.segment_max;
        log
    }

    /// Every handle is gone, as after the processes holding them died.
    fn reopen(&mut self) {
        let n = self.writers.len();
        self.writers = (0..n)
            .map(|_| Writer {
                log: self.open(),
                claim: None,
                dead: None,
            })
            .collect();
        self.reader = self.open();
    }

    /// Check what a read returned against the ledger and the never-set.
    fn check(&self, all: &[Record]) -> Result<(), String> {
        for (p, b) in &self.never {
            if all.get(*p as usize - 1) == Some(b) {
                return Err(format!("an entry with no effect is at {p}"));
            }
        }
        for (p, (b, _)) in self.ledger.range(..=all.len() as u64) {
            if all[*p as usize - 1] != *b {
                return Err(format!("a read returned another record at {p}"));
            }
        }
        Ok(())
    }
}

fn read_all(log: &mut Log) -> Result<Vec<Record>, String> {
    log.read(0, Level(0), u64::MAX)
        .map_err(|e| format!("read: {e:?}"))
}

/// One schedule; `Err` names the first broken law.
fn schedule(seed: u64, steps: u32) -> Result<(), String> {
    let mut rng = Rng(seed | 1);
    let mut odds = [0; 6];
    for o in &mut odds {
        *o = if rng.below(4) == 0 {
            0
        } else {
            6 + rng.below(40)
        };
    }
    DISK.with(|d| {
        *d.borrow_mut() = Some(Disk {
            rng: Rng(seed.wrapping_mul(0x9E37_79B9) | 1),
            odds,
            synced: HashMap::new(),
            named: HashSet::new(),
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let mut probe = Log::open(dir.path(), Level(0)).unwrap();
    probe.segment_max = 300 + rng.below(700);
    let mut run = Run {
        segment_max: probe.segment_max,
        dir,
        writers: Vec::new(),
        reader: probe,
        ledger: Ledger::new(),
        never: Vec::new(),
    };
    run.writers = (0..3)
        .map(|_| Writer {
            log: run.open(),
            claim: None,
            dead: None,
        })
        .collect();
    let mut tag = 0u64;
    for _ in 0..steps {
        let w = rng.below(run.writers.len() as u64) as usize;
        let at = Level(rng.below(2) as u8);
        tag += 1;
        match rng.below(13) {
            0 | 1 => {
                let attempt = (rng.next() as u128).to_le_bytes();
                let mut fresh = [0u8; 32];
                fresh[..8].copy_from_slice(&rng.next().to_le_bytes());
                let wr = &mut run.writers[w];
                match wr.log.claim(attempt, fresh) {
                    Ok(c) => {
                        let all = read_all(&mut wr.log)?;
                        if all.len() as u64 != c.end
                            || !matches!(all.last(), Some(Record::Claimed { attempt: a, .. }) if *a == attempt)
                        {
                            return Err("the claim is not at the history's end".into());
                        }
                        let end = c.end;
                        if let Some(old) = wr.claim.replace(c.claim) {
                            wr.dead = Some(old);
                        }
                        let record = all[end as usize - 1].clone();
                        owe(&mut run.ledger, end, &record, HOST.0)?;
                        run.check(&all)?;
                    }
                    Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                        return Err(format!("claim refused the history: {e:?}"));
                    }
                    Err(_) => {}
                }
            }
            2..=6 => {
                let wr = &mut run.writers[w];
                let Some(c) = wr.claim.as_mut() else {
                    continue;
                };
                let entries: Vec<(Vec<u8>, Level)> = (0..1 + rng.below(3))
                    .map(|i| {
                        let at = Level(rng.below(2) as u8);
                        (entry(tag << 2 | i, 1 + rng.below(4) as u32), at)
                    })
                    .collect();
                let newest = rng.below(2) == 0;
                let mut dead = false;
                for ((e, at), (p, answer)) in
                    entries
                        .iter()
                        .zip(window(&mut wr.log, c, &entries, newest)?)
                {
                    let e = Record::Appended {
                        payload: e.clone(),
                        created: vec![],
                        retired: vec![],
                    };
                    match answer {
                        Advanced::Advanced(_) => owe(&mut run.ledger, p, &e, at.0)?,
                        Advanced::Refused(_) | Advanced::Fenced => {
                            run.never.push((p, e));
                            dead = true;
                        }
                        Advanced::Undecided(_) => dead = true,
                    }
                }
                if dead {
                    wr.dead = wr.claim.take();
                }
            }
            7 => {
                let wr = &mut run.writers[w];
                let Some(c) = wr.claim.as_mut() else {
                    continue;
                };
                match wr.log.durable(c, at) {
                    Ok(d) => {
                        let held: Vec<(u64, Record)> = run
                            .ledger
                            .range(..=d)
                            .map(|(p, (b, _))| (*p, b.clone()))
                            .collect();
                        for (p, b) in held {
                            owe(&mut run.ledger, p, &b, at.0)?;
                        }
                    }
                    Err(Error::Corrupt(e)) => return Err(format!("durable: corrupt {e}")),
                    Err(Error::Fenced) => wr.dead = wr.claim.take(),
                    Err(_) => wr.claim = None,
                }
            }
            8 => match run.reader.read(0, at, u64::MAX) {
                Ok(all) => {
                    run.check(&all)?;
                    for (i, b) in all.iter().enumerate() {
                        owe(&mut run.ledger, i as u64 + 1, b, at.0)?;
                    }
                }
                Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                    return Err(format!("read refused the history: {e:?}"));
                }
                Err(_) => {}
            },
            9 | 10 => {
                let wr = &mut run.writers[w];
                let Some(c) = wr.dead.as_mut() else {
                    continue;
                };
                // A superseded claim may not know it yet: its submit queues,
                // and the append is fenced with no effect.
                if append(&mut wr.log, c, entry(tag << 2 | 3, 1), at) != Advanced::Fenced
                    || !stays_dead(&mut wr.log, c, at)
                {
                    return Err("a dead claim was used again".into());
                }
            }
            k => {
                let host = k == 12;
                if host {
                    fail_host(run.dir.path());
                }
                run.reopen();
                // What the failure may lose is no longer owed.
                let survives = if host { HOST.0 } else { 0 };
                run.ledger.retain(|_, (_, l)| *l >= survives);
                let all = read_all(&mut run.reader)?;
                for (p, (b, l)) in &run.ledger {
                    if all.get(*p as usize - 1) != Some(b) {
                        return Err(format!(
                            "position {p}, owed at level {l}, lost by a {} failure",
                            if host { "host" } else { "process" }
                        ));
                    }
                }
                run.check(&all)?;
                // Past what the failure kept nothing is owed or excluded.
                let keep = all.len() as u64;
                run.ledger.retain(|p, _| *p <= keep);
                run.never.retain(|(p, _)| *p <= keep);
            }
        }
    }
    Ok(())
}

fn sweep(seeds: u64) -> Option<String> {
    let found = (1..=seeds).find_map(|seed| {
        schedule(seed * 7919, 300)
            .err()
            .map(|e| format!("seed {seed}: {e}"))
    });
    DISK.with(|d| *d.borrow_mut() = None);
    found
}

#[test]
fn the_log_keeps_the_crash_and_level_law_through_disk_faults() {
    assert_eq!(sweep(400), None);
}

/// Runs reclamation until it is done.
fn reclaimed(a: &mut Log, c: &mut Claim) -> Result<(), Error> {
    for _ in 0..100 {
        if a.reclaim(c, 64)? == crate::Reclaimed::Done {
            return Ok(());
        }
    }
    panic!("reclamation never finished");
}

fn object(lifetime: u64, epoch: u64, data: &[u8]) -> crate::ObjectRef {
    crate::ObjectRef {
        lifetime,
        epoch,
        digest: crate::envelope::digest(data),
    }
}

/// Reclamation's eligibility at S, the claim's strongest-durable end, over a
/// real log (storage.md STORE-11, Reclamation), each rule beside its refused
/// twin. R1: a retirement removes its object once its record is in the history
/// at or below S, never while the record is only queued. R2: an object of a
/// recorded position its record does not inventory goes (I1: a same-epoch put
/// beside the record's own object), the inventoried one stays, and a record
/// that cannot be read keeps its candidate and fails the call. R3: an object
/// past S goes only when its epoch is below the claim's; an earlier epoch's object
/// inventoried below the later claim stays. A superseded writer's
/// publication of the continuation is fenced and leaves the file as it was.
#[test]
fn reclamation_removes_exactly_what_the_history_gives_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Log::open(dir.path(), Level(0)).unwrap();
    let mut c = a.claim([1; 16], [2; 32]).unwrap().claim;
    let (kept, stray) = (object(2, 1, b"kept"), object(2, 1, b"stray"));
    let (held, ahead) = (object(2, 1, b"held"), object(5, 1, b"ahead"));
    for (r, data) in [
        (kept, &b"kept"[..]),
        (stray, b"stray"),
        (held, b"held"),
        (ahead, b"ahead"),
    ] {
        a.put(&mut c, r, data.to_vec()).unwrap();
    }
    let p2 = a
        .submit(&mut c, b"record-two".to_vec(), &[kept, held], &[], Level(1))
        .unwrap();
    assert_eq!(a.wait(&mut c, p2), Advanced::Advanced(2));
    let p3 = a
        .submit(&mut c, b"record-three".to_vec(), &[], &[kept], Level(1))
        .unwrap();
    reclaimed(&mut a, &mut c).unwrap();
    assert_eq!(a.get(stray), Ok(None), "R2: not inventoried at 2");
    assert!(
        a.get(kept).unwrap().is_some(),
        "R1 twin: retired by a queued record"
    );
    assert!(
        a.get(ahead).unwrap().is_some(),
        "R3 twin: the claim's own epoch"
    );
    assert_eq!(a.wait(&mut c, p3), Advanced::Advanced(3));
    reclaimed(&mut a, &mut c).unwrap();
    assert_eq!(a.get(kept), Ok(None), "R1: retired at 3");
    // A takeover at 4, epoch 2: `ahead` is past S of an earlier epoch.
    let continuation = || fs::read(dir.path().join("reclaim")).ok();
    let before = continuation();
    assert!(before.is_some());
    let mut b = Log::open(dir.path(), Level(0)).unwrap();
    let mut c2 = b.claim([3; 16], [2; 32]).unwrap().claim;
    assert_eq!(
        crate::reclaim::Space::publish(&mut a, &mut c, &crate::Sweep::default()),
        Err(Error::Fenced)
    );
    assert_eq!(continuation(), before, "a fenced publication moved nothing");
    let mine = object(6, 2, b"mine");
    b.put(&mut c2, mine, b"mine".to_vec()).unwrap();
    reclaimed(&mut b, &mut c2).unwrap();
    assert_eq!(b.get(ahead), Ok(None), "R3: epoch 1 past S under epoch 2");
    assert!(
        b.get(mine).unwrap().is_some(),
        "R3 twin: the claim's own epoch"
    );
    assert!(
        b.get(held).unwrap().is_some(),
        "R3 twin: an earlier epoch's object inventoried below the later claim"
    );
    // A candidate at 3 whose record cannot be read stays: the next claim's
    // walk, which reads that record, fails.
    let candidate = object(3, 1, b"candidate");
    b.put_object(&candidate.name(), b"candidate".to_vec())
        .unwrap();
    let segment = dir.path().join(format!("{:020}.seg", 1));
    let mut bytes = fs::read(&segment).unwrap();
    let at = bytes
        .windows(12)
        .position(|w| w == b"record-three")
        .unwrap();
    bytes[at] ^= 1;
    fs::write(&segment, &bytes).unwrap();
    let mut c3 = b.claim([4; 16], [2; 32]).unwrap().claim;
    assert!(matches!(b.reclaim(&mut c3, 64), Err(Error::Corrupt(_))));
    assert!(
        b.get(candidate).unwrap().is_some(),
        "R2 twin: unreadable record"
    );
}
