//! What the loader does with dead code (PACK-4 step 5): retiring a dead image and
//! recycling a dead cohort.

use super::{Cohort, ContentHash, Image, Shared, Slab, State, host};
use host::Access;
use std::ptr::null_mut;
use std::sync::Weak;
use std::sync::atomic::Ordering;

/// What a dead image leaves for the loader to retire. It is made at install, so the
/// last drop of a handle allocates nothing and takes no lock.
pub(super) struct Retire {
    pub(super) next: *mut Retire,
    /// Which install of its digest this was.
    pub(super) serial: u64,
    pub(super) digest: ContentHash,
    pub(super) cohort: Option<u64>,
    /// Its text's offset in the band and its length.
    pub(super) text: (usize, usize),
    pub(super) bytes: u64,
    /// Whether its bytes counted against the retired-code budget when it died.
    pub(super) retired: bool,
}

// SAFETY: `next` is read only by the thread that took the whole pending list.
unsafe impl Send for Retire {}
// SAFETY: as above.
unsafe impl Sync for Retire {}

impl Drop for Image {
    /// Hand the image to the loader (PACK-4 step 5). This is on the path of whoever
    /// dropped the last handle, a dispatching worker included, so it takes no lock,
    /// allocates nothing and makes no system call.
    fn drop(&mut self) {
        let mut record = self.retire.take().unwrap();
        record.retired = self.retired.load(Ordering::Relaxed);
        let pending = &self.shared.pending;
        let record = Box::into_raw(record);
        let mut head = pending.load(Ordering::Relaxed);
        loop {
            // SAFETY: the record is this thread's until the exchange below succeeds.
            unsafe { (*record).next = head };
            match pending.compare_exchange_weak(head, record, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(now) => head = now,
            }
        }
    }
}

impl Shared {
    /// Retire every image that died since the last call (PACK-4 step 5): its interning
    /// entry and PC table row go, each text page it was the last live image on is
    /// closed, and a cohort with no live image is recycled.
    pub(super) fn reclaim(&self, slab: &mut Slab) {
        let mut next = self.pending.swap(null_mut(), Ordering::Acquire);
        while !next.is_null() {
            // SAFETY: the list owns each record on it, and the swap took the whole list.
            let record = unsafe { Box::from_raw(next) };
            next = record.next;
            let mine = |held: &(u64, Weak<Image>)| held.0 == record.serial;
            if slab.images.get(&record.digest).is_some_and(mine) {
                slab.images.remove(&record.digest);
            }
            let Some(id) = record.cohort else { continue };
            let cohort = slab.cohorts.get_mut(&id).unwrap();
            cohort.live -= 1;
            if record.retired {
                cohort.retired += record.bytes;
            }
            let (at, length) = record.text;
            let row = cohort.images.iter().position(|(row, digest)| {
                (row.start as usize, row.length as usize, *digest) == (at, length, record.digest)
            });
            cohort.images.swap_remove(row.unwrap());
            let published = cohort.state == State::Published;
            if cohort.state == State::Open {
                // The batch is still writable: the dead image's text never runs.
                self.trap(at, at + length);
            }
            if length > 0 {
                if published {
                    // No fresh lookup names the image once this returns.
                    self.table.change((at, at + length), &[]);
                }
                let first = cohort.text.0 / self.page;
                let pages = (
                    at / self.page - first,
                    (at + length - 1) / self.page - first + 1,
                );
                for page in pages.0..pages.1 {
                    cohort.pages[page] -= 1;
                }
                if published {
                    self.revoke(cohort, pages, (at, at + length));
                }
            }
            if cohort.live == 0 && cohort.state != State::Open {
                self.recycle(slab, id);
            }
        }
    }

    /// Close each page of `pages` (indices in the cohort's text run) that holds no live
    /// image. Where the host can write published text, fill the part of `dead` that lies
    /// on pages a live image still shares with the trap pattern.
    pub(super) fn revoke(&self, cohort: &Cohort, pages: (usize, usize), dead: (usize, usize)) {
        let first = cohort.text.0 / self.page;
        let mut page = pages.0;
        while page < pages.1 {
            let start = page;
            let live = cohort.pages[page] != 0;
            while page < pages.1 && (cohort.pages[page] != 0) == live {
                page += 1;
            }
            let run = ((first + start) * self.page, (first + page) * self.page);
            if !live {
                let (at, bytes) = (self.text(run.0), run.1 - run.0);
                match host::protect(at, bytes, Access::None) {
                    Ok(()) => host::discard(at, bytes),
                    Err(_) => {
                        self.unrevoked.fetch_add(bytes as u64, Ordering::Relaxed);
                    }
                }
            } else if host::FILLS {
                let (from, to) = (dead.0.max(run.0), dead.1.min(run.1));
                self.trap(from, to);
                if to > from && host::publish(self.text(from), to - from).is_err() {
                    self.unrevoked
                        .fetch_add((to - from) as u64, Ordering::Relaxed);
                }
            }
        }
    }

    /// Unmap a dead cohort and return its runs (PACK-4 step 5).
    pub(super) fn recycle(&self, slab: &mut Slab, id: u64) {
        let cohort = slab.cohorts.remove(&id).unwrap();
        // No reader of the PC table names the cohort once this returns.
        self.table.change(cohort.text, &[]);
        // A run that cannot be unmapped stays as it is and is never used again.
        let gone = |at: usize, (start, end): (usize, usize)| {
            let gone = end == start || host::protect(at, end - start, Access::None).is_ok();
            if gone && end > start {
                host::discard(at, end - start);
            }
            gone
        };
        if gone(self.text(cohort.text.0), cohort.text) {
            slab.text.give(cohort.text.0, cohort.text.1 - cohort.text.0);
        }
        if gone(self.data(cohort.data.0), cohort.data) {
            slab.data.give(cohort.data.0, cohort.data.1 - cohort.data.0);
        }
        slab.installed -= cohort.bytes;
        *self.retired.lock().unwrap() -= cohort.retired;
    }
}
