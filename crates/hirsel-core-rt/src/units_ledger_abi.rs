//! Entry points for the unit ledger and its connections, in the shape of
//! `abi.rs`: C calls on opaque handles, each with its contract. They are not
//! exported (`no_mangle`) until the emitter lowers units onto them; the pool
//! benchmark drives the connection calls.
//!
//! Handles:
//! - a unit: `*mut UnitCtx`, from `hirsel_unit_root`, used only by the pool
//!   unit that runs it;
//! - a value: a caller-owned slot of `HIRSEL_UNIT_VALUE_SIZE` bytes, aligned
//!   to `HIRSEL_UNIT_VALUE_ALIGN`. A call that takes a value reads it out of
//!   the slot (the slot is then empty); a call that gives one writes it in.
//!   Values never live on the heap on their own, so a send moves the value
//!   once, into its arrival;
//! - an inbox or client: `*mut Inbox` or `*mut Client`, boxed;
//! - a shared node: `*mut Shared`, boxed; a board: `*const BoardCore` from
//!   `Arc::into_raw`, one per handle.
//!
//! Refusals write a `Capacity` (bytes asked, bytes left) and admit nothing.
#![allow(dead_code)]

use super::conn::{Client, Inbox, Received, Sent};
use super::{BoardCore, Cap, Shared, Took, UnitCtx, Value};
use crate::Capacity;
use std::{mem::MaybeUninit, ptr, sync::Arc};

pub(crate) const HIRSEL_UNIT_VALUE_SIZE: usize = size_of::<Value>();
pub(crate) const HIRSEL_UNIT_VALUE_ALIGN: usize = align_of::<Value>();

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
    };
    if !ident.is_null() {
        // SAFETY: a non-null ident out-pointer is writable.
        unsafe { ident.write(u.took_ident) };
    }
    code
}

/// The identity a take reports for this inbox's items, close and seal answer.
/// # Safety
/// `inbox` is live.
pub(crate) unsafe extern "C" fn hirsel_unit_inbox_ident(inbox: *const Inbox) -> usize {
    // SAFETY: by the contract.
    Arc::as_ptr(unsafe { &(*inbox).0 }) as usize
}

// -- unit lifetime ------------------------------------------------------------

/// A run root with `limit` bytes, bound to the pool unit running the call.
/// # Safety
/// Called on a running pool unit. The unit returned is used only by that
/// pool unit, and ended with `hirsel_unit_end_root`.
pub(crate) unsafe extern "C" fn hirsel_unit_root(limit: u64) -> *mut UnitCtx {
    let u = Box::new(UnitCtx::root(limit));
    // SAFETY: called by a running pool unit (the contract).
    u.bind(unsafe { (*super::super::cur()).id });
    Box::into_raw(u)
}

/// Bind a unit to the pool unit running the call: a publish to its parked
/// port wakes that pool unit. A helper calls this first, on its own unit.
/// # Safety
/// `u` is live and owned by the calling pool unit.
pub(crate) unsafe extern "C" fn hirsel_unit_bind(u: *mut UnitCtx) {
    // SAFETY: by the contract.
    unsafe { (*u).bind((*super::super::cur()).id) };
}

/// End the run root and free the unit. Later arrivals to it are dropped.
/// # Safety
/// `u` is a run root from `hirsel_unit_root`, live, owned by the caller, and
/// not used again. Every value, inbox and client of the unit is gone.
pub(crate) unsafe extern "C" fn hirsel_unit_end_root(u: *mut UnitCtx) {
    // SAFETY: by the contract.
    let mut u = unsafe { Box::from_raw(u) };
    u.end_root();
}

/// Take the next arrival that has reached the unit and apply it, without
/// waiting. Returns `TOOK_NONE` if nothing has reached it. Writes to `ident`
/// what the take was about: the send id for a settlement; the connection
/// (`hirsel_unit_inbox_ident`) for an item, close, seal answer or last
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
pub(crate) unsafe extern "C" fn hirsel_unit_drop(u: *mut UnitCtx, v: *mut ValueSlot, cause: *mut ValueSlot) -> bool {
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
        cap_out(cap, Cap { req: room, avail: 0 });
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

/// Another client of the same connection, for any unit.
/// # Safety
/// `c` is a live client.
pub(crate) unsafe extern "C" fn hirsel_unit_client_clone(c: *const Client) -> *mut Client {
    // SAFETY: by the contract.
    Box::into_raw(Box::new(unsafe { (*c).clone_client() }))
}

/// Send the value in `v` (`blocking`: wait for room; otherwise a try-send).
/// - `SENT_ACCEPTED`: the item is on its way; the slot is empty.
/// - `SENT_WAITING`: the send waits; its id is written to `id`, the slot is
///   empty, and the settlement arrives at the unit (`TOOK_ACCEPTED`,
///   `TOOK_CLOSED` with the value kept for `hirsel_unit_closed_value`, or
///   `TOOK_WITHDRAWN`).
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

/// Withdraw this unit's waiting send `id` on cancellation. True if it was
/// still waiting: its `TOOK_WITHDRAWN` settlement then arrives at the unit,
/// and dropping the value there releases the sender's reservation.
/// # Safety
/// As `hirsel_unit_send`.
pub(crate) unsafe extern "C" fn hirsel_unit_withdraw(u: *mut UnitCtx, c: *const Client, id: u32) -> bool {
    // SAFETY: by the contract.
    unsafe { (*u).withdraw(&*c, id) }
}

/// The value of a send settled `TOOK_CLOSED`, back to its sender.
/// # Safety
/// `u` is live and owned by the caller; `out` is a writable, empty slot.
pub(crate) unsafe extern "C" fn hirsel_unit_closed_value(u: *mut UnitCtx, id: u32, out: *mut ValueSlot) -> bool {
    // SAFETY: by the contract.
    match unsafe { (*u).closed_value(id) } {
        Some(v) => {
            put_value(out, v);
            true
        }
        None => false,
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

/// Seal the inbox: every waiting send settles closed, and `TOOK_SEAL_ANSWER`
/// arrives after every item accepted before the seal.
/// # Safety
/// `u` owns the inbox, which is not sealed yet.
pub(crate) unsafe extern "C" fn hirsel_unit_seal(u: *mut UnitCtx, inbox: *const Inbox) {
    // SAFETY: by the contract.
    unsafe { (*u).seal(&*inbox) };
}

/// Clean the inbox after its seal answer was taken: drop the items not
/// received, last accepted first, and split the room. Failing finalizers'
/// causes are joined into one value in `cause` (true returned). The
/// connection's header and unused room come back with its last release; with
/// no client left that is this call, which takes in port order through the
/// notice (the arrivals before it are applied as by `hirsel_unit_take`).
/// # Safety
/// `u` owns the inbox and took its seal answer; it was not cleaned before;
/// `cause` is a writable, empty slot.
pub(crate) unsafe extern "C" fn hirsel_unit_clean(u: *mut UnitCtx, inbox: *const Inbox, cause: *mut ValueSlot) -> bool {
    // SAFETY: by the contract.
    let u = unsafe { &mut *u };
    // SAFETY: by the contract.
    let (causes, _) = u.clean(unsafe { &*inbox });
    if causes.is_empty() {
        return false;
    }
    put_value(cause, u.combine(causes));
    true
}

/// Free an inbox handle after the cleaning.
/// # Safety
/// `inbox` came from `hirsel_unit_conn_new`, was cleaned, and is not used again.
pub(crate) unsafe extern "C" fn hirsel_unit_inbox_free(inbox: *mut Inbox) {
    // SAFETY: by the contract.
    drop(unsafe { Box::from_raw(inbox) });
}

/// Drop a client. The last one closes an unsealed inbox (`TOOK_CLOSE` at its
/// owner) or, after the cleaning, retires the connection; the owner's own
/// retirement takes through its notice, as `hirsel_unit_clean` does.
/// # Safety
/// `u` is the calling pool unit's; `c` is a live client not used again.
pub(crate) unsafe extern "C" fn hirsel_unit_client_drop(u: *mut UnitCtx, c: *mut Client) {
    // SAFETY: by the contract.
    unsafe { (*u).drop_client(*Box::from_raw(c)) };
}

// -- shared nodes and boards ----------------------------------------------------

/// A shared node of `size` bytes holding the value in `payload` (if
/// `has_payload`). Null on refusal, with the payload back in its slot.
/// # Safety
/// `u` is live and owned by the caller; `payload` holds a value if
/// `has_payload`; `cap` is null or writable.
pub(crate) unsafe extern "C" fn hirsel_unit_shared_make(
    u: *mut UnitCtx,
    size: u64,
    payload: *mut ValueSlot,
    has_payload: bool,
    cap: *mut Capacity,
) -> *mut Shared {
    // SAFETY: by the contract.
    let p = has_payload.then(|| unsafe { take_value(payload) });
    // SAFETY: by the contract.
    match unsafe { (*u).make_shared(size, p) } {
        Ok(s) => Box::into_raw(Box::new(s)),
        Err((c, p)) => {
            if let Some(p) = p {
                put_value(payload, p);
            }
            cap_out(cap, c);
            ptr::null_mut()
        }
    }
}

/// Another share of the node, for any unit.
/// # Safety
/// `s` is a live share.
pub(crate) unsafe extern "C" fn hirsel_unit_shared_share(s: *const Shared) -> *mut Shared {
    // SAFETY: by the contract.
    Box::into_raw(Box::new(unsafe { (*s).share() }))
}

/// Release a share. The last release credits the node's payers by an arrival
/// at its home unit, even when that is this unit.
/// # Safety
/// `u` is the calling pool unit's; `s` is a live share not used again.
pub(crate) unsafe extern "C" fn hirsel_unit_shared_drop(u: *mut UnitCtx, s: *mut Shared) {
    // SAFETY: by the contract.
    unsafe { (*u).drop_shared(*Box::from_raw(s)) };
}

/// A board holding `init` as its first post (the share moves in). The
/// pointer returned is both the poster and the first reading handle; each
/// holds one count of the board, given up by its own drop. Null on refusal,
/// with the share back in `*init`.
/// # Safety
/// `u` is live and owned by the caller; `init` points to a live share
/// handle, which the call consumes on success; `cap` is null or writable.
pub(crate) unsafe extern "C" fn hirsel_unit_board_make(u: *mut UnitCtx, init: *mut *mut Shared, cap: *mut Capacity) -> *const BoardCore {
    // SAFETY: by the contract.
    let s = unsafe { *Box::from_raw(*init) };
    // SAFETY: by the contract.
    match unsafe { (*u).make_board(s) } {
        Ok(b) => {
            let p = Arc::into_raw(b);
            // SAFETY: `p` is live; the second count is the poster's.
            unsafe { Arc::increment_strong_count(p) };
            p
        }
        Err((c, s)) => {
            // SAFETY: by the contract.
            unsafe { init.write(Box::into_raw(Box::new(s))) };
            cap_out(cap, c);
            ptr::null()
        }
    }
}

/// Another reading handle on the board.
/// # Safety
/// `b` is a live board handle.
pub(crate) unsafe extern "C" fn hirsel_unit_board_add_handle(b: *const BoardCore) -> *const BoardCore {
    // SAFETY: by the contract; each handle owns one count of the Arc.
    unsafe {
        UnitCtx::board_add_handle(&*b);
        Arc::increment_strong_count(b);
    }
    b
}

/// A share of the board's latest post.
/// # Safety
/// `u` is the calling pool unit's; `b` is a live board handle.
pub(crate) unsafe extern "C" fn hirsel_unit_board_read(u: *mut UnitCtx, b: *const BoardCore) -> *mut Shared {
    // SAFETY: by the contract.
    Box::into_raw(Box::new(unsafe { (*u).board_read(&*b) }))
}

/// Post a share to the board; the board releases the post it replaces.
/// # Safety
/// `u` is the calling pool unit's and holds the poster; `b` is a live board
/// handle; `s` is a live share consumed by the call.
pub(crate) unsafe extern "C" fn hirsel_unit_board_post(u: *mut UnitCtx, b: *const BoardCore, s: *mut Shared) {
    // SAFETY: by the contract.
    unsafe { (*u).board_post(&*b, *Box::from_raw(s)) };
}

/// Drop the board's poster.
/// # Safety
/// `u` holds the board's poster, not used again; `b` is the poster's pointer.
pub(crate) unsafe extern "C" fn hirsel_unit_board_drop_poster(u: *mut UnitCtx, b: *const BoardCore) {
    // SAFETY: by the contract; the poster's count of the Arc is given up.
    unsafe {
        (*u).board_drop_poster(&*b);
        drop(Arc::from_raw(b));
    }
}

/// Drop a board handle. The last one releases the board's post and root.
/// # Safety
/// `u` is the calling pool unit's; `b` is a live board handle not used again.
pub(crate) unsafe extern "C" fn hirsel_unit_board_drop_handle(u: *mut UnitCtx, b: *const BoardCore) {
    // SAFETY: by the contract; this handle's count of the Arc is given up.
    unsafe {
        (*u).board_drop_handle(&*b);
        drop(Arc::from_raw(b));
    }
}
