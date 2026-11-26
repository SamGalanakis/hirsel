//! The empty dedicated-page cache: emptied mappings kept resident for reuse,
//! indexed so a request finds its page without scanning the whole cache.

use super::{Class, DEDICATED_REUSE_MARGIN, DEDICATED_REUSE_WASTE, Page, PagePointer, page_bytes};
use crate::abi_layout::own_prefix;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What a retained dedicated page can serve besides its size: the headroom
/// before its payload and its payload's alignment, capped at a native page.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Shape {
    headroom: usize,
    align: usize,
}

#[derive(Default)]
pub(super) struct DedicatedCache {
    /// Empty pages with resident backing, keyed by retain order (oldest
    /// first). A retained page's backing is not touched until it is removed,
    /// so the two indexes below stay exact.
    pub(super) retained: BTreeMap<u64, PagePointer>,
    /// Retained pages by the stride their backing was born for, then by
    /// shape: payload headroom and payload alignment, which decide the
    /// ownership prefix and alignment a page can serve. A lookup skips whole
    /// shapes that cannot fit instead of testing their pages one by one.
    by_born: HashMap<usize, BTreeMap<Shape, BTreeSet<u64>>>,
    /// Retained pages whose extent has not grown, by shape, then by used
    /// bytes, newest first among equal sizes.
    ungrown: BTreeMap<Shape, BTreeSet<(usize, Reverse<u64>)>>,
    next_retained: u64,
    pub(super) retained_bytes: usize,
    /// Backing that can be resident in live mappings, including reuse slack.
    demand: usize,
    peak_demand: usize,
}

impl DedicatedCache {
    /// When backing grows, return older stock above the live high-water mark.
    /// Reusing a reservation can bring back a later phase of the same
    /// workload, so leave room for its other extents.
    #[inline(never)]
    pub(super) fn add_live(
        &mut self,
        from: usize,
        to: usize,
        grew: bool,
        reused_reservation: bool,
    ) {
        self.demand = self.demand - from + to;
        self.peak_demand = self.peak_demand.max(self.demand);
        if !grew {
            return;
        }
        let margin = if reused_reservation {
            (self.peak_demand / 6).min(DEDICATED_REUSE_MARGIN)
        } else {
            0
        };
        while !self.retained.is_empty()
            && self.demand + self.retained_bytes > self.peak_demand + margin
        {
            let oldest = self.remove_oldest().unwrap();
            self.purge(oldest);
        }
    }

    pub(super) fn take(&mut self, class: Class) -> Option<PagePointer> {
        let needed = class.stride.checked_next_multiple_of(page_bytes())?;
        let fit = |page: PagePointer| {
            // SAFETY: the heap owns these empty pages and their mappings.
            let (backing, payload) = unsafe {
                let page = page.as_ref();
                (page.backing.dedicated(), (*page.slots.get()).first)
            };
            let extent = backing.extent(payload);
            extent >= needed
                && payload.addr().get() & (class.align - 1) == 0
                && backing.headroom(payload) >= own_prefix(class.kind)
        };
        // Preserve the reservation for a repeated growth sequence: the newest
        // fitting page born for this stride. Otherwise only extents that have
        // not grown since their last purge can serve a different starting
        // size. Among those, take the smallest resident fit (the newest of
        // equal sizes), so buffers that cycle through varying sizes keep
        // reusing their warm extents instead of evicting them for fresh ones.
        // Both are found through the indexes, not by scanning every page.
        // A shape can hold a fitting page only when its headroom holds the
        // class's prefix and its payload alignment meets the class's. `fit`
        // still decides each page.
        let serves = |shape: Shape| {
            shape.headroom >= own_prefix(class.kind)
                && (shape.align >= class.align || class.align > page_bytes())
        };
        let mut best = None;
        let born = self.by_born.get(&class.stride).into_iter().flatten();
        for (&shape, seqs) in born {
            if !serves(shape) {
                continue;
            }
            if let Some(seq) = seqs
                .iter()
                .rev()
                .copied()
                .find(|seq| fit(self.retained[seq]))
                && best.is_none_or(|newest| seq > newest)
            {
                best = Some(seq);
            }
        }
        if best.is_none() {
            let mut smallest: Option<(usize, Reverse<u64>)> = None;
            for (&shape, sizes) in &self.ungrown {
                if !serves(shape) {
                    continue;
                }
                if let Some(&key) = sizes
                    .range((needed, Reverse(u64::MAX))..)
                    .take_while(|(used, _)| used / DEDICATED_REUSE_WASTE <= needed)
                    .find(|(_, Reverse(seq))| fit(self.retained[seq]))
                    && smallest.is_none_or(|current| key < current)
                {
                    smallest = Some(key);
                }
            }
            best = smallest.map(|(_, Reverse(seq))| seq);
        }
        let page = self.remove_retained(best?);
        // SAFETY: the page has been taken off the cache and has no live slots.
        unsafe {
            let header = page.as_ref();
            let payload = (*header.slots.get()).first;
            let backing = header.backing.dedicated();
            if backing.extend(payload, class.stride) {
                Some(page)
            } else {
                Page::destroy(page);
                None
            }
        }
    }

    pub(super) fn return_to_pool(&mut self, page: PagePointer) {
        // SAFETY: this heap owns the empty mapping until it reaches the pool.
        let bytes = unsafe {
            let page = page.as_ref();
            page.backing.dedicated().backed((*page.slots.get()).first)
        };
        self.demand -= bytes;
        self.purge(page);
    }

    /// Move an emptied page's backing from live demand to retained stock.
    pub(super) fn retain(&mut self, page: PagePointer) {
        // SAFETY: this heap owns the empty page, its mapping and its key.
        let bytes = unsafe {
            let page = page.as_ref();
            page.backing.dedicated().backed((*page.slots.get()).first)
        };
        self.demand -= bytes;
        self.retained_bytes += bytes;
        let seq = self.next_retained;
        self.next_retained += 1;
        self.retained.insert(seq, page);
        let (born, shape, used, grown) = Self::retained_key(page);
        self.by_born
            .entry(born)
            .or_default()
            .entry(shape)
            .or_default()
            .insert(seq);
        if !grown {
            self.ungrown
                .entry(shape)
                .or_default()
                .insert((used, Reverse(seq)));
        }
    }

    /// A retained page's index keys: its backing's born stride, its shape,
    /// its used bytes and whether its extent grew.
    fn retained_key(page: PagePointer) -> (usize, Shape, usize, bool) {
        // SAFETY: this heap owns the empty page and its mapping.
        let (backing, payload) = unsafe {
            let page = page.as_ref();
            (page.backing.dedicated(), (*page.slots.get()).first)
        };
        let address = payload.addr().get();
        let shape = Shape {
            headroom: backing.headroom(payload),
            align: (address & address.wrapping_neg()).min(page_bytes()),
        };
        (
            backing.born.get(),
            shape,
            backing.used.get(),
            backing.grown.get(),
        )
    }

    pub(super) fn remove_oldest(&mut self) -> Option<PagePointer> {
        let (&seq, _) = self.retained.first_key_value()?;
        Some(self.remove_retained(seq))
    }

    fn remove_retained(&mut self, seq: u64) -> PagePointer {
        let page = self.retained.remove(&seq).unwrap();
        let (born, shape, used, grown) = Self::retained_key(page);
        if let Some(shapes) = self.by_born.get_mut(&born) {
            if let Some(seqs) = shapes.get_mut(&shape) {
                seqs.remove(&seq);
                if seqs.is_empty() {
                    shapes.remove(&shape);
                }
            }
            if shapes.is_empty() {
                self.by_born.remove(&born);
            }
        }
        if !grown && let Some(sizes) = self.ungrown.get_mut(&shape) {
            sizes.remove(&(used, Reverse(seq)));
            if sizes.is_empty() {
                self.ungrown.remove(&shape);
            }
        }
        // SAFETY: this heap exclusively owns the removed empty page.
        self.retained_bytes -= unsafe {
            let page = page.as_ref();
            page.backing.dedicated().backed((*page.slots.get()).first)
        };
        page
    }

    pub(super) fn purge(&mut self, page: PagePointer) {
        // SAFETY: the page is empty and detached from the retained index.
        // Destroying it discards its pages and returns its granules to the arena.
        unsafe { Page::destroy(page) };
    }
}
