//! io_uring SEND_ZC from runtime-owned transmit slots.
//!
//! The ring owns every slot whose bytes the kernel may still read. A slot
//! counts the sends it has in the kernel (`outstanding`) and the zero-copy
//! notifications still owed for them (`notifs`), and its bytes are reused or
//! freed only when both are 0: after the last `IORING_CQE_F_NOTIF`. A
//! holder's [`TxSlot`] is only a claim; dropping it (cancel, abort, a closed
//! connection) leaves a busy slot to the ring, which recycles it at its last
//! notification. A ring dropped with notifications owed leaks those slots'
//! bytes rather than free memory the kernel may still read.
//!
//! Each slot is registered as a fixed buffer while it holds bytes, at its own
//! index of a sparse table, so a send pins no pages per call. A slot that
//! ends idle keeps its bytes for the next send while sends continue, as many
//! as the busiest moment needed (a stalled peer holds a socket buffer's worth
//! awaiting notifications), so steady sending allocates nothing. Once a full
//! [`TRIM`] passes with no slot taken, the driver turn unregisters and frees
//! every idle slot: a ring that stopped sending holds no slot bytes, and a
//! connection that is not sending never holds a slot. A refused registration (a memory-lock limit) leaves the slot
//! unregistered, and its sends name its bytes directly.
use super::{Backend, Poller, Uring};
use std::{
    io,
    ops::Range,
    sync::{Arc, MutexGuard},
};

/// Completions of transmit slots: the unit 0xffff, which no shared tag uses
/// (WAKE and REMOVED sit at its top), then the slot index.
pub(super) const TX: u64 = 0xffff << 48;
const TX_MASK: u64 = 0xffff_ffff << 32;
/// Registered-buffer table entries; slots above it send unregistered.
pub(super) const REGISTERED: u32 = 64;
/// `IORING_SEND_ZC_REPORT_USAGE`, and the notification bit it sets when the
/// kernel copied the data after all (`IORING_NOTIF_USAGE_ZC_COPIED`).
const REPORT_USAGE: u16 = 1 << 3;
const ZC_COPIED: u32 = 1 << 31;
/// How long idle slots outlive the last slot taken; a blocking driver turn
/// waits at most this long while any are kept.
pub(super) const TRIM: std::time::Duration = std::time::Duration::from_secs(1);

pub(super) fn is_tx(user_data: u64) -> bool {
    user_data & TX_MASK == TX
}

#[derive(Default)]
struct Entry {
    bytes: Box<[u8]>,
    registered: bool,
    outstanding: u8,
    notifs: u8,
    held: bool,
    /// The owning socket's registration tag, readied at each completion.
    tag: u64,
    /// The first completion of the latest send, once it has arrived.
    result: Option<i32>,
}

impl Entry {
    fn busy(&self) -> bool {
        self.outstanding != 0 || self.notifs != 0
    }
}

#[derive(Default)]
pub(super) struct Table {
    entries: Vec<Entry>,
    /// Entries with no bytes and nothing in the kernel.
    free: Vec<u32>,
    /// Idle entries that keep their (registered) bytes.
    idle: Vec<u32>,
    /// Slots taken so far, and the count and time at the last trim check.
    taken: u64,
    trim_seen: u64,
    trim_since: Option<std::time::Instant>,
    /// Notifications whose data the kernel copied after all.
    pub(super) copied: u64,
    pub(super) notified: u64,
}

impl Drop for Table {
    fn drop(&mut self) {
        // The ring is already closed; its requests may still be read by the
        // kernel's exit work until their notifications, so never free them.
        for entry in &mut self.entries {
            if entry.busy() {
                std::mem::forget(std::mem::take(&mut entry.bytes));
            }
        }
    }
}

/// What a slot's latest send has done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxState {
    /// Nothing submitted since the slot was taken.
    Filling,
    /// The kernel has not answered the send yet.
    Sending,
    /// The kernel accepted this many bytes, or failed with `-errno`. Its
    /// bytes stay immutable until the slot is dropped.
    Sent(i32),
}

/// A claim on one transmit slot. The bytes are writable only before the
/// slot's first send; after it they belong to the kernel until its last
/// notification, and dropping the claim hands them back to the ring.
pub struct TxSlot {
    poller: Arc<Poller>,
    index: u32,
    data: *mut u8,
    len: usize,
    submitted: bool,
}

// SAFETY: the claim is the only user-space writer of its bytes, which the
// ring keeps alive; every shared field is behind the table lock.
unsafe impl Send for TxSlot {}

impl Poller {
    /// A transmit slot of `len` bytes, when this poller is an io_uring ring
    /// that sends zero-copy. None under epoll.
    pub fn tx_slot(self: &Arc<Self>, len: usize) -> Option<TxSlot> {
        let Backend::Uring(ring) = &self.backend else {
            return None;
        };
        if !ring.zc {
            return None;
        }
        let mut table = ring.tx();
        let index = match table
            .idle
            .iter()
            .position(|&i| table.entries[i as usize].bytes.len() == len)
        {
            Some(at) => table.idle.swap_remove(at),
            None => {
                let index = match table.free.pop() {
                    Some(index) => index,
                    None => {
                        let index = u32::try_from(table.entries.len()).ok()?;
                        table.entries.push(Entry::default());
                        index
                    }
                };
                let entry = &mut table.entries[index as usize];
                entry.bytes = vec![0; len].into_boxed_slice();
                entry.registered = index < REGISTERED && ring.register(index, &mut entry.bytes);
                index
            }
        };
        table.taken += 1;
        let entry = &mut table.entries[index as usize];
        entry.held = true;
        entry.result = None;
        let data = entry.bytes.as_mut_ptr();
        drop(table);
        Some(TxSlot {
            poller: self.clone(),
            index,
            data,
            len,
            submitted: false,
        })
    }

    /// Notifications so far, and how many of them report a copy.
    pub fn tx_notifications(&self) -> (u64, u64) {
        match &self.backend {
            Backend::Uring(ring) => {
                let table = ring.tx();
                (table.notified, table.copied)
            }
            Backend::Epoll(_) => (0, 0),
        }
    }
}

impl TxSlot {
    fn ring(&self) -> &Uring {
        match &self.poller.backend {
            Backend::Uring(ring) => ring,
            Backend::Epoll(_) => unreachable!(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The slot's bytes, before its first send.
    pub fn bytes(&mut self) -> &mut [u8] {
        assert!(!self.submitted, "a sent slot's bytes belong to the kernel");
        // SAFETY: the ring keeps these bytes alive while the claim exists,
        // and nothing else writes them before the first send.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.len) }
    }

    /// Send `range` of the slot on `fd`, readying `tag`'s writer when the
    /// kernel answers. One send at a time: the previous must have answered.
    pub fn send(&mut self, fd: i32, tag: u64, range: Range<usize>) -> io::Result<()> {
        use io_uring::{opcode, types};
        assert!(range.start <= range.end && range.end <= self.len);
        self.submitted = true;
        // SAFETY: in bounds of the slot's live bytes.
        let start = unsafe { self.data.add(range.start) };
        let ring = self.ring();
        let mut table = ring.tx();
        let entry = &mut table.entries[self.index as usize];
        assert_eq!(entry.outstanding, 0, "one send per slot at a time");
        let registered = entry.registered;
        entry.tag = tag;
        entry.result = None;
        entry.outstanding += 1;
        entry.notifs += 1;
        let send = opcode::SendZc::new(types::Fd(fd), start, range.len() as u32)
            .buf_index(registered.then_some(self.index as u16))
            .flags(libc::MSG_NOSIGNAL | libc::MSG_WAITALL)
            .zc_flags(REPORT_USAGE)
            .build()
            .user_data(TX | u64::from(self.index));
        // The counts are raised before the push, so its answer finds them.
        // The lock is not held across it: a full queue waits on the reaper,
        // which takes this lock for transmit completions.
        drop(table);
        let pushed = ring.push(send);
        if let Err(e) = &pushed {
            let mut table = ring.tx();
            let entry = &mut table.entries[self.index as usize];
            entry.outstanding -= 1;
            entry.notifs -= 1;
            entry.result = Some(-e.raw_os_error().unwrap_or(libc::EIO));
        }
        pushed
    }

    pub fn state(&self) -> TxState {
        let table = self.ring().tx();
        let entry = &table.entries[self.index as usize];
        match (entry.result, entry.outstanding) {
            (Some(result), 0) => TxState::Sent(result),
            (_, 0) if !self.submitted => TxState::Filling,
            _ => TxState::Sending,
        }
    }

    /// Ask the kernel to end this slot's send now. Its answer (a partial
    /// count or `-ECANCELED`) arrives as any other.
    pub fn cancel(&self) {
        let ring = self.ring();
        let outstanding = ring.tx().entries[self.index as usize].outstanding;
        if outstanding != 0 {
            let _ = ring.push(
                io_uring::opcode::AsyncCancel::new(TX | u64::from(self.index))
                    .build()
                    .user_data(super::REMOVED),
            );
        }
    }
}

impl Drop for TxSlot {
    fn drop(&mut self) {
        let ring = self.ring();
        let mut table = ring.tx();
        table.entries[self.index as usize].held = false;
        if !table.entries[self.index as usize].busy() {
            ring.recycle(&mut table, self.index);
        }
    }
}

impl Uring {
    pub(super) fn tx(&self) -> MutexGuard<'_, Table> {
        self.tx.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn register(&self, index: u32, bytes: &mut [u8]) -> bool {
        if !self.sparse {
            return false;
        }
        let iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        // SAFETY: the bytes stay alive and in place until unregistered or
        // leaked with the ring (Table::drop).
        unsafe {
            self.ring
                .submitter()
                .register_buffers_update(index, &[iov], None)
        }
        .is_ok()
    }

    fn unregister(&self, index: u32) {
        let iov = libc::iovec {
            iov_base: std::ptr::null_mut(),
            iov_len: 0,
        };
        // SAFETY: an empty entry names no memory.
        let _ = unsafe {
            self.ring
                .submitter()
                .register_buffers_update(index, &[iov], None)
        };
    }

    /// An entry neither held nor busy keeps its bytes for the next send.
    fn recycle(&self, table: &mut Table, index: u32) {
        table.entries[index as usize].result = None;
        table.idle.push(index);
    }

    /// Whether idle slots wait for a trim, so a blocking turn must wake for it.
    pub(super) fn tx_kept(&self) -> bool {
        !self.tx().idle.is_empty()
    }

    /// Under the driver turn: free every idle slot once a full [`TRIM`] has
    /// passed with none taken.
    pub(super) fn trim(&self) {
        let mut table = self.tx();
        if table.idle.is_empty() {
            table.trim_since = None;
            return;
        }
        let now = std::time::Instant::now();
        let taken = table.taken;
        match table.trim_since {
            Some(since) if table.trim_seen == taken => {
                if now.duration_since(since) < TRIM {
                    return;
                }
            }
            _ => {
                table.trim_seen = taken;
                table.trim_since = Some(now);
                return;
            }
        }
        table.trim_since = None;
        let table = &mut *table;
        for index in table.idle.drain(..) {
            let entry = &mut table.entries[index as usize];
            if entry.registered {
                self.unregister(index);
                entry.registered = false;
            }
            entry.bytes = Box::default();
            table.free.push(index);
        }
    }

    /// Apply one transmit completion. Returns the tag to ready, if any.
    pub(super) fn tx_complete(&self, user_data: u64, result: i32, flags: u32) -> Option<u64> {
        use io_uring::cqueue;
        let index = (user_data & u64::from(u32::MAX)) as u32;
        let mut table = self.tx();
        let entry = table.entries.get_mut(index as usize)?;
        if cqueue::notif(flags) {
            entry.notifs -= 1;
            let copied = result as u32 & ZC_COPIED != 0;
            let (held, tag, busy) = (entry.held, entry.tag, entry.busy());
            table.notified += 1;
            table.copied += u64::from(copied);
            if !held && !busy {
                self.recycle(&mut table, index);
            }
            return held.then_some(tag);
        }
        entry.outstanding -= 1;
        entry.result = Some(result);
        if !cqueue::more(flags) {
            // No notification follows a send that failed before taking data.
            entry.notifs -= 1;
        }
        let (held, tag, busy) = (entry.held, entry.tag, entry.busy());
        if !held && !busy {
            self.recycle(&mut table, index);
        }
        held.then_some(tag)
    }
}
