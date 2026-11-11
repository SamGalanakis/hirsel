//! Campaign 1 (5a): the delayed-I/O schedules of the 5a recheck's table, each a
//! named case over the objects authority and the simulated bucket, each saying
//! which rule of storage.md STORE-11, Reclamation, makes it safe. A request
//! held in [`Transit`] is answered `Unavailable` with no effect and takes
//! effect later, under its own condition, as an unanswered request that
//! arrives late does. Three schedules of the table have their case elsewhere:
//! the log retirement a host crash loses is
//! `harness::a_retirement_removes_its_object_only_once_strongest_durable`, and
//! the listing that misses an object behind its cursor is
//! `harness_objects::a_claim_walks_the_listing_once_and_the_next_claim_walks_it_again`,
//! and the head replace whose reply is lost and that lands after a successor's
//! is the H2 head cases there: every head write, a continuation's publication
//! included, is conditioned on the writer's own last head.
use crate::harness::entry;
use crate::objects::Objects;
use crate::sim::Bucket;
use crate::{Advanced, Authority, Claim, Level, ObjectRef, Reclaimed};
use hirsel_object_storage::{Condition, Listing, ObjectStorage, Put, Replaced, StorageError, Tag};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A write in transit.
enum Write {
    Put(String, Vec<u8>),
    Replace(String, Vec<u8>, Condition),
    Delete(String),
}

/// The simulated bucket, with the next writes to chosen keys held in transit.
struct Transit {
    inner: Arc<Bucket>,
    armed: Mutex<Vec<String>>,
    held: Mutex<Vec<Write>>,
}

impl Transit {
    fn new(inner: Arc<Bucket>) -> Arc<Transit> {
        Arc::new(Transit {
            inner,
            armed: Mutex::default(),
            held: Mutex::default(),
        })
    }

    /// Hold each of the next `n` writes to `key`.
    fn hold(&self, key: &str, n: usize) {
        let mut armed = self.armed.lock().unwrap();
        armed.extend(std::iter::repeat_n(key.to_string(), n));
    }

    /// Stop holding writes.
    fn disarm(&self) {
        self.armed.lock().unwrap().clear();
    }

    /// Whether this write to `key` is held, using up one hold.
    fn holds(&self, key: &str) -> bool {
        let mut armed = self.armed.lock().unwrap();
        match armed.iter().position(|k| k == key) {
            Some(i) => {
                armed.remove(i);
                true
            }
            None => false,
        }
    }

    /// Every held write takes effect now, oldest first, each under its own
    /// condition.
    fn land(&self) {
        for w in std::mem::take(&mut *self.held.lock().unwrap()) {
            let _ = match w {
                Write::Put(k, b) => self.inner.put_new(&k, &b).map(|_| ()),
                Write::Replace(k, b, c) => self.inner.replace(&k, &b, c).map(|_| ()),
                Write::Delete(k) => self.inner.delete(&k),
            };
        }
    }
}

fn in_transit() -> StorageError {
    StorageError::Unavailable("in transit".into())
}

impl ObjectStorage for Transit {
    fn get_tagged(&self, key: &str) -> Result<Option<(Vec<u8>, Tag)>, StorageError> {
        self.inner.get_tagged(key)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.get(key)
    }
    fn get_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>, StorageError>> {
        self.inner.get_many(keys)
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<Put, StorageError> {
        if self.holds(key) {
            let w = Write::Put(key.into(), bytes.to_vec());
            self.held.lock().unwrap().push(w);
            return Err(in_transit());
        }
        self.inner.put_new(key, bytes)
    }
    fn replace(
        &self,
        key: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<Replaced, StorageError> {
        if self.holds(key) {
            let w = Write::Replace(key.into(), bytes.to_vec(), condition);
            self.held.lock().unwrap().push(w);
            return Err(in_transit());
        }
        self.inner.replace(key, bytes, condition)
    }
    fn list(&self, prefix: &str, after: &str) -> Result<Listing, StorageError> {
        self.inner.list(prefix, after)
    }
    fn delete(&self, key: &str) -> Result<(), StorageError> {
        if self.holds(key) {
            self.held.lock().unwrap().push(Write::Delete(key.into()));
            return Err(in_transit());
        }
        self.inner.delete(key)
    }
}

fn bucket() -> Arc<Bucket> {
    Arc::new(Bucket::new(1, BTreeMap::new()))
}

/// A claimed handle over `store`.
fn claimed(store: Arc<dyn ObjectStorage>, attempt: u8) -> (Objects, Claim) {
    let mut h = Objects::new(store, "zone".into(), Level(0));
    let c = h.claim([attempt; 16], [7; 32]).unwrap().claim;
    (h, c)
}

fn object(lifetime: u64, epoch: u64, data: &[u8]) -> ObjectRef {
    ObjectRef {
        lifetime,
        epoch,
        digest: crate::envelope::digest(data),
    }
}

fn key(r: &ObjectRef) -> String {
    format!("o/{}", r.name())
}

/// Whether the bucket holds `r`, read past every fault.
fn holds(b: &Bucket, r: &ObjectRef) -> bool {
    b.contents().contains_key(&key(r))
}

/// Appends a record creating `created` and retiring `retired`; its position.
fn record(h: &mut Objects, c: &mut Claim, created: &[ObjectRef], retired: &[ObjectRef]) -> u64 {
    let p = h
        .submit(c, entry(c.end() + 1, 1), created, retired, Level(0))
        .unwrap();
    assert_eq!(h.wait(c, p), Advanced::Advanced(p));
    p
}

fn reclaimed(h: &mut Objects, c: &mut Claim) {
    for _ in 0..100 {
        if h.reclaim(c, 64).unwrap() == Reclaimed::Done {
            return;
        }
    }
    panic!("reclamation never finished");
}

/// Schedule 1. A's PUT of r is delayed and its retry succeeds; record 2
/// inventories r and the live object k. Record 3 retires r and reclamation
/// deletes it; then the delayed PUT lands and r is back. Safe by R1: r stays
/// retired, so no later version needs it, and a successor's reclamation keeps
/// k, which no record retired.
#[test]
fn a_late_put_of_a_retired_object_needs_nothing_back() {
    let b = bucket();
    let t = Transit::new(b.clone());
    let (mut h, mut c) = claimed(t.clone(), 1);
    let r = object(2, c.epoch, b"r");
    let k = object(2, c.epoch, b"k");
    t.hold(&key(&r), 1);
    assert!(h.put(&mut c, r, b"r".to_vec()).is_err());
    h.put(&mut c, r, b"r".to_vec()).unwrap();
    h.put(&mut c, k, b"k".to_vec()).unwrap();
    record(&mut h, &mut c, &[r, k], &[]);
    record(&mut h, &mut c, &[], &[r]);
    reclaimed(&mut h, &mut c);
    assert!(!holds(&b, &r), "the retirement is deleted");
    t.land();
    assert!(holds(&b, &r), "the late PUT brings r back");
    let (mut h2, mut c2) = claimed(b.clone(), 2);
    reclaimed(&mut h2, &mut c2);
    assert_eq!(
        h2.get(k),
        Ok(Some(b"k".to_vec())),
        "the live object is kept"
    );
}

/// Schedule 2. A puts r for its next position, 2, and the PUT is delayed; B
/// claims, so B's claim record occupies 2; then A's PUT lands. Safe by R2: the
/// strongest-durable record at r's lifetime is a claim record, which
/// inventories nothing, so B's reclamation deletes r, and A is fenced and can
/// name r in no record.
#[test]
fn a_put_that_lands_under_a_successors_claim_record_is_garbage() {
    let b = bucket();
    let t = Transit::new(b.clone());
    let (mut h, mut c) = claimed(t.clone(), 1);
    let r = object(2, c.epoch, b"r");
    t.hold(&key(&r), 1);
    assert!(h.put(&mut c, r, b"r".to_vec()).is_err());
    let (mut h2, mut c2) = claimed(b.clone(), 2);
    t.land();
    assert!(holds(&b, &r));
    let refused = h.submit(&mut c, entry(2, 1), &[r], &[], Level(0));
    if let Ok(p) = refused {
        assert_ne!(h.wait(&mut c, p), Advanced::Advanced(p), "A is fenced");
    }
    reclaimed(&mut h2, &mut c2);
    assert!(!holds(&b, &r), "R2 at a claim record deletes r");
}

/// Schedule 3. A's DELETE of its retired r goes unanswered and is held; A's
/// retry deletes r. B claims and puts an object of equal bytes, r', under its
/// own record; then A's DELETE arrives. Safe because the DELETE names r, not
/// r': r' has another lifetime and epoch, so another name, and stays.
#[test]
fn a_late_delete_of_a_retired_object_spares_its_equal_successor() {
    let b = bucket();
    let t = Transit::new(b.clone());
    let (mut h, mut c) = claimed(t.clone(), 1);
    let r = object(2, c.epoch, b"same");
    h.put(&mut c, r, b"same".to_vec()).unwrap();
    record(&mut h, &mut c, &[r], &[]);
    record(&mut h, &mut c, &[], &[r]);
    t.hold(&key(&r), 1);
    reclaimed(&mut h, &mut c);
    assert!(!holds(&b, &r), "the retry deleted r");
    let (mut h2, mut c2) = claimed(b.clone(), 2);
    let r2 = object(c2.end() + 1, c2.epoch, b"same");
    h2.put(&mut c2, r2, b"same".to_vec()).unwrap();
    record(&mut h2, &mut c2, &[r2], &[]);
    t.land();
    assert_eq!(h2.get(r2), Ok(Some(b"same".to_vec())), "r' stays");
}

/// Schedule 4. A puts r for position 2 and submits the record creating it,
/// but every head replace of the append is held, so A's wait is undecided and
/// S stays 1. While the record may still win, nothing deletes r. Then either
/// the held replace lands (the record at 2 inventories r: B keeps it) or B's
/// claim occupies 2 first (R2 at a claim record: B deletes r, and the late
/// replace, conditioned on A's old head, has no effect).
#[test]
fn an_undecided_records_object_is_deleted_only_when_another_record_takes_its_position() {
    for lands in [true, false] {
        let b = bucket();
        let t = Transit::new(b.clone());
        let (mut h, mut c) = claimed(t.clone(), 1);
        let r = object(2, c.epoch, b"r");
        h.put(&mut c, r, b"r".to_vec()).unwrap();
        let p = h.submit(&mut c, entry(2, 1), &[r], &[], Level(0)).unwrap();
        t.hold("head", 16);
        assert!(matches!(h.wait(&mut c, p), Advanced::Undecided(_)));
        t.disarm();
        if lands {
            t.land();
        }
        let (mut h2, mut c2) = claimed(b.clone(), 2);
        assert!(holds(&b, &r), "nothing deletes r before B decides");
        reclaimed(&mut h2, &mut c2);
        t.land();
        assert_eq!(holds(&b, &r), lands, "lands {lands}");
        if lands {
            assert_eq!(h2.get(r), Ok(Some(b"r".to_vec())));
        }
    }
}

/// Schedule 7. Record 2 creates the node q and record 3 the root naming it,
/// without inventorying q again. Safe by R2 read at q's own lifetime: the
/// record at 2 inventories q, so reclamation keeps it; the record at 3, which
/// does not list q, decides nothing about it.
#[test]
fn an_object_is_judged_by_the_record_at_its_own_lifetime() {
    let b = bucket();
    let (mut h, mut c) = claimed(b.clone(), 1);
    let q = object(2, c.epoch, b"q");
    h.put(&mut c, q, b"q".to_vec()).unwrap();
    record(&mut h, &mut c, &[q], &[]);
    let root = object(3, c.epoch, b"root");
    h.put(&mut c, root, b"root".to_vec()).unwrap();
    record(&mut h, &mut c, &[root], &[]);
    reclaimed(&mut h, &mut c);
    let (mut h2, mut c2) = claimed(b.clone(), 2);
    reclaimed(&mut h2, &mut c2);
    assert!(holds(&b, &q) && holds(&b, &root));
}

/// Schedule 8. A's reclamation, two requests at a time, publishes where it
/// stopped among record 4's three retirements, its DELETE of the next one
/// held, and A dies. B inherits the continuation and finishes; then A's DELETE
/// arrives. Safe because each retirement's R1 proof is a durable record, not
/// the cursor: B deletes exactly the retired objects, and the live k stays.
#[test]
fn a_successor_finishes_an_inherited_reclamation_and_a_late_delete_is_harmless() {
    let b = bucket();
    let t = Transit::new(b.clone());
    let (mut h, mut c) = claimed(t.clone(), 1);
    let rs: Vec<ObjectRef> = [b"a", b"b", b"c"]
        .iter()
        .map(|d| object(2, c.epoch, *d))
        .collect();
    let k = object(2, c.epoch, b"k");
    for (r, d) in rs.iter().zip([b"a", b"b", b"c"]) {
        h.put(&mut c, *r, d.to_vec()).unwrap();
    }
    h.put(&mut c, k, b"k".to_vec()).unwrap();
    let mut all = rs.clone();
    all.push(k);
    record(&mut h, &mut c, &all, &[]);
    record(&mut h, &mut c, &[], &[]);
    record(&mut h, &mut c, &[], &rs);
    assert_eq!(h.reclaim(&mut c, 3), Ok(Reclaimed::More));
    let deleted = rs.iter().filter(|r| !holds(&b, r)).count();
    assert!(
        deleted > 0 && deleted < 3,
        "A stopped among the retirements"
    );
    let next = rs.iter().find(|r| holds(&b, r)).unwrap();
    t.hold(&key(next), DELETE_HOLDS);
    let _ = h.reclaim(&mut c, 2);
    let (mut h2, mut c2) = claimed(b.clone(), 2);
    reclaimed(&mut h2, &mut c2);
    t.land();
    assert!(rs.iter().all(|r| !holds(&b, r)), "every retirement deleted");
    assert_eq!(
        h2.get(k),
        Ok(Some(b"k".to_vec())),
        "the live object is kept"
    );
}

/// More than reclamation's tries for one DELETE.
const DELETE_HOLDS: usize = 4;
