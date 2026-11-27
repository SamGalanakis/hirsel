//! The code slab and `Code[F]` (PACK-4, PACK-6, PACK-7, PACK-10).
//!
//! A [`Loader`] installs `.hirsel-mod` images into its slab and gives counted [`Code`]
//! handles to their entries. It holds no compiler and calls no linker: an image is
//! copied and fixed up at RX-relative addresses. Linux keeps two whole memfd views.
//! Only the trusted loader knows the RW address. Loaded code receives RX addresses
//! and admitted runtime rows. Native FFI, exec and launch belong in other processes.

use hirsel_core_ids::ContentHash;
use hirsel_mod::{Import, Malformed, Mask, Module};
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

mod fixup;
mod host;
mod raw;
mod retire;
mod runs;
mod slot;
mod table;

use host::Access;
pub use raw::RawCode;
use retire::Retire;
use runs::Runs;
pub use slot::{BorrowToken, CodePin, RawSlot, Slot, Worker};
pub use table::Hit;
use table::{DataRow, DataTable, Owner, Table};

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;

/// The target whose modules this host loads.
pub const TARGET: hirsel_core_layout::Target = host::HOST_TARGET;

/// How much of each band an open cohort maps writable ahead of its installs.
const WINDOW: usize = 256 << 10;

/// Why a load installed nothing (PACK-10).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    NotAdmitted,
    Malformed(Malformed),
    Mismatch(&'static str),
    Import(u32),
    Unbound(ContentHash),
    Fixup(u32),
    NoEntry,
    Capacity,
    Quarantined,
    NotPrelinked,
    Unsupported,
}

/// Why an `activate` changed nothing (PACK-6, PACK-14).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    WrongPool,
    Stale(u64),
    Capacity,
}

/// A callable type and the signature digest of the entries that have it.
///
/// # Safety
/// `Self` is a function pointer type or a transparent wrapper of one, and every entry
/// whose signature is `DIGEST` and `waits == WAITS` can be called through it. Invoking
/// a copied pointer must require unsafe code: it may run only under a live image count
/// and the host's executor gate.
pub unsafe trait Signature: Copy {
    const DIGEST: ContentHash;
    const WAITS: bool = false;
}

/// Opaque handle to one image's independent R/NX cause data owner (PACK-9.7).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CauseDataHandle {
    /// Non-zero aligned pointer to the host's native `CauseData`.
    pub owner: usize,
    /// Address of the immutable 8-byte word holding `owner`, bound to
    /// `hirsel_core_cause_owner` in the image's import table.
    pub slot_addr: usize,
    /// Base address of the copied RO literal bytes inside `owner`'s mapping.
    pub ro_literals_base: usize,
    /// Byte length of the copied RO literal bytes.
    pub ro_literals_len: usize,
}

/// Host callbacks managing per-image independent R/NX cause data owners (PACK-9.7).
/// `hirsel-code` does not link `hirsel-core-rt`; `hirsel-pack` supplies these.
#[derive(Clone, Copy)]
pub struct CauseDataOps {
    /// Establish an independent R/NX data owner before publishing an entry.
    pub create: fn(literals: &[u8], sites: &[u8]) -> Option<CauseDataHandle>,
    /// Retain one count on `owner` while pinned under hazard protection.
    pub retain: unsafe fn(owner: usize),
    /// Release the image's count on `owner` after its data-table row is gone.
    pub release: unsafe fn(owner: usize),
}

/// What a loader admits and how much it may hold.
pub struct Config {
    pub abi: ContentHash,
    pub policy: u64,
    pub mask: Mask,
    /// The length of each band, at most 1 GiB. Zero is a loader with no slab, which
    /// gives prelinked images only.
    pub band_bytes: usize,
    pub installed_bytes: u64,
    pub retired_bytes: u64,
    /// How long a batch stays open after its first install.
    pub batch: Duration,
    /// The host image's address of a runtime row.
    pub row: Box<dyn Fn(u32) -> Option<usize> + Send + Sync>,
    /// The host image's address of a prelinked instance.
    pub instance: Box<dyn Fn(&ContentHash) -> Option<usize> + Send + Sync>,
    /// The images compiled into the host image (PACK-5).
    pub prelinked: &'static [Prelinked],
    /// Independent R/NX cause data owner callbacks (PACK-9.7).
    pub cause_data: Option<CauseDataOps>,
}

/// An image that was compiled ahead of time and linked into the host image.
pub struct Prelinked {
    /// The module key and the module digest of the release it was built from.
    pub key: ContentHash,
    pub digest: ContentHash,
    /// The address of its text.
    pub text: usize,
    /// Each entry's identity, signature and address.
    pub entries: &'static [(ContentHash, ContentHash, usize)],
}

impl Config {
    pub const BATCH: Duration = Duration::from_micros(250);
    pub const RETIRED_BYTES: u64 = 64 << 20;
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum State {
    Open,
    Published,
    Failed,
}

struct Cohort {
    text: (usize, usize),
    data: (usize, usize),
    state: State,
    first: Instant,
    /// Images with a count above zero.
    live: usize,
    bytes: u64,
    /// Bytes of images that were retired when their count reached zero.
    retired: u64,
    /// Every live image installed in it: its PC table row and its module digest.
    images: Vec<(Owner, ContentHash)>,
    /// Every live image's literal-range row in the data band (PACK-9.7).
    data_owners: Vec<DataRow>,
    /// A failed retirement permanently withholds both runs.
    withheld: bool,
}

/// The writable pages ahead of the open cohort: `(next free offset, end)` per band.
struct Window {
    text: (usize, usize),
    data: (usize, usize),
}

struct Slab {
    text: Runs,
    data: Runs,
    window: Option<Window>,
    cohorts: HashMap<u64, Cohort>,
    open: Option<u64>,
    next: u64,
    /// The live image of each digest and which install it is.
    images: HashMap<ContentHash, (u64, Weak<Image>)>,
    installs: u64,
    admitted: HashMap<ContentHash, ContentHash>,
    /// Optional local module bytes registered for admitted digests (`code.load` by key).
    admitted_bytes: HashMap<ContentHash, Arc<[u8]>>,
    quarantined: HashSet<ContentHash>,
    installed: u64,
}

struct Shared {
    config: Config,
    /// The text band's address; the data band follows it.
    base: usize,
    /// Loader-private writable text base. Only RX addresses escape in handles and fixups.
    writer: usize,
    page: usize,
    slab: Mutex<Slab>,
    published: Condvar,
    /// The pool's retired bytes. Lock order: a slot, then the slab, then this.
    retired: Mutex<u64>,
    updates: AtomicU64,
    table: Table,
    /// Lock-free lookup from data-band literal addresses to independent `CauseData` owners.
    data_table: DataTable,
    /// The dead images whose retirement the loader has not done yet.
    pending: AtomicPtr<Retire>,
    /// Retired instruction bytes whose post-write synchronization failed.
    unrevoked: AtomicU64,
}

/// One published entry of an installed image.
pub(crate) struct ImageEntry {
    pub(crate) identity: ContentHash,
    pub(crate) signature: ContentHash,
    pub(crate) waits: bool,
    pub(crate) entry: usize,
    pub(crate) descriptor: usize,
}

/// One installed module.
struct Image {
    shared: Arc<Shared>,
    digest: ContentHash,
    /// Its cohort; a prelinked image has none.
    cohort: Option<u64>,
    text: usize,
    data: usize,
    bytes: u64,
    cause_data: Option<CauseDataHandle>,
    /// Each entry's identity, signature, waits flag, address, and optional descriptor.
    entries: Vec<ImageEntry>,
    /// How many slots name this image, and whether none does after one did. Both
    /// change only under the retired lock.
    slots: AtomicU32,
    retired: AtomicBool,
    retire: Option<Box<Retire>>,
}

impl Shared {
    fn text(&self, offset: usize) -> usize {
        self.base + offset
    }

    fn data(&self, offset: usize) -> usize {
        self.base + self.config.band_bytes + offset
    }

    fn pages(&self, bytes: usize) -> usize {
        bytes.next_multiple_of(self.page)
    }

    /// Close the open cohort and publish it (PACK-4 step 4).
    fn publish(&self, slab: &mut Slab) {
        let Some(id) = slab.open.take() else { return };
        let window = slab.window.as_mut().unwrap();
        let ends = (self.pages(window.text.0), self.pages(window.data.0));
        self.trap(window.text.0, ends.0);
        (window.text.0, window.data.0) = ends;
        let cohort = slab.cohorts.get_mut(&id).unwrap();
        (cohort.text.1, cohort.data.1) = ends;
        let (text, data) = (cohort.text, cohort.data);
        let publish = || -> std::io::Result<()> {
            if cohort.withheld {
                return Err(std::io::ErrorKind::Unsupported.into());
            }
            if data.1 > data.0 {
                host::protect(self.data(data.0), data.1 - data.0, Access::Read)?;
            }
            self.synchronize(text.0, text.1)
        };
        cohort.state = match publish() {
            Ok(()) => {
                let rows: Vec<Owner> = cohort.images.iter().map(|(row, _)| *row).collect();
                self.table.change(text, &rows);
                if !cohort.data_owners.is_empty() {
                    self.data_table.change(data, &cohort.data_owners);
                }
                State::Published
            }
            Err(_) => State::Failed,
        };
        let dead = cohort.live == 0 && !cohort.withheld;
        if dead {
            self.recycle(slab, id);
        }
        self.published.notify_all();
    }

    /// Give the unused window back, publishing the cohort that was filling it.
    fn close(&self, slab: &mut Slab) {
        self.publish(slab);
        if let Some(window) = slab.window.take() {
            let (text, data) = (window.text, window.data);
            slab.text.give(text.0, text.1 - text.0);
            if data.1 > data.0
                && host::protect(self.data(data.0), data.1 - data.0, Access::None).is_ok()
            {
                slab.data.give(data.0, data.1 - data.0);
            }
        }
    }

    /// Room for an image in the open cohort, opening a window or a cohort as needed:
    /// its text and data offsets and its cohort.
    fn place(
        &self,
        slab: &mut Slab,
        text: (usize, usize),
        data: (usize, usize),
    ) -> Option<(usize, usize, u64)> {
        for _ in 0..2 {
            if let Some((free, ends)) = slab
                .window
                .as_ref()
                .map(|w| ((w.text.0, w.data.0), (w.text.1, w.data.1)))
            {
                let at = (
                    free.0.next_multiple_of(text.1),
                    free.1.next_multiple_of(data.1),
                );
                if at.0 + text.0 <= ends.0 && at.1 + data.0 <= ends.1 {
                    let id = match slab.open {
                        Some(id) => id,
                        None => {
                            let id = slab.next;
                            slab.next += 1;
                            slab.open = Some(id);
                            slab.cohorts.insert(
                                id,
                                Cohort {
                                    text: (free.0, free.0),
                                    data: (free.1, free.1),
                                    state: State::Open,
                                    first: Instant::now(),
                                    live: 0,
                                    bytes: 0,
                                    retired: 0,
                                    images: Vec::new(),
                                    data_owners: Vec::new(),
                                    withheld: false,
                                },
                            );
                            id
                        }
                    };
                    self.trap(free.0, at.0);
                    let window = slab.window.as_mut().unwrap();
                    (window.text.0, window.data.0) = (at.0 + text.0, at.1 + data.0);
                    return Some((at.0, at.1, id));
                }
                self.close(slab);
            }
            let (code, words) = (self.pages(text.0), self.pages(data.0));
            let code = slab.text.take(code, code.max(WINDOW))?;
            let Some(words) = slab.data.take(words, words.max(WINDOW)) else {
                slab.text.give(code.0, code.1);
                return None;
            };
            if host::protect(self.data(words.0), words.1, Access::Write).is_err() {
                slab.text.give(code.0, code.1);
                return None;
            }
            slab.window = Some(Window {
                text: (code.0, code.0 + code.1),
                data: (words.0, words.0 + words.1),
            });
        }
        None
    }

    /// Wait until `image`'s cohort is published, publishing it when this caller is the
    /// one whose install opened it or asks for an immediate flush.
    fn settle<'a>(
        &'a self,
        mut slab: MutexGuard<'a, Slab>,
        image: &Image,
        leads: bool,
        flush: bool,
    ) -> (MutexGuard<'a, Slab>, bool) {
        loop {
            let Some(cohort) = image.cohort.map(|id| &slab.cohorts[&id]) else {
                return (slab, true);
            };
            match cohort.state {
                State::Published => return (slab, true),
                State::Failed => return (slab, false),
                State::Open => {}
            }
            let wait = (cohort.first + self.config.batch).saturating_duration_since(Instant::now());
            if flush || (leads && wait.is_zero()) {
                self.publish(&mut slab);
            } else if leads {
                slab = self.published.wait_timeout(slab, wait).unwrap().0;
            } else {
                slab = self.published.wait(slab).unwrap();
            }
        }
    }
}

/// A pool's code: its slab, its interning table and its budgets.
#[derive(Clone)]
pub struct Loader(Arc<Shared>);

impl Loader {
    /// A loader with an empty slab, or `Unsupported` on a host that cannot publish code.
    pub fn new(config: Config) -> Result<Self, Refusal> {
        if host::NEEDS_GATED_EXECUTORS && config.band_bytes != 0 {
            return Err(Refusal::Unsupported);
        }
        Self::reserve(config)
    }

    /// Create a slab for a host wired to the executor gate, including Darwin MAP_JIT.
    ///
    /// # Safety
    /// Every OS thread must enter [`Loader::on_executor`] around actual slab execution,
    /// including every fiber resume, callback and cleanup. It must leave the gate before
    /// parking or taking loader locks. A parked return address must retain its image.
    /// New threads have the same obligations. Darwin’s default constructor stays prelinked-only.
    pub unsafe fn with_gated_executors(config: Config) -> Result<Self, Refusal> {
        Self::reserve(config)
    }

    /// Run one uninterrupted execution interval on this OS thread. A scheduler calls
    /// this around the switch into a fiber, including switches that resume slab code.
    pub fn on_executor<R>(work: impl FnOnce() -> R) -> R {
        host::execute(work)
    }

    fn reserve(config: Config) -> Result<Self, Refusal> {
        let page = host::page();
        assert!(config.band_bytes <= 1 << 30 && config.band_bytes.is_multiple_of(page.max(WINDOW)));
        let (mut base, mut writer) = (0, 0);
        if config.band_bytes > 0 {
            host::register().map_err(|_| Refusal::Unsupported)?;
            (base, writer) = host::reserve(config.band_bytes).map_err(|_| Refusal::Unsupported)?;
        }
        let table = Table::new(base, config.band_bytes);
        let data_table = DataTable::new(base + config.band_bytes, config.band_bytes);
        let slab = Slab {
            text: Runs::new(config.band_bytes),
            data: Runs::new(config.band_bytes),
            window: None,
            cohorts: HashMap::new(),
            open: None,
            next: 0,
            images: HashMap::new(),
            installs: 0,
            admitted: HashMap::new(),
            admitted_bytes: HashMap::new(),
            quarantined: HashSet::new(),
            installed: 0,
        };
        Ok(Self(Arc::new(Shared {
            config,
            base,
            writer,
            page,
            slab: Mutex::new(slab),
            published: Condvar::new(),
            retired: Mutex::new(0),
            updates: AtomicU64::new(0),
            table,
            data_table,
            pending: AtomicPtr::new(null_mut()),
            unrevoked: AtomicU64::new(0),
        })))
    }

    /// Record that the host's trust policy admits `digest` as the module of `key`.
    pub fn admit(&self, key: ContentHash, digest: ContentHash) {
        self.0.slab.lock().unwrap().admitted.insert(key, digest);
    }

    /// Refuse every later load of `digest` (PACK-9).
    pub fn quarantine(&self, digest: ContentHash) {
        self.0.slab.lock().unwrap().quarantined.insert(digest);
    }

    /// The text band `[low, high)`: every loaded code address is in it.
    pub fn text_band(&self) -> (usize, usize) {
        (self.0.base, self.0.base + self.0.config.band_bytes)
    }

    /// The data band `[low, high)`: every loaded module's data slice is in it.
    pub fn data_band(&self) -> (usize, usize) {
        let low = self.0.base + self.0.config.band_bytes;
        (low, low + self.0.config.band_bytes)
    }

    /// The image whose text holds the code address `pc` and the address's offset in
    /// that text (PACK-9). Async-signal-safe: a fault or a sampling handler calls it.
    pub fn resolve(&self, pc: usize) -> Option<Hit> {
        self.0.table.resolve(pc)
    }

    /// Resolve a static literal range `[ptr, ptr + len)` inside the data band to its
    /// independent `CauseData` owner (retaining one count while pinned) and its
    /// corresponding byte address inside that owner's R/NX mapping (PACK-9.7).
    pub fn resolve_literal(&self, ptr: usize, len: usize) -> Option<(usize, usize)> {
        let ops = self.0.config.cause_data?;
        self.0.data_table.resolve_with(ptr, len, ops.retain)
    }

    /// Record a hardware fault in the image `hit` names, as the fault handler resolved
    /// it: the module's digest is quarantined, so this host loads it no more (PACK-9).
    /// The fault reporter calls this outside its signal handler, after the pool has
    /// stopped. It goes by the digest and never by the address, which another image may
    /// own by then.
    pub fn fault(&self, hit: Hit) -> Option<ContentHash> {
        let mut slab = self.0.slab.lock().unwrap();
        let named = |digest: &&ContentHash| digest.0[..16] == hit.digest;
        let live = slab.cohorts.values().flat_map(|cohort| &cohort.images);
        let digest = *live
            .map(|(_, digest)| digest)
            .find(named)
            .or_else(|| slab.admitted.values().find(named))?;
        slab.quarantined.insert(digest);
        Some(digest)
    }

    /// The bytes of every image in a cohort that is not yet recycled.
    pub fn installed(&self) -> u64 {
        let _ = self.reclaim();
        self.0.slab.lock().unwrap().installed
    }

    /// The bytes that count against the retired-code budget.
    pub fn retired(&self) -> u64 {
        let _ = self.reclaim();
        *self.0.retired.lock().unwrap()
    }

    /// Retire every image whose last handle was dropped (PACK-4 step 5). A load does
    /// this too; a pack host also calls it from a thread that dispatches nothing, so
    /// dead code stops being reachable without waiting for the next load.
    pub fn reclaim(&self) -> Result<(), Refusal> {
        self.0.reclaim(&mut self.0.slab.lock().unwrap())
    }

    /// How many mappings the text band is in and how many of them are closed, where the
    /// host counts them.
    pub fn mappings(&self) -> Option<(usize, usize)> {
        let text = self.0.text(0);
        host::mappings(text, self.0.writer, self.0.config.band_bytes)
    }

    /// Bytes of dead text the host refused to close or fill. They stay out of use.
    pub fn unrevoked(&self) -> u64 {
        self.0.unrevoked.load(Ordering::Relaxed)
    }

    /// The `Code` of the entry `identity` of the module `bytes`, validating the entry
    /// identity, typed signature, and `waits` flag before publishing any image (PACK-14).
    /// `flush` publishes at once instead of when the batch closes.
    pub fn load<F: Signature>(
        &self,
        bytes: &[u8],
        identity: &ContentHash,
        flush: bool,
    ) -> Result<Code<F>, Refusal> {
        const { assert!(size_of::<F>() == size_of::<usize>()) };
        let raw = self.load_entry_raw(bytes, identity, &F::DIGEST, F::WAITS, flush)?;
        Ok(Code {
            image: raw.image.expect("loaded image"),
            entry: raw.entry,
            descriptor: raw.descriptor,
            callable: PhantomData,
        })
    }

    /// The image of the module `bytes`, installed and published.
    pub fn image(&self, bytes: &[u8], flush: bool) -> Result<Installed, Refusal> {
        let shared = &*self.0;
        let config = &shared.config;
        let module = Module::parse(bytes).map_err(Refusal::Malformed)?;
        let header = &module.header;
        if header.target != TARGET {
            return Err(Refusal::Mismatch("target"));
        }
        if header.abi != config.abi {
            return Err(Refusal::Mismatch("abi"));
        }
        if header.policy != config.policy {
            return Err(Refusal::Mismatch("policy"));
        }
        if config.band_bytes == 0 {
            return self.prelinked(&header.key());
        }
        let digest = hirsel_mod::digest(bytes);
        let image;
        let published;
        {
            let mut slab = shared.slab.lock().unwrap();
            shared.reclaim(&mut slab)?;
            if slab.admitted.get(&header.key()) != Some(&digest) {
                return Err(Refusal::NotAdmitted);
            }
            if slab.quarantined.contains(&digest) {
                return Err(Refusal::Quarantined);
            }
            let held = slab.images.get(&digest).and_then(|held| held.1.upgrade());
            let (leads, held) = match held {
                Some(held) => (false, held),
                None => self.install(&mut slab, &module, digest)?,
            };
            image = held;
            (slab, published) = shared.settle(slab, &image, leads, flush);
            drop(slab);
        }
        if !published {
            return Err(Refusal::Unsupported);
        }
        Ok(Installed(image))
    }

    /// The image the host image holds for the module key `key`, or `NotPrelinked`
    /// (PACK-5). It is counted and interned like an installed image and uses no budget.
    pub fn prelinked(&self, key: &ContentHash) -> Result<Installed, Refusal> {
        let config = &self.0.config;
        let held = config.prelinked.iter().find(|held| held.key == *key);
        let held = held.ok_or(Refusal::NotPrelinked)?;
        let mut slab = self.0.slab.lock().unwrap();
        self.0.reclaim(&mut slab)?;
        if slab.quarantined.contains(&held.digest) {
            return Err(Refusal::Quarantined);
        }
        let live = slab
            .images
            .get(&held.digest)
            .and_then(|held| held.1.upgrade());
        let image = match live {
            Some(image) => image,
            None => {
                let image = Arc::new(Image {
                    shared: self.0.clone(),
                    digest: held.digest,
                    cohort: None,
                    text: held.text,
                    data: 0,
                    bytes: 0,
                    cause_data: None,
                    entries: held
                        .entries
                        .iter()
                        .map(|&(identity, signature, entry)| ImageEntry {
                            identity,
                            signature,
                            waits: false,
                            entry,
                            descriptor: 0,
                        })
                        .collect(),
                    slots: AtomicU32::new(0),
                    retired: AtomicBool::new(false),
                    retire: Some(Box::new(Retire {
                        next: null_mut(),
                        serial: slab.installs,
                        digest: held.digest,
                        cohort: None,
                        text: (0, 0),
                        owned: 0,
                        data: (0, 0),
                        bytes: 0,
                        retired: false,
                        cause_data: None,
                    })),
                });
                let serial = slab.installs;
                slab.images
                    .insert(held.digest, (serial, Arc::downgrade(&image)));
                slab.installs += 1;
                image
            }
        };
        drop(slab);
        Ok(Installed(image))
    }

    /// Copy a module into the open cohort. Nothing of it is reachable until its cohort
    /// is published.
    fn install(
        &self,
        slab: &mut Slab,
        module: &Module<'_>,
        digest: ContentHash,
    ) -> Result<(bool, Arc<Image>), Refusal> {
        let shared = &*self.0;
        let config = &shared.config;
        let cause_owner_row = hirsel_mod::row("hirsel_core_cause_owner");
        let mut cause_owner_slots = Vec::new();
        let mut slots = Vec::with_capacity(module.imports().len());
        for (slot_index, import) in module.imports().enumerate() {
            slots.push(match import {
                Import::Row(row) => {
                    if cause_owner_row == Some(row) {
                        cause_owner_slots.push(slot_index);
                    }
                    hirsel_mod::ROWS
                        .get(row as usize)
                        .filter(|(_, class)| config.mask.admits(*class))
                        .and_then(|_| (config.row)(row))
                        .ok_or(Refusal::Import(row))?
                }
                Import::Instance(instance) => {
                    (config.instance)(&instance).ok_or(Refusal::Unbound(instance))?
                }
            });
        }
        let aligns = (
            module.header.text_align as usize,
            module.header.data_align as usize,
        );
        if !aligns.0.is_power_of_two()
            || !aligns.1.is_power_of_two()
            || aligns.0.max(aligns.1) > shared.page
        {
            return Err(Refusal::Unsupported);
        }
        let (mut text, mut data) = (module.text().to_vec(), module.data().to_vec());
        let bytes = (text.len() + data.len()) as u64;
        if slab.installed + bytes > config.installed_bytes {
            return Err(Refusal::Capacity);
        }
        let logical = text.len();
        #[cfg(target_arch = "aarch64")]
        let owned = logical.next_multiple_of(4);
        #[cfg(target_arch = "x86_64")]
        let owned = logical;
        let before = slab
            .window
            .as_ref()
            .map(|window| (window.text.0, window.data.0));
        let opened = slab.open;
        let (at, words, cohort) = shared
            .place(slab, (owned, aligns.0), (data.len(), aligns.1))
            .ok_or(Refusal::Capacity)?;
        let leads = slab.open != opened;
        let (t, d) = (shared.text(at), shared.data(words));
        for (slot, address) in data.chunks_exact_mut(8).zip(&slots) {
            slot.copy_from_slice(&(*address as u64).to_le_bytes());
        }
        let fixed = fixup::apply(
            TARGET.arch(),
            (&mut text, t as u64),
            (&mut data, d as u64),
            module.text_fixups(),
            module.data_fixups(),
        );
        if let Err(index) = fixed {
            // Take the room back; a cohort this install opened never existed.
            let start = if leads {
                slab.open = None;
                slab.cohorts
                    .remove(&cohort)
                    .map(|mine| (mine.text.0, mine.data.0))
            } else {
                before
            };
            if let (Some(window), Some(start)) = (&mut slab.window, start) {
                (window.text.0, window.data.0) = start;
            }
            return Err(Refusal::Fixup(index));
        }
        // Establish independent R/NX cause data ownership before publishing any
        // entry (PACK-9.7). Skip allocation for synthetic fixup-only modules
        // with no sites and no literal bytes.
        let imports_bytes = 8 * slots.len();
        let literals = &data[imports_bytes..];
        let fixup_bytes = module.data_fixups().len() * 8;
        let needs_cause_data = !module.sites().is_empty() || literals.len() > fixup_bytes;
        let cause_data = if needs_cause_data {
            config
                .cause_data
                .and_then(|ops| (ops.create)(literals, module.sites()))
        } else {
            None
        };
        if let Some(handle) = cause_data {
            for slot_index in cause_owner_slots {
                let at = 8 * slot_index;
                data[at..at + 8].copy_from_slice(&(handle.slot_addr as u64).to_le_bytes());
            }
        }
        text.resize(owned, 0);
        // SAFETY: `place` gave this image these writable ranges and nothing else holds them.
        host::write(&mut || unsafe {
            std::ptr::copy_nonoverlapping(
                text.as_ptr(),
                (shared.writer + at) as *mut u8,
                text.len(),
            );
            std::ptr::copy_nonoverlapping(data.as_ptr(), d as *mut u8, data.len());
        });
        let row = Owner {
            start: at as u32,
            length: logical as u32,
            digest: digest.0[..16].try_into().unwrap(),
        };
        let data_range = if let Some(handle) = cause_data
            && handle.ro_literals_len != 0
        {
            let range = (words + imports_bytes, handle.ro_literals_len);
            slab.cohorts
                .get_mut(&cohort)
                .unwrap()
                .data_owners
                .push(DataRow {
                    start: range.0 as u32,
                    length: range.1 as u32,
                    owner: handle.owner,
                    ro_base: handle.ro_literals_base,
                });
            range
        } else {
            (0, 0)
        };
        slab.cohorts
            .get_mut(&cohort)
            .unwrap()
            .images
            .push((row, digest));
        let image = Arc::new(Image {
            shared: self.0.clone(),
            digest,
            cohort: Some(cohort),
            text: t,
            data: d,
            bytes,
            cause_data,
            entries: module
                .entries()
                .map(|entry| ImageEntry {
                    identity: entry.identity,
                    signature: entry.signature,
                    waits: entry.waits,
                    entry: t + entry.text as usize,
                    descriptor: if entry.descriptor == hirsel_mod::NO_DESCRIPTOR {
                        0
                    } else {
                        d + entry.descriptor as usize
                    },
                })
                .collect(),
            slots: AtomicU32::new(0),
            retired: AtomicBool::new(false),
            retire: Some(Box::new(Retire {
                next: null_mut(),
                serial: slab.installs,
                digest,
                cohort: Some(cohort),
                text: (at, logical),
                owned: text.len(),
                data: data_range,
                bytes,
                retired: false,
                cause_data,
            })),
        });
        let serial = slab.installs;
        slab.images.insert(digest, (serial, Arc::downgrade(&image)));
        slab.installs += 1;
        slab.installed += bytes;
        let record = slab.cohorts.get_mut(&cohort).unwrap();
        record.live += 1;
        record.bytes += bytes;
        Ok((leads, image))
    }

    /// An empty slot at version 0.
    pub fn slot<F: Signature>(&self) -> Slot<F> {
        Slot::new(self.0.clone())
    }

    /// An empty worker table.
    pub fn worker(&self) -> Worker {
        Worker::new(self.0.clone())
    }
}

/// A counted handle to an installed image.
#[derive(Clone)]
pub struct Installed(Arc<Image>);

impl Installed {
    /// The module digest of the image.
    pub fn digest(&self) -> ContentHash {
        self.0.digest
    }

    /// The `Code` of the entry `identity`, when its signature and `waits` flag match `F`.
    pub fn code<F: Signature>(&self, identity: &ContentHash) -> Result<Code<F>, Refusal> {
        const { assert!(size_of::<F>() == size_of::<usize>()) };
        let found = self.0.entries.iter().find(|entry| {
            entry.identity == *identity && entry.signature == F::DIGEST && entry.waits == F::WAITS
        });
        let found = found.ok_or(Refusal::NoEntry)?;
        Ok(Code {
            image: self.0.clone(),
            entry: found.entry,
            descriptor: found.descriptor,
            callable: PhantomData,
        })
    }
}

/// A counted handle to one entry of one image (PACK-6).
pub struct Code<F> {
    image: Arc<Image>,
    entry: usize,
    descriptor: usize,
    callable: PhantomData<fn() -> F>,
}

impl<F> Clone for Code<F> {
    fn clone(&self) -> Self {
        Self {
            image: self.image.clone(),
            entry: self.entry,
            descriptor: self.descriptor,
            callable: PhantomData,
        }
    }
}

impl<F: Signature> Code<F> {
    /// Borrow the image count through the call and synchronize the executing thread.
    /// A copied raw entry may only be invoked under a live count on a gated executor.
    pub fn with_entry<R>(&self, call: impl FnOnce(F) -> R) -> R {
        host::execute(|| call(self.entry()))
    }

    /// The host ABI entry. The caller must retain this image through its last use,
    /// including suspended returns, and invoke it inside `Loader::on_executor`.
    pub fn entry(&self) -> F {
        // SAFETY: `F` is a function pointer type (`Signature`) and the entry has its
        // signature.
        unsafe { std::mem::transmute_copy(&self.entry) }
    }
}

impl<F> Code<F> {
    /// The module digest of the image.
    pub fn digest(&self) -> ContentHash {
        self.image.digest
    }

    /// The address of the image's `text[0]`.
    pub fn text(&self) -> usize {
        self.image.text
    }

    /// The address of the image's `data[0]` in the data band (0 for prelinked).
    pub fn data(&self) -> usize {
        self.image.data
    }

    /// The image's independent R/NX cause data owner pointer, if established.
    pub fn cause_owner(&self) -> Option<usize> {
        self.image.cause_data.map(|handle| handle.owner)
    }
}
