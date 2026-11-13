//! Opaque destination identities (FORM v1b, MEM-4). Process-wide uniqueness also
//! prevents reuse after an earlier run or region has released its storage.

use std::sync::atomic::{AtomicU64, Ordering};

static DESTINATIONS: AtomicU64 = AtomicU64::new(0);

fn take(counter: &AtomicU64) -> u64 {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map(|previous| previous + 1)
        .unwrap_or_else(|_| std::process::abort())
}

pub(crate) fn next() -> u64 {
    take(&DESTINATIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_u64_identity_is_representable_without_wrapping() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(take(&counter), u64::MAX);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
