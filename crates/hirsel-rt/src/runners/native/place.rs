//! A `db@1` place that home serves (STORE-12, runners.md `db@1`): the rows its head's
//! operations and home share. Home queues each arrival and wakes the head's parked take; the
//! head takes arrivals, reads them and ends each once, and home completes the caller's call
//! with that end. Only owned bytes cross between the head's workers and home.
use super::*;
use std::collections::BTreeMap;

/// A served call as `arrival.request` states it.
#[derive(Clone)]
pub(crate) struct Arrived {
    pub form: CallForm,
    pub path: String,
    pub fingerprint: [u8; 32],
    pub code: [u8; 32],
    pub caller: String,
    pub stream: [u8; 32],
    pub epoch: u64,
    pub position: u64,
    pub digest: [u8; 32],
    pub acknowledged: u64,
    pub level: Option<String>,
    pub argument: Vec<u8>,
}

/// An arrival the place owes an end: the caller's call key, and the call.
pub(super) struct Owed {
    pub key: [u8; 32],
    pub call: Arrived,
}

#[derive(Default)]
struct Rows {
    /// `io.inbox` granted the inbox.
    granted: bool,
    /// The inbox was dropped or the head ended: nothing more arrives.
    closed: bool,
    queued: VecDeque<Owed>,
    taken: BTreeMap<u64, Owed>,
    arrivals: u64,
    /// The head task parked in `inbox.take(i, true)`.
    waiter: Option<(u32, u32)>,
    /// Ends home has yet to give the callers.
    ends: Vec<([u8; 32], CallOutcome)>,
}

pub(crate) struct Place {
    rows: Mutex<Rows>,
    /// Home's work signal: an end is ready to complete.
    signal: Arc<crate::readiness::Signal>,
}

/// What a take hands over.
pub(crate) enum Take {
    /// The arrivals' ids, oldest first (empty when the place is closed or nothing waited).
    Arrivals(Vec<u64>),
    /// Nothing queued yet: park until home wakes `waiter`.
    Park,
}

impl Place {
    pub(super) fn new(signal: Arc<crate::readiness::Signal>) -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::default(),
            signal,
        })
    }

    // ----- The head's side ----------------------------------------------------------------

    /// `io.inbox`: the inbox, once.
    pub(crate) fn grant(&self) -> bool {
        !std::mem::replace(&mut self.rows.lock().unwrap().granted, true)
    }

    /// `inbox.take(i, wait)`: every queued arrival; with `wait`, park `waiter` while none is
    /// queued and the place is open.
    pub(crate) fn take(&self, wait: bool, waiter: (u32, u32)) -> Take {
        let mut rows = self.rows.lock().unwrap();
        if rows.queued.is_empty() && !rows.closed && wait {
            rows.waiter = Some(waiter);
            return Take::Park;
        }
        let queued = std::mem::take(&mut rows.queued);
        let mut ids = Vec::with_capacity(queued.len());
        for owed in queued {
            rows.arrivals += 1;
            let id = rows.arrivals;
            rows.taken.insert(id, owed);
            ids.push(id);
        }
        Take::Arrivals(ids)
    }

    /// `arrival.request`: the taken arrival `id`'s call.
    pub(crate) fn request(&self, id: u64) -> Option<Arrived> {
        self.rows
            .lock()
            .unwrap()
            .taken
            .get(&id)
            .map(|o| o.call.clone())
    }

    /// `arrival.end`: arrival `id` ends with `outcome`; false when it already ended.
    pub(crate) fn end(&self, id: u64, outcome: CallOutcome) -> bool {
        let mut rows = self.rows.lock().unwrap();
        let Some(owed) = rows.taken.remove(&id) else {
            return false;
        };
        rows.ends.push((owed.key, outcome));
        drop(rows);
        self.signal.notify();
        true
    }

    /// Dropping the inbox (`id` 0) ends every untaken call `NotRun(Unreachable())`; dropping
    /// an arrival ends it `InDoubt(Abandoned())`.
    pub(crate) fn close(&self, id: u64) {
        let mut rows = self.rows.lock().unwrap();
        if id == 0 {
            rows.closed = true;
            let queued = std::mem::take(&mut rows.queued);
            rows.ends.extend(
                queued
                    .into_iter()
                    .map(|o| (o.key, CallOutcome::NotRun(UNREACHABLE))),
            );
        } else if let Some(owed) = rows.taken.remove(&id) {
            rows.ends.push((owed.key, CallOutcome::InDoubt(ABANDONED)));
        }
        drop(rows);
        self.signal.notify();
    }

    /// Whether the head may still receive calls: its parked take has an event source.
    pub(crate) fn open(&self) -> bool {
        !self.rows.lock().unwrap().closed
    }

    // ----- Home's side --------------------------------------------------------------------

    /// `owed` arrives: queued for the head, whose parked take is returned to wake; a closed
    /// place ends it `NotRun(Unreachable())` at once.
    pub(super) fn arrive(&self, owed: Owed) -> Option<(u32, u32)> {
        let mut rows = self.rows.lock().unwrap();
        if rows.closed {
            rows.ends.push((owed.key, CallOutcome::NotRun(UNREACHABLE)));
            return None;
        }
        rows.queued.push_back(owed);
        rows.waiter.take()
    }

    /// The head ended: the calls it never took never ran, those it took are in doubt.
    pub(super) fn head_ended(&self) {
        let mut rows = self.rows.lock().unwrap();
        rows.closed = true;
        rows.waiter = None;
        let queued = std::mem::take(&mut rows.queued);
        let taken = std::mem::take(&mut rows.taken);
        let ends = queued
            .into_iter()
            .map(|o| (o.key, CallOutcome::NotRun(UNREACHABLE)))
            .chain(
                taken
                    .into_values()
                    .map(|o| (o.key, CallOutcome::InDoubt(ABANDONED))),
            )
            .collect::<Vec<_>>();
        rows.ends.extend(ends);
    }

    /// The ends home has yet to give, oldest first.
    pub(super) fn ends(&self) -> Vec<([u8; 32], CallOutcome)> {
        std::mem::take(&mut self.rows.lock().unwrap().ends)
    }
}

/// `NotRunReason::Unreachable`.
pub(crate) const UNREACHABLE: u32 = 2;
