//! The host allowance is shared by request partitions and process-owned stores.
mod budget;
pub use budget::{BudgetReport, MemoryReservation, OwnerRefusal, ProcessBudget, sites};
use std::sync::{Arc, OnceLock};
mod owners;
pub use owners::{Owner, OwnerScope};
pub(crate) use owners::{
    allocated, allocator_installed, before_alloc, deallocated, listed, oracle, oracle_on,
    reallocated,
};

pub const OWNERS: usize = 5;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum OwnerKind {
    SessionTypes,
    SharedTypeBase,
    Symbols,
    CoreSignatures,
    Ledger,
}
impl OwnerKind {
    pub const ALL: [Self; OWNERS] = [
        Self::SessionTypes,
        Self::SharedTypeBase,
        Self::Symbols,
        Self::CoreSignatures,
        Self::Ledger,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::SessionTypes => "session type interner",
            Self::SharedTypeBase => "shared type base",
            Self::Symbols => "process symbols",
            Self::CoreSignatures => "process core signature cache",
            Self::Ledger => "owner ledger backing",
        }
    }
}
/// The host budget keeps `1 / HEADROOM_SHARE` of its limit free of header
/// facts for the unevictable owners that grow between two eviction points.
/// Measured on 2026-11-11 at 2 GiB: the named owners grew at most 13.1 MB
/// between eviction points (fresh std; Tasks check, Tasks debug and
/// LangArena release at most 6.7 MB), and a tenth of the limit, 161 MB, is
/// twelve times that. `--timings` prints the stretch, so drift shows.
pub const HEADROOM_SHARE: u64 = 10;
static HOST: OnceLock<(Arc<ProcessBudget>, u64)> = OnceLock::new();
pub fn host_budget() -> (&'static Arc<ProcessBudget>, u64) {
    let (budget, cap) = HOST.get_or_init(|| {
        let cap = std::env::var("HIRSEL_MEMORY_LIMIT")
            .ok()
            .and_then(|text| crate::parse_limit(text.as_bytes()))
            .unwrap_or(crate::DEFAULT_LIMIT);
        let limit = cap.saturating_sub(cap / 4);
        (
            ProcessBudget::with_headroom(limit, limit / HEADROOM_SHARE),
            cap,
        )
    });
    (budget, *cap)
}
