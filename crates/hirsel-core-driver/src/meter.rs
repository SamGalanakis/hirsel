//! Exclusive counters for the serial request. No work is inferred from elapsed time.
use hirsel_intern::instructions::InstructionCounter;
use serde::Serialize;
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
}

pub struct Meter {
    counter: Option<InstructionCounter>,
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
                Ok(c) => Some(c),
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
        Self {
            counter,
            first,
            last: first,
            clock: Instant::now(),
            result,
        }
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
    }
}
