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
            self.set_tape(tasks::current_group());
        }
        self.tape
    }

    /// The unit's recording is `group`: a replaying unit parks what is
    /// published to it from now on, and takes only what its `TAKE`s name.
    pub(super) fn set_tape(&mut self, group: Option<crate::tape::schedule::Group>) {
        self.tape = group;
        let context = tasks::current_context();
        if self.files.is_null() && !context.is_null() {
            // SAFETY: the running task pins its context.
            self.files = unsafe { (*context).host_files };
        }
        if group.is_some_and(|group| group.replaying()) {
            self.ctx.park();
        }
    }

    /// The unit's code reached an in-operation take point.
    pub(crate) fn reach(&mut self) {
        tape::reach(self.tape());
    }

    /// The home's own drop of a node's last holder: its `shared release`
    /// take, at the position that drop reached.
    pub(crate) fn shared_release_take(&mut self, node: usize) {
        let group = self.tape();
        // SAFETY: a shared release carries no item.
        unsafe { tape::take(group, self.pumping, Arrival::SharedRelease(node)) };
    }

    /// The owner's own release took nothing because the unit replays: the
    /// take of its notice is due at this take point.
    pub(super) fn released_own(&mut self) -> bool {
        std::mem::take(&mut self.ctx.released_own)
    }

    /// Takes from now are at a scheduler pump (point kind 1), or not.
    pub(super) fn set_pumping(&mut self, pumping: bool) {
        self.pumping = pumping;
    }
}

#[cfg(not(feature = "record"))]
impl super::LangUnit {
    #[inline(always)]
    pub(crate) fn reach(&mut self) {}
    #[inline(always)]
    pub(super) fn set_pumping(&mut self, _: bool) {}
    #[inline(always)]
    pub(super) fn released_own(&mut self) -> bool {
        false
    }
    #[inline(always)]
    pub(crate) fn take_due(&self) -> bool {
        false
    }
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
    let grants;
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
            let (flat, content) = (inbox.flat_ptr(t), t.value().cross_parts().1);
            let sockets = t.value().item_sockets();
            // SAFETY: the inbox belongs to this unit, whose thread this is;
            // the item is its newest, still in its connection.
            let files = unsafe { (*h).files };
            let words = unsafe { tape::item_words(id, flat, Some(content)) };
            // As at the send: a unit without host files resolves none.
            let registry = if files.is_null() {
                std::ptr::null_mut()
            } else {
                unsafe { &raw mut (*files).registry }
            };
            grants = if words.is_empty() || registry.is_null() {
                Vec::new()
            } else {
                // SAFETY: this unit's running task's context is the run's.
                unsafe {
                    crate::tape::units::item_grants::resolve(
                        tasks::current_context(),
                        &*registry,
                        &words,
                    )
                }
            };
            Arrival::Item {
                conn: id,
                flat,
                content,
                grants: &grants,
                registry,
                sockets,
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
        // Only a model-ledger unit is sent credits; a language unit's charges
        // return as refunds, releases and survivors, and the format has no
        // credits take.
        Took::Credits => Arrival::Unrecorded("a credits take, which the format has no frame for"),
        Took::SharedRelease => Arrival::SharedRelease(id),
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
    /// `context` is the sending unit's own.
    pub(super) unsafe fn new(
        context: *const crate::Context,
        client: *mut ClientH,
        value: *const u8,
        wait: bool,
        sockets: Option<&crate::net::CrossingSockets>,
    ) -> Self {
        let group = tasks::current_group();
        // SAFETY: by the contract; the sender's handle pins the client, and
        // its context's registry resolves its own capability words.
        let item = unsafe {
            let conn = Arc::as_ptr(&(*client).client.0) as usize;
            let words = tape::item_words(conn, value, None);
            // A context without host files (a model unit) resolves none, and
            // a capability word left unresolved cuts the send.
            let files = (*context).host_files;
            let grants = if words.is_empty() || files.is_null() {
                Vec::new()
            } else {
                crate::tape::units::item_grants::resolve(context, &(*files).registry, &words)
            };
            tape::send_item(group, client as usize, conn, value, &grants, sockets)
        };
        let op = if wait {
            hirsel_tape_format::op::CONN_SEND
        } else {
            hirsel_tape_format::op::CONN_TRY_SEND
        };
        Self { group, op, item }
    }

    /// A replaying sender's send, checked against its stream: the recorded
    /// outcome, or None inside once the replay has ended. None when the
    /// sender does not replay.
    pub(super) fn replayed(&mut self) -> Option<Option<u64>> {
        if !self.group.is_some_and(|group| group.replaying()) {
            return None;
        }
        let item = self.item.take()?;
        Some(crate::tape::units::replay::send(self.group, self.op, item))
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
    pub(super) unsafe fn new(
        _: *const crate::Context,
        _: *mut super::ClientH,
        _: *const u8,
        _: bool,
        _: Option<&crate::net::CrossingSockets>,
    ) -> Self {
        Self
    }
    #[inline(always)]
    pub(super) fn note(&mut self, _: Option<u64>, _: Option<u32>) {}
    #[inline(always)]
    pub(super) fn replayed(&mut self) -> Option<Option<u64>> {
        None
    }
}

/// The owner's helper-end `TAKE`, with the helper's outcome and the
/// connections, `Shared` nodes and `taken` charges it paid for that survive it.
#[cfg(feature = "record")]
pub(super) unsafe fn end_take(
    u: &mut LangUnit,
    key: usize,
    ended: &Ended,
    taken: &[usize],
    files: *mut crate::host_file::Files,
) {
    let Some(outcome) = &ended.tape_end else {
        return;
    };
    let mut charges = Vec::with_capacity(taken.len());
    let mut unnamed = None;
    for &index in taken {
        let handed = &ended.charges[index];
        match handed.name {
            Some((kind, origin, stream, ordinal)) => {
                charges.push((kind, origin, stream, ordinal, handed.bytes()));
            }
            None => unnamed = Some(UNNAMED_CHARGE),
        }
    }
    let registry = if files.is_null() {
        std::ptr::null_mut()
    } else {
        // SAFETY: the owner's host files, live while the owner runs.
        unsafe { &raw mut (*files).registry }
    };
    let arrival = Arrival::HelperEnd {
        key,
        outcome,
        registry,
        survivors: &ended.survivors,
        charges: &charges,
        unnamed,
    };
    // SAFETY: an end take carries no item; its registry is the owner's.
    unsafe { tape::take(u.tape(), u.pumping, arrival) };
}

#[cfg(feature = "record")]
const UNNAMED_CHARGE: &str = "a helper end keeps a charge with no survivor name";
