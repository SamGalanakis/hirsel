//! Exclusive counters for the serial request. No work is inferred from elapsed time.
use hirsel_intern::instructions::InstructionCounter;
use serde::Serialize;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Instant;

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
    pub counter_refusal: Option<String>,
}
struct ImageMeter {
    counter: Rc<InstructionCounter>,
    result: RefCell<ImageAccess>,
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
        let start = meter.read()?;
        Some(Self { meter, start })
    }
    pub(crate) fn opened(&self) {
        self.meter.result.borrow_mut().records_opened += 1;
    }
    pub(crate) fn bytes(&self, bytes: usize) {
        self.meter.result.borrow_mut().bytes_read += bytes as u64;
    }
    pub(crate) fn digest<T>(&self, bytes: usize, f: impl FnOnce() -> T) -> T {
        let before = self.meter.read();
        let value = f();
        let after = self.meter.read();
        let mut result = self.meter.result.borrow_mut();
        result.digests += 1;
        result.digest_bytes += bytes as u64;
        if let Some((a, b)) = before.zip(after) {
            result.digest_instructions += b - a;
        }
        value
    }
}
impl ImageMeter {
    fn read(&self) -> Option<u64> {
        self.counter
            .read()
            .map_err(|e| self.result.borrow_mut().counter_refusal = Some(e))
            .ok()
    }
}
impl Drop for ImageRead {
    fn drop(&mut self) {
        if let Some(end) = self.meter.read() {
            self.meter.result.borrow_mut().read_instructions += end - self.start;
        }
    }
}

pub struct Meter {
    counter: Option<Rc<InstructionCounter>>,
    image: Option<Rc<ImageMeter>>,
    previous_image: Weak<ImageMeter>,
    first: Option<u64>,
    last: Option<u64>,
    clock: Instant,
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
            })
        });
        let previous_image = IMAGE_METER.with_borrow_mut(|active| {
            std::mem::replace(active, image.as_ref().map_or_else(Weak::new, Rc::downgrade))
        });
        Self {
            image,
            previous_image,
            counter,
            first,
            last: first,
            clock: Instant::now(),
            result,
        }
    }
    pub(crate) fn enabled(&self) -> bool {
        self.counter.is_some()
    }
    /// Measure a nested interval without moving the legacy phase boundary.
    pub fn measure<T>(&mut self, name: &'static str, f: impl FnOnce() -> T) -> T {
        if self.counter.is_none() {
            return f();
        }
        let start = self.read();
        let clock = Instant::now();
        let value = f();
        let end = self.read();
        let instructions = start.zip(end).map(|(a, b)| b - a);
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
        value
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
        let now = Instant::now();
        let value = self.counter.as_ref().and_then(|c| match c.read() {
            Ok(n) => Some(n),
            Err(e) => {
                self.result.counter_refusal = Some(e);
                None
            }
        });
        let instructions = value.zip(self.last).map(|(n, p)| n - p);
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
        self.result.instructions = value.zip(self.first).map(|(n, f)| n - f);
        self.last = value;
        self.clock = now;
        if let Some(image) = &self.image {
            self.result.image_access = image.result.borrow().clone();
            if let Some(error) = &self.result.image_access.counter_refusal {
                self.result.counter_refusal = Some(error.clone());
            }
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        IMAGE_METER.with_borrow_mut(|active| *active = self.previous_image.clone());
    }
}
