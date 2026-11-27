//! The code slab and `Code[F]` (PACK-4, PACK-6, PACK-7, PACK-10).
//!
//! A [`Loader`] installs `.hirsel-mod` images into its slab and gives counted [`Code`]
//! handles to their entries. It holds no compiler and calls no linker: an image is
//! copied, fixed up and published by changing page protection.

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
mod retire;
mod runs;
mod table;

use host::Access;
use retire::Retire;
use runs::Runs;
pub use table::Hit;
use table::{Owner, Table};

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

/// Why an `activate` changed nothing (PACK-6).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    Stale(u64),
    Capacity,
}

/// A callable type and the signature digest of the entries that have it.
///
/// # Safety
/// `Self` is a function pointer type or a transparent wrapper of one, and every entry
/// whose signature is `DIGEST` can be called through it.
pub unsafe trait Signature: Copy {
    const DIGEST: ContentHash;
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
    /// How many live images have text on each page of the text run.
    pages: Vec<u32>,
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
    quarantined: HashSet<ContentHash>,
    installed: u64,
}

struct Shared {
    config: Config,
    /// The text band's address; the data band follows it.
    base: usize,
    page: usize,
    slab: Mutex<Slab>,
    published: Condvar,
    /// The pool's retired bytes. Lock order: a slot, then the slab, then this.
    retired: Mutex<u64>,
    updates: AtomicU64,
    table: Table,
    /// The dead images whose retirement the loader has not done yet.
    pending: AtomicPtr<Retire>,
    /// Bytes of dead text pages the host refused to close.
    unrevoked: AtomicU64,
}

impl Drop for Shared {
    fn drop(&mut self) {
        let mut next = *self.pending.get_mut();
        while !next.is_null() {
            // SAFETY: the list owns each record on it and nothing else reaches the list.
            next = unsafe { Box::from_raw(next) }.next;
        }
        if self.config.band_bytes > 0 {
            host::release(self.base, 2 * self.config.band_bytes);
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
    bytes: u64,
    /// Each entry's identity, signature and address.
    entries: Vec<(ContentHash, ContentHash, usize)>,
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

    /// Fill `[from, to)` of the text band with the trap pattern.
    fn trap(&self, from: usize, to: usize) {
        let pattern: [u8; 4] = match TARGET.arch() {
            hirsel_core_layout::Arch::X86_64 => [0xcc; 4],
            hirsel_core_layout::Arch::Aarch64 => 0xd420_0000u32.to_le_bytes(),
        };
        if to > from {
            host::write(&mut || {
                for offset in from..to {
                    // SAFETY: the range is in the writable window, which no image occupies.
                    unsafe { (self.text(offset) as *mut u8).write(pattern[offset % 4]) };
                }
            });
        }
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
            if text.1 > text.0 {
                host::protect(self.text(text.0), text.1 - text.0, Access::Execute)?;
            }
            if data.1 > data.0 {
                host::protect(self.data(data.0), data.1 - data.0, Access::Read)?;
            }
            host::publish(self.text(text.0), text.1 - text.0)
        };
        cohort.state = match publish() {
            Ok(()) => {
                let rows: Vec<Owner> = cohort.images.iter().map(|(row, _)| *row).collect();
                self.table.change(text, &rows);
                // Close the pages whose every image died before the batch closed.
                self.revoke(cohort, (0, cohort.pages.len()), (0, 0));
                State::Published
            }
            Err(_) => State::Failed,
        };
        let dead = cohort.live == 0;
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
            if text.1 > text.0
                && host::protect(self.text(text.0), text.1 - text.0, Access::None).is_ok()
            {
                slab.text.give(text.0, text.1 - text.0);
            }
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
                                    pages: Vec::new(),
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
            let mapped = host::protect(self.text(code.0), code.1, Access::Patch)
                .and_then(|()| host::protect(self.data(words.0), words.1, Access::Write));
            if mapped.is_err() {
                // Whatever became writable is left out of the free runs.
                return None;
            }
            if host::KEEPS {
                // The range may hold a recycled cohort's code.
                self.trap(code.0, code.0 + code.1);
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
        let page = host::page();
        assert!(config.band_bytes <= 1 << 30 && config.band_bytes.is_multiple_of(page.max(WINDOW)));
        let mut base = 0;
        if config.band_bytes > 0 {
            host::register().map_err(|_| Refusal::Unsupported)?;
            base = host::reserve(2 * config.band_bytes).map_err(|_| Refusal::Unsupported)?;
        }
        let table = Table::new(base, config.band_bytes);
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
            quarantined: HashSet::new(),
            installed: 0,
        };
        Ok(Self(Arc::new(Shared {
            config,
            base,
            page,
            slab: Mutex::new(slab),
            published: Condvar::new(),
            retired: Mutex::new(0),
            updates: AtomicU64::new(0),
            table,
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

    /// The image whose text holds the code address `pc` and the address's offset in
    /// that text (PACK-9). Async-signal-safe: a fault or a sampling handler calls it.
    pub fn resolve(&self, pc: usize) -> Option<Hit> {
        self.0.table.resolve(pc)
    }

    /// Record a hardware fault at `pc`: when it is in loaded code, its image's digest
    /// is quarantined, so this host loads it no more (PACK-9). The fault reporter calls
    /// this outside its signal handler, after the pool has stopped.
    pub fn fault(&self, pc: usize) -> Option<(ContentHash, u32)> {
        let hit = self.resolve(pc)?;
        // No reclaim here: it could retire the image the hit names before it is found.
        let mut slab = self.0.slab.lock().unwrap();
        let at = pc - self.0.base - hit.offset as usize;
        let digest = slab
            .cohorts
            .values()
            .flat_map(|cohort| &cohort.images)
            .find(|(row, _)| row.start as usize == at)
            .map(|(_, digest)| *digest)?;
        slab.quarantined.insert(digest);
        Some((digest, hit.offset))
    }

    /// The bytes of every image in a cohort that is not yet recycled.
    pub fn installed(&self) -> u64 {
        self.reclaim();
        self.0.slab.lock().unwrap().installed
    }

    /// The bytes that count against the retired-code budget.
    pub fn retired(&self) -> u64 {
        self.reclaim();
        *self.0.retired.lock().unwrap()
    }

    /// Retire every image whose last handle was dropped (PACK-4 step 5). A load does
    /// this too; a pack host also calls it from a thread that dispatches nothing, so
    /// dead code stops being reachable without waiting for the next load.
    pub fn reclaim(&self) {
        self.0.reclaim(&mut self.0.slab.lock().unwrap());
    }

    /// How many mappings the text band is in and how many of them are closed, where the
    /// host counts them.
    pub fn mappings(&self) -> Option<(usize, usize)> {
        let text = self.0.text(0);
        host::mappings(text, text + self.0.config.band_bytes)
    }

    /// Bytes of dead text the host refused to close or fill. They stay out of use.
    pub fn unrevoked(&self) -> u64 {
        self.0.unrevoked.load(Ordering::Relaxed)
    }

    /// The `Code` of the entry `identity` of the module `bytes`, installing the module
    /// unless its digest is installed already. `flush` publishes at once instead of
    /// when the batch closes.
    pub fn load<F: Signature>(
        &self,
        bytes: &[u8],
        identity: &ContentHash,
        flush: bool,
    ) -> Result<Code<F>, Refusal> {
        self.image(bytes, flush)?.code(identity)
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
            shared.reclaim(&mut slab);
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
        self.0.reclaim(&mut slab);
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
                    bytes: 0,
                    entries: held.entries.to_vec(),
                    slots: AtomicU32::new(0),
                    retired: AtomicBool::new(false),
                    retire: Some(Box::new(Retire {
                        next: null_mut(),
                        serial: slab.installs,
                        digest: held.digest,
                        cohort: None,
                        text: (0, 0),
                        bytes: 0,
                        retired: false,
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
        let mut slots = Vec::with_capacity(module.imports().len());
        for import in module.imports() {
            slots.push(match import {
                Import::Row(row) => hirsel_mod::ROWS
                    .get(row as usize)
                    .filter(|(_, class)| config.mask.admits(*class))
                    .and_then(|_| (config.row)(row))
                    .ok_or(Refusal::Import(row))?,
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
        let before = slab
            .window
            .as_ref()
            .map(|window| (window.text.0, window.data.0));
        let opened = slab.open;
        let (at, words, cohort) = shared
            .place(slab, (text.len(), aligns.0), (data.len(), aligns.1))
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
        // SAFETY: `place` gave this image these writable ranges and nothing else holds them.
        host::write(&mut || unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), t as *mut u8, text.len());
            std::ptr::copy_nonoverlapping(data.as_ptr(), d as *mut u8, data.len());
        });
        let row = Owner {
            start: at as u32,
            length: text.len() as u32,
            digest: digest.0[..16].try_into().unwrap(),
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
            bytes,
            entries: module
                .entries()
                .map(|entry| (entry.identity, entry.signature, t + entry.text as usize))
                .collect(),
            slots: AtomicU32::new(0),
            retired: AtomicBool::new(false),
            retire: Some(Box::new(Retire {
                next: null_mut(),
                serial: slab.installs,
                digest,
                cohort: Some(cohort),
                text: (at, text.len()),
                bytes,
                retired: false,
            })),
        });
        let serial = slab.installs;
        slab.images.insert(digest, (serial, Arc::downgrade(&image)));
        slab.installs += 1;
        slab.installed += bytes;
        let record = slab.cohorts.get_mut(&cohort).unwrap();
        record.live += 1;
        record.bytes += bytes;
        if !text.is_empty() {
            let first = record.text.0 / shared.page;
            let pages = (
                at / shared.page - first,
                (at + text.len() - 1) / shared.page - first,
            );
            if record.pages.len() <= pages.1 {
                record.pages.resize(pages.1 + 1, 0);
            }
            for page in pages.0..=pages.1 {
                record.pages[page] += 1;
            }
        }
        Ok((leads, image))
    }

    /// An empty slot at version 0.
    pub fn slot<F: Signature>(&self) -> Slot<F> {
        Slot {
            cell: Arc::new(Cell {
                shared: self.0.clone(),
                version: AtomicU64::new(0),
                current: Mutex::new(None),
            }),
            callable: PhantomData,
        }
    }

    /// An empty worker table.
    pub fn worker(&self) -> Worker {
        Worker {
            shared: self.0.clone(),
            rows: Vec::with_capacity(Worker::ROWS),
            clock: 0,
            seen: 0,
        }
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

    /// The `Code` of the entry `identity`, when its signature is `F`'s.
    pub fn code<F: Signature>(&self, identity: &ContentHash) -> Result<Code<F>, Refusal> {
        const { assert!(size_of::<F>() == size_of::<usize>()) };
        let found = self
            .0
            .entries
            .iter()
            .find(|(named, signature, _)| named == identity && *signature == F::DIGEST);
        Ok(Code {
            image: self.0.clone(),
            entry: found.ok_or(Refusal::NoEntry)?.2,
            callable: PhantomData,
        })
    }
}

/// A counted handle to one entry of one image (PACK-6).
pub struct Code<F> {
    image: Arc<Image>,
    entry: usize,
    callable: PhantomData<fn() -> F>,
}

impl<F> Clone for Code<F> {
    fn clone(&self) -> Self {
        Self {
            image: self.image.clone(),
            entry: self.entry,
            callable: PhantomData,
        }
    }
}

impl<F: Signature> Code<F> {
    /// The entry, callable while this handle or a clone of it is alive.
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
}

type Raw = (Arc<Image>, usize);

struct Cell {
    shared: Arc<Shared>,
    version: AtomicU64,
    current: Mutex<Option<Raw>>,
}

impl Drop for Cell {
    fn drop(&mut self) {
        let Some((image, _)) = self.current.get_mut().unwrap().take() else {
            return;
        };
        {
            let mut retired = self.shared.retired.lock().unwrap();
            if image.slots.fetch_sub(1, Ordering::Relaxed) == 1 {
                image.retired.store(true, Ordering::Relaxed);
                *retired += image.bytes;
            }
        }
        drop(image);
    }
}

/// A shared `(version, Code)` that workers dispatch through (PACK-6).
pub struct Slot<F> {
    cell: Arc<Cell>,
    callable: PhantomData<fn() -> F>,
}

impl<F> Clone for Slot<F> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            callable: PhantomData,
        }
    }
}

impl<F: Signature> Slot<F> {
    pub fn version(&self) -> u64 {
        self.cell.version.load(Ordering::Acquire)
    }

    /// Store `code` when the slot is at version `expected`, and return the new version.
    /// It waits for no call, worker or unit that still uses the old code.
    pub fn activate(&self, expected: u64, code: Code<F>) -> Result<u64, Stop> {
        let cell = &*self.cell;
        let new = code.image.clone();
        let old;
        {
            let mut current = cell.current.lock().unwrap();
            let version = cell.version.load(Ordering::Relaxed);
            if version != expected {
                return Err(Stop::Stale(version));
            }
            let same = current
                .as_ref()
                .is_some_and(|(held, _)| Arc::ptr_eq(held, &new));
            if !same {
                let mut retired = cell.shared.retired.lock().unwrap();
                let returns = new.retired.load(Ordering::Relaxed);
                let leaves = current
                    .as_ref()
                    .map(|(held, _)| held)
                    .filter(|held| held.slots.load(Ordering::Relaxed) == 1);
                let after = *retired - if returns { new.bytes } else { 0 }
                    + leaves.map_or(0, |held| held.bytes);
                if leaves.is_some() && after > cell.shared.config.retired_bytes {
                    return Err(Stop::Capacity);
                }
                new.slots.fetch_add(1, Ordering::Relaxed);
                new.retired.store(false, Ordering::Relaxed);
                if let Some((held, _)) = &*current {
                    held.slots.fetch_sub(1, Ordering::Relaxed);
                }
                if let Some(held) = leaves {
                    held.retired.store(true, Ordering::Relaxed);
                }
                *retired = after;
            }
            old = current.replace((new, code.entry));
            cell.version.store(version + 1, Ordering::Release);
            cell.shared.updates.fetch_add(1, Ordering::Release);
        }
        // The old code's count is given up outside every lock.
        drop(old);
        Ok(expected + 1)
    }
}

struct Row {
    cell: Arc<Cell>,
    version: u64,
    code: Raw,
    used: u64,
}

/// One worker's private table of the code it dispatches to (PACK-6).
pub struct Worker {
    shared: Arc<Shared>,
    rows: Vec<Row>,
    clock: u64,
    seen: u64,
}

impl Worker {
    pub const ROWS: usize = 64;

    /// Call `with` on the slot's current entry, or return `None` for an empty slot. A
    /// hit reads the slot's version and writes nothing another thread reads.
    pub fn dispatch<F: Signature, R>(
        &mut self,
        slot: &Slot<F>,
        with: impl FnOnce(F) -> R,
    ) -> Option<R> {
        let updates = self.shared.updates.load(Ordering::Relaxed);
        if updates != self.seen {
            self.seen = updates;
            self.park();
        }
        self.clock += 1;
        let cell = &slot.cell;
        let version = cell.version.load(Ordering::Relaxed);
        let at = self
            .rows
            .iter()
            .position(|row| Arc::ptr_eq(&row.cell, cell));
        let entry = match at {
            Some(at) if self.rows[at].version == version => {
                self.rows[at].used = self.clock;
                self.rows[at].code.1
            }
            _ => {
                let (version, code) = {
                    let current = cell.current.lock().unwrap();
                    (cell.version.load(Ordering::Relaxed), current.clone()?)
                };
                let entry = code.1;
                let row = Row {
                    cell: cell.clone(),
                    version,
                    code,
                    used: self.clock,
                };
                // The replaced row's count is given up after the slot's lock.
                match at {
                    Some(at) => self.rows[at] = row,
                    None if self.rows.len() < Self::ROWS => self.rows.push(row),
                    None => {
                        let least = (0..Self::ROWS)
                            .min_by_key(|at| self.rows[*at].used)
                            .unwrap();
                        self.rows[least] = row;
                    }
                }
                entry
            }
        };
        // SAFETY: as `Code::entry`; this worker's row owns a count while `with` runs.
        Some(with(unsafe { std::mem::transmute_copy(&entry) }))
    }

    /// Drop every row whose slot has moved on. A worker calls this before it parks.
    pub fn park(&mut self) {
        self.rows
            .retain(|row| row.cell.version.load(Ordering::Relaxed) == row.version);
    }
}
