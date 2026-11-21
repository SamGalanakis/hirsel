//! Reclaimable runtime stock shares one process allowance, which it keeps
//! while workers idle. Live storage has its own owners and is outside this
//! counter. Stacks recycled beyond it are surplus until their worker idles
//! (`fiber::Stock`). The native allocator returns blocks to its backend
//! directly, so it has no runtime-owned stock.
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

const HEADROOM: usize = 48 << 20;
/// mc-units-untied attribution variant: `HIRSEL_RETENTION_MB` overrides the
/// headroom (throwaway switch; the budget itself is unchanged).
fn headroom() -> usize {
    static H: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *H.get_or_init(|| {
        std::env::var("HIRSEL_RETENTION_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map_or(HEADROOM, |mb| mb << 20)
    })
}
pub(crate) static RETENTION: Budget = Budget::new();

pub(crate) struct Budget {
    used: AtomicUsize,
    headroom: usize,
}

impl Budget {
    const fn new() -> Self {
        Self::with_headroom(HEADROOM)
    }

    pub(crate) const fn with_headroom(headroom: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            headroom,
        }
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Relaxed)
    }

    pub(crate) fn retain(&self, bytes: usize) -> Option<Retained<'_>> {
        let headroom = if std::ptr::eq(self, &RETENTION) {
            headroom()
        } else {
            self.headroom
        };
        self.used
            .fetch_update(Relaxed, Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|&next| next <= headroom)
            })
            .ok()?;
        Some(Retained {
            budget: self,
            bytes,
        })
    }
}

/// Held until stock is discarded or moved back into active use.
pub(crate) struct Retained<'a> {
    budget: &'a Budget,
    bytes: usize,
}

impl Drop for Retained<'_> {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::Budget;

    #[test]
    fn one_worker_stock_burst_hits_process_headroom() {
        // ATTACK M2. On one worker, submit the sixteen 4 MiB bursts in
        // sequence, alternating stack, tape and allocator stock. With
        // 48 MiB of headroom, twelve fit and the other four must be discarded.
        let budget = Budget::new();
        let mut stock: [Vec<_>; 3] = std::array::from_fn(|_| Vec::new());
        let accepted: Vec<_> = (0..16)
            .map(|burst| {
                if let Some(held) = budget.retain(4 << 20) {
                    stock[burst % 3].push(held);
                    true
                } else {
                    false
                }
            })
            .collect();
        assert_eq!(accepted[..12], [true; 12]);
        assert_eq!(accepted[12..], [false; 4]);
        drop(stock[1].pop());
        assert!(budget.retain(4 << 20).is_some());
        assert!(budget.retain((4 << 20) + 1).is_none());
    }
}
