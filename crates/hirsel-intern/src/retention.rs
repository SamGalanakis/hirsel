//! Original owners of canonical process and session storage.
pub use hirsel_guard::admission::{Owner, OwnerKind, OwnerScope, host_budget};
use std::sync::OnceLock;
static SYMBOLS: OnceLock<Owner> = OnceLock::new();
pub fn symbols() -> &'static Owner {
    SYMBOLS.get_or_init(|| Owner::new(OwnerKind::Symbols))
}
pub fn symbol_bytes() -> u64 {
    SYMBOLS.get().map_or(0, Owner::bytes)
}
