//! The crash and level law (see `harness.rs`) run against `authority.log@1` on
//! real files, with the file-system calls an append makes faulted and a host
//! failure modelled.
//!
//! Racing handles claim, append windows of one to three entries of one to four
//! commits at either level, wait for durability, read, put objects, and use
//! claims that died; process and host failures strike between steps. The disk
//! seam (`log::disk`) faults the file-system calls as `harness_disk` says,
//! and a host failure keeps what that model keeps. The ledger is kept apart
//! from the authority:
//!
//! 1. an entry acknowledged durable at a level (an `Advanced` there, a covering
//!    `durable`, a read there, or a claim, which is durable at `host`)
//!    survives every later failure that level survives, with its bytes;
//! 2. no position is acknowledged with two different entries;
//! 3. an entry refused, or fenced behind one that failed, has no effect;
//! 4. a claim that died (`Fenced`, `Undecided`) is `Fenced` at every later use;
//! 5. a claimed history follows, its claim is at its end, and its versions are
//!    the commits counted, not the entries;
//! 6. nothing here damages the history, so nothing is ever refused as corrupt;
//! 7. an object put acknowledged survives every later failure with its bytes,
//!    and any object present is whole;
//! 8. the selected base is one a selection made, its `first` never below an
//!    acknowledged selection's, and its tip and live objects are those of the
//!    history's records below it.

use crate::harness::{Rng, append, entry, stays_dead, window};
use crate::harness_disk::{self, KINDS};
use crate::log::{Bug, Log};
use crate::{Advanced, Authority, Claim, Error, Level, Locator, ObjectRef, Record};
use std::collections::BTreeMap;
use std::fs;

const HOST: Level = Level(1);

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
    bug: Option<Bug>,
    writers: Vec<Writer>,
    reader: Log,
    ledger: Ledger,
    /// Entries that must never be in the history: refused or tried dead.
    never: Vec<(u64, Record)>,
    /// Every object put, by name: its bytes, and whether a put acknowledged it.
    objects: BTreeMap<String, (Vec<u8>, bool)>,
    /// Every selection tried, and the highest `first` one acknowledged.
    tried: Vec<(u64, Vec<u8>)>,
    selected: u64,
}

impl Run {
    fn open(&self) -> Log {
        let mut log = Log::open(self.dir.path(), Level(0)).unwrap();
        log.segment_max = self.segment_max;
        log.bug = self.bug;
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

/// Law 8 for `got`, what `log`'s `base` answered, against `all`, the history
/// `log` has just read whole.
pub(crate) fn check_base(
    log: &mut impl Authority,
    all: &[Record],
    got: Result<Option<crate::Base>, Error>,
    tried: &[(u64, Vec<u8>)],
    selected: u64,
) -> Result<(), String> {
    let b = match got {
        Ok(None) if selected == 0 => return Ok(()),
        Ok(None) => return Err(format!("the selection of {selected} is gone")),
        Ok(Some(b)) => b,
        Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
            return Err(format!("base refused: {e:?}"));
        }
        Err(_) => return Ok(()),
    };
    if b.first < selected {
        return Err(format!(
            "the base at {} is below the selection of {selected}",
            b.first
        ));
    }
    if !tried
        .iter()
        .any(|(t, boot)| *t == b.first && *boot == b.boot)
    {
        return Err(format!("no selection made the base at {}", b.first));
    }
    let below = &all[..(b.first as usize - 1).min(all.len())];
    if below.len() as u64 != b.first - 1 {
        return Err(format!("the base at {} is past the history", b.first));
    }
    // The fold: the newest claim's epoch and identity, the newest append, and
    // what was created and not retired.
    let (mut epoch, mut identity, mut newest) = (0, None, None);
    let mut live = std::collections::BTreeSet::<ObjectRef>::new();
    for (i, r) in below.iter().enumerate() {
        match r {
            Record::Claimed {
                epoch: e,
                identity: d,
                ..
            } => (epoch, identity) = (*e, Some(*d)),
            Record::Appended {
                created, retired, ..
            } => {
                live.extend(created.iter().copied());
                for r in retired {
                    live.remove(r);
                }
                newest = Some(i as u64 + 1);
            }
        }
    }
    let locate = |log: &mut _, p| -> Result<Locator, String> {
        Authority::followed_locator(log, p).map_err(|e| format!("followed {p}: {e:?}"))
    };
    let appended = newest.map(|p| locate(log, p)).transpose()?;
    let digest = locate(log, b.first - 1)?.digest;
    let tip = (b.tip.epoch, b.tip.identity, b.tip.appended, b.tip.digest);
    if tip != (epoch, identity, appended, digest) || b.tip.end != b.first - 1 {
        return Err(format!("the base at {} names another tip", b.first));
    }
    if b.live != live.into_iter().collect::<Vec<_>>() {
        return Err(format!("the base at {} names other live objects", b.first));
    }
    Ok(())
}

fn read_all(log: &mut Log) -> Result<Vec<Record>, String> {
    log.read(0, Level(0), u64::MAX)
        .map_err(|e| format!("read: {e:?}"))
}

/// One schedule; `Err` names the first broken law.
fn schedule(seed: u64, steps: u32, bug: Option<Bug>) -> Result<(), String> {
    let mut rng = Rng(seed | 1);
    let mut odds = [0; KINDS];
    for o in &mut odds {
        *o = if rng.below(4) == 0 {
            0
        } else {
            6 + rng.below(40)
        };
    }
    harness_disk::install(seed.wrapping_mul(0x9E37_79B9), odds);
    let dir = tempfile::tempdir().unwrap();
    let mut probe = Log::open(dir.path(), Level(0)).unwrap();
    probe.segment_max = 300 + rng.below(700);
    probe.bug = bug;
    let mut run = Run {
        segment_max: probe.segment_max,
        bug,
        dir,
        writers: Vec::new(),
        reader: probe,
        ledger: Ledger::new(),
        never: Vec::new(),
        objects: BTreeMap::new(),
        tried: Vec::new(),
        selected: 0,
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
        match rng.below(15) {
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
                        // The claim names the newest append before it, by the
                        // forward read; a client that has read nothing finds
                        // that append from `last` and reads back to Genesis
                        // from it alone.
                        let newest = all[..end as usize - 1]
                            .iter()
                            .rposition(|r| matches!(r, Record::Appended { .. }))
                            .map(|i| wr.log.followed_locator(i as u64 + 1))
                            .transpose()
                            .map_err(|e| format!("followed: {e:?}"))?;
                        if !matches!(&record, Record::Claimed { appended, .. } if *appended == newest)
                        {
                            return Err(format!(
                                "the claim at {end} names another append than {newest:?}"
                            ));
                        }
                        let mut cold = run.open();
                        match cold.last(Level(0)) {
                            Ok(l) if l == newest => {
                                if let Some(l) = l {
                                    match cold.at(&l, 0, Level(0), u64::MAX) {
                                        Ok(back) if back[..] == all[..l.position as usize] => {}
                                        Ok(back) => {
                                            return Err(format!(
                                                "a cold read back from {l:?} after the claim at {end} has {} records",
                                                back.len()
                                            ));
                                        }
                                        Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                                            return Err(format!(
                                                "a cold read after the claim at {end}: {e:?}"
                                            ));
                                        }
                                        Err(_) => {}
                                    }
                                }
                            }
                            Ok(l) => {
                                return Err(format!(
                                    "a cold last after the claim at {end} is {l:?}, not {newest:?}"
                                ));
                            }
                            Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                                return Err(format!("a cold last after the claim at {end}: {e:?}"));
                            }
                            Err(_) => {}
                        }
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
                let mut located = Vec::new();
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
                        Advanced::Advanced(_) => {
                            // The writer names what it appended with no read.
                            let l = wr
                                .log
                                .locate(c, p)
                                .map_err(|e| format!("locate {p} after its append: {e:?}"))?;
                            located.push(l);
                            owe(&mut run.ledger, p, &e, at.0)?
                        }
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
                // A handle that never wrote follows the records forward and
                // names each the same.
                if !located.is_empty() {
                    let mut cold = run.open();
                    match cold.read(0, Level(0), u64::MAX) {
                        Ok(_) => {
                            for l in located {
                                let want = cold.followed_locator(l.position);
                                if want.as_ref().ok() != Some(&l) {
                                    return Err(format!(
                                        "the writer locates {l:?}, a forward read {want:?}"
                                    ));
                                }
                            }
                        }
                        Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                            return Err(format!("a forward read after an append: {e:?}"));
                        }
                        Err(_) => {}
                    }
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
            13 => {
                let data = entry(tag << 2, 1 + rng.below(4) as u32);
                let r = crate::ObjectRef {
                    lifetime: tag,
                    epoch: 1,
                    digest: crate::envelope::digest(&data),
                };
                let put = run.writers[w].log.put_object(&r.name(), data.clone());
                match put {
                    Ok(()) => {
                        run.objects.insert(r.name(), (data, true));
                    }
                    Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                        return Err(format!("an object put refused the objects: {e:?}"));
                    }
                    Err(_) => {
                        run.objects.insert(r.name(), (data, false));
                    }
                }
            }
            14 => {
                let wr = &mut run.writers[w];
                let Some(c) = wr.claim.as_mut() else {
                    continue;
                };
                let first = 2 + rng.below(c.end());
                let boot = entry(tag << 2, 1);
                run.tried.push((first, boot.clone()));
                match wr.log.select(c, first, boot) {
                    Ok(()) => {
                        run.selected = run.selected.max(first);
                        let all = read_all(&mut run.reader)?;
                        let got = run.reader.base();
                        check_base(&mut run.reader, &all, got, &run.tried, run.selected)?;
                    }
                    Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                        return Err(format!("select refused the history: {e:?}"));
                    }
                    Err(Error::Refused(_)) => {}
                    Err(Error::Fenced) => wr.dead = wr.claim.take(),
                    // A storage failure leaves the claim usable or dead
                    // without saying which.
                    Err(_) => wr.claim = None,
                }
            }
            k => {
                let host = k == 12;
                if host {
                    harness_disk::fail_host(run.dir.path());
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
                let got = run.reader.base();
                check_base(&mut run.reader, &all, got, &run.tried, run.selected)?;
                for (name, (data, owed)) in &run.objects {
                    match run.reader.get_object(name) {
                        Ok(Some(b)) if b == *data => {}
                        Ok(None) if !owed => {}
                        other => {
                            return Err(format!(
                                "object {name}, acknowledged {owed}, is {other:?} after a {} failure",
                                if host { "host" } else { "process" }
                            ));
                        }
                    }
                }
                // Past what the failure kept nothing is owed or excluded.
                let keep = all.len() as u64;
                run.ledger.retain(|p, _| *p <= keep);
                run.never.retain(|(p, _)| *p <= keep);
            }
        }
    }
    Ok(())
}

fn sweep(seeds: u64, bug: Option<Bug>) -> Option<String> {
    let found = (1..=seeds).find_map(|seed| {
        schedule(seed * 7919, 300, bug)
            .err()
            .map(|e| format!("seed {seed}: {e}"))
    });
    harness_disk::remove();
    found
}

#[test]
fn the_log_keeps_the_crash_and_level_law_through_disk_faults() {
    assert_eq!(sweep(400, None), None);
}

#[test]
fn a_selection_the_directory_never_names_breaks_the_base_law() {
    assert!(sweep(400, Some(Bug::BaseUnsynced)).is_some());
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
