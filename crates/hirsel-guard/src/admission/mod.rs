//! The host allowance is shared by request partitions and process-owned stores.
mod budget;
pub use budget::{MemoryReservation, ProcessBudget};
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
static HOST: OnceLock<(Arc<ProcessBudget>, u64)> = OnceLock::new();
pub fn host_budget() -> (&'static Arc<ProcessBudget>, u64) {
    let (budget, cap) = HOST.get_or_init(|| {
        let cap = std::env::var("HIRSEL_MEMORY_LIMIT")
            .ok()
            .and_then(|text| crate::parse_limit(text.as_bytes()))
            .unwrap_or(crate::DEFAULT_LIMIT);
        (ProcessBudget::new(cap.saturating_sub(cap / 4)), cap)
    });
    (budget, *cap)
}
