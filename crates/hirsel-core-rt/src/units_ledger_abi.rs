//! Entry points for the unit ledger and its connections, in the shape of
//! `abi.rs`: C calls on opaque handles, each with its contract. The pool
//! benchmark drives the connection calls.
//!
//! Handles:
//! - a unit: `*mut UnitCtx`, from `hirsel_unit_root`, used only by the pool
//!   unit that runs it;
//! - a value: a caller-owned slot large enough for a `Value` and aligned for
//!   it. A call that takes a value reads it out of the slot (the slot is
//!   then empty); a call that gives one writes it in.
//!   Values never live on the heap on their own, so a send moves the value
//!   once, into its arrival;
//! - an inbox or client: `*mut Inbox` or `*mut Client`, boxed.
//!
//! Refusals write a `Capacity` (bytes asked, bytes left) and admit nothing.

use super::conn::{Client, Inbox, Received, Sent};
use super::{Cap, Took, UnitCtx, Value};
use crate::Capacity;
use std::mem::MaybeUninit;

pub(crate) type ValueSlot = MaybeUninit<Value>;

/// What a take did (`hirsel_unit_take`, `hirsel_unit_wait`).
pub(crate) const TOOK_NONE: u32 = 0;
pub(crate) const TOOK_CREDITS: u32 = 1;
pub(crate) const TOOK_LAST_RELEASE: u32 = 2;
pub(crate) const TOOK_HELPER_END: u32 = 3;
pub(crate) const TOOK_ACCEPTED: u32 = 4;
pub(crate) const TOOK_CLOSED: u32 = 5;
pub(crate) const TOOK_WITHDRAWN: u32 = 6;
pub(crate) const TOOK_ITEM: u32 = 7;
pub(crate) const TOOK_CLOSE: u32 = 8;
pub(crate) const TOOK_SEAL_ANSWER: u32 = 9;
pub(crate) const TOOK_CANCEL: u32 = 10;
pub(crate) const TOOK_SHARED_RELEASE: u32 = 11;

/// Send outcomes.
pub(crate) const SENT_ACCEPTED: u32 = 0;
pub(crate) const SENT_WAITING: u32 = 1;
pub(crate) const SENT_FULL: u32 = 2;
pub(crate) const SENT_CLOSED: u32 = 3;
pub(crate) const SENT_BOUNDS: u32 = 4;

/// Receive outcomes.
pub(crate) const RECV_OK: u32 = 0;
pub(crate) const RECV_EMPTY: u32 = 1;
pub(crate) const RECV_CLOSED: u32 = 2;
pub(crate) const RECV_CAPACITY: u32 = 3;

fn cap_out(out: *mut Capacity, c: Cap) {
    if !out.is_null() {
        // SAFETY: a non-null capacity out-pointer is writable (every contract).
        unsafe {
            out.write(Capacity {
                requested: c.req,
                available: c.avail,
            })
        };
    }
}

/// # Safety
/// `slot` holds a value.
unsafe fn take_value(slot: *mut ValueSlot) -> Value {
    // SAFETY: by the contract; the slot is empty afterwards.
    unsafe { (*slot).assume_init_read() }
}

fn put_value(slot: *mut ValueSlot, v: Value) {
    // SAFETY: every caller's contract makes the out slot writable and empty.
    unsafe { (*slot).write(v) };
}

fn took_code(u: &UnitCtx, t: &Took, ident: *mut usize) -> u32 {
    let code = match *t {
        Took::Credits => TOOK_CREDITS,
        Took::LastRelease => TOOK_LAST_RELEASE,
        Took::HelperEnd => TOOK_HELPER_END,
        Took::Accepted(_) => TOOK_ACCEPTED,
        Took::Closed(_) => TOOK_CLOSED,
        Took::Withdrawn(_) => TOOK_WITHDRAWN,
        Took::Item => TOOK_ITEM,
        Took::Close => TOOK_CLOSE,
        Took::SealAnswer => TOOK_SEAL_ANSWER,
        Took::Cancel => TOOK_CANCEL,
        Took::SharedRelease => TOOK_SHARED_RELEASE,
    };
    if !ident.is_null() {
        // SAFETY: a non-null ident out-pointer is writable.
        unsafe { ident.write(u.took_ident) };
    }
    code
}

// -- unit lifetime ------------------------------------------------------------

/// A run root with `limit` bytes, bound to the pool unit running the call.
/// # Safety
/// Called on a running pool unit. The unit returned is used only by that
/// pool unit.
pub(crate) unsafe extern "C" fn hirsel_unit_root(limit: u64) -> *mut UnitCtx {
    let u = Box::new(UnitCtx::root(limit));
    // SAFETY: called by a running pool unit (the contract).
    u.bind(unsafe { (*super::super::cur()).id });
    Box::into_raw(u)
}

/// Take the next arrival that has reached the unit and apply it, without
/// waiting. Returns `TOOK_NONE` if nothing has reached it. Writes to `ident`
/// what the take was about: the send id for a settlement; the connection
/// identity for an item, close, seal answer or last
/// release; the helper index for a helper end; 0 for credits, which need no
/// action by the unit.
/// # Safety
/// `u` is live and owned by the calling pool unit; `ident` is null or writable.
pub(crate) unsafe extern "C" fn hirsel_unit_take(u: *mut UnitCtx, ident: *mut usize) -> u32 {
    // SAFETY: by the contract.
    let u = unsafe { &mut *u };
    match u.take() {
        Some((t, _arrival)) => took_code(u, &t, ident),
        None => TOOK_NONE,
    }
}

/// As `hirsel_unit_take`, parking the pool unit until something is published.
/// # Safety
/// As `hirsel_unit_take`; `u` is bound to the calling pool unit.
pub(crate) unsafe extern "C" fn hirsel_unit_wait(u: *mut UnitCtx, ident: *mut usize) -> u32 {
    // SAFETY: by the contract.
    let u = unsafe { &mut *u };
    let (t, _arrival) = u.wait();
    took_code(u, &t, ident)
}

// -- values ---------------------------------------------------------------------

/// Allocate a block of `size` bytes in the record that admits now.
/// # Safety
/// `u` is live and owned by the caller; `out` is a writable, empty value slot;
/// `cap` is null or writable. False on refusal, with `out` left empty.
pub(crate) unsafe extern "C" fn hirsel_unit_alloc(
    u: *mut UnitCtx,
    size: u64,
    out: *mut ValueSlot,
    cap: *mut Capacity,
) -> bool {
    // SAFETY: by the contract.
    match unsafe { (*u).alloc_block(size) } {
        Ok(v) => {
            put_value(out, v);
            true
        }
        Err(c) => {
            cap_out(cap, c);
            false
        }
    }
}

/// Drop the value in `v`. A failing finalizer's cause chain is written to
/// `cause` (true returned); otherwise `cause` stays empty.
/// # Safety
/// `u` is live and owned by the caller; `v` holds a value of this unit (the
/// slot is empty afterwards); `cause` is a writable, empty slot.
pub(crate) unsafe extern "C" fn hirsel_unit_drop(
    u: *mut UnitCtx,
    v: *mut ValueSlot,
    cause: *mut ValueSlot,
) -> bool {
    // SAFETY: by the contract.
    match unsafe { (*u).drop_value(take_value(v)) } {
        Some(c) => {
            put_value(cause, c);
            true
        }
        None => false,
    }
}

// -- connections ----------------------------------------------------------------

/// A connection with `slots` slots of `elem` header bytes each and a room of
/// `room` bytes, all admitted in the current record. The calling unit owns the
/// inbox. False on refusal or on a size the connection word cannot hold
/// (`slots` 0 or over 2^22 - 1, `room` 2^40 or more, reported as Capacity).
/// # Safety
/// `u` is live and owned by the caller; `inbox` and `client` are writable;
/// `cap` is null or writable.
pub(crate) unsafe extern "C" fn hirsel_unit_conn_new(
    u: *mut UnitCtx,
    slots: u64,
    elem: u64,
    room: u64,
    inbox: *mut *mut Inbox,
    client: *mut *mut Client,
    cap: *mut Capacity,
) -> bool {
    if slots == 0 || slots > super::conn::MAX_CAP || room > super::conn::MAX_ROOM {
        cap_out(
            cap,
            Cap {
                req: room,
                avail: 0,
            },
        );
        return false;
    }
    // SAFETY: by the contract.
    match unsafe { (*u).conn_new(slots, elem, room) } {
        Ok((i, c)) => {
            // SAFETY: by the contract.
            unsafe {
                inbox.write(Box::into_raw(Box::new(i)));
                client.write(Box::into_raw(Box::new(c)));
            }
            true
        }
        Err(c) => {
            cap_out(cap, c);
            false
        }
    }
}

/// Send the value in `v` (`blocking`: wait for room; otherwise a try-send).
/// - `SENT_ACCEPTED`: the item is on its way; the slot is empty.
/// - `SENT_WAITING`: the send waits; its id is written to `id`, the slot is
///   empty, and the settlement arrives at the unit (`TOOK_ACCEPTED`,
///   `TOOK_CLOSED` or `TOOK_WITHDRAWN`).
/// - `SENT_FULL`, `SENT_CLOSED`: refused; the value is back in the slot.
/// - `SENT_BOUNDS`: larger than the whole room; the value was dropped.
///
/// # Safety
/// `u` is live and owned by the calling pool unit; `c` is a live client
/// used by this unit only for the call; `v` holds a value of this unit; `id`
/// is null or writable.
pub(crate) unsafe extern "C" fn hirsel_unit_send(
    u: *mut UnitCtx,
    c: *const Client,
    v: *mut ValueSlot,
    blocking: bool,
    id: *mut u32,
) -> u32 {
    // SAFETY: by the contract.
    let sent = unsafe { (*u).send(&*c, take_value(v), blocking) };
    match sent {
        Sent::Accepted => SENT_ACCEPTED,
        Sent::Waiting(n) => {
            if !id.is_null() {
                // SAFETY: by the contract.
                unsafe { id.write(n) };
            }
            SENT_WAITING
        }
        Sent::Full(back) => {
            put_value(v, back);
            SENT_FULL
        }
        Sent::Closed(back) => {
            put_value(v, back);
            SENT_CLOSED
        }
        Sent::Bounds => SENT_BOUNDS,
    }
}

/// Receive the oldest item taken on the inbox into the record that admits now.
/// `RECV_OK` writes it to `out`; `RECV_EMPTY` means take or wait first;
/// `RECV_CLOSED` means every client is gone and nothing is left;
/// `RECV_CAPACITY` writes the refusal and leaves the item in place.
/// # Safety
/// `u` owns the inbox and is the calling pool unit's; `out` is a writable,
/// empty slot; `cap` is null or writable.
pub(crate) unsafe extern "C" fn hirsel_unit_receive(
    u: *mut UnitCtx,
    inbox: *const Inbox,
    out: *mut ValueSlot,
    cap: *mut Capacity,
) -> u32 {
    // SAFETY: by the contract.
    match unsafe { (*u).receive(&*inbox) } {
        Received::Ok(v) => {
            put_value(out, v);
            RECV_OK
        }
        Received::Empty => RECV_EMPTY,
        Received::Closed => RECV_CLOSED,
        Received::Capacity(c) => {
            cap_out(cap, c);
            RECV_CAPACITY
        }
    }
}
