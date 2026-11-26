//! Warm mappings and tape buffers share one retention budget.
use super::{Extent, Reclaim, Slab, Stack, UnitStack};
use std::collections::VecDeque;

pub(super) struct Stock<T> {
    idle: VecDeque<(T, usize, u64)>,
    bytes: usize,
    peak: usize,
    hits: usize,
    misses: usize,
}

impl<T> Stock<T> {
    fn new() -> Self {
        Self {
            idle: VecDeque::new(),
            bytes: 0,
            peak: 0,
            hits: 0,
            misses: 0,
        }
    }

    fn take(&mut self, mut fits: impl FnMut(&T) -> bool) -> Option<(T, usize)> {
        let Some(index) = self.idle.iter().rposition(|(item, _, _)| fits(item)) else {
            self.misses += 1;
            return None;
        };
        let (item, bytes, _) = self.idle.remove(index).unwrap();
        self.bytes -= bytes;
        self.hits += 1;
        Some((item, bytes))
    }

    fn push(&mut self, item: T, bytes: usize, seq: u64) {
        self.idle.push_back((item, bytes, seq));
        self.bytes += bytes;
        self.peak = self.peak.max(self.bytes);
    }

    fn first(&self) -> Option<u64> {
        self.idle.front().map(|(_, _, seq)| *seq)
    }

    fn pop(&mut self) -> Option<(T, usize)> {
        let (item, bytes, _) = self.idle.pop_front()?;
        self.bytes -= bytes;
        Some((item, bytes))
    }

    pub(super) fn report(&self, name: &str, shared_cap: usize) -> String {
        format!(
            "{name}_cap={shared_cap} {name}_peak={} {name}_hits={} {name}_misses={}",
            self.peak, self.hits, self.misses
        )
    }
}

pub(super) struct Retention {
    pub(super) unit_stacks: Stock<UnitStack>,
    pub(super) task_stacks: Stock<Stack>,
    pub(super) slabs: Stock<Slab>,
    pub(super) pages: Stock<Extent>,
    #[cfg(feature = "record")]
    pub(super) tapes: Stock<Vec<u8>>,
    pub(super) used: usize,
    pub(super) peak: usize,
    next: u64,
}

impl Retention {
    pub(super) fn new() -> Self {
        Self {
            unit_stacks: Stock::new(),
            task_stacks: Stock::new(),
            slabs: Stock::new(),
            pages: Stock::new(),
            #[cfg(feature = "record")]
            tapes: Stock::new(),
            used: 0,
            peak: 0,
            next: 0,
        }
    }

    pub(super) fn take<T>(
        &mut self,
        select: impl Fn(&mut Self) -> &mut Stock<T>,
        fits: impl FnMut(&T) -> bool,
    ) -> Option<T> {
        let (item, bytes) = select(self).take(fits)?;
        self.used -= bytes;
        Some(item)
    }

    pub(super) fn keep<T>(
        &mut self,
        select: impl Fn(&mut Self) -> &mut Stock<T>,
        item: T,
        bytes: usize,
    ) {
        let seq = self.next;
        self.next += 1;
        select(self).push(item, bytes, seq);
        self.used += bytes;
        self.peak = self.peak.max(self.used);
    }

    pub(super) fn cutoff(&self) -> u64 {
        self.next
    }

    pub(super) fn pop_oldest(&mut self, cutoff: u64) -> Option<(usize, Reclaim)> {
        type Pop = fn(&mut Retention) -> Option<(usize, Reclaim)>;
        let stocks: &[(Option<u64>, Pop)] = &[
            (self.unit_stacks.first(), |s| {
                s.unit_stacks.pop().map(|(x, n)| (n, Reclaim::UnitStack(x)))
            }),
            (self.task_stacks.first(), |s| {
                s.task_stacks.pop().map(|(x, n)| (n, Reclaim::TaskStack(x)))
            }),
            (self.slabs.first(), |s| {
                s.slabs.pop().map(|(x, n)| (n, Reclaim::Slab(x)))
            }),
            (self.pages.first(), |s| {
                s.pages.pop().map(|(x, n)| (n, Reclaim::Page(x)))
            }),
            #[cfg(feature = "record")]
            (self.tapes.first(), |s| {
                s.tapes.pop().map(|(x, n)| (n, Reclaim::Tape(x)))
            }),
        ];
        let (seq, pop) = stocks
            .iter()
            .copied()
            .filter_map(|(seq, pop)| Some((seq?, pop)))
            .min_by_key(|(seq, _)| *seq)?;
        if seq >= cutoff {
            return None;
        }
        let (bytes, item) = pop(self).unwrap();
        self.used -= bytes;
        Some((bytes, item))
    }
}
