//! Exclusive counters for the serial request. No work is inferred from elapsed time.
use hirsel_intern::instructions::InstructionCounter;
use serde::Serialize;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Instant;

pub(crate) mod trace;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stage {
    pub name: &'static str,
    pub instructions: Option<u64>,
    pub nanoseconds: u128,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Measurements {
    pub stages: Vec<Stage>,
    pub instructions: Option<u64>,
    pub counter_refusal: Option<String>,
    /// Library selection for this request: an installed image or source check,
    /// including the reason the selector made that choice.
    pub image_selection: Option<String>,
    /// Nested instruction intervals. These overlap the serial `stages`, and
    /// must not be added to them. Body keys retain the old check/lower boundary.
    pub substages: Vec<Stage>,
    /// Bytes and semantic tokens actually parsed, excluding library image input.
    pub source_bytes: u64,
    pub tokens: u64,
    pub image_access: ImageAccess,
    /// Image reads through the front/check boundary, before native emission.
    pub front_image_access: ImageAccess,
    /// Measured empty observer costs, [whole caller interval, inner scope].
    /// Counter pause/resume excludes observer bodies; these remove the residual
    /// entry/return instructions outside the paused window.
    pub observer_tares: [[u64; 2]; 3],
    pub observer_residual_instructions: u64,
    /// The request's work counts, from the meter's creation to its last finish.
    pub counters: Counters,
    /// The gate's class (OPTION-L L2): None when the request reached no front.
    pub layer_class: Option<LayerClass>,
}

/// Whether the store selects the record of a request's shape, decided after
/// the index and before resolve: Fast installs it in place of declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerClass {
    Fast,
    Fallback,
}

/// Counts of the work a request does, wherever in the process it happens.
/// Each is counted at the one place that work is done.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Counters {
    /// Body keys derived (`Keys::for_body`), and the signature-closure parts
    /// they hash, in bytes.
    pub body_keys: u64,
    pub body_key_bytes: u64,
    /// Rows of the reach record decoded.
    pub record_rows_read: u64,
    /// Stored native objects opened: for a link, or for their bytes.
    pub objects_opened: u64,
    /// Files opened for their bytes: store records, selections and objects,
    /// image records and pages, and sources.
    pub files_opened: u64,
    /// Image bytes read, by what they are: the directory and the declaration
    /// table (whole, for every module), index pages, one file's product, one
    /// body, one module's source text.
    pub image_directory_bytes: u64,
    pub image_declaration_bytes: u64,
    pub image_page_bytes: u64,
    pub image_file_bytes: u64,
    pub image_body_bytes: u64,
    pub image_source_bytes: u64,
    /// Image file products and bodies read: one per module or body demanded.
    pub image_files: u64,
    pub image_bodies: u64,
    /// Records of a shape's declared tables installed in place of
    /// declaration (OPTION-L v3.2): one per request on the fast path.
    pub declared_installs: u64,
}

/// One kind of work [`Counters`] counts.
#[derive(Clone, Copy)]
pub(crate) enum Count {
    BodyKeys,
    BodyKeyBytes,
    RecordRowsRead,
    ObjectsOpened,
    FilesOpened,
    ImageDirectoryBytes,
    ImageDeclarationBytes,
    ImagePageBytes,
    ImageFileBytes,
    ImageBodyBytes,
    ImageSourceBytes,
    ImageFiles,
    ImageBodies,
    DeclaredInstalls,
}
const COUNTS: usize = Count::DeclaredInstalls as usize + 1;
static COUNTED: [std::sync::atomic::AtomicU64; COUNTS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; COUNTS];

/// Count `n` of `kind` for every meter now running.
#[inline]
pub(crate) fn count(kind: Count, n: u64) {
    COUNTED[kind as usize].fetch_add(n, std::sync::atomic::Ordering::Relaxed);
}

fn counted() -> [u64; COUNTS] {
    std::array::from_fn(|i| COUNTED[i].load(std::sync::atomic::Ordering::Relaxed))
}

impl Counters {
    fn since(start: &[u64; COUNTS]) -> Self {
        let now = counted();
        let n = |kind: Count| now[kind as usize] - start[kind as usize];
        Self {
            body_keys: n(Count::BodyKeys),
            body_key_bytes: n(Count::BodyKeyBytes),
            record_rows_read: n(Count::RecordRowsRead),
            objects_opened: n(Count::ObjectsOpened),
            files_opened: n(Count::FilesOpened),
            image_directory_bytes: n(Count::ImageDirectoryBytes),
            image_declaration_bytes: n(Count::ImageDeclarationBytes),
            image_page_bytes: n(Count::ImagePageBytes),
            image_file_bytes: n(Count::ImageFileBytes),
            image_body_bytes: n(Count::ImageBodyBytes),
            image_source_bytes: n(Count::ImageSourceBytes),
            image_files: n(Count::ImageFiles),
            image_bodies: n(Count::ImageBodies),
            declared_installs: n(Count::DeclaredInstalls),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImageAccess {
    pub records_opened: u64,
    pub bytes_read: u64,
    pub digests: u64,
    pub digest_bytes: u64,
    /// Entire image Store::get interval, including its nested digest.
    pub read_instructions: u64,
    pub digest_instructions: u64,
    /// Union of published-table, index, lookup and decode scopes. Includes reads/digests.
    pub index_instructions: u64,
    pub index_calls: u64,
    pub key_probes: u64,
    pub row_demands: u64,
    pub nodes_decoded: u64,
    pub counter_refusal: Option<String>,
}
struct ImageMeter {
    counter: Rc<InstructionCounter>,
    result: RefCell<ImageAccess>,
    index_depth: Cell<u32>,
    index_enabled: Cell<bool>,
    overhead: Cell<u64>,
    tares: Cell<[[u64; 2]; 3]>,
}
thread_local! {
    static IMAGE_METER: RefCell<Weak<ImageMeter>> = const { RefCell::new(Weak::new()) };
}

/// A read of an installed image record. The request's counter is shared, so
/// nested image reads use no extra PMU slot and include only actual disk reads.
pub(crate) struct ImageRead {
    meter: Rc<ImageMeter>,
    start: u64,
}
impl ImageRead {
    pub(crate) fn start(image: bool) -> Option<Self> {
        if !image {
            return None;
        }
        let meter = IMAGE_METER.with_borrow(Weak::upgrade)?;
        meter.pause();
        let start = meter.read()?;
        meter.resume();
        Some(Self { meter, start })
    }
    pub(crate) fn opened(&self) {
        self.meter.pause();
        self.meter.result.borrow_mut().records_opened += 1;
        self.meter.resume();
    }
    pub(crate) fn bytes(&self, bytes: usize) {
        self.meter.pause();
        self.meter.result.borrow_mut().bytes_read += bytes as u64;
        self.meter.resume();
    }
    pub(crate) fn digest<T>(&self, bytes: usize, f: impl FnOnce() -> T) -> T {
        self.meter.pause();
        let before = self.meter.read();
        self.meter.resume();
        let value = f();
        self.meter.pause();
        let after = self.meter.read();
        let mut result = self.meter.result.borrow_mut();
        result.digests += 1;
        result.digest_bytes += bytes as u64;
        if let Some((a, b)) = before.zip(after) {
            result.digest_instructions += b - a;
        }
        drop(result);
        self.meter.resume();
        value
    }
}
impl ImageMeter {
    #[inline(always)]
    fn control(&self, enabled: bool) {
        let result = if enabled {
            self.counter.resume()
        } else {
            self.counter.pause()
        };
        if let Err(e) = result {
            self.result.borrow_mut().counter_refusal = Some(e);
        }
    }
    #[inline(always)]
    fn pause(&self) {
        self.control(false);
    }
    #[inline(always)]
    fn resume(&self) {
        self.control(true);
    }
    fn enter_index(&self, kind: ImageIndexKind, style: usize) -> [u64; 4] {
        let start = self.read().unwrap_or(0);
        let depth = self.index_depth.get();
        self.index_depth.set(depth + 1);
        {
            let mut result = self.result.borrow_mut();
            result.index_calls += 1;
            match kind {
                ImageIndexKind::Access => (),
                ImageIndexKind::Probe => result.key_probes += 1,
                ImageIndexKind::Row => result.row_demands += 1,
                ImageIndexKind::Decode => result.nodes_decoded += 1,
            }
        }
        self.overhead
            .set(self.overhead.get() + self.tares.get()[style][0]);
        [
            start,
            u64::from(depth == 0),
            self.overhead.get(),
            style as u64,
        ]
    }
    fn leave_index(&self, state: [u64; 4]) {
        if let Some(end) = self.read() {
            if state[1] != 0 {
                self.result.borrow_mut().index_instructions += (end - state[0])
                    .saturating_sub(self.overhead.get() - state[2])
                    .saturating_sub(self.tares.get()[state[3] as usize][1]);
            }
        }
        self.index_depth.set(self.index_depth.get() - 1);
    }
    fn read(&self) -> Option<u64> {
        self.counter
            .read()
            .map_err(|e| self.result.borrow_mut().counter_refusal = Some(e))
            .ok()
    }
}
impl Drop for ImageRead {
    fn drop(&mut self) {
        self.meter.pause();
        if let Some(end) = self.meter.read() {
            self.meter.result.borrow_mut().read_instructions += end - self.start;
        }
        self.meter.resume();
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ImageIndexKind {
    Access,
    Probe,
    Row,
    Decode,
}
pub(crate) struct ImageIndex {
    meter: Rc<ImageMeter>,
    state: [u64; 4],
}
impl ImageIndex {
    #[inline(always)]
    pub(crate) fn start(kind: ImageIndexKind) -> Option<Self> {
        if !hirsel_core_ids::published::FRONT_OBSERVER_AVAILABLE {
            return None;
        }
        let meter = IMAGE_METER.with_borrow(Weak::upgrade)?;
        if !meter.index_enabled.get() {
            return None;
        }
        meter.pause();
        let state = meter.enter_index(kind, 0);
        meter.resume();
        Some(Self { meter, state })
    }
}
impl Drop for ImageIndex {
    fn drop(&mut self) {
        self.meter.pause();
        self.meter.leave_index(self.state);
        self.meter.resume();
    }
}
fn published_enter() -> Option<hirsel_core_ids::published::Observation> {
    let meter = IMAGE_METER.with_borrow(Weak::upgrade)?;
    let state = meter.enter_index(ImageIndexKind::Access, 1);
    Some(hirsel_core_ids::published::Observation::new(
        state,
        published_leave,
    ))
}
fn published_leave(state: [u64; 4]) {
    if let Some(meter) = IMAGE_METER.with_borrow(Weak::upgrade) {
        meter.leave_index(state);
    }
}

pub struct Meter {
    counter: Option<Rc<InstructionCounter>>,
    image: Option<Rc<ImageMeter>>,
    previous_image: Weak<ImageMeter>,
    previous_observer: Option<hirsel_core_ids::published::Observer>,
    first: Option<u64>,
    last: Option<u64>,
    last_overhead: u64,
    clock: Instant,
    counted: [u64; COUNTS],
    pub result: Measurements,
}
impl Meter {
    pub fn new(count: bool) -> Self {
        let mut result = Measurements::default();
        let counter = if count {
            match InstructionCounter::open() {
                Ok(c) => Some(Rc::new(c)),
                Err(e) => {
                    result.counter_refusal = Some(e);
                    None
                }
            }
        } else {
            None
        };
        let first = counter.as_ref().and_then(|c| match c.read() {
            Ok(n) => Some(n),
            Err(e) => {
                result.counter_refusal = Some(e);
                None
            }
        });
        let image = counter.as_ref().map(|counter| {
            Rc::new(ImageMeter {
                counter: Rc::clone(counter),
                result: RefCell::default(),
                index_depth: Cell::new(0),
                index_enabled: Cell::new(false),
                overhead: Cell::new(0),
                tares: Cell::new([[0; 2]; 3]),
            })
        });
        let previous_image = IMAGE_METER.with_borrow_mut(|active| {
            std::mem::replace(active, image.as_ref().map_or_else(Weak::new, Rc::downgrade))
        });
        let previous_observer = hirsel_core_ids::published::observe_with(None);
        let mut meter = Self {
            previous_observer,
            image,
            previous_image,
            counter,
            first,
            last: first,
            last_overhead: 0,
            clock: Instant::now(),
            counted: counted(),
            result,
        };
        meter.calibrate();
        meter
    }
    /// Enable before opening images. Ordinary --metrics avoids per-lookup PMU work.
    pub fn profile_image_index(&mut self) {
        if let Some(image) = &self.image {
            image.index_enabled.set(true);
            hirsel_core_ids::published::observe_with(Some((
                Rc::clone(&image.counter),
                published_enter as hirsel_core_ids::published::Observe,
            )));
            self.calibrate();
        }
    }
    fn calibrate(&mut self) {
        let Some(image) = self.image.as_ref().map(Rc::clone) else {
            return;
        };
        image.tares.set([[0; 2]; 3]);
        image.overhead.set(0);
        self.last_overhead = 0;
        const N: u64 = 128;
        let Some(before) = image.read() else {
            return;
        };
        for _ in 0..N {
            std::hint::black_box(());
        }
        let Some(after) = image.read() else {
            return;
        };
        let loop_tare = after - before;
        let dummy = std::hint::black_box([0usize; 1]);
        let mut tares = [[0; 2]; 3];
        for (style, tare) in tares.iter_mut().enumerate().take(2) {
            let inner = image.result.borrow().index_instructions;
            let Some(before) = image.read() else {
                return;
            };
            for _ in 0..N {
                if style == 0 {
                    let _scope = ImageIndex::start(ImageIndexKind::Access);
                } else {
                    let i = std::hint::black_box(0usize);
                    std::hint::black_box(hirsel_core_ids::published::observed(|| &dummy[i]));
                }
            }
            let Some(after) = image.read() else {
                return;
            };
            *tare = [
                // A noisy sample can read under the loop tare: count it as no cost.
                (after - before).saturating_sub(loop_tare) / N,
                (image.result.borrow().index_instructions - inner) / N,
            ];
        }
        let Some(before) = image.read() else {
            return;
        };
        for _ in 0..N {
            self.measure("observer calibration", || ());
        }
        let Some(after) = image.read() else {
            return;
        };
        tares[2] = [
            // A noisy sample can read under the loop tare: count it as no cost.
            (after - before).saturating_sub(loop_tare) / N,
            self.result.substages[0].instructions.unwrap_or(0) / N,
        ];
        self.result.substages.clear();
        self.result.observer_tares = tares;
        image.tares.set(tares);
        *image.result.borrow_mut() = ImageAccess::default();
        // Calibration is diagnostics, outside every compiler interval.
        self.first = image.read();
        self.last = self.first;
    }
    /// Write optional diagnostics after the external cost counter has stopped.
    /// PERF_CTL_FIFO is set only by the native cost harness; ordinary requests
    /// have no external counter to control. perf acknowledges disable before
    /// any timestamp or counter-value JSON is formatted.
    pub fn write_metrics(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        if let Some(control) = std::env::var_os("PERF_CTL_FIFO") {
            use std::io::{BufRead, Write};
            let control = std::path::PathBuf::from(control);
            let mut ack =
                std::io::BufReader::new(std::fs::File::open(control.with_extension("ack"))?);
            std::fs::OpenOptions::new()
                .write(true)
                .open(control)?
                .write_all(b"disable\n")?;
            let mut line = String::new();
            ack.read_line(&mut line)?;
            if line != "ack\n" {
                return Err(std::io::Error::other(
                    "perf did not acknowledge counter stop",
                ));
            }
        }
        std::fs::write(path, serde_json::to_vec_pretty(&self.result)?)
    }
    pub(crate) fn enabled(&self) -> bool {
        self.counter.is_some()
    }
    /// Measure a nested interval without moving the legacy phase boundary.
    #[inline(always)]
    pub fn measure<T>(&mut self, name: &'static str, f: impl FnOnce() -> T) -> T {
        if self.counter.is_none() {
            return f();
        }
        self.pause();
        let clock = Instant::now();
        if let Some(image) = &self.image {
            image
                .overhead
                .set(image.overhead.get() + image.tares.get()[2][0]);
        }
        let start = self.read();
        let overhead = self.overhead();
        self.resume();
        let value = f();
        self.pause();
        let end = self.read();
        let instructions = start.zip(end).map(|(a, b)| {
            (b - a)
                .saturating_sub(self.overhead() - overhead)
                .saturating_sub(
                    self.image
                        .as_ref()
                        .map_or(0, |image| image.tares.get()[2][1]),
                )
        });
        if let Some(stage) = self.result.substages.iter_mut().find(|s| s.name == name) {
            stage.instructions = stage.instructions.zip(instructions).map(|(a, b)| a + b);
            stage.nanoseconds += clock.elapsed().as_nanos();
        } else {
            self.result.substages.push(Stage {
                name,
                instructions,
                nanoseconds: clock.elapsed().as_nanos(),
            });
        }
        self.resume();
        value
    }
    fn overhead(&self) -> u64 {
        self.image.as_ref().map_or(0, |image| image.overhead.get())
    }
    #[inline(always)]
    fn pause(&self) {
        if let Some(image) = &self.image {
            image.pause();
        }
    }
    #[inline(always)]
    fn resume(&self) {
        if let Some(image) = &self.image {
            image.resume();
        }
    }
    fn read(&mut self) -> Option<u64> {
        self.counter.as_ref().and_then(|c| {
            c.read()
                .map_err(|e| self.result.counter_refusal = Some(e))
                .ok()
        })
    }
    pub(crate) fn finish_front(&mut self) {
        self.result.front_image_access = self
            .image
            .as_ref()
            .map_or_else(ImageAccess::default, |image| image.result.borrow().clone());
    }
    pub fn finish(&mut self, name: &'static str) {
        self.pause();
        crate::meter::trace::mark(name);
        let now = Instant::now();
        let value = self.counter.as_ref().and_then(|c| match c.read() {
            Ok(n) => Some(n),
            Err(e) => {
                self.result.counter_refusal = Some(e);
                None
            }
        });
        let overhead = self.overhead();
        let instructions = value
            .zip(self.last)
            .map(|(n, p)| n - p - (overhead - self.last_overhead));
        self.last_overhead = overhead;
        self.result.observer_residual_instructions = overhead;
        if let Some(stage) = self.result.stages.iter_mut().find(|s| s.name == name) {
            stage.instructions = stage.instructions.zip(instructions).map(|(a, b)| a + b);
            stage.nanoseconds += now.duration_since(self.clock).as_nanos();
        } else {
            self.result.stages.push(Stage {
                name,
                instructions,
                nanoseconds: now.duration_since(self.clock).as_nanos(),
            });
        }
        self.result.instructions = value.zip(self.first).map(|(n, f)| n - f - overhead);
        self.last = value;
        self.clock = now;
        self.result.counters = Counters::since(&self.counted);
        if let Some(image) = &self.image {
            self.result.image_access = image.result.borrow().clone();
            if let Some(error) = &self.result.image_access.counter_refusal {
                self.result.counter_refusal = Some(error.clone());
            }
        }
        self.resume();
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        hirsel_core_ids::published::observe_with(self.previous_observer.take());
        IMAGE_METER.with_borrow_mut(|active| *active = self.previous_image.clone());
    }
}
