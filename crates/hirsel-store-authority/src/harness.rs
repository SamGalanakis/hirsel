//! The crash and level law, run against the simulated authority.
//!
//! Racing writers claim with fresh identities, submit windows of one to three
//! entries of one to four commits at random levels, wait on them newest or
//! oldest first, and wait for durability;
//! readers read at random levels; failures at random levels strike between
//! steps, with every fault the simulation injects. The oracle is a ledger of
//! what was acknowledged, kept apart from the authority:
//!
//! 1. an entry acknowledged durable at level `l` (an `Advanced` at `l`, or a
//!    `durable(l)` covering it) survives every later failure that `l` survives,
//!    with its record;
//! 2. an entry a read at `l` returned survives the same failures;
//! 3. no position is acknowledged with two different entries while the first
//!    is still owed (so a fenced writer never acknowledges);
//! 4. a read returns at each position the record acknowledged there, if any;
//! 5. recovery refuses a history only at a position no writer was acknowledged at;
//! 6. a claim that died (`Fenced`, `Undecided`) is `Fenced` when used again.
//!
//! Damage the failures leave (a torn tail, garbage) may only make recovery
//! refuse; it may never change or drop what the ledger owes. Each planted
//! defect must break a law.

use crate::sim::{Bug, Config, Sim};
use crate::{Advanced, Authority, Claim, Error, Level, Record};
use std::collections::BTreeMap;

/// An engine payload unique by `tag`, `parts` times as long as the smallest:
/// no authority reads it, so only its bytes and size matter.
pub(crate) fn entry(tag: u64, parts: u32) -> Vec<u8> {
    tag.to_le_bytes().repeat(parts as usize * 4)
}

/// Appends a window of `entries` (each a payload and its level) on `c`: submits
/// them all, then waits on them newest or oldest first; either way the first
/// wait appends the window with one barrier. Each entry's position and outcome, in submit order; a wait
/// answered for another position than its submit's is an error, and an entry
/// a dead claim refused to queue is `Fenced`.
pub(crate) fn window(
    a: &mut dyn Authority,
    c: &mut Claim,
    entries: &[(Vec<u8>, Level)],
    newest: bool,
) -> Result<Vec<(u64, Advanced)>, String> {
    let start = c.end() + 1;
    let mut queued = Vec::new();
    for (i, (e, at)) in entries.iter().enumerate() {
        let expect = start + i as u64;
        match a.submit(c, e.clone(), &[], &[], *at) {
            Ok(p) if p == expect => queued.push(p),
            Ok(p) => return Err(format!("submitted at {p}, not {expect}")),
            Err(Error::Fenced) => break,
            Err(e) => return Err(format!("a submit was refused: {e:?}")),
        }
    }
    if newest {
        queued.reverse();
    }
    let mut out = BTreeMap::new();
    for p in queued {
        let answer = a.wait(c, p);
        if matches!(answer, Advanced::Advanced(q) if q != p) {
            return Err(format!("a wait at {p} answered {answer:?}"));
        }
        out.insert(p, answer);
    }
    Ok((0..entries.len() as u64)
        .map(|i| {
            let p = start + i;
            (p, out.remove(&p).unwrap_or(Advanced::Fenced))
        })
        .collect())
}

/// Appends `e` alone at `at`: a submit and its wait.
pub(crate) fn append(a: &mut dyn Authority, c: &mut Claim, e: Vec<u8>, at: Level) -> Advanced {
    match a.submit(c, e, &[], &[], at) {
        Ok(p) => a.wait(c, p),
        Err(Error::Refused(e)) => Advanced::Refused(e),
        Err(_) => Advanced::Fenced,
    }
}

/// Whether a dead claim stays dead: used again, it queues nothing and is fenced.
pub(crate) fn stays_dead(a: &mut dyn Authority, c: &mut Claim, at: Level) -> bool {
    a.submit(c, entry(0, 1), &[], &[], at) == Err(Error::Fenced)
        && a.durable(c, at) == Err(Error::Fenced)
}

/// The position a refusal names, when it names a history record.
pub(crate) fn refused_at(e: &Error) -> Option<u64> {
    match e {
        Error::Corrupt(o) | Error::Format { object: o, .. } => {
            o.strip_prefix("history entry ")?.parse().ok()
        }
        _ => None,
    }
}

pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The history at the weakest level, retrying refused reads, up to the first
/// record a failure damaged: a read refuses there, and the ledger's checks
/// then prove nothing owed is at or past it.
fn read_all(sim: &mut Sim) -> Result<Vec<Record>, String> {
    let mut max = u64::MAX;
    loop {
        match sim.read(0, Level(0), max) {
            Ok(all) => return Ok(all),
            Err(Error::Refused(_)) => continue,
            Err(e) if max == u64::MAX && refused_at(&e).is_some() => {
                max = refused_at(&e).unwrap() - 1
            }
            Err(e) => return Err(format!("read: {e:?}")),
        }
    }
}

struct Writer {
    sim: Sim,
    claim: Option<Claim>,
}

/// What the ledger owes at a position: the record, the strongest level it
/// was acknowledged or observed at, and whether a writer was acknowledged.
type Ledger = BTreeMap<u64, (Record, u8, bool)>;

fn owe(ledger: &mut Ledger, p: u64, record: &Record, level: u8, acked: bool) -> Result<(), String> {
    match ledger.get_mut(&p) {
        Some((b, l, a)) if b == record => {
            *l = (*l).max(level);
            *a |= acked;
            Ok(())
        }
        Some(_) => Err(format!(
            "position {p} acknowledged with two different entries"
        )),
        None => {
            ledger.insert(p, (record.clone(), level, acked));
            Ok(())
        }
    }
}

/// One schedule; `Err` names the first broken law.
fn schedule(seed: u64, steps: u32, levels: u8, bug: Option<Bug>) -> Result<(), String> {
    let mut rng = Rng(seed | 1);
    let mut faults = BTreeMap::new();
    for f in crate::sim::FAULTS {
        faults.insert(f.to_string(), 4 + rng.below(12));
    }
    let names = (0..levels).map(|i| format!("l{i}")).collect();
    let root = Sim::new(
        Config {
            seed,
            represent: crate::sim::Represent::Log,
            levels: names,
            faults,
        },
        Level(0),
    );
    if let Some(b) = bug {
        root.plant(b);
    }
    let mut writers: Vec<Writer> = (0..3)
        .map(|_| Writer {
            sim: root.share(),
            claim: None,
        })
        .collect();
    let mut reader = root.share();
    let mut ledger = Ledger::new();
    let mut tag = 0u64;
    for _ in 0..steps {
        let w = rng.below(writers.len() as u64) as usize;
        let at = Level(rng.below(levels as u64) as u8);
        match rng.below(10) {
            0 | 1 => {
                let attempt = (rng.next() as u128).to_le_bytes();
                let mut fresh = [0u8; 32];
                fresh[..8].copy_from_slice(&rng.next().to_le_bytes());
                let wr = &mut writers[w];
                match wr.sim.claim(attempt, fresh) {
                    Ok(c) => {
                        // The writer reads the history it now owns to its end,
                        // which is its own claim.
                        let all = read_all(&mut wr.sim)?;
                        if all.len() as u64 != c.end {
                            return Err("claim end is not the history's end".into());
                        }
                        if !matches!(all.last(), Some(Record::Claimed { attempt: a, .. }) if *a == attempt)
                        {
                            return Err("a claim's last record is not its own".into());
                        }
                        wr.claim = Some(c.claim);
                    }
                    Err(e @ Error::Corrupt(_)) => {
                        let p = refused_at(&e).ok_or("recovery refused without a position")?;
                        if matches!(ledger.get(&p), Some((_, _, true))) {
                            return Err(format!("recovery refused the acknowledged entry at {p}"));
                        }
                    }
                    Err(_) => {}
                }
            }
            2..=6 => {
                let wr = &mut writers[w];
                let Some(c) = wr.claim.as_mut() else {
                    continue;
                };
                let entries: Vec<(Vec<u8>, Level)> = (0..1 + rng.below(3))
                    .map(|_| {
                        tag += 1;
                        let at = Level(rng.below(levels as u64) as u8);
                        (entry(tag, 1 + rng.below(4) as u32), at)
                    })
                    .collect();
                let newest = rng.below(2) == 0;
                let mut dead = false;
                for ((e, at), (p, answer)) in
                    entries
                        .iter()
                        .zip(window(&mut wr.sim, c, &entries, newest)?)
                {
                    match answer {
                        Advanced::Advanced(_) => owe(
                            &mut ledger,
                            p,
                            &Record::Appended {
                                payload: e.clone(),
                                created: vec![],
                                retired: vec![],
                            },
                            at.0,
                            true,
                        )?,
                        _ => dead = true,
                    }
                }
                if dead {
                    if !stays_dead(&mut wr.sim, c, at) {
                        return Err("a dead claim was used again".into());
                    }
                    wr.claim = None;
                }
            }
            7 => {
                let wr = &mut writers[w];
                let Some(c) = wr.claim.as_mut() else {
                    continue;
                };
                if let Ok(d) = wr.sim.durable(c, at) {
                    let held: Vec<(u64, Record)> = ledger
                        .range(..=d)
                        .map(|(p, (b, _, _))| (*p, b.clone()))
                        .collect();
                    for (p, b) in held {
                        owe(&mut ledger, p, &b, at.0, false)?;
                    }
                } else {
                    wr.claim = None;
                }
            }
            8 => {
                if let Ok(all) = reader.read(0, at, u64::MAX) {
                    for (i, b) in all.iter().enumerate() {
                        owe(&mut ledger, i as u64 + 1, b, at.0, false).map_err(|_| {
                            format!("a read at {} returned other bytes at {}", at.0, i + 1)
                        })?;
                    }
                }
            }
            _ => {
                let k = rng.below(levels as u64) as u8;
                root.crash(Level(k));
                for wr in &mut writers {
                    wr.claim = None;
                }
                // What the failure may lose is no longer owed.
                ledger.retain(|_, (_, l, _)| *l >= k);
                let all = read_all(&mut reader)?;
                for (p, (b, l, _)) in &ledger {
                    if all.get(*p as usize - 1) != Some(b) {
                        return Err(format!(
                            "position {p}, owed at level {l}, lost by a failure at level {k}"
                        ));
                    }
                }
                // Positions past the owed ones were not acknowledged at a level
                // this failure spares, so they are no longer owed either.
                let keep = all.len() as u64;
                ledger.retain(|p, _| *p <= keep);
            }
        }
    }
    Ok(())
}

fn sweep(bug: Option<Bug>, seeds: u64) -> Option<String> {
    for seed in 1..=seeds {
        for levels in 1..=3 {
            if let Err(e) = schedule(seed * 7919 + levels as u64, 300, levels, bug) {
                return Some(format!("seed {seed}, {levels} levels: {e}"));
            }
        }
    }
    None
}

#[test]
fn crash_and_level_law_holds() {
    assert_eq!(sweep(None, 1000), None);
}

#[test]
fn every_planted_defect_breaks_the_law() {
    for bug in [
        Bug::NoFence,
        Bug::AckWithoutBarrier,
        Bug::ReadWithoutBarrier,
    ] {
        assert!(sweep(Some(bug), 1000).is_some(), "{bug:?} went unnoticed");
    }
}

/// Reclamation decides at the strongest-durable end, never a weaker one
/// (storage.md STORE-11, Reclamation): a retirement acknowledged only at the
/// weakest of two levels is made durable at the strongest before its object is
/// removed, so a failure the strongest level survives never leaves a recovered
/// history inventorying an object that is gone.
#[test]
fn a_retirement_removes_its_object_only_once_strongest_durable() {
    let config = Config {
        seed: 1,
        represent: crate::sim::Represent::Log,
        levels: vec!["cache".into(), "disk".into()],
        faults: BTreeMap::new(),
    };
    let mut sim = Sim::new(config, Level(0));
    let mut c = sim.claim([1; 16], [2; 32]).unwrap().claim;
    let kept = crate::ObjectRef {
        lifetime: 2,
        epoch: 1,
        digest: crate::envelope::digest(b"kept"),
    };
    sim.put(&mut c, kept, b"kept".to_vec()).unwrap();
    let p2 = sim
        .submit(&mut c, entry(2, 1), &[kept], &[], Level(1))
        .unwrap();
    assert_eq!(sim.wait(&mut c, p2), Advanced::Advanced(2));
    let p3 = sim
        .submit(&mut c, entry(3, 1), &[], &[kept], Level(0))
        .unwrap();
    assert_eq!(sim.wait(&mut c, p3), Advanced::Advanced(3));
    let mut done = false;
    for _ in 0..100 {
        if sim.reclaim(&mut c, 64).unwrap() == crate::Reclaimed::Done {
            done = true;
            break;
        }
    }
    assert!(done, "reclamation never finished");
    sim.crash(Level(1));
    let mut needed = Vec::new();
    for record in read_all(&mut sim).unwrap() {
        if let Record::Appended {
            created, retired, ..
        } = record
        {
            needed.extend(created);
            needed.retain(|r| !retired.contains(r));
        }
    }
    for r in needed {
        assert!(
            sim.get(r).unwrap().is_some(),
            "an object the recovered history inventories is gone: {r:?}"
        );
    }
}
