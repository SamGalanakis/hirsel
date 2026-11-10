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
//! (`authority.log@1`) and [`sim::Sim`] (`authority.sim@1`, the one fault
//! path). [`open`] makes one from a run-configuration entry.

#[cfg(test)]
hirsel_guard::install!();

pub mod group;
#[cfg(test)]
mod harness;
pub mod log;
pub mod memory;
pub mod sim;
pub mod wire;

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

/// The outcome of [`Authority::advance`].
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
    /// The writer's own end of the history.
    pub(crate) end: u64,
    /// The position durable at each level, by level index.
    pub(crate) durable: Vec<u64>,
    /// Provider state: the log's active segment and its length after the
    /// writer's own last append.
    pub(crate) segment: u64,
    pub(crate) length: u64,
    pub(crate) dead: bool,
}

impl Claim {
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn attempt(&self) -> [u8; 16] {
        self.attempt
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
    /// The largest entry, in bytes.
    fn max_entry(&self) -> u64;
    /// Make the caller the history's only writer: settle a torn tail and append
    /// a claim entry naming `attempt`, minting `fresh` as the store identity when
    /// the history has none.
    fn claim(&mut self, attempt: [u8; 16], fresh: [u8; 32]) -> Result<Claimed, Error>;
    /// Put `entry` at `from + 1` if `c` is the newest claim and the history ends
    /// at `from`, and wait until it is durable at `at`.
    fn advance(&mut self, c: &mut Claim, from: u64, entry: Vec<u8>, at: Level) -> Advanced;
    /// The highest position durable at `at`, issuing at most one barrier.
    fn durable(&mut self, c: &mut Claim, at: Level) -> Result<u64, Error>;
    /// Up to `max` entries after `from` that are durable at `at`.
    fn read(&mut self, from: u64, at: Level, max: u64) -> Result<Vec<Vec<u8>>, Error>;
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

/// Map an entry that could not be followed at `position` to a store error.
pub(crate) fn bad(b: group::Bad, position: u64) -> Error {
    let object = format!("history entry {position}");
    match b {
        group::Bad::Corrupt => Error::Corrupt(object),
        group::Bad::Unsupported(found) => Error::Format { found, object },
    }
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

/// The default bound on one entry: a group holds at most this many bytes.
pub const MAX_ENTRY: u64 = 64 << 20;

/// Why a configuration entry was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError(pub String);

/// The levels `g`'s provider declares and its floor among them, refusing what the
/// grant cannot mean: an unknown provider, a member another provider takes, a sim
/// configuration out of range or a floor the provider does not declare.
pub fn check(g: &AuthorityGrant) -> Result<(Vec<String>, Level), ConfigError> {
    let fail = |m: String| ConfigError(m);
    let p = g.provider.as_str();
    let members = [
        ("mount", g.mount.is_some(), p == "authority.log@1"),
        ("path", g.path.is_some(), p == "authority.log@1"),
        ("seed", g.seed.is_some(), p == "authority.sim@1"),
        ("represent", g.represent.is_some(), p == "authority.sim@1"),
        ("levels", g.levels.is_some(), p == "authority.sim@1"),
        ("faults", g.faults.is_some(), p == "authority.sim@1"),
    ];
    let levels = match p {
        "authority.memory@1" => memory::levels(),
        "authority.log@1" => log::levels(),
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
        _ => Box::new(sim::Sim::new(sim::Config::from_grant(g)?, floor)),
    })
}
