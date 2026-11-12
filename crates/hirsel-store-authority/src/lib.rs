//! Store authorities (storage.md STORE-11): the host providers that keep one
//! store's history, a sequence of entries at positions 1, 2, ..., and its
//! immutable derived objects.
//!
//! A history has one writer at a time, the holder of the newest [`Claim`]. A
//! claim is an entry in the history itself, so the fence is never an object
//! anything could remove. Each provider declares its own ordered levels,
//! weakest first, and what each survives; a [`Level`] is an index into that
//! list and means nothing to any other authority.
//!
//! Providers: [`memory::Memory`] (`authority.memory@1`), [`log::Log`]
//! (`authority.log@1`), [`objects::Objects`] (`authority.objects@1`) and
//! `authority.sim@1`, the one fault path: [`sim::Sim`] for a log
//! representation, [`objects::Objects`] over a [`sim::Bucket`] for an object
//! one. [`open`] makes one from a run-configuration entry.

#[cfg(test)]
hirsel_guard::install!();

pub mod envelope;
#[cfg(test)]
mod harness;
#[cfg(test)]
mod harness_disk;
#[cfg(test)]
mod harness_log;
#[cfg(test)]
mod harness_objects;
#[cfg(test)]
mod harness_reclaim;
pub mod log;
pub mod memory;
pub mod objects;
mod reclaim;
pub mod sim;
pub mod wire;

pub use envelope::Record;
pub use hirsel_object_storage::Worlds;
pub use hirsel_run_schema::AuthorityGrant;
pub use reclaim::{Reclaimed, Sweep};
use std::io::ErrorKind;

/// An immutable object: the history position whose record creates it, the
/// writer epoch that put it and the SHA-256 of its bytes. The authority names
/// it; the engine never does. The epoch keeps a successor's object at a
/// position a dead claim also put at distinct, equal bytes included.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ObjectRef {
    pub lifetime: u64,
    pub epoch: u64,
    pub digest: [u8; 32],
}

impl ObjectRef {
    /// The object's name, which each provider keeps under its own object
    /// space: the lifetime and the epoch in 20 digits each, then the digest in
    /// lowercase hex, separated by `-`.
    pub fn name(&self) -> String {
        let mut n = format!("{:020}-{:020}-", self.lifetime, self.epoch);
        for b in self.digest {
            n.push_str(&format!("{b:02x}"));
        }
        n
    }

    /// The reference [`ObjectRef::name`] wrote `name` for.
    pub fn parse(name: &str) -> Option<ObjectRef> {
        let (lifetime, rest) = name.split_once('-')?;
        let (epoch, digest) = rest.split_once('-')?;
        if lifetime.len() != 20 || epoch.len() != 20 || digest.len() != 64 {
            return None;
        }
        let digit = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if !digit(lifetime) || !digit(epoch) {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            let pair = &digest[2 * i..2 * i + 2];
            if pair.bytes().any(|b| b.is_ascii_uppercase()) {
                return None;
            }
            *o = u8::from_str_radix(pair, 16).ok()?;
        }
        Some(ObjectRef {
            lifetime: lifetime.parse().ok()?,
            epoch: epoch.parse().ok()?,
            digest: out,
        })
    }
}

/// How far past the claim's next position a put may name a lifetime: the
/// objects an entry creates are put before it is submitted.
pub const PUT_AHEAD: u64 = 64;

/// One of the authority's declared levels: an index into [`Authority::levels`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Level(pub u8);

/// A failure of the medium under an authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityError {
    /// A file failure, as the `FileError` variant ordinal: 0 is `FileFault(1)`, 3
    /// `NotFound()` through 9 `CrossDevice()`. Host errno never reaches the language.
    Io(u8),
    Status(u16),
    Unreachable,
}

/// What a store request is told when it cannot be served.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The medium failed; the request may have had an effect.
    Storage(AuthorityError),
    /// The authority refused the request before it had any effect.
    Refused(AuthorityError),
    /// The named part of the history or object failed a check.
    Corrupt(String),
    /// A later claim holds the history.
    Fenced,
    /// The named record is in another history format version.
    Format { found: [u8; 8], object: String },
}

/// The outcome of [`Authority::wait`] for one queued entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Advanced {
    /// The entry is in the history at this position and durable at the level asked.
    Advanced(u64),
    /// A later claim entry is in the history; nothing was appended.
    Fenced,
    /// Refused before any effect.
    Refused(AuthorityError),
    /// Whether the entry is in the history could not be read, or it is and the
    /// barrier failed.
    Undecided(AuthorityError),
}

/// The right to extend one history: affine, valid for the authority that made it.
/// After `Fenced` or `Undecided` it is dead and every later use is `Fenced`.
#[derive(Debug)]
pub struct Claim {
    pub(crate) owner: u64,
    pub(crate) attempt: [u8; 16],
    /// The writer's own end of the history, and the digest and epoch of the
    /// record there: what its next append chains from.
    pub(crate) end: u64,
    pub(crate) digest: [u8; 32],
    pub(crate) epoch: u64,
    /// The position durable at each level, by level index.
    pub(crate) durable: Vec<u64>,
    /// Provider state: the log's active segment and its length after the
    /// writer's own last append.
    pub(crate) segment: u64,
    pub(crate) length: u64,
    /// `authority.objects@1`: the head's tag from the writer's own last replace.
    pub(crate) tag: Option<hirsel_object_storage::Tag>,
    pub(crate) dead: bool,
    /// Entries submitted and not yet appended, oldest first, each chained on
    /// the one before it and the first on `end`.
    pub(crate) queue: Vec<Queued>,
    /// Outcomes a wait decided for positions no wait has asked for yet.
    pub(crate) settled: Vec<(u64, Advanced)>,
    /// The store's reclamation continuation as the claim loaded or last
    /// published it, and that publication's revision.
    pub(crate) sweep: Sweep,
    pub(crate) revision: u64,
    /// What the records up to the claim's last walk leave live.
    pub(crate) live: reclaim::Live,
}

/// An entry [`Authority::submit`] queued: its record, built on the record
/// before it, and the level its wait makes it durable at.
#[derive(Debug)]
pub struct Queued {
    pub(crate) position: u64,
    pub(crate) record: Vec<u8>,
    pub(crate) digest: [u8; 32],
    pub(crate) at: Level,
}

impl Claim {
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn attempt(&self) -> [u8; 16] {
        self.attempt
    }
    /// The tip the next submitted entry follows: the newest queued one, or the
    /// writer's own end.
    pub(crate) fn queued_tip(&self) -> envelope::Tip {
        let (digest, end) = match self.queue.last() {
            Some(q) => (q.digest, q.position),
            None => (self.digest, self.end),
        };
        envelope::Tip {
            digest,
            end,
            epoch: self.epoch,
            identity: None,
        }
    }
    /// Records the append of `q`: the writer's end moves to it.
    pub(crate) fn appended(&mut self, q: &Queued) {
        self.end = q.position;
        self.digest = q.digest;
    }
    /// A new claim whose own claim record is the last one `tip` covers.
    pub(crate) fn new(owner: u64, attempt: [u8; 16], tip: &envelope::Tip, levels: usize) -> Self {
        Claim {
            owner,
            attempt,
            end: tip.end,
            digest: tip.digest,
            epoch: tip.epoch,
            durable: vec![tip.end; levels],
            segment: 0,
            length: 0,
            tag: None,
            dead: false,
            queue: Vec::new(),
            settled: Vec::new(),
            sweep: Sweep::default(),
            revision: 0,
            live: reclaim::Live::default(),
        }
    }
}

#[derive(Debug)]
pub struct Claimed {
    pub claim: Claim,
    pub end: u64,
}

/// The authority seam. Every operation is on one store's history and objects.
pub trait Authority: Send {
    /// The declared levels, weakest first.
    fn levels(&self) -> &[String];
    /// The deployment's floor for the place.
    fn floor(&self) -> Level;
    /// Make the caller the history's only writer: settle a torn tail and append
    /// a claim record naming `attempt`, minting `fresh` as the store identity when
    /// the history has none.
    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error>;
    /// Append `batch`, the claim's oldest queued entries in order, if `c` is the
    /// newest claim and the history ends at the claim's end, and make each
    /// durable at its level with at most one barrier: one outcome per entry, in
    /// order. An entry after one not `Advanced` is never appended: it is
    /// `Fenced`, or `Undecided` when its own append may have had an effect.
    fn append(&mut self, c: &mut Claim, batch: &[Queued]) -> Vec<Advanced>;
    /// Queue a record of `payload` inventorying `created` at the claim's next
    /// position and retiring `retired` there, and return the position, with
    /// no I/O. A dead claim is `Fenced`; a payload that with 32 bytes per
    /// created and 48 per retired object is past [`MAX_ENTRY`], a created
    /// object at another position or epoch, or a retired one of a lifetime not
    /// below the position or an epoch past the claim's, is refused; neither
    /// has an effect.
    fn submit(
        &mut self,
        c: &mut Claim,
        payload: Vec<u8>,
        created: &[ObjectRef],
        retired: &[ObjectRef],
        at: Level,
    ) -> Result<u64, Error> {
        if c.dead {
            return Err(Error::Fenced);
        }
        let tip = c.queued_tip();
        let position = tip.end + 1;
        if oversized(&payload, created.len(), retired.len())
            || created
                .iter()
                .any(|r| r.lifetime != position || r.epoch != c.epoch)
            || retired
                .iter()
                .any(|r| !envelope::retirable(r, position, c.epoch))
        {
            return Err(Error::Refused(AuthorityError::io(ErrorKind::InvalidInput)));
        }
        let digests: Vec<[u8; 32]> = created.iter().map(|r| r.digest).collect();
        let record = envelope::appended(&tip, &digests, retired, &payload);
        c.queue.push(Queued {
            position,
            digest: envelope::digest(&record),
            record,
            at,
        });
        Ok(position)
    }
    /// The outcome of the entry queued at `p`: appends every queued entry,
    /// those after `p` too, with one barrier, and keeps the others' outcomes
    /// for their own waits, so waits taken oldest first still batch. A position no submit returned, or one already
    /// answered, is refused.
    fn wait(&mut self, c: &mut Claim, p: u64) -> Advanced {
        if let Some(i) = c.settled.iter().position(|(q, _)| *q == p) {
            return c.settled.remove(i).1;
        }
        let Some(n) = c.queue.iter().position(|q| q.position == p) else {
            return Advanced::Refused(AuthorityError::io(ErrorKind::InvalidInput));
        };
        let batch: Vec<Queued> = std::mem::take(&mut c.queue);
        let mut outcomes = self.append(c, &batch);
        debug_assert_eq!(outcomes.len(), batch.len());
        if outcomes.iter().any(|a| !matches!(a, Advanced::Advanced(_))) {
            c.dead = true;
        }
        let answer = outcomes.remove(n);
        c.settled.extend(
            batch
                .iter()
                .map(|q| q.position)
                .filter(|q| *q != p)
                .zip(outcomes),
        );
        answer
    }
    /// The highest position durable at `at`, issuing at most one barrier.
    fn durable(&mut self, c: &mut Claim, at: Level) -> Result<u64, Error>;
    /// Up to `max` records after `from` that are durable at `at`, each checked
    /// against the chain.
    fn read(&mut self, from: u64, at: Level, max: u64) -> Result<Vec<Record>, Error>;
    /// The object `r`, `None` when absent; bytes whose digest is not `r`'s are
    /// corrupt.
    fn get(&mut self, r: ObjectRef) -> Result<Option<Vec<u8>>, Error> {
        let name = r.name();
        match self.get_object(&name)? {
            Some(data) if envelope::digest(&data) != r.digest => Err(Error::Corrupt(name)),
            got => Ok(got),
        }
    }
    /// Create the immutable object `r` with `data` if it is absent; equal bytes
    /// are success. Only a live claim puts, only in its own epoch, only with
    /// `data` hashing to `r`'s digest and a lifetime from the claim's next
    /// position to [`PUT_AHEAD`] past it: a position the claim can still submit
    /// at. The bound is on positions, not bytes.
    fn put(&mut self, c: &mut Claim, r: ObjectRef, data: Vec<u8>) -> Result<(), Error> {
        if c.dead {
            return Err(Error::Fenced);
        }
        let next = c.queued_tip().end + 1;
        if envelope::digest(&data) != r.digest
            || r.epoch != c.epoch
            || r.lifetime < next
            || r.lifetime > next + PUT_AHEAD
        {
            return Err(Error::Refused(AuthorityError::io(ErrorKind::InvalidInput)));
        }
        self.put_object(&r.name(), data)
    }
    /// The provider's object `name`, unchecked.
    fn get_object(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error>;
    /// Create the provider's object `name` if it is free; equal bytes are
    /// success, other bytes `Corrupt(name)`.
    fn put_object(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error>;
    /// Remove objects no version the history keeps needs, decided from the
    /// history alone at the claim's strongest-durable end, making at most
    /// `budget` provider requests besides that barrier, and publish where it
    /// stopped. `More` when the budget ran out first; a budget below 2 is
    /// refused.
    fn reclaim(&mut self, c: &mut Claim, budget: u64) -> Result<Reclaimed, Error>;
    /// The strongest declared level.
    fn top(&self) -> Level {
        Level(self.levels().len() as u8 - 1)
    }
}

impl AuthorityError {
    /// The portable condition of a host failure (core-library.md, File errors).
    pub fn io(kind: ErrorKind) -> Self {
        AuthorityError::Io(match kind {
            ErrorKind::NotFound => 3,
            ErrorKind::PermissionDenied => 4,
            ErrorKind::AlreadyExists => 5,
            ErrorKind::IsADirectory => 6,
            ErrorKind::NotADirectory => 7,
            ErrorKind::DirectoryNotEmpty => 8,
            ErrorKind::CrossesDevices => 9,
            _ => 0,
        })
    }
}

/// Fence a dead or foreign claim.
pub(crate) fn live(owner: u64, c: &Claim) -> bool {
    !c.dead && c.owner == owner
}

/// Map a record that could not be followed at `position` to a store error.
pub(crate) fn bad(b: envelope::Bad, position: u64) -> Error {
    let object = format!("history entry {position}");
    match b {
        envelope::Bad::Corrupt => Error::Corrupt(object),
        envelope::Bad::Unsupported(found) => Error::Format { found, object },
    }
}

/// Refuse an entry past [`MAX_ENTRY`] before any effect: its payload, 32
/// bytes for each object it inventories and 48 for each it retires.
pub(crate) fn oversized(payload: &[u8], created: usize, retired: usize) -> bool {
    payload.len() as u64 + 32 * created as u64 + 48 * retired as u64 > MAX_ENTRY
}

/// The outcomes of a batch whose first `done` entries had `head` and whose
/// rest were never appended.
pub(crate) fn outcomes(
    batch: &[Queued],
    done: usize,
    head: impl Fn(&Queued) -> Advanced,
) -> Vec<Advanced> {
    batch
        .iter()
        .enumerate()
        .map(|(i, q)| if i < done { head(q) } else { Advanced::Fenced })
        .collect()
}

/// A process-unique number for a provider handle, so a claim is valid only
/// with the handle that made it.
pub(crate) fn owner_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A level name: lowercase ASCII letters and digits, starting with a letter.
pub fn level_name_ok(name: &str) -> bool {
    let mut b = name.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The index of `name` among `levels`, or `None` when it is not declared.
pub fn level(levels: &[String], name: &str) -> Option<Level> {
    levels
        .iter()
        .position(|l| l == name)
        .map(|i| Level(i as u8))
}

/// Every provider's bound on one entry's payload.
pub const MAX_ENTRY: u64 = 64 << 20;

/// Why a configuration entry was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError(pub String);

/// The object-storage provider an `authority.objects@1` grant's entry names.
pub fn objects_provider(g: &AuthorityGrant) -> Option<&str> {
    g.objects.as_ref()?.get("provider")?.as_str()
}

/// The levels `g`'s provider declares and its floor among them, refusing what the
/// grant cannot mean: an unknown provider, a member another provider takes, a sim
/// configuration out of range or a floor the provider does not declare.
pub fn check(g: &AuthorityGrant) -> Result<(Vec<String>, Level), ConfigError> {
    let fail = |m: String| ConfigError(m);
    let p = g.provider.as_str();
    let members = [
        ("mount", g.mount.is_some(), p == "authority.log@1"),
        ("path", g.path.is_some(), p == "authority.log@1"),
        ("objects", g.objects.is_some(), p == "authority.objects@1"),
        ("level", g.level.is_some(), p == "authority.objects@1"),
        ("seed", g.seed.is_some(), p == "authority.sim@1"),
        ("represent", g.represent.is_some(), p == "authority.sim@1"),
        ("levels", g.levels.is_some(), p == "authority.sim@1"),
        ("faults", g.faults.is_some(), p == "authority.sim@1"),
    ];
    let levels = match p {
        "authority.memory@1" => memory::levels(),
        "authority.log@1" => log::levels(),
        "authority.objects@1" => {
            let level = g.level.clone().unwrap_or_default();
            if !level_name_ok(&level) {
                return Err(fail(format!(
                    "`authority.objects@1` level `{level}` is not a lowercase name"
                )));
            }
            vec![level]
        }
        "authority.sim@1" => sim::Config::from_grant(g)?.levels,
        _ => return Err(fail(format!("unknown authority provider `{p}`"))),
    };
    if let Some((m, _, takes)) = members.iter().find(|(_, present, takes)| present != takes) {
        return Err(fail(format!(
            "`{p}` {} `{m}`",
            if *takes { "requires" } else { "takes no" }
        )));
    }
    let floor = level(&levels, &g.floor)
        .ok_or_else(|| fail(format!("floor `{}` is not a level `{p}` declares", g.floor)))?;
    Ok((levels, floor))
}

/// The provider a checked grant names. `dir` is the host directory a log keeps its
/// history in, which the caller resolved from the grant's mount. `worlds` are the
/// run's named `objects.sim@1` worlds: one per run, so that grants naming the same
/// world are clients of the same objects.
pub fn open(
    g: &AuthorityGrant,
    dir: Option<&std::path::Path>,
    worlds: &Worlds,
) -> Result<Box<dyn Authority>, ConfigError> {
    let (_, floor) = check(g)?;
    Ok(match g.provider.as_str() {
        "authority.memory@1" => Box::new(memory::Memory::new(floor)),
        "authority.log@1" => {
            let dir =
                dir.ok_or_else(|| ConfigError("authority.log@1 needs its directory".into()))?;
            Box::new(
                log::Log::open(dir, floor)
                    .map_err(|e| ConfigError(format!("authority log: {e}")))?,
            )
        }
        "authority.objects@1" => {
            let entry = g
                .objects
                .as_ref()
                .expect("checked: objects@1 has its entry");
            let level = g.level.clone().expect("checked: objects@1 names its level");
            match hirsel_object_storage::provider(entry, Some(worlds))
                .map_err(|e| ConfigError(format!("authority objects: {e}")))?
            {
                hirsel_object_storage::Provider::Dir(store) => Box::new(objects::Objects::new(
                    std::sync::Arc::new(store),
                    level,
                    floor,
                )),
                hirsel_object_storage::Provider::S3(store) => Box::new(objects::Objects::new(
                    std::sync::Arc::new(store),
                    level,
                    floor,
                )),
                hirsel_object_storage::Provider::Sim(bucket) => {
                    Box::new(sim::Client::new(bucket, level, floor))
                }
            }
        }
        _ => {
            let config = sim::Config::from_grant(g)?;
            match config.represent {
                sim::Represent::Log => Box::new(sim::Sim::new(config, floor)),
                sim::Represent::Objects => {
                    let bucket = sim::Bucket::new(config.seed, config.faults);
                    let level = config.levels[0].clone();
                    Box::new(objects::Objects::new(
                        std::sync::Arc::new(bucket),
                        level,
                        floor,
                    ))
                }
            }
        }
    })
}

/// The object law (storage.md STORE-11, Objects; DECISIONS D3/D14), over the
/// one implementation every provider shares: only a live claim puts, only
/// bytes hashing to the reference's digest, only at a lifetime the claim can
/// still submit at; a get checks the bytes it returns; an entry inventories
/// only objects whose lifetime is its own position. Each refusal has no
/// effect, and what is accepted reads back as written.
#[cfg(test)]
mod object_law {
    use super::*;

    #[test]
    fn objects_are_put_only_at_a_claimable_lifetime_and_read_back_checked() {
        let mut a = memory::Memory::new(Level(0));
        let mut c = a.claim([1; 16], [2; 32]).unwrap().claim;
        let refused = Err(Error::Refused(AuthorityError::io(ErrorKind::InvalidInput)));
        let data = b"node".to_vec();
        let at = |lifetime| ObjectRef {
            lifetime,
            epoch: 1,
            digest: envelope::digest(b"node"),
        };
        // The claim record is at 1: the next position is 2.
        assert_eq!(c.end(), 1);
        for bad in [1, 2 + PUT_AHEAD + 1] {
            assert_eq!(
                a.put(&mut c, at(bad), data.clone()),
                refused,
                "lifetime {bad}"
            );
            assert_eq!(a.get(at(bad)), Ok(None));
        }
        let other_epoch = ObjectRef { epoch: 2, ..at(2) };
        for wrong in [
            ObjectRef {
                digest: [0; 32],
                ..at(2)
            },
            other_epoch,
        ] {
            assert_eq!(a.put(&mut c, wrong, data.clone()), refused);
            assert_eq!(a.get(wrong), Ok(None));
        }
        assert_eq!(a.put(&mut c, at(2 + PUT_AHEAD), data.clone()), Ok(()));
        assert_eq!(a.put(&mut c, at(2), data.clone()), Ok(()));
        assert_eq!(a.get(at(2)), Ok(Some(data.clone())));
        // Bytes under a reference's name that are not its bytes are corrupt.
        a.put_object(&at(3).name(), b"other".to_vec()).unwrap();
        assert_eq!(a.get(at(3)), Err(Error::Corrupt(at(3).name())));
        // An entry inventories only objects at its own position.
        for wrong in [at(3), other_epoch] {
            let r = a.submit(&mut c, b"e".to_vec(), &[wrong], &[], Level(0));
            assert_eq!(r.map(|_| ()), refused);
        }
        // An entry retires only objects an earlier record could have created.
        for wrong in [at(2), at(3), ObjectRef { epoch: 2, ..at(1) }] {
            let r = a.submit(&mut c, b"e".to_vec(), &[], &[wrong], Level(0));
            assert_eq!(r.map(|_| ()), refused);
        }
        let p = a
            .submit(&mut c, b"e".to_vec(), &[at(2)], &[at(1)], Level(0))
            .unwrap();
        assert_eq!(a.wait(&mut c, p), Advanced::Advanced(2));
        assert_eq!(
            a.read(0, Level(0), 8).unwrap()[1],
            Record::Appended {
                payload: b"e".to_vec(),
                created: vec![at(2)],
                retired: vec![at(1)]
            }
        );
        // Position 2 is written: a put there could only be garbage.
        assert_eq!(a.put(&mut c, at(2), data.clone()), refused);
        c.dead = true;
        assert_eq!(a.put(&mut c, at(3), data), Err(Error::Fenced));
    }
}
