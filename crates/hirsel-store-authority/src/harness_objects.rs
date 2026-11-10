//! The object-storage law, run against `authority.objects@1` over the simulated
//! bucket (the place-authority-674 model's search, on the real provider).
//!
//! Racing writers, each on its own handle and thread, claim, advance, reclaim,
//! check durability and read; exactly one runs at a time, and before every
//! request the turn passes to a writer chosen by the seed, so one writer's
//! advance can be cut between its group put and its head replace by another's
//! claim or reclamation. The bucket refuses requests, loses responses after
//! their effect and returns incomplete listings. The oracle is a ledger of
//! acknowledged advances, kept apart from the provider:
//!
//! 1. no position is acknowledged with two different entries (so a fenced
//!    writer never acknowledges);
//! 2. a read returns at each position the bytes acknowledged there;
//! 3. after the race, a fresh handle recovers the history, whatever the
//!    listings omit, and every acknowledged entry is in it with its object;
//! 4. no claim, read or recovery refuses the history as corrupt: nothing here
//!    damages it.
//!
//! Each planted defect must break a law.

use crate::group::{self, Tip};
use crate::harness::{Rng, entry};
use crate::objects::{Bug, Objects};
use crate::sim::Bucket;
use crate::{Advanced, Authority, Claim, Error, Level};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

/// One running thread at a time; the turn moves at every request.
struct Baton {
    state: Mutex<(usize, Vec<usize>, Rng)>,
    turned: Condvar,
}

thread_local! {
    static ME: Cell<usize> = const { Cell::new(usize::MAX) };
}

impl Baton {
    fn wait(&self, me: usize) {
        let mut s = self.state.lock().unwrap();
        while s.0 != me {
            s = self.turned.wait(s).unwrap();
        }
    }

    /// Give the turn to a seeded choice among the live writers, and wait for
    /// it to come back.
    fn pass(&self) {
        let me = ME.get();
        if me == usize::MAX {
            return;
        }
        let mut s = self.state.lock().unwrap();
        let n = s.1.len() as u64;
        let i = s.2.below(n) as usize;
        s.0 = s.1[i];
        self.turned.notify_all();
        while s.0 != me {
            s = self.turned.wait(s).unwrap();
        }
    }

    fn exit(&self, me: usize) {
        let mut s = self.state.lock().unwrap();
        s.1.retain(|w| *w != me);
        if !s.1.is_empty() {
            let n = s.1.len() as u64;
            let i = s.2.below(n) as usize;
            s.0 = s.1[i];
        }
        self.turned.notify_all();
    }
}

/// The whole history, reading on from what each read returned and retrying
/// refused reads.
fn read_all(h: &mut Objects) -> Result<Vec<Vec<u8>>, String> {
    let mut all = Vec::new();
    let mut refused = 0;
    loop {
        match h.read(all.len() as u64, Level(0), u64::MAX) {
            Ok(more) if more.is_empty() => return Ok(all),
            Ok(more) => all.extend(more),
            Err(Error::Refused(_)) if refused < 1000 => refused += 1,
            Err(e) => return Err(format!("read: {e:?}")),
        }
    }
}

type Ledger = Arc<Mutex<BTreeMap<u64, Vec<u8>>>>;

fn writer(
    me: usize,
    seed: u64,
    steps: u32,
    bucket: Arc<Bucket>,
    bug: Option<Bug>,
    ledger: Ledger,
) -> Result<(), String> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9) ^ (me as u64 + 1) | 1);
    let mut h = Objects::new(bucket, "zone".into(), Level(0));
    h.bug = bug;
    let mut held: Option<(Claim, Tip)> = None;
    let mut n = 0u64;
    for _ in 0..steps {
        match rng.below(10) {
            0 | 1 => match h.claim((rng.next() as u128).to_le_bytes(), [7; 32]) {
                Ok(c) => {
                    let all = read_all(&mut h)?;
                    let mut tip = Tip::origin();
                    for e in all.iter().take(c.end as usize) {
                        tip = group::follow(&tip, e)
                            .map_err(|_| "a claimed history does not follow".to_string())?;
                    }
                    held = Some((c.claim, tip));
                }
                Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                    return Err(format!("claim refused the history: {e:?}"));
                }
                Err(_) => {}
            },
            2..=6 => {
                let Some((c, tip)) = held.as_mut() else {
                    continue;
                };
                n += 1;
                let e = entry(tip, (me as u64) << 32 | n);
                let from = c.end();
                match h.advance(c, from, e.clone(), Level(0)) {
                    Advanced::Advanced(p) => {
                        if p != from + 1 {
                            return Err(format!("advanced from {from} to {p}"));
                        }
                        let mut l = ledger.lock().unwrap();
                        if l.get(&p).is_some_and(|b| *b != e) {
                            return Err(format!("position {p} acknowledged twice"));
                        }
                        l.insert(p, e.clone());
                        *tip = group::follow(tip, &e).unwrap();
                    }
                    Advanced::Refused(_) => {}
                    Advanced::Fenced | Advanced::Undecided(_) => held = None,
                }
            }
            7 => {
                let Some((c, _)) = held.as_mut() else {
                    continue;
                };
                match h.reclaim(c) {
                    Ok(()) => {}
                    Err(Error::Fenced) => held = None,
                    Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                        return Err(format!("reclaim refused the history: {e:?}"));
                    }
                    Err(_) => {}
                }
            }
            8 => {
                if let Some((c, _)) = held.as_mut()
                    && let Err(Error::Fenced) = h.durable(c, Level(0))
                {
                    held = None;
                }
            }
            _ => {
                let all = read_all(&mut h)?;
                let l = ledger.lock().unwrap();
                for (p, b) in l.range(..=all.len() as u64) {
                    if all[*p as usize - 1] != *b {
                        return Err(format!("a read returned other bytes at {p}"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// One schedule; `Err` names the first broken law.
fn schedule(seed: u64, writers: usize, steps: u32, bug: Option<Bug>) -> Result<(), String> {
    let mut rng = Rng(seed | 1);
    let mut faults = BTreeMap::new();
    for f in crate::sim::OBJECT_FAULTS {
        faults.insert(f.to_string(), 3 + rng.below(10));
    }
    let bucket = Arc::new(Bucket::new(seed, faults));
    let baton = Arc::new(Baton {
        state: Mutex::new((0, (0..writers).collect(), Rng(seed ^ 0x5bd1_e995 | 1))),
        turned: Condvar::new(),
    });
    let b = baton.clone();
    *bucket.before.lock().unwrap() = Some(Arc::new(move || b.pass()));
    let ledger: Ledger = Arc::default();
    let results: Vec<Result<(), String>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..writers)
            .map(|me| {
                let (bucket, baton, ledger) = (bucket.clone(), baton.clone(), ledger.clone());
                s.spawn(move || {
                    ME.set(me);
                    baton.wait(me);
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        writer(me, seed, steps, bucket, bug, ledger)
                    }))
                    .unwrap_or_else(|_| Err(format!("writer {me} panicked")));
                    baton.exit(me);
                    r
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for r in results {
        r?;
    }
    // Recovery by a fresh handle, through the same faults.
    *bucket.before.lock().unwrap() = None;
    let mut fresh = Objects::new(bucket.clone(), "zone".into(), Level(0));
    fresh.bug = bug;
    let all = read_all(&mut fresh).map_err(|e| format!("recovery: {e}"))?;
    for (p, b) in ledger.lock().unwrap().iter() {
        if all.get(*p as usize - 1) != Some(b) {
            return Err(format!(
                "acknowledged position {p} is not in the recovered history"
            ));
        }
    }
    Ok(())
}

fn sweep(bug: Option<Bug>, seeds: u64) -> Option<String> {
    (1..=seeds).find_map(|seed| {
        schedule(seed * 7919, 3, 120, bug)
            .err()
            .map(|e| format!("seed {seed}: {e}"))
    })
}

#[test]
fn object_storage_law_holds() {
    assert_eq!(sweep(None, 200), None);
}

#[test]
fn every_planted_defect_breaks_the_object_storage_law() {
    for bug in [Bug::NoCas, Bug::AnyEpoch, Bug::TrustListing] {
        assert!(sweep(Some(bug), 400).is_some(), "{bug:?} went unnoticed");
    }
}

/// The cost the spec states: an advance is two requests (the group put and the
/// head replace), and recovering `n` groups is a head get, one listing page and
/// `n` group gets.
#[test]
fn an_advance_is_two_requests_and_recovery_gets_each_group_once() {
    let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
    let mut h = Objects::new(bucket.clone(), "zone".into(), Level(0));
    let mut c = h.claim([1; 16], [7; 32]).unwrap().claim;
    let mut tip = group::follow(&Tip::origin(), &read_all(&mut h).unwrap()[0]).unwrap();
    for tag in 0..20 {
        let e = entry(&tip, tag);
        let before = bucket.requests();
        let from = c.end();
        assert_eq!(
            h.advance(&mut c, from, e.clone(), Level(0)),
            Advanced::Advanced(from + 1)
        );
        assert_eq!(bucket.requests() - before, 2);
        tip = group::follow(&tip, &e).unwrap();
    }
    let before = bucket.requests();
    let mut fresh = Objects::new(bucket.clone(), "zone".into(), Level(0));
    let all = fresh.read(0, Level(0), u64::MAX).unwrap();
    assert_eq!(all.len(), 21);
    assert_eq!(bucket.requests() - before, 1 + 1 + 21);
}
