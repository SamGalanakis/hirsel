//! The object-storage law, run against `authority.objects@1` over the simulated
//! bucket (the place-authority-674 model's search, on the real provider).
//!
//! Racing writers, each on its own handle and thread, claim with fresh
//! identities, append windows of one to three entries of one to four commits,
//! reclaim, check durability and read; exactly one runs at a time, and before
//! every request the turn passes to a writer chosen by the seed, so one
//! writer's append can be cut between its record puts and its head replace by
//! another's claim or reclamation. The bucket refuses requests, loses responses
//! after their effect and returns incomplete listings. The oracle is a ledger
//! of acknowledged entries, kept apart from the provider:
//!
//! 1. no position is acknowledged with two different entries (so a fenced
//!    writer never acknowledges);
//! 2. a read returns at each position the bytes acknowledged there;
//! 3. after the race, a fresh handle recovers the history, whatever the
//!    listings omit, and every acknowledged entry is in it with its object;
//! 4. no claim, read or recovery refuses the history as corrupt: nothing here
//!    damages it;
//! 5. a claim that died (`Fenced`, `Undecided`) is `Fenced` when used again;
//! 6. an entry answered `Fenced` (nothing was appended) is not in the
//!    recovered history;
//! 7. the recovered base keeps `harness_log`'s law 8: a selection made it, at
//!    or above every acknowledged one, with the tip and live objects of the
//!    records below it.
//!
//! Each planted defect must break a law.
//!
//! The same schedules run over two media: the simulated bucket, and
//! `objects.s3@1` over the fake S3 server (`hirsel_object_storage::fake_s3`)
//! drawing the same three faults from the same seed, each writer on its own
//! provider handle and connections. Over S3 the turn passes before every
//! provider call rather than every request, since a `get_many` sends its gets
//! at once. A third medium, the simulated bucket with late writes, holds back
//! a seeded share of puts and replaces: the writer hears `Unavailable`, and
//! the write lands (its condition checked then) just before a later request of
//! any writer, as an unanswered request that arrives late does.

use crate::harness::{Rng, append, entry, stays_dead, window};
use crate::objects::{Bug, Objects};
use crate::sim::Bucket;
use crate::{Advanced, Authority, Claim, Error, Level, Record};
use hirsel_object_storage::fake_s3::FakeS3;
use hirsel_object_storage::{Condition, Listing, ObjectStorage, Put, Replaced, StorageError, Tag};
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
fn read_all(h: &mut Objects) -> Result<Vec<Record>, String> {
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

type Ledger = Arc<Mutex<BTreeMap<u64, Record>>>;
/// The records of entries answered `Fenced`.
type Denied = Arc<Mutex<Vec<Record>>>;
type Hook = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;
/// Every selection tried, and the highest `first` one acknowledged.
type Selections = Arc<Mutex<(Vec<(u64, Vec<u8>)>, u64)>>;

/// Where a schedule's history lives.
#[derive(Clone, Copy, Debug)]
enum Medium {
    Sim,
    S3,
    /// The simulated bucket behind [`Late`].
    Late,
}

/// A write [`Late`] holds back.
enum HeldWrite {
    Put(String, Vec<u8>),
    Replace(String, Vec<u8>, Condition),
}

/// The simulated bucket, one handle shared by every writer, that holds back
/// one put or replace in four: the caller hears `Unavailable` at once, and the
/// write lands just before the call one to four calls later (or before
/// recovery), whoever makes it.
struct Late {
    inner: Arc<Bucket>,
    state: Mutex<(u64, Rng, Vec<(u64, HeldWrite)>)>,
}

impl Late {
    /// Count a call and land the held writes now due; outside the lock, since
    /// landing is a bucket request and may pass the turn.
    fn call(&self) {
        let due: Vec<HeldWrite> = {
            let mut s = self.state.lock().unwrap();
            s.0 += 1;
            let now = s.0;
            let (due, keep) = std::mem::take(&mut s.2)
                .into_iter()
                .partition(|(at, _)| *at <= now);
            s.2 = keep;
            due.into_iter().map(|(_, w)| w).collect()
        };
        for w in due {
            self.land(w);
        }
    }

    /// Land a held write, retrying past the bucket's own faults: a late write
    /// lands exactly once or never, and here it lands.
    fn land(&self, w: HeldWrite) {
        for _ in 0..1000 {
            let r = match &w {
                HeldWrite::Put(k, b) => self.inner.put_new(k, b).map(|_| ()),
                HeldWrite::Replace(k, b, c) => self.inner.replace(k, b, c.clone()).map(|_| ()),
            };
            match r {
                // A lost response after the effect: landed.
                Err(StorageError::Unavailable(e)) if e.contains("lost response") => return,
                Err(_) => {}
                Ok(()) => return,
            }
        }
    }

    /// Whether to hold this write back.
    fn hold(&self, w: HeldWrite) -> Result<(), HeldWrite> {
        let mut s = self.state.lock().unwrap();
        if s.1.below(4) != 0 {
            return Err(w);
        }
        let at = s.0 + 1 + s.1.below(4);
        s.2.push((at, w));
        Ok(())
    }

    /// Land every write still held.
    fn flush(&self) {
        let held = std::mem::take(&mut self.state.lock().unwrap().2);
        for (_, w) in held {
            self.land(w);
        }
    }
}

fn held_back() -> StorageError {
    StorageError::Unavailable("simulated late write".into())
}

impl ObjectStorage for Late {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        self.call();
        self.inner.get_tagged(key)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.call();
        self.inner.get(key)
    }
    fn get_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>, StorageError>> {
        self.call();
        self.inner.get_many(keys)
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        self.call();
        match self.hold(HeldWrite::Put(key.into(), bytes.to_vec())) {
            Ok(()) => Err(held_back()),
            Err(_) => self.inner.put_new(key, bytes),
        }
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        self.call();
        match self.hold(HeldWrite::Replace(
            key.into(),
            bytes.to_vec(),
            condition.clone(),
        )) {
            Ok(()) => Err(held_back()),
            Err(_) => self.inner.replace(key, bytes, condition),
        }
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        self.call();
        self.inner.list(prefix, after)
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.call();
        self.inner.delete(key)
    }
}

/// A provider handle that runs the scheduler hook before each call.
struct Turned {
    inner: Arc<dyn ObjectStorage>,
    before: Hook,
}

impl Turned {
    fn turn(&self) {
        let f = self.before.lock().unwrap().clone();
        if let Some(f) = f {
            f();
        }
    }
}

impl ObjectStorage for Turned {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        self.turn();
        self.inner.get_tagged(key)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.turn();
        self.inner.get(key)
    }
    fn get_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>, StorageError>> {
        self.turn();
        self.inner.get_many(keys)
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        self.turn();
        self.inner.put_new(key, bytes)
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        self.turn();
        self.inner.replace(key, bytes, condition)
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        self.turn();
        self.inner.list(prefix, after)
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.turn();
        self.inner.delete(key)
    }
}

/// One schedule's storage: a fresh handle per writer, and the hook the
/// scheduler interleaves them with.
struct Media {
    bucket: Option<Arc<Bucket>>,
    late: Option<Arc<Late>>,
    fake: Option<(FakeS3, tempfile::TempDir)>,
    hook: Hook,
}

impl Media {
    fn new(medium: Medium, seed: u64, faults: BTreeMap<String, u64>) -> Media {
        match medium {
            Medium::Sim | Medium::Late => {
                let bucket = Arc::new(Bucket::new(seed, faults));
                let hook: Hook = Arc::default();
                let h = hook.clone();
                *bucket.before.lock().unwrap() = Some(Arc::new(move || {
                    let f = h.lock().unwrap().clone();
                    if let Some(f) = f {
                        f();
                    }
                }));
                let late = matches!(medium, Medium::Late).then(|| {
                    Arc::new(Late {
                        inner: bucket.clone(),
                        state: Mutex::new((0, Rng(seed ^ 0x1a7e | 1), Vec::new())),
                    })
                });
                Media {
                    bucket: Some(bucket),
                    late,
                    fake: None,
                    hook,
                }
            }
            Medium::S3 => Media {
                bucket: None,
                late: None,
                fake: Some((FakeS3::start(seed, faults), tempfile::tempdir().unwrap())),
                hook: Arc::default(),
            },
        }
    }

    fn handle(&self) -> Arc<dyn ObjectStorage> {
        if let Some(l) = &self.late {
            return l.clone();
        }
        match (&self.bucket, &self.fake) {
            (Some(b), _) => b.clone(),
            (_, Some((fake, dir))) => {
                // Opening probes the server; keep its requests out of the
                // fault stream's way by opening with faults paused.
                let paused = std::mem::take(&mut fake.world.lock().unwrap().faults);
                let opened = hirsel_object_storage::open(&fake.entry(dir.path(), "zone", 5000));
                fake.world.lock().unwrap().faults = paused;
                Arc::new(Turned {
                    inner: opened.expect("objects.s3@1 opens on the fake"),
                    before: self.hook.clone(),
                })
            }
            _ => unreachable!(),
        }
    }
}

fn writer(
    me: usize,
    seed: u64,
    steps: u32,
    store: Arc<dyn ObjectStorage>,
    bug: Option<Bug>,
    ledger: Ledger,
    denied: Denied,
    selections: Selections,
) -> Result<(), String> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9) ^ (me as u64 + 1) | 1);
    let mut h = Objects::new(store, "zone".into(), Level(0));
    h.bug = bug;
    let mut held: Option<Claim> = None;
    let mut n = 0u64;
    for _ in 0..steps {
        match rng.below(11) {
            0 | 1 => {
                let attempt = (rng.next() as u128).to_le_bytes();
                match h.claim(attempt, [rng.next() as u8; 32]) {
                    Ok(c) => {
                        // The history the writer now owns ends at its own claim,
                        // unless a later claim has already passed it.
                        let all = read_all(&mut h)?;
                        if !matches!(all.get(c.end as usize - 1), Some(Record::Claimed { attempt: a, .. }) if *a == attempt)
                        {
                            return Err(format!("position {} is not the claim's own", c.end));
                        }
                        held = Some(c.claim);
                    }
                    Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                        return Err(format!("claim refused the history: {e:?}"));
                    }
                    Err(_) => {}
                }
            }
            2..=6 => {
                let Some(c) = held.as_mut() else {
                    continue;
                };
                let entries: Vec<(Vec<u8>, Level)> = (0..1 + rng.below(3))
                    .map(|_| {
                        n += 1;
                        (
                            entry((me as u64) << 32 | n, 1 + rng.below(4) as u32),
                            Level(0),
                        )
                    })
                    .collect();
                let newest = rng.below(2) == 0;
                let mut dead = false;
                for ((e, _), (p, answer)) in
                    entries.iter().zip(window(&mut h, c, &entries, newest)?)
                {
                    let record = Record::Appended {
                        payload: e.clone(),
                        created: vec![],
                        retired: vec![],
                    };
                    match answer {
                        Advanced::Advanced(_) => {
                            let mut l = ledger.lock().unwrap();
                            if l.get(&p).is_some_and(|b| *b != record) {
                                return Err(format!("position {p} acknowledged twice"));
                            }
                            l.insert(p, record);
                        }
                        Advanced::Fenced => {
                            denied.lock().unwrap().push(record);
                            dead = true;
                        }
                        Advanced::Refused(_) | Advanced::Undecided(_) => dead = true,
                    }
                }
                if dead {
                    // A dead claim stays dead: used again, it is fenced.
                    if !stays_dead(&mut h, c, Level(0)) {
                        return Err("a dead claim was used again".into());
                    }
                    held = None;
                }
            }
            7 => {
                let Some(c) = held.as_mut() else {
                    continue;
                };
                match h.reclaim(c, 64) {
                    Ok(_) => {}
                    Err(Error::Fenced) => held = None,
                    Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                        return Err(format!("reclaim refused the history: {e:?}"));
                    }
                    Err(_) => {}
                }
            }
            8 => {
                if let Some(c) = held.as_mut()
                    && let Err(Error::Fenced) = h.durable(c, Level(0))
                {
                    held = None;
                }
            }
            10 => {
                let Some(c) = held.as_mut() else {
                    continue;
                };
                n += 1;
                let first = 2 + rng.below(c.end());
                let boot = entry((me as u64) << 32 | n, 1);
                selections.lock().unwrap().0.push((first, boot.clone()));
                match h.select(c, first, boot) {
                    Ok(()) => {
                        let mut s = selections.lock().unwrap();
                        s.1 = s.1.max(first);
                    }
                    Err(e @ (Error::Corrupt(_) | Error::Format { .. })) => {
                        return Err(format!("select refused the history: {e:?}"));
                    }
                    Err(Error::Refused(_)) => {}
                    Err(_) => held = None,
                }
            }
            _ => {
                let all = read_all(&mut h)?;
                let l = ledger.lock().unwrap();
                for (p, b) in l.range(..=all.len() as u64) {
                    if all[*p as usize - 1] != *b {
                        return Err(format!("a read returned another record at {p}"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// One schedule; `Err` names the first broken law.
fn schedule(
    medium: Medium,
    seed: u64,
    writers: usize,
    steps: u32,
    bug: Option<Bug>,
) -> Result<(), String> {
    let mut rng = Rng(seed | 1);
    let mut faults = BTreeMap::new();
    for f in crate::sim::OBJECT_FAULTS {
        faults.insert(f.to_string(), 3 + rng.below(10));
    }
    let media = Media::new(medium, seed, faults);
    let baton = Arc::new(Baton {
        state: Mutex::new((0, (0..writers).collect(), Rng(seed ^ 0x5bd1_e995 | 1))),
        turned: Condvar::new(),
    });
    let b = baton.clone();
    *media.hook.lock().unwrap() = Some(Arc::new(move || b.pass()));
    let ledger: Ledger = Arc::default();
    let denied: Denied = Arc::default();
    let selections: Selections = Arc::default();
    let results: Vec<Result<(), String>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..writers)
            .map(|me| {
                let (store, baton, ledger, denied, selections) = (
                    media.handle(),
                    baton.clone(),
                    ledger.clone(),
                    denied.clone(),
                    selections.clone(),
                );
                s.spawn(move || {
                    ME.set(me);
                    baton.wait(me);
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        writer(me, seed, steps, store, bug, ledger, denied, selections)
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
    *media.hook.lock().unwrap() = None;
    if let Some(l) = &media.late {
        l.flush();
    }
    let mut fresh = Objects::new(media.handle(), "zone".into(), Level(0));
    fresh.bug = bug;
    let all = read_all(&mut fresh).map_err(|e| format!("recovery: {e}"))?;
    for (p, b) in ledger.lock().unwrap().iter() {
        if all.get(*p as usize - 1) != Some(b) {
            return Err(format!(
                "acknowledged position {p} is not in the recovered history"
            ));
        }
    }
    for e in denied.lock().unwrap().iter() {
        if let Some(p) = all.iter().position(|a| a == e) {
            return Err(format!(
                "an entry answered Fenced is at position {} of the recovered history",
                p + 1
            ));
        }
    }
    let (tried, selected) = &*selections.lock().unwrap();
    // A refused read of the base decides nothing; retry it as reads are.
    let mut got = fresh.base();
    for _ in 0..1000 {
        if !matches!(got, Err(Error::Refused(_))) {
            break;
        }
        got = fresh.base();
    }
    crate::harness_log::check_base(&mut fresh, &all, got, tried, *selected)
        .map_err(|e| format!("recovery: {e}"))
}

fn sweep(medium: Medium, bug: Option<Bug>, seeds: u64) -> Option<String> {
    (1..=seeds).find_map(|seed| {
        schedule(medium, seed * 7919, 3, 120, bug)
            .err()
            .map(|e| format!("seed {seed}: {e}"))
    })
}

#[test]
fn object_storage_law_holds() {
    assert_eq!(sweep(Medium::Sim, None, 200), None);
}

#[test]
fn every_planted_defect_breaks_the_object_storage_law() {
    for bug in [Bug::NoCas, Bug::AnyEpoch, Bug::TrustListing, Bug::DropBase] {
        assert!(
            sweep(Medium::Sim, Some(bug), 400).is_some(),
            "{bug:?} went unnoticed"
        );
    }
}

#[test]
fn object_storage_law_holds_with_late_writes() {
    assert_eq!(sweep(Medium::Late, None, 200), None);
}

#[test]
fn object_storage_law_holds_over_s3() {
    assert_eq!(sweep(Medium::S3, None, 200), None);
}

#[test]
fn every_planted_defect_breaks_the_object_storage_law_over_s3() {
    for bug in [Bug::NoCas, Bug::AnyEpoch, Bug::TrustListing] {
        assert!(
            sweep(Medium::S3, Some(bug), 400).is_some(),
            "{bug:?} went unnoticed over S3"
        );
    }
}

/// The spec's cost over `objects.s3@1`, counted by the server: a window of `k`
/// entries is `k` record PUTs and one head replace, `durable` one request, and
/// recovering `n` records one head GET, `ceil(n / 1000)` listing pages and `n`
/// record GETs.
#[test]
fn over_s3_a_window_is_a_put_each_and_one_replace_and_recovery_gets_each_record_once() {
    let media = Media::new(Medium::S3, 1, BTreeMap::new());
    let fake = &media.fake.as_ref().unwrap().0;
    let mut h = Objects::new(media.handle(), "zone".into(), Level(0));
    let mut c = h.claim([1; 16], [7; 32]).unwrap().claim;
    for w in 0..250 {
        let entries: Vec<_> = (0..4).map(|i| (entry(w * 4 + i, 1), Level(0))).collect();
        let before = fake.requests();
        let all = window(&mut h, &mut c, &entries, w % 2 == 0).unwrap();
        assert!(all.iter().all(|(_, a)| matches!(a, Advanced::Advanced(_))));
        assert_eq!(fake.requests() - before, 4 + 1);
    }
    let before = fake.requests();
    assert_eq!(h.durable(&mut c, Level(0)), Ok(1001));
    assert_eq!(fake.requests() - before, 1);
    let mut fresh = Objects::new(media.handle(), "zone".into(), Level(0));
    let before = fake.requests();
    let all = fresh.read(0, Level(0), u64::MAX).unwrap();
    assert_eq!(all.len(), 1001);
    assert_eq!(fake.requests() - before, 1 + 2 + 1001);
}

/// The cost the spec states: a window of `k` entries is `k + 1` requests (a put
/// per record and one head replace), and recovering `n` records is a head get,
/// one listing page and `n` record gets.
#[test]
fn a_window_is_a_put_each_and_one_replace_and_recovery_gets_each_record_once() {
    let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
    let mut h = Objects::new(bucket.clone(), "zone".into(), Level(0));
    let mut c = h.claim([1; 16], [7; 32]).unwrap().claim;
    let mut tag = 0;
    for k in [1, 2, 3, 4, 5, 5] {
        let entries: Vec<_> = (0..k)
            .map(|_| {
                tag += 1;
                (entry(tag, 1), Level(0))
            })
            .collect();
        let before = bucket.requests();
        let all = window(&mut h, &mut c, &entries, k % 2 == 0).unwrap();
        assert!(all.iter().all(|(_, a)| matches!(a, Advanced::Advanced(_))));
        assert_eq!(bucket.requests() - before, k + 1);
    }
    let before = bucket.requests();
    let mut fresh = Objects::new(bucket.clone(), "zone".into(), Level(0));
    let all = fresh.read(0, Level(0), u64::MAX).unwrap();
    assert_eq!(all.len(), 21);
    assert_eq!(bucket.requests() - before, 1 + 1 + 21);
}

/// What a scripted head request does, in script order; an empty script passes.
enum Act {
    Pass,
    /// Unanswered: the caller hears `Unavailable` and the write is held for
    /// [`Scripted::land`].
    Hold,
    /// The head read fails.
    Fail,
    /// Run the closure (another writer's requests), then pass.
    Before(Box<dyn FnOnce() + Send>),
}

/// The simulated bucket with a script for one writer's head requests: the H2
/// cases (STORE-11's Advance outcome) built request by request.
struct Scripted {
    inner: Arc<Bucket>,
    replaces: Mutex<std::collections::VecDeque<Act>>,
    reads: Mutex<std::collections::VecDeque<Act>>,
    held: Mutex<Vec<(Vec<u8>, Condition)>>,
}

impl Scripted {
    fn new(inner: Arc<Bucket>) -> Arc<Scripted> {
        Arc::new(Scripted {
            inner,
            replaces: Mutex::default(),
            reads: Mutex::default(),
            held: Mutex::default(),
        })
    }

    /// Script the head requests from now on.
    fn arm(&self, replaces: Vec<Act>, reads: Vec<Act>) {
        *self.replaces.lock().unwrap() = replaces.into();
        *self.reads.lock().unwrap() = reads.into();
    }

    /// Land every held replace, each under its own condition, as a late
    /// request does.
    fn land(&self) {
        for (bytes, condition) in std::mem::take(&mut *self.held.lock().unwrap()) {
            let _ = self.inner.replace("head", &bytes, condition);
        }
    }
}

impl ObjectStorage for Scripted {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        if key == "head" {
            match self.reads.lock().unwrap().pop_front() {
                Some(Act::Fail) => return Err(StorageError::Unavailable("scripted".into())),
                Some(Act::Before(f)) => f(),
                _ => {}
            }
        }
        self.inner.get_tagged(key)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.get(key)
    }
    fn get_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>, StorageError>> {
        self.inner.get_many(keys)
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        self.inner.put_new(key, bytes)
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        if key == "head" {
            match self.replaces.lock().unwrap().pop_front() {
                Some(Act::Hold) => {
                    self.held.lock().unwrap().push((bytes.to_vec(), condition));
                    return Err(held_back());
                }
                Some(Act::Before(f)) => f(),
                _ => {}
            }
        }
        self.inner.replace(key, bytes, condition)
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        self.inner.list(prefix, after)
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key)
    }
}

/// Writer A, claimed over `s`, about to append from its claim with `e`.
fn writer_a(s: &Arc<Scripted>) -> (Objects, Claim, Vec<u8>) {
    let mut h = Objects::new(s.clone(), "zone".into(), Level(0));
    let c = h.claim([1; 16], [7; 32]).unwrap().claim;
    (h, c, entry(1, 1))
}

/// Writer B claims the bucket on its own handle.
fn b_claims(bucket: &Arc<Bucket>) {
    let mut b = Objects::new(bucket.clone(), "zone".into(), Level(0));
    b.claim([2; 16], [7; 32]).unwrap();
}

fn recovered(bucket: &Arc<Bucket>) -> Vec<Record> {
    read_all(&mut Objects::new(bucket.clone(), "zone".into(), Level(0))).unwrap()
}

/// H2 case 1, our digest at p: A's first replace goes unanswered, its retry
/// is answered with a mismatch because the late first replace landed, then
/// either nothing else happened (the head is A's own at p) or B claimed (the
/// chain holds A's record at p). Either way A is `Advanced`, though the
/// request that told it something was answered, and for a window of entries
/// p is the window's last position, which every entry in it is answered at.
#[test]
fn an_append_whose_entry_is_at_its_position_under_a_later_claim_is_advanced() {
    for (k, later) in [(1, true), (2, true), (1, false), (2, false)] {
        let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
        let s = Scripted::new(bucket.clone());
        let (mut h, mut c, _) = writer_a(&s);
        let (s2, b2) = (s.clone(), bucket.clone());
        let lands = Box::new(move || {
            s2.land();
            if later {
                b_claims(&b2);
            }
        });
        s.arm(vec![Act::Hold, Act::Before(lands)], vec![]);
        let entries: Vec<_> = (0..k).map(|i| (entry(10 + i, 1), Level(0))).collect();
        let all = window(&mut h, &mut c, &entries, true).unwrap();
        let history = recovered(&bucket);
        for ((e, _), (p, answer)) in entries.iter().zip(all) {
            assert_eq!(answer, Advanced::Advanced(p), "k {k}, later {later}");
            assert_eq!(
                history[p as usize - 1],
                Record::Appended {
                    payload: e.clone(),
                    created: vec![],
                    retired: vec![]
                }
            );
        }
    }
}

/// H2 case 2, another digest at p: B claims before A's replace (answered
/// mismatch), or while A's replace is unanswered (that request, conditional
/// on a tag the head has left for good, lands later with no effect). Both
/// are `Fenced`, and A's entry is never in the history.
#[test]
fn an_append_whose_position_holds_another_entry_is_fenced() {
    for held in [false, true] {
        let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
        let s = Scripted::new(bucket.clone());
        let (mut h, mut c, e) = writer_a(&s);
        let b2 = bucket.clone();
        let b_claims: Box<dyn FnOnce() + Send> = Box::new(move || b_claims(&b2));
        if held {
            s.arm(vec![Act::Hold], vec![Act::Before(b_claims)]);
        } else {
            s.arm(vec![Act::Before(b_claims)], vec![]);
        }
        assert_eq!(
            append(&mut h, &mut c, e.clone(), Level(0)),
            Advanced::Fenced,
            "held {held}"
        );
        s.land();
        assert!(
            !recovered(&bucket).contains(&Record::Appended {
                payload: e,
                created: vec![],
                retired: vec![]
            }),
            "held {held}"
        );
    }
}

/// H2 case 3, p unproved: with the head unchanged (every replace unanswered)
/// or unreadable after an unanswered replace, the answer is `Undecided`,
/// never `Fenced` or `Refused`, and the doubt outlives a later answered
/// mismatch: here the late replace lands, so A's entry is in the history.
#[test]
fn an_append_whose_absence_is_unproved_is_undecided_across_retries() {
    let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
    let s = Scripted::new(bucket.clone());
    let (mut h, mut c, e) = writer_a(&s);
    s.arm(vec![Act::Hold, Act::Hold, Act::Hold], vec![]);
    let from = c.end();
    assert!(matches!(
        append(&mut h, &mut c, e.clone(), Level(0)),
        Advanced::Undecided(_)
    ));
    s.land();
    assert_eq!(
        recovered(&bucket)[from as usize],
        Record::Appended {
            payload: e,
            created: vec![],
            retired: vec![]
        }
    );

    let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
    let s = Scripted::new(bucket.clone());
    let (mut h, mut c, e) = writer_a(&s);
    let s2 = s.clone();
    s.arm(
        vec![Act::Hold, Act::Before(Box::new(move || s2.land()))],
        vec![Act::Pass, Act::Fail],
    );
    let from = c.end();
    assert!(matches!(
        append(&mut h, &mut c, e.clone(), Level(0)),
        Advanced::Undecided(_)
    ));
    assert_eq!(
        recovered(&bucket)[from as usize],
        Record::Appended {
            payload: e,
            created: vec![],
            retired: vec![]
        }
    );
}

/// A claim after a lost reply names the append the head replace selected
/// (`appended`, ruling 6), in both orders: A appends P, then X with its first
/// replace unanswered. Landed before B claims, the late replace selects X and
/// B's claim names X; B claiming first leaves X an orphan object after P, B's
/// claim names P, and X is never returned: `at` reads back from the locator it
/// is given, so `at(P)` answers the records up to P whatever objects follow it
/// (ruling 3).
#[test]
fn a_claim_after_a_lost_reply_names_the_append_its_replace_selected() {
    let appended = |payload: &[u8]| Record::Appended {
        payload: payload.to_vec(),
        created: vec![],
        retired: vec![],
    };
    for landed_first in [true, false] {
        let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
        let s = Scripted::new(bucket.clone());
        let (mut h, mut c, p) = writer_a(&s);
        assert_eq!(
            append(&mut h, &mut c, p.clone(), Level(0)),
            Advanced::Advanced(2)
        );
        let x = entry(99, 1);
        let (s2, b2) = (s.clone(), bucket.clone());
        let answer = if landed_first {
            let lands = Box::new(move || {
                s2.land();
                b_claims(&b2);
            });
            s.arm(vec![Act::Hold, Act::Before(lands)], vec![]);
            append(&mut h, &mut c, x.clone(), Level(0))
        } else {
            s.arm(
                vec![Act::Hold],
                vec![Act::Before(Box::new(move || b_claims(&b2)))],
            );
            let a = append(&mut h, &mut c, x.clone(), Level(0));
            s.land();
            a
        };
        let mut fresh = Objects::new(bucket.clone(), "zone".into(), Level(0));
        let history = read_all(&mut fresh).unwrap();
        let newest = if landed_first { 3 } else { 2 };
        assert_eq!(
            answer,
            if landed_first {
                Advanced::Advanced(3)
            } else {
                Advanced::Fenced
            }
        );
        assert_eq!(
            history[newest as usize - 1],
            appended(if landed_first { &x } else { &p })
        );
        assert!(
            matches!(history.last(), Some(Record::Claimed { .. })),
            "B's claim is last"
        );
        assert_eq!(history.len() as u64, newest + 1);
        // A cold reader: `last` is the append B's claim names.
        let mut cold = Objects::new(bucket.clone(), "zone".into(), Level(0));
        let l = cold.last(Level(0)).unwrap().expect("an append");
        assert_eq!(
            l,
            fresh.followed_locator(newest).unwrap(),
            "landed first {landed_first}"
        );
        let p_at = fresh.followed_locator(2).unwrap();
        let mut cold = Objects::new(bucket.clone(), "zone".into(), Level(0));
        assert_eq!(
            cold.at(&p_at, 0, Level(0), 8).unwrap(),
            history[..2].to_vec()
        );
        let mut cold = Objects::new(bucket.clone(), "zone".into(), Level(0));
        assert_eq!(cold.at(&p_at, 1, Level(0), 8).unwrap(), vec![appended(&p)]);
        // A locator nothing authenticates, another digest at P or a position
        // past the history, is a restart, not corruption.
        let mut other = p_at;
        other.digest[0] ^= 1;
        let past = crate::Locator {
            position: 9,
            ..p_at
        };
        for l in [other, past] {
            let mut cold = Objects::new(bucket.clone(), "zone".into(), Level(0));
            assert_eq!(cold.at(&l, 0, Level(0), 8).unwrap(), vec![]);
        }
    }
}

/// A superseded writer's continuation publication is a replace on a tag the
/// head has left for good: it is fenced, kills the claim, and the head stays
/// the successor's bytes (storage.md STORE-11, Object storage).
#[test]
fn a_superseded_writer_never_replaces_the_head_with_its_continuation() {
    let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
    let s = Scripted::new(bucket.clone());
    let (mut h, mut c, _) = writer_a(&s);
    b_claims(&bucket);
    let head = || bucket.get_tagged("head").unwrap().unwrap().0;
    let successor = head();
    let sweep = crate::Sweep {
        scanned: 1,
        index: 0,
        cursor: "o/".into(),
        ..Default::default()
    };
    assert_eq!(
        crate::reclaim::Space::publish(&mut h, &mut c, &sweep),
        Err(Error::Fenced)
    );
    assert!(c.dead);
    assert_eq!(head(), successor);
}

/// A bucket handle that counts listing requests and head reads.
struct Lists {
    inner: Arc<Bucket>,
    n: std::sync::atomic::AtomicU64,
    heads: std::sync::atomic::AtomicU64,
}

impl Lists {
    fn new(inner: Arc<Bucket>) -> Self {
        Lists {
            inner,
            n: Default::default(),
            heads: Default::default(),
        }
    }
    fn count(&self) -> u64 {
        self.n.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn heads(&self) -> u64 {
        self.heads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl ObjectStorage for Lists {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        if key == "head" {
            self.heads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.inner.get_tagged(key)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.get(key)
    }
    fn get_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>, StorageError>> {
        self.inner.get_many(keys)
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        self.inner.put_new(key, bytes)
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        self.inner.replace(key, bytes, condition)
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        self.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.list(prefix, after)
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key)
    }
}

fn reclaimed(h: &mut Objects, c: &mut Claim) {
    for _ in 0..100 {
        if h.reclaim(c, 64).unwrap() == crate::Reclaimed::Done {
            return;
        }
    }
    panic!("reclamation never finished");
}

/// Reclamation walks the listing once per claim (storage.md STORE-11,
/// Reclamation): after a claim's walk reached the listing's end, its reclaim
/// makes no listing request and still deletes what a record retired, and a
/// put no record followed waits for the next claim, whose own walk removes it.
#[test]
fn a_claim_walks_the_listing_once_and_the_next_claim_walks_it_again() {
    let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
    let lists = Arc::new(Lists::new(bucket.clone()));
    let object = |lifetime, data: &[u8]| crate::ObjectRef {
        lifetime,
        epoch: 1,
        digest: crate::envelope::digest(data),
    };
    let mut h = Objects::new(lists.clone(), "zone".into(), Level(0));
    let mut c = h.claim([1; 16], [7; 32]).unwrap().claim;
    let kept = object(2, b"kept");
    h.put(&mut c, kept, b"kept".to_vec()).unwrap();
    let p2 = h
        .submit(&mut c, entry(2, 1), &[kept], &[], Level(0))
        .unwrap();
    assert_eq!(h.wait(&mut c, p2), Advanced::Advanced(2));
    reclaimed(&mut h, &mut c);
    assert!(
        lists.count() > 0,
        "the claim's first reclaim walks the listing"
    );
    let stray = object(3, b"stray");
    h.put(&mut c, stray, b"stray".to_vec()).unwrap();
    let p3 = h
        .submit(&mut c, entry(3, 1), &[], &[kept], Level(0))
        .unwrap();
    assert_eq!(h.wait(&mut c, p3), Advanced::Advanced(3));
    let walked = lists.count();
    reclaimed(&mut h, &mut c);
    assert_eq!(lists.count(), walked, "no listing after the claim's walk");
    assert_eq!(h.get(kept), Ok(None), "the retirement is deleted");
    assert!(
        h.get(stray).unwrap().is_some(),
        "garbage after the walk waits"
    );
    let mut h2 = Objects::new(lists.clone(), "zone".into(), Level(0));
    let mut c2 = h2.claim([2; 16], [7; 32]).unwrap().claim;
    reclaimed(&mut h2, &mut c2);
    assert!(lists.count() > walked, "the next claim walks the listing");
    assert_eq!(
        h2.get(stray),
        Ok(None),
        "and removes what the last walk missed"
    );
}

/// A reclaim call reads the head once (storage.md STORE-11, Reclamation): the
/// records it reads to find each retirement's creator come from the history
/// the call already followed, not from a head read per record.
#[test]
fn a_reclaim_call_reads_the_head_once() {
    let bucket = Arc::new(Bucket::new(1, BTreeMap::new()));
    let lists = Arc::new(Lists::new(bucket.clone()));
    let mut h = Objects::new(lists.clone(), "zone".into(), Level(0));
    let mut c = h.claim([1; 16], [7; 32]).unwrap().claim;
    let mut made = Vec::new();
    for p in 2..10u64 {
        let data = p.to_le_bytes().to_vec();
        let r = crate::ObjectRef {
            lifetime: p,
            epoch: 1,
            digest: crate::envelope::digest(&data),
        };
        h.put(&mut c, r, data).unwrap();
        let at = h.submit(&mut c, entry(p, 1), &[r], &[], Level(0)).unwrap();
        assert_eq!(h.wait(&mut c, at), Advanced::Advanced(p));
        made.push(r);
    }
    let at = h
        .submit(&mut c, entry(10, 1), &[], &made, Level(0))
        .unwrap();
    assert_eq!(h.wait(&mut c, at), Advanced::Advanced(10));
    let before = lists.heads();
    assert_eq!(h.reclaim(&mut c, 64), Ok(crate::Reclaimed::Done));
    assert_eq!(lists.heads() - before, 1);
    for r in made {
        assert_eq!(h.get(r), Ok(None), "every retirement is deleted");
    }
}
