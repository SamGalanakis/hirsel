//! Admission against a payer record: block charges, which the block's payer
//! word keeps on this record's `outstanding` until its refund, and frame
//! charges, which no block names and so move only the room.
use super::{Capacity, LimitRecord, Ordering, Payer};

impl Payer {
    #[inline(always)]
    pub(crate) fn admit(self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.debit::<true>(bytes)
    }
    /// Admit a frame charge (`alloc .. frame`). No block holds a payer word
    /// for frame bytes, so the charge pins no slot and stays out of this
    /// record's `outstanding`: a union while the frame lives (an isolate's
    /// cell merging into a bound scope that then outranks this record) may
    /// make another record current, and `refund_frame` credits that one.
    #[inline(always)]
    pub(crate) fn admit_frame(self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.debit::<false>(bytes)
    }
    /// Return a frame charge to this record, the context's current one. The
    /// body that admitted it enters no bound scope (frame lists and logs), so
    /// the current limit is the admitting limit, now led by this record.
    #[inline(always)]
    pub(crate) fn refund_frame(self, bytes: u64) {
        // SAFETY: the current record is canonical and task-exclusive.
        let r = unsafe { &mut *self.local() };
        #[cfg(feature = "prof")]
        self.observe(r.available);
        r.available = r.available.wrapping_add(bytes);
    }
    #[inline(always)]
    fn debit<const PINS: bool>(self, bytes: Option<u64>) -> Result<(), Capacity> {
        let payer = self;
        #[cfg(any(test, feature = "testing"))]
        let injected = payer.injected();
        #[cfg(not(any(test, feature = "testing")))]
        let injected = false;
        // SAFETY: admission takes a canonical live payer under the
        // task's exclusive custody. Block words canonicalize before admission.
        let r = unsafe { &mut *payer.0.as_ptr().cast::<LimitRecord>() };
        match bytes {
            Some(n) if !injected && n <= r.available => {
                r.available = r.available.wrapping_sub(n);
                if PINS {
                    r.outstanding = r.outstanding.wrapping_add(n);
                }
                #[cfg(feature = "prof")]
                payer.observe(r.available);
                #[cfg(any(test, feature = "charge-observation"))]
                crate::peak::sample();
                Ok(())
            }
            n => Err(crate::tasks::capacity_refused(
                n.unwrap_or(u64::MAX),
                r.available,
            )),
        }
    }
    pub(crate) fn admit_run_owned(self, bytes: Option<u64>) -> Result<(), Capacity> {
        self.admit(bytes)?;
        self.domain()
            .run_owned
            .fetch_add(bytes.unwrap(), Ordering::Relaxed);
        Ok(())
    }
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn injected(self) -> bool {
        let f = self.forest();
        f.attempts += 1;
        f.fail
            .is_some_and(|(n, later)| f.attempts == n || later && f.attempts >= n)
    }
}
