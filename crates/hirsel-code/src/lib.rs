//! The code slab and `Code[F]` (PACK-4, PACK-6, PACK-7, PACK-10).
//!
//! A [`Loader`] installs `.hirsel-mod` images into its slab and gives counted [`Code`]
//! handles to their entries. It holds no compiler and calls no linker: an image is
//! copied and fixed up at RX-relative addresses. Linux keeps two whole memfd views.
//! Only the trusted loader knows the RW address. Loaded code receives RX addresses
//! and admitted runtime rows. Native FFI, exec and launch belong in other processes.

use hirsel_core_ids::ContentHash;
use hirsel_mod::{Malformed, Mask, Module};
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

mod fixup;
mod host;
mod install;
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
use table::{DataRow, Owner, Table};

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
    /// The host refused to synchronize its cores with new or dead code. The code
    /// stays out of use and the loader tries again at its next reclaim.
    Unsynchronized,
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
/// a copied pointer must require unsafe code: it may run only under a live image count.
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
    /// Dead text `(offset, length)` that holds traps the host has not synchronized
    /// its cores with. While there is any, the cohort is neither published nor recycled.
    unsynchronized: Vec<(usize, usize)>,
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
    table: Table<Owner>,
    /// Lock-free lookup from data-band literal addresses to independent `CauseData` owners.
    data_table: Table<DataRow>,
    /// The dead images whose retirement the loader has not done yet.
    pending: AtomicPtr<Retire>,
    /// Bytes of dead text in the cohorts' `unsynchronized` lists.
    unrevoked: AtomicU64,
}

/// The entry a load asks for: its identity, its signature and whether it waits.
pub(crate) struct Want<'a> {
    pub(crate) identity: &'a ContentHash,
    pub(crate) signature: &'a ContentHash,
    pub(crate) waits: bool,
}

impl Want<'_> {
    fn names(&self, entry: &hirsel_mod::Entry) -> bool {
        (&entry.identity, &entry.signature, entry.waits)
            == (self.identity, self.signature, self.waits)
    }
}

/// Native rows keep their PACK-2 offsets and validated identity order. Prelinked
/// addresses already belong to the host image and use its static directory.
enum ImageEntries {
    Native(Box<[hirsel_mod::Entry]>),
    Prelinked(&'static [(ContentHash, ContentHash, usize)]),
}

impl Image {
    fn entry_index(&self, want: &Want<'_>) -> Option<usize> {
        match &self.entries {
            ImageEntries::Native(entries) => {
                let index = entries
                    .binary_search_by_key(want.identity, |row| row.identity)
                    .ok()?;
                want.names(&entries[index]).then_some(index)
            }
            ImageEntries::Prelinked(entries) => entries.iter().position(|(id, sig, _)| {
                !want.waits && id == want.identity && sig == want.signature
            }),
        }
    }

    fn addresses(&self, index: usize) -> (usize, usize) {
        match &self.entries {
            ImageEntries::Native(entries) => {
                let row = &entries[index];
                let descriptor = if row.descriptor == hirsel_mod::NO_DESCRIPTOR {
                    0
                } else {
                    self.data + row.descriptor as usize
                };
                (self.text + row.text as usize, descriptor)
            }
            ImageEntries::Prelinked(entries) => (entries[index].2, 0),
        }
    }
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
    entries: ImageEntries,
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
        let (Some(window), Some(cohort)) = (slab.window.as_mut(), slab.cohorts.get_mut(&id)) else {
            return;
        };
        let ends = (self.pages(window.text.0), self.pages(window.data.0));
        self.trap(window.text.0, ends.0);
        (window.text.0, window.data.0) = ends;
        (cohort.text.1, cohort.data.1) = ends;
        let (text, data) = (cohort.text, cohort.data);
        let publish = || -> std::io::Result<()> {
            if data.1 > data.0 {
                host::protect(self.data(data.0), data.1 - data.0, Access::Read)?;
            }
            self.synchronize(text.0, text.1)
        };
        cohort.state = match publish() {
            Ok(()) => {
                // The whole run was synchronized, the dead text in it included.
                let dead = cohort.unsynchronized.drain(..);
                let dead = dead.map(|(_, length)| length as u64).sum();
                self.unrevoked.fetch_sub(dead, Ordering::Relaxed);
                let rows: Vec<Owner> = cohort.images.iter().map(|(row, _)| row.clone()).collect();
                self.table.change(text, &rows);
                if !cohort.data_owners.is_empty() {
                    self.data_table.change(data, &cohort.data_owners);
                }
                State::Published
            }
            Err(_) => State::Failed,
        };
        if cohort.live == 0 && cohort.unsynchronized.is_empty() {
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

    /// Wait until `image`'s cohort is published: whether it was. The cohort is
    /// published by whoever asks for an immediate flush or first finds its batch over.
    fn settle<'a>(
        &'a self,
        mut slab: MutexGuard<'a, Slab>,
        image: &Image,
        flush: bool,
    ) -> (MutexGuard<'a, Slab>, bool) {
        loop {
            let Some(id) = image.cohort else {
                return (slab, true);
            };
            let Some(cohort) = slab.cohorts.get(&id) else {
                return (slab, false);
            };
            match cohort.state {
                State::Published => return (slab, true),
                State::Failed => return (slab, false),
                State::Open => {}
            }
            let wait = (cohort.first + self.config.batch).saturating_duration_since(Instant::now());
            if flush || wait.is_zero() {
                self.publish(&mut slab);
            } else {
                slab = self.published.wait_timeout(slab, wait).unwrap().0;
            }
        }
    }

    /// `image` once its cohort is published, or `Unsynchronized` when the host
    /// refused to publish it.
    fn settled(
        &self,
        slab: MutexGuard<'_, Slab>,
        image: Arc<Image>,
        flush: bool,
    ) -> Result<Arc<Image>, Refusal> {
        let (slab, published) = self.settle(slab, &image, flush);
        drop(slab);
        match published {
            true => Ok(image),
            false => Err(Refusal::Unsynchronized),
        }
    }
}

/// A pool's code: its slab, its interning table and its budgets.
#[derive(Clone)]
pub struct Loader(Arc<Shared>);

impl Loader {
    /// A loader with an empty slab, or `Unsupported` on a host that cannot publish code.
    pub fn new(config: Config) -> Result<Self, Refusal> {
        let page = host::page();
        assert!(config.band_bytes <= 1 << 30 && config.band_bytes.is_multiple_of(page.max(WINDOW)));
        let (mut base, mut writer) = (0, 0);
        if config.band_bytes > 0 {
            host::register().map_err(|_| Refusal::Unsupported)?;
            (base, writer) = host::reserve(config.band_bytes).map_err(|_| Refusal::Unsupported)?;
        }
        let table = Table::new(base, config.band_bytes);
        let data_table = Table::new(base + config.band_bytes, config.band_bytes);
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

    /// Whether `other` is a handle on this loader's pool.
    pub fn same_pool(&self, other: &Loader) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// The data band `[low, high)`: every loaded module's data slice is in it.
    pub fn data_band(&self) -> (usize, usize) {
        let low = self.0.base + self.0.config.band_bytes;
        (low, low + self.0.config.band_bytes)
    }

    /// The image whose text holds the code address `pc` and the address's offset in
    /// that text (PACK-9). Async-signal-safe: a fault or a sampling handler calls it.
    pub fn resolve(&self, pc: usize) -> Option<Hit> {
        self.0.table.find(pc, 1, |row, offset| Hit {
            digest: row.digest,
            offset: offset as u32,
        })
    }

    /// Resolve a static literal range `[ptr, ptr + len)` inside the data band to its
    /// independent `CauseData` owner (retaining one count while pinned) and its
    /// corresponding byte address inside that owner's R/NX mapping (PACK-9.7).
    pub fn resolve_literal(&self, ptr: usize, len: usize) -> Option<(usize, usize)> {
        let ops = self.0.config.cause_data?;
        self.0.data_table.find(ptr, len, |row, offset| {
            // SAFETY: the row's image holds a count on its owner until the row is out
            // of the table and of every hazard slot.
            unsafe { (ops.retain)(row.owner) };
            (row.owner, row.ro_base + offset)
        })
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
    /// `Unsynchronized` says that some dead text is still withheld ([`Self::unrevoked`]);
    /// every other run of the pool stays in use, and the next reclaim tries again.
    pub fn reclaim(&self) -> Result<(), Refusal> {
        self.0.reclaim(&mut self.0.slab.lock().unwrap())
    }

    /// How many mappings the text band is in and how many of them are closed, where the
    /// host counts them.
    pub fn mappings(&self) -> Option<(usize, usize)> {
        let text = self.0.text(0);
        host::mappings(text, self.0.writer, self.0.config.band_bytes)
    }

    /// Bytes of dead text the host has not synchronized its cores with. Their cohorts
    /// stay out of use until it does.
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
        let digest = hirsel_mod::digest(bytes);
        self.load_image(bytes, digest, None, flush).map(Installed)
    }

    /// The image of the module `bytes`, whose digest is `digest`, installed and
    /// published. Every load takes this path. The module must have the entry `want`
    /// names, and every refusal comes before anything is installed (PACK-14).
    fn load_image(
        &self,
        bytes: &[u8],
        digest: ContentHash,
        want: Option<&Want<'_>>,
        flush: bool,
    ) -> Result<Arc<Image>, Refusal> {
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
            return self.prelinked_image(&header.key(), want);
        }
        let mut slab = shared.slab.lock().unwrap();
        let _ = shared.reclaim(&mut slab);
        if slab.admitted.get(&header.key()) != Some(&digest) {
            return Err(Refusal::NotAdmitted);
        }
        if slab.quarantined.contains(&digest) {
            return Err(Refusal::Quarantined);
        }
        let has = |want: &Want<'_>| {
            module
                .entry(want.identity)
                .is_some_and(|entry| want.names(&entry))
        };
        if want.is_some_and(|want| !has(want)) {
            return Err(Refusal::NoEntry);
        }
        let held = slab.images.get(&digest).and_then(|held| held.1.upgrade());
        let image = match held {
            Some(held) => held,
            None => self.install(&mut slab, &module, digest)?,
        };
        shared.settled(slab, image, flush)
    }

    /// The image the host image holds for the module key `key`, or `NotPrelinked`
    /// (PACK-5). It is counted and interned like an installed image and uses no budget.
    pub fn prelinked(&self, key: &ContentHash) -> Result<Installed, Refusal> {
        self.prelinked_image(key, None).map(Installed)
    }

    fn prelinked_image(
        &self,
        key: &ContentHash,
        want: Option<&Want<'_>>,
    ) -> Result<Arc<Image>, Refusal> {
        let config = &self.0.config;
        let held = config.prelinked.iter().find(|held| held.key == *key);
        let held = held.ok_or(Refusal::NotPrelinked)?;
        let has = |want: &Want<'_>| {
            let named = |(identity, signature, _): &(ContentHash, ContentHash, usize)| {
                (identity, signature) == (want.identity, want.signature)
            };
            !want.waits && held.entries.iter().any(named)
        };
        if want.is_some_and(|want| !has(want)) {
            return Err(Refusal::NoEntry);
        }
        let mut slab = self.0.slab.lock().unwrap();
        let _ = self.0.reclaim(&mut slab);
        if slab.quarantined.contains(&held.digest) {
            return Err(Refusal::Quarantined);
        }
        let live = slab
            .images
            .get(&held.digest)
            .and_then(|held| held.1.upgrade());
        if let Some(image) = live {
            return Ok(image);
        }
        let serial = slab.installs;
        let image = Arc::new(Image {
            shared: self.0.clone(),
            digest: held.digest,
            cohort: None,
            text: held.text,
            data: 0,
            bytes: 0,
            cause_data: None,
            entries: ImageEntries::Prelinked(held.entries),
            slots: AtomicU32::new(0),
            retired: AtomicBool::new(false),
            retire: Some(Box::new(Retire {
                next: null_mut(),
                serial,
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
        slab.images
            .insert(held.digest, (serial, Arc::downgrade(&image)));
        slab.installs += 1;
        Ok(image)
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

    /// Resolve a typed candidate once for selection under a worker's generation pin.
    pub fn resolve<F: Signature>(&self, identity: &ContentHash) -> Result<EntryIndex<F>, Refusal> {
        let index = self
            .0
            .entry_index(&Want {
                identity,
                signature: &F::DIGEST,
                waits: F::WAITS,
            })
            .ok_or(Refusal::NoEntry)?;
        Ok(EntryIndex {
            image: Arc::downgrade(&self.0),
            index,
            callable: PhantomData,
        })
    }

    /// The `Code` of the entry `identity`, when its signature and `waits` flag match `F`.
    pub fn code<F: Signature>(&self, identity: &ContentHash) -> Result<Code<F>, Refusal> {
        const { assert!(size_of::<F>() == size_of::<usize>()) };
        let index = self
            .0
            .entry_index(&Want {
                identity,
                signature: &F::DIGEST,
                waits: F::WAITS,
            })
            .ok_or(Refusal::NoEntry)?;
        let (entry, descriptor) = self.0.addresses(index);
        Ok(Code {
            image: self.0.clone(),
            entry,
            descriptor,
            callable: PhantomData,
        })
    }
}

/// A typed ordinal in one installation. It can reside in the installation's
/// registration table without keeping that installation's executable pages alive.
pub struct EntryIndex<F> {
    image: Weak<Image>,
    index: usize,
    callable: PhantomData<fn() -> F>,
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
    /// Borrow the image count through the call. A copied raw entry may only be invoked
    /// under a live count.
    pub fn with_entry<R>(&self, call: impl FnOnce(F) -> R) -> R {
        call(self.entry())
    }

    /// The host ABI entry. The caller must retain this image through its last use,
    /// including suspended returns.
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
