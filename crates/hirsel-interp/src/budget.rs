//! The host memory budget measures exact logical bytes across the run's values.
//! Constructions and library results charge shallow estimates; a measurement is
//! due before the next step once the estimate exceeds remaining headroom.
//! Function entries, back-edges and primitive work retain the existing periodic
//! sampling cadence for growth not covered by shallow charges. Sampling never
//! limits CPU work. Only an exact live-byte measurement can stop the run.
use crate::BudgetKind;

/// The host's budgets for one run; None leaves a dimension unbounded.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunBudgets {
    pub memory_bytes: Option<u64>,
}

/// The sampling interval is at least this, and at least a sixty-fourth of the
/// last measured bytes, so measuring costs a bounded share of the run.
const MEASURE_INTERVAL: u64 = 4096;

pub(crate) struct Meter {
    budgets: RunBudgets,
    /// Sampling ticks charged so far.
    sampling_ticks: u64,
    /// Logical bytes at the last measurement, and the sample counter then.
    live: u64,
    measured_at: u64,
    /// Estimated bytes charged since the last measurement.
    charged: u64,
}
impl Meter {
    pub fn new(budgets: RunBudgets) -> Self {
        Self {
            budgets,
            sampling_ticks: 0,
            live: 0,
            measured_at: 0,
            charged: 0,
        }
    }
    pub fn sample_point(&mut self) {
        self.sampling_ticks = self.sampling_ticks.saturating_add(1);
    }
    pub fn sample_work(&mut self, cost: u64) {
        self.sampling_ticks = self.sampling_ticks.saturating_add(cost);
    }
    pub fn metered(&self) -> bool {
        self.budgets.memory_bytes.is_some()
    }
    /// Charge an estimate of newly held bytes.
    pub fn charge(&mut self, bytes: u64) {
        self.charged = self.charged.saturating_add(bytes);
    }
    /// Whether the evaluator measures before its next step.
    pub fn due(&self) -> bool {
        let Some(limit) = self.budgets.memory_bytes else {
            return false;
        };
        self.charged > limit.saturating_sub(self.live)
            || self.sampling_ticks - self.measured_at >= MEASURE_INTERVAL.max(self.live / 64)
    }
    /// Record a measurement of `live` bytes, or report that it exceeds the budget.
    pub fn measured(&mut self, live: u64) -> Result<(), BudgetKind> {
        self.live = live;
        self.charged = 0;
        self.measured_at = self.sampling_ticks;
        match self.budgets.memory_bytes {
            Some(limit) if live > limit => Err(BudgetKind::Memory),
            _ => Ok(()),
        }
    }
}
