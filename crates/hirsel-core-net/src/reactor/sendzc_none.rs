//! Hosts without io_uring send from the caller's own bytes: no poller hands
//! out a transmit slot, so no [`TxSlot`] ever exists.
use super::Poller;
use std::{convert::Infallible, io, ops::Range, sync::Arc};

/// What a slot's latest send has done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxState {
    /// Nothing submitted since the slot was taken.
    Filling,
    /// The kernel has not answered the send yet.
    Sending,
    /// The kernel accepted this many bytes, or failed with `-errno`.
    Sent(i32),
}

pub struct TxSlot(Infallible);

impl Poller {
    /// None: only an io_uring ring sends zero-copy.
    pub fn tx_slot(self: &Arc<Self>, _len: usize) -> Option<TxSlot> {
        None
    }

    pub fn tx_notifications(&self) -> (u64, u64) {
        (0, 0)
    }
}

impl TxSlot {
    pub fn len(&self) -> usize {
        match self.0 {}
    }

    pub fn is_empty(&self) -> bool {
        match self.0 {}
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        match self.0 {}
    }

    pub fn send(&mut self, _tag: u64, _range: Range<usize>) -> io::Result<()> {
        match self.0 {}
    }

    pub fn state(&self) -> TxState {
        match self.0 {}
    }

    pub fn cancel(&self) {
        match self.0 {}
    }
}
