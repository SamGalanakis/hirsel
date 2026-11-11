//! The crash and level law, run against the simulated authority.
//!
//! Racing writers claim with fresh identities, advance groups of one to four
//! commits at random levels and wait for durability;
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
                tag += 1;
                let e = entry(tag, 1 + rng.below(4) as u32);
                let from = c.end();
                match wr.sim.advance(c, from, e.clone(), at) {
                    Advanced::Advanced(p) => {
                        if p != from + 1 {
                            return Err(format!("advanced from {from} to {p}"));
                        }
                        owe(&mut ledger, p, &Record::Appended(e), at.0, true)?;
                    }
                    Advanced::Refused(_) => {}
                    Advanced::Fenced | Advanced::Undecided(_) => {
                        // A dead claim stays dead: used again, it is fenced.
                        let from = c.end();
                        if wr.sim.advance(c, from, e, at) != Advanced::Fenced
                            || wr.sim.durable(c, at) != Err(Error::Fenced)
                        {
                            return Err("a dead claim was used again".into());
                        }
                        wr.claim = None;
                    }
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
