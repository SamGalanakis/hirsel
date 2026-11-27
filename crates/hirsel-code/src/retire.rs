//! What the loader does with dead code (PACK-4 step 5): retiring a dead image and
//! recycling a dead cohort.

use super::{CauseDataHandle, ContentHash, Image, Refusal, Shared, Slab, State, host};
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
    /// Includes the private tail of a partial final AArch64 instruction.
    pub(super) owned: usize,
    /// Its literal range `(offset, length)` in the data band (0, 0 when none).
    pub(super) data: (usize, usize),
    pub(super) bytes: u64,
    /// Whether its bytes counted against the retired-code budget when it died.
    pub(super) retired: bool,
    /// Independent R/NX cause data owner held by this image (released at reclaim).
    pub(super) cause_data: Option<CauseDataHandle>,
}

// SAFETY: `next` is read only by the thread that took the whole pending list.
unsafe impl Send for Retire {}
// SAFETY: as above.
unsafe impl Sync for Retire {}

impl Drop for Shared {
    fn drop(&mut self) {
        let mut next = *self.pending.get_mut();
        while !next.is_null() {
            // SAFETY: the list owns each record on it and nothing else reaches the list.
            let record = unsafe { Box::from_raw(next) };
            next = record.next;
            if let (Some(handle), Some(ops)) = (record.cause_data, &self.config.cause_data) {
                // SAFETY: `Shared` is being dropped so no lookup can observe this owner.
                unsafe { (ops.release)(handle.owner) };
            }
        }
        if self.config.band_bytes > 0 {
            host::release(self.base, 2 * self.config.band_bytes);
            if self.writer != self.base {
                host::release(self.writer, self.config.band_bytes);
            }
        }
    }
}

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
    /// Fill `[from, to)` of the text band with the trap pattern.
    pub(super) fn trap(&self, from: usize, to: usize) {
        debug_assert!(from <= to && to <= self.config.band_bytes);
        if from == to {
            return;
        }
        host::write(&mut || {
            let at = self.writer + from;
            // SAFETY: the slab lock owns this disjoint extent; the last call has returned.
            unsafe {
                #[cfg(target_arch = "x86_64")]
                std::ptr::write_bytes(at as *mut u8, 0xcc, to - from);
                #[cfg(target_arch = "aarch64")]
                {
                    debug_assert!((at | to).is_multiple_of(4));
                    for offset in (0..to - from).step_by(4) {
                        ((at + offset) as *mut u32).write_volatile(0); // one aligned UDF #0
                    }
                }
            }
        });
    }

    pub(super) fn synchronize(&self, start: usize, end: usize) -> std::io::Result<()> {
        host::publish(self.writer + start, self.text(start), end - start)
    }

    /// Retire every image that died since the last call (PACK-4 step 5): its interning
    /// entry and PC table row go before its instructions are overwritten.
    pub(super) fn reclaim(&self, slab: &mut Slab) -> Result<(), Refusal> {
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
            if cohort.state == State::Published {
                self.table.change((at, at + length), &[]);
            }
            if record.data.1 != 0 {
                if let Some(pos) = cohort.data_owners.iter().position(|r| {
                    r.start as usize == record.data.0 && r.length as usize == record.data.1
                }) {
                    cohort.data_owners.swap_remove(pos);
                }
                if cohort.state == State::Published {
                    // Removes the row and waits for all hazard slots before releasing `cause_data`.
                    self.data_table
                        .change((record.data.0, record.data.0 + record.data.1), &[]);
                }
            }
            if let (Some(handle), Some(ops)) = (record.cause_data, &self.config.cause_data) {
                // SAFETY: image's data-table row is gone and no hazard reader holds it;
                // release the image's one count (surviving causes retain their own).
                unsafe { (ops.release)(handle.owner) };
            }
            if record.owned != 0 {
                self.trap(at, at + record.owned);
                if self.synchronize(at, at + record.owned).is_err() {
                    cohort.withheld = true;
                    self.unrevoked
                        .fetch_add(record.owned as u64, Ordering::Relaxed);
                }
            }
            if cohort.live == 0 && cohort.state != State::Open && !cohort.withheld {
                self.recycle(slab, id);
            }
        }
        if self.unrevoked.load(Ordering::Relaxed) != 0 {
            return Err(Refusal::Unsupported);
        }
        Ok(())
    }

    /// Return a dead cohort’s runs after the last successful trap fill (PACK-4 step 5).
    pub(super) fn recycle(&self, slab: &mut Slab, id: u64) {
        let cohort = slab.cohorts.remove(&id).unwrap();
        // No reader of the PC table or data table names the cohort once this returns.
        self.table.change(cohort.text, &[]);
        self.data_table.change(cohort.data, &[]);
        // Keep the file-backed trap bytes intact until the first overwrite for reuse.
        slab.text.give(cohort.text.0, cohort.text.1 - cohort.text.0);
        let (start, end) = cohort.data;
        let bytes = end - start;
        if bytes > 0 && host::protect(self.data(start), bytes, Access::None).is_ok() {
            host::discard(self.data(start), bytes);
            slab.data.give(start, bytes);
        }
        slab.installed -= cohort.bytes;
        *self.retired.lock().unwrap() -= cohort.retired;
    }
}
