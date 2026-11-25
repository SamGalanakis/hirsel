//! A unit's recording (docs/recording.md, unit tapes U3): its unit
//! operations, its take points and its `TAKE` frames. Each hook is a no-op in
//! a build without the recorder.
#[cfg(feature = "record")]
use super::{ClientH, Ended, LangUnit, Took, tasks};
#[cfg(feature = "record")]
use crate::tape::units::{self as tape, Arrival};
#[cfg(feature = "record")]
use std::sync::Arc;

#[cfg(feature = "record")]
impl LangUnit {
    /// The unit's recording: its helper stream's, or that of the run whose
    /// task first reaches it.
    pub(super) fn tape(&mut self) -> Option<crate::tape::schedule::Group> {
        if self.tape.is_none() {
            self.tape = tasks::current_group();
        }
        self.tape
    }

    /// The unit's code reached an in-operation take point.
    pub(super) fn reach(&mut self) {
        tape::reach(self.tape());
    }

    /// Takes from now are at a scheduler pump (point kind 1), or not.
    pub(super) fn set_pumping(&mut self, pumping: bool) {
        self.pumping = pumping;
    }
}

#[cfg(not(feature = "record"))]
impl super::LangUnit {
    #[inline(always)]
    pub(super) fn reach(&mut self) {}
    #[inline(always)]
    pub(super) fn set_pumping(&mut self, _: bool) {}
}

/// Record one take: its `TAKE` in the unit's stream.
/// # Safety
/// As `on_take`, before it acts on the take.
#[cfg(feature = "record")]
#[cold]
#[inline(never)]
pub(super) unsafe fn take(u: &mut LangUnit, took: &Took, id: usize) {
    let Some(group) = u.tape() else {
        return;
    };
    let arrival = match took {
        Took::Item => {
            let Some(&h) = u.inboxes.get(&id) else {
                return;
            };
            // SAFETY: a registered inbox is pinned while registered.
            let inbox = unsafe { &(*h).inbox };
            let Some(t) = inbox.0.side().taken.back() else {
                return;
            };
            Arrival::Item {
                conn: id,
                flat: inbox.flat_ptr(t),
                content: t.value().cross_parts().1,
            }
        }
        Took::Close => Arrival::Close(id),
        Took::SealAnswer => Arrival::SealAnswer(id),
        Took::Accepted(s) => Arrival::Settlement(*s, 0),
        Took::Closed(s) => Arrival::Settlement(*s, 1),
        Took::Withdrawn(s) => Arrival::Settlement(*s, 2),
        // Its end take writes it, with the helper's outcome.
        Took::HelperEnd => return,
        Took::LastRelease => Arrival::LastRelease(id),
        Took::Credits => {
            Arrival::Unrecorded("a credits take, whose limit ordinals this build does not write")
        }
        Took::SharedRelease => {
            Arrival::Unrecorded("a Shared node's release, whose ordinal this build does not write")
        }
        Took::Cancel => Arrival::Cancel,
    };
    // SAFETY: the item is the inbox's newest, still in its connection.
    unsafe { tape::take(Some(group), u.pumping, arrival) };
}

#[cfg(not(feature = "record"))]
#[inline(always)]
pub(super) unsafe fn take(_: &mut super::LangUnit, _: &super::Took, _: usize) {}

/// `conn.new`: the connection's ordinal, then its client's.
#[cfg(feature = "record")]
pub(super) fn conn_new(
    u: &mut LangUnit,
    key: usize,
    client: *mut ClientH,
    item: &super::Cross,
    cap: u64,
    b: u64,
) {
    tape::conn_new(u.tape(), key, client as usize, item, cap, b);
}

/// `conn.client`: the new client's ordinal.
#[cfg(feature = "record")]
pub(super) fn client_new(u: &mut LangUnit, client: *mut ClientH) {
    tape::client_new(u.tape(), client as usize);
}

/// A client's drop: its `CLOSE`.
#[cfg(feature = "record")]
pub(super) fn client_drop(u: &mut LangUnit, client: *mut ClientH) {
    tape::client_drop(u.tape(), client as usize);
}

/// An inbox's drop: its `CLOSE`.
#[cfg(feature = "record")]
pub(super) fn inbox_drop(u: &mut LangUnit, key: usize) {
    tape::inbox_drop(u.tape(), key);
}

/// The connection a client handle's word names, by its address.
/// # Safety
/// `word` is a live client handle.
#[cfg(feature = "record")]
pub(crate) fn client_conn(word: u64) -> usize {
    // SAFETY: by the contract.
    unsafe { Arc::as_ptr(&(*(word as *const ClientH)).client.0) as usize }
}

/// One send's `CONN_SEND` or `CONN_TRY_SEND`: the item, read before the send
/// moves it, written once its outcome is known.
#[cfg(feature = "record")]
pub(super) struct SendNote {
    group: Option<crate::tape::schedule::Group>,
    op: u8,
    item: Option<Result<(u64, Vec<u8>), &'static str>>,
}

#[cfg(feature = "record")]
impl SendNote {
    /// # Safety
    /// `value` is the item, laid out as its connection's items are.
    pub(super) unsafe fn new(client: *mut ClientH, value: *const u8, wait: bool) -> Self {
        let group = tasks::current_group();
        // SAFETY: by the contract; the sender's handle pins the client.
        let item = unsafe {
            let conn = Arc::as_ptr(&(*client).client.0) as usize;
            tape::send_item(group, client as usize, conn, value)
        };
        let op = if wait {
            hirsel_tape_format::op::CONN_SEND
        } else {
            hirsel_tape_format::op::CONN_TRY_SEND
        };
        Self { group, op, item }
    }

    /// An outcome at once as an `OP`, or a wait as an `ISSUE`.
    pub(super) fn note(&mut self, outcome: Option<u64>, waiting: Option<u32>) {
        if let Some(item) = self.item.take() {
            tape::send(self.group, self.op, item, outcome, waiting);
        }
    }
}

#[cfg(not(feature = "record"))]
pub(super) struct SendNote;

#[cfg(not(feature = "record"))]
impl SendNote {
    #[inline(always)]
    pub(super) unsafe fn new(_: *mut super::ClientH, _: *const u8, _: bool) -> Self {
        Self
    }
    #[inline(always)]
    pub(super) fn note(&mut self, _: Option<u64>, _: Option<u32>) {}
}

/// The owner's helper-end `TAKE`, with the helper's outcome and the
/// connections it paid for that survive it.
#[cfg(feature = "record")]
pub(super) unsafe fn end_take(u: &mut LangUnit, key: usize, ended: &Ended) {
    let Some(outcome) = &ended.tape_end else {
        return;
    };
    let kept = ended.survivors.len() - ended.homed;
    let unnamed = (ended.homed != 0 || !ended.charges.is_empty())
        .then_some("a helper end keeps a Shared node or a Secret or UDP charge, which this build does not name");
    let arrival = Arrival::HelperEnd {
        key,
        outcome,
        survivors: &ended.survivors[..kept],
        unnamed,
    };
    // SAFETY: an end take carries no item.
    unsafe { tape::take(u.tape(), u.pumping, arrival) };
}
