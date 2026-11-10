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
//!    with its bytes;
//! 2. an entry a read at `l` returned survives the same failures;
//! 3. no position is acknowledged with two different entries while the first
//!    is still owed (so a fenced writer never acknowledges);
//! 4. a read returns at each position the bytes acknowledged there, if any;
//! 5. recovery refuses a history only at a position no writer was acknowledged at;
//! 6. a claim that died (`Fenced`, `Undecided`) is `Fenced` when used again.
//!
//! Damage the failures leave (a torn tail, garbage) may only make recovery
//! refuse; it may never change or drop what the ledger owes. Each planted
//! defect must break a law.

use crate::group::{self, Tip};
use crate::sim::{Bug, Config, Sim};
use crate::{Advanced, Authority, Claim, Error, Level};
use std::collections::BTreeMap;

/// A non-claim group of `commits` engine Library commits following `tip`,
/// unique by `tag`.
pub(crate) fn entry(tip: &Tip, tag: u64, commits: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"HSHIST01");
    out.extend_from_slice(&tip.digest);
    out.extend_from_slice(&(tip.last + 1).to_le_bytes());
    out.extend_from_slice(&tip.epoch.to_le_bytes());
    out.extend_from_slice(&commits.to_le_bytes());
    for _ in 0..commits {
        out.extend_from_slice(&[2, 7]); // an engine Library commit
        out.extend_from_slice(&12u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // writes
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // definitions
    out
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

/// The whole history at the weakest level, retrying refused reads.
fn read_all(sim: &mut Sim) -> Result<Vec<Vec<u8>>, String> {
    loop {
        match sim.read(0, Level(0), u64::MAX) {
            Ok(all) => return Ok(all),
            Err(Error::Refused(_)) => continue,
            Err(e) => return Err(format!("read: {e:?}")),
        }
    }
}

struct Writer {
    sim: Sim,
    claim: Option<(Claim, Tip)>,
}

/// What the ledger owes at a position: the bytes, the strongest level they
/// were acknowledged or observed at, and whether a writer was acknowledged.
type Ledger = BTreeMap<u64, (Vec<u8>, u8, bool)>;

fn owe(ledger: &mut Ledger, p: u64, bytes: &[u8], level: u8, acked: bool) -> Result<(), String> {
    match ledger.get_mut(&p) {
        Some((b, l, a)) if b == bytes => {
            *l = (*l).max(level);
            *a |= acked;
            Ok(())
        }
        Some(_) => Err(format!(
            "position {p} acknowledged with two different entries"
        )),
        None => {
            ledger.insert(p, (bytes.to_vec(), level, acked));
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
                        // The writer reads the history it now owns to its end.
                        let all = read_all(&mut wr.sim)?;
                        let mut tip = Tip::origin();
                        for e in &all {
                            tip = group::follow(&tip, e)
                                .map_err(|_| "a claimed history does not follow".to_string())?;
                        }
                        if all.len() as u64 != c.end {
                            return Err("claim end is not the history's end".into());
                        }
                        wr.claim = Some((c.claim, tip));
                    }
                    Err(Error::Corrupt(_)) => {
                        let all = read_all(&mut wr.sim)?;
                        let mut tip = Tip::origin();
                        let bad = all.iter().position(|e| match group::follow(&tip, e) {
                            Ok(t) => {
                                tip = t;
                                false
                            }
                            Err(_) => true,
                        });
                        let p = bad.ok_or("recovery refused a history that follows")? as u64 + 1;
                        if matches!(ledger.get(&p), Some((_, _, true))) {
                            return Err(format!("recovery refused the acknowledged entry at {p}"));
                        }
                    }
                    Err(_) => {}
                }
            }
            2..=6 => {
                let wr = &mut writers[w];
                let Some((c, tip)) = wr.claim.as_mut() else {
                    continue;
                };
                tag += 1;
                let e = entry(tip, tag, 1 + rng.below(4) as u32);
                let from = c.end();
                match wr.sim.advance(c, from, e.clone(), at) {
                    Advanced::Advanced(p) => {
                        if p != from + 1 {
                            return Err(format!("advanced from {from} to {p}"));
                        }
                        owe(&mut ledger, p, &e, at.0, true)?;
                        *tip = group::follow(tip, &e).unwrap();
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
                let Some((c, _)) = wr.claim.as_mut() else {
                    continue;
                };
                if let Ok(d) = wr.sim.durable(c, at) {
                    let held: Vec<(u64, Vec<u8>)> = ledger
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
