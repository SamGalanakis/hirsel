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
mod harness_log;
#[cfg(test)]
mod harness_objects;
pub mod log;
pub mod memory;
pub mod objects;
pub mod sim;
pub mod wire;

pub use envelope::Record;
pub use hirsel_run_schema::AuthorityGrant;
use std::io::ErrorKind;

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
    /// Queue a record of `payload` at the claim's next position and return the
    /// position, with no I/O. A dead claim is `Fenced` and a payload past
    /// [`MAX_ENTRY`] refused; neither has an effect.
    fn submit(&mut self, c: &mut Claim, payload: Vec<u8>, at: Level) -> Result<u64, Error> {
        if c.dead {
            return Err(Error::Fenced);
        }
        if oversized(&payload) {
            return Err(Error::Refused(AuthorityError::io(ErrorKind::InvalidInput)));
        }
        let tip = c.queued_tip();
        let record = envelope::appended(&tip, &payload);
        let position = tip.end + 1;
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
    fn get(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error>;
    /// Create an immutable object if its name is free; equal bytes are success.
    fn put(&mut self, name: &str, data: Vec<u8>) -> Result<(), Error>;
    /// Remove what no retained version needs, decided from the history alone.
    fn reclaim(&mut self, c: &mut Claim) -> Result<(), Error>;
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

/// Refuse a payload past [`MAX_ENTRY`] before any effect.
pub(crate) fn oversized(payload: &[u8]) -> bool {
    payload.len() as u64 > MAX_ENTRY
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
/// history in, which the caller resolved from the grant's mount.
pub fn open(
    g: &AuthorityGrant,
    dir: Option<&std::path::Path>,
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
            match hirsel_object_storage::provider(entry)
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
