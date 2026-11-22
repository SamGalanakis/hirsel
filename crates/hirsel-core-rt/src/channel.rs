//! Bounded FIFO ownership transfer. A queue admits its whole logical storage
//! before publication and never allocates at send, receive, close or cleanup.
use crate::{
    Context,
    block::Block,
    causes::{self, Cause},
    counted::{self, Holder, Worklist},
    tasks::{self, Storage, ValueLayout},
};
use hirsel_core_layout::charge;
use std::ptr;
#[derive(PartialEq, Eq)]
enum Receiver {
    Open,
    Cleaning,
    Gone,
}
pub struct Channel {
    /// The receiver's cleanup frame once it is Cleaning.
    holder: std::mem::MaybeUninit<Holder>,
    /// How far the newest queued item's cleanup has got.
    cursor: u64,
    id: u64,
    senders: u64,
    closing_sender: bool,
    receiver: Receiver,
    capacity: usize,
    send_waiters: tasks::Waiters,
    receive_waiters: tasks::Waiters,
    head: usize,
    len: usize,
    item: ValueLayout,
    storage: Storage,
    _charge: Block,
}
#[repr(C)]
pub struct New {
    pub sender: *mut Channel,
    pub receiver: *mut Channel,
    pub available: u64,
    pub status: u8,
}
#[repr(C)]
pub struct Transfer {
    pub transferred: bool,
    pub status: u8,
}
/// # Safety
/// Context is exclusively owned. Item layout/drop are compiler-produced for T.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_channel_new(
    context: *mut Context,
    capacity: u64,
    item: ValueLayout,
    site: u64,
) -> New {
    // SAFETY: creation owns context; no custody changes until admission succeeds.
    unsafe {
        if capacity == 0 {
            causes::record(&mut *context, Cause::new(causes::BOUNDS, site));
            return New {
                sender: ptr::null_mut(),
                receiver: ptr::null_mut(),
                available: 0,
                status: 1,
            };
        }
        let charge = match Block::new(&*context, charge::channel(capacity, item.logical), 1, 8) {
            Ok(block) => block,
            Err(error) => {
                return New {
                    sender: ptr::null_mut(),
                    receiver: ptr::null_mut(),
                    available: error.available,
                    status: 0,
                };
            }
        };
        let count =
            usize::try_from(capacity).unwrap_or_else(|_| crate::process::stop(b"HostMemory"));
        let bytes = count
            .checked_mul(item.bytes)
            .unwrap_or_else(|| crate::process::stop(b"HostMemory"));
        let _commit = tasks::before_commit();
        let id = tasks::identity();
        let channel = Box::into_raw(Box::new(Channel {
            holder: std::mem::MaybeUninit::uninit(),
            cursor: 0,
            id,
            senders: 1,
            closing_sender: false,
            receiver: Receiver::Open,
            capacity: count,
            send_waiters: tasks::Waiters::default(),
            receive_waiters: tasks::Waiters::default(),
            head: 0,
            len: 0,
            item,
            storage: Storage::new(bytes, item.align),
            _charge: charge,
        }));
        crate::backing_observation::observe!(
            allocate,
            channel.cast(),
            size_of::<Channel>(),
            Channel,
        );
        tasks::event(tasks::SENDERS, id, 1);
        New {
            sender: channel,
            receiver: channel,
            available: 0,
            status: 0,
        }
    }
}
/// # Safety
/// A live sender borrow pins this channel. Duplication admits nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_channel_sender(channel: *mut Channel) -> *mut Channel {
    // SAFETY: the live sender pins the exact counter through its event.
    unsafe {
        let _commit = tasks::before_commit();
        (*channel).senders = (*channel)
            .senders
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"ChannelSenders"));
        tasks::event(tasks::SENDERS, (*channel).id, (*channel).senders);
        channel
    }
}
/// Move one item. Word-sized items, the common case, skip the libc call.
/// # Safety
/// Both ranges hold `bytes` bytes and do not overlap.
#[inline(always)]
unsafe fn copy_item(from: *const u8, to: *mut u8, bytes: usize) {
    // SAFETY: caller contract; the 8-byte arm copies exactly `bytes`.
    unsafe {
        if bytes == 8 {
            ptr::copy_nonoverlapping(from, to, 8);
        } else {
            ptr::copy_nonoverlapping(from, to, bytes);
        }
    }
}
unsafe fn publish(channel: *mut Channel, value: *mut u8) -> u8 {
    // SAFETY: current task pins a sender and transfers value only after room exists.
    unsafe {
        if (*channel).receiver != Receiver::Open {
            return 2;
        }
        if (*channel).len == (*channel).capacity {
            return 1;
        }
        store(channel, value);
        tasks::event_wake_waiters(
            tasks::PUBLISH,
            (*channel).id,
            0,
            &raw mut (*channel).receive_waiters,
        );
        0
    }
}
/// Copy an item into the room the caller has checked.
/// # Safety
/// The queue is open, not full, and the caller holds the token.
unsafe fn store(channel: *mut Channel, value: *mut u8) {
    // SAFETY: as stated; value is an initialized T.
    unsafe {
        let rest = (*channel).capacity - (*channel).head;
        let at = if (*channel).len >= rest {
            (*channel).len - rest
        } else {
            (*channel).head + (*channel).len
        };
        copy_item(
            value,
            (*channel).storage.as_ptr().add(at * (*channel).item.bytes),
            (*channel).item.bytes,
        );
        (*channel).len += 1;
    }
}
/// A holder completes a waiting sender's publish when it has room and needs
/// no cancellation, fold or poll turn; otherwise the sender takes the token.
/// # Safety
/// Called by the sequencer under the token for a blocked requester.
unsafe fn serve_send(request: *mut tasks::Delegated) -> bool {
    // SAFETY: the requester pins its sender, value and task until signalled.
    unsafe {
        let request = &*request;
        let channel = request.channel.cast::<Channel>();
        if (*channel).receiver != Receiver::Open
            || (*channel).len == (*channel).capacity
            || !tasks::delegable(request)
        {
            return false;
        }
        store(channel, request.value);
        tasks::event_wake_waiters_for(
            request.task,
            tasks::PUBLISH,
            (*channel).id,
            0,
            &raw mut (*channel).receive_waiters,
            false,
        );
        true
    }
}
/// The receiving counterpart of `serve_send`.
/// # Safety
/// As for `serve_send`.
unsafe fn serve_receive(request: *mut tasks::Delegated) -> bool {
    // SAFETY: the requester pins its receiver, destination and task until signalled.
    unsafe {
        let request = &*request;
        let channel = request.channel.cast::<Channel>();
        if (*channel).len == 0 || !tasks::delegable(request) {
            return false;
        }
        load(channel, request.value);
        tasks::event_wake_waiters_for(
            request.task,
            tasks::RECEIVE,
            (*channel).id,
            0,
            &raw mut (*channel).send_waiters,
            false,
        );
        true
    }
}
/// # Safety
/// Borrowed live sender and initialized T. Result 0 transfers it, 1 Full and 2
/// Closed preserve it. No cancellation point, including when the queue is full.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_try_send(channel: *mut Channel, value: *mut u8) -> u8 {
    // SAFETY: no suspension or outside access between deciding and committing.
    unsafe {
        let _commit = tasks::before_commit();
        let result = publish(channel, value);
        let _commit = tasks::before_commit();
        tasks::event(tasks::TRY_SEND, (*channel).id, result as u64);
        result
    }
}
/// # Safety
/// Live sender, initialized T, exclusively borrowed task context. A failure keeps
/// T in the caller's cleanup; success transfers only when transferred is true.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_send(
    context: *mut Context,
    channel: *mut Channel,
    value: *mut u8,
    site: u64,
) -> Transfer {
    // SAFETY: sender and pending input are pinned by the suspended caller.
    unsafe {
        let Some(_held) =
            tasks::delegate(context, tasks::PUBLISH, channel.cast(), value, serve_send)
        else {
            return Transfer {
                transferred: true,
                status: 0,
            };
        };
        let mut _resumed = None;
        loop {
            let _commit = tasks::before_commit();
            if tasks::check_cancel(context, site) != 0 {
                return Transfer {
                    transferred: false,
                    status: 1,
                };
            }
            match publish(channel, value) {
                0 => {
                    return Transfer {
                        transferred: true,
                        status: 0,
                    };
                }
                2 => {
                    tasks::event(
                        tasks::PUBLISH,
                        (*channel).id,
                        hirsel_core_layout::event::CHANNEL_CLOSED,
                    );
                    return Transfer {
                        transferred: false,
                        status: 0,
                    };
                }
                _ => {
                    _resumed = Some(tasks::park(
                        &raw mut (*channel).send_waiters,
                        #[cfg(feature = "prof")]
                        (crate::prof::Reason::ChannelSend, (*channel).id),
                    ))
                }
            }
        }
    }
}
/// Take the front item into `destination`, or return false when the queue is empty.
/// A free slot can let any sender on this channel publish. Wake them in park
/// order so progress also survives the first sender observing cancellation.
///
/// # Safety
/// Sole receiver borrow and writable T destination.
unsafe fn take(channel: *mut Channel, destination: *mut u8) -> bool {
    // SAFETY: the caller's receiver pins the queue and destination for this call.
    unsafe {
        if (*channel).len == 0 {
            return false;
        }
        load(channel, destination);
        tasks::event_wake_waiters(
            tasks::RECEIVE,
            (*channel).id,
            0,
            &raw mut (*channel).send_waiters,
        );
        true
    }
}
/// Move the front item out of a nonempty queue.
/// # Safety
/// The queue is nonempty and the caller holds the token.
unsafe fn load(channel: *mut Channel, destination: *mut u8) {
    // SAFETY: as stated; destination is writable T.
    unsafe {
        copy_item(
            (*channel)
                .storage
                .as_ptr()
                .add((*channel).head * (*channel).item.bytes),
            destination,
            (*channel).item.bytes,
        );
        (*channel).head += 1;
        if (*channel).head == (*channel).capacity {
            (*channel).head = 0;
        }
        (*channel).len -= 1;
    }
}
/// # Safety
/// Sole receiver borrow and writable T destination. Cancellation and closure
/// initialize nothing; only transferred true moves queue custody into destination.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_receive(
    context: *mut Context,
    channel: *mut Channel,
    destination: *mut u8,
    site: u64,
) -> Transfer {
    // SAFETY: receiver keeps the queue alive across each suspension.
    unsafe {
        let Some(_held) = tasks::delegate(
            context,
            tasks::RECEIVE,
            channel.cast(),
            destination,
            serve_receive,
        ) else {
            return Transfer {
                transferred: true,
                status: 0,
            };
        };
        let mut _resumed = None;
        loop {
            let _commit = tasks::before_commit();
            if tasks::check_cancel(context, site) != 0 {
                return Transfer {
                    transferred: false,
                    status: 1,
                };
            }
            if take(channel, destination) {
                return Transfer {
                    transferred: true,
                    status: 0,
                };
            }
            if (*channel).senders == 0 {
                tasks::event(
                    tasks::RECEIVE,
                    (*channel).id,
                    hirsel_core_layout::event::CHANNEL_CLOSED,
                );
                return Transfer {
                    transferred: false,
                    status: 0,
                };
            }
            _resumed = Some(tasks::park(
                &raw mut (*channel).receive_waiters,
                #[cfg(feature = "prof")]
                (crate::prof::Reason::ChannelRecv, (*channel).id),
            ));
        }
    }
}

/// Receive at a source call in a bounded loop of private work. An empty or
/// cancelled call releases the acquisition before the ordinary scalar recheck.
///
/// # Safety
/// As for hirsel_core_receive. The caller ends the receive run before any
/// shared operation (except this receive on the same channel), cleanup,
/// callback, suspension or exit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_receive_in_loop(
    context: *mut Context,
    channel: *mut Channel,
    destination: *mut u8,
    site: u64,
) -> Transfer {
    // SAFETY: the caller pins the channel and its one result place. There
    // is no copy, credit fold or event for any later source call.
    unsafe {
        if (*channel).capacity > 1 && tasks::can_fuse_receives() {
            let commit = crate::sequencer::enter();
            if !tasks::cancel_pending(context) && take(channel, destination) {
                commit.receive_succeeded((*channel).len != 0);
                return Transfer {
                    transferred: true,
                    status: 0,
                };
            }
        }
        hirsel_core_receive(context, channel, destination, site)
    }
}

/// End the active fiber's bounded receive run at a lowering boundary.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_receive_loop_end() {
    crate::sequencer::end_receive_run();
}

/// # Safety
/// Sole receiver borrow and writable T destination. Empty and Closed return
/// immediately; Empty is the deterministic scheduler poll point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_try_receive(
    channel: *mut Channel,
    destination: *mut u8,
) -> u8 {
    // SAFETY: receiver pins the channel and destination for this call.
    unsafe {
        let _commit = tasks::before_commit();
        let status = if take(channel, destination) {
            0
        } else if (*channel).senders == 0 {
            2
        } else {
            1
        };
        let _commit = tasks::before_commit();
        tasks::event(tasks::TRY_RECEIVE, (*channel).id, status as u64);
        status
    }
}
/// # Safety
/// Transfers one live sender. No allocation and no finalizer failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_sender_drop(channel: *mut Channel) {
    // SAFETY: exact live sender count, with receiver retaining storage during cleanup.
    unsafe {
        let _commit = tasks::before_commit();
        (*channel).senders -= 1;
        let last = (*channel).senders == 0;
        (*channel).closing_sender = last;
        tasks::event(tasks::SENDERS, (*channel).id, (*channel).senders);
        if last {
            let _commit = tasks::before_commit();
            tasks::event_wake_waiters(
                tasks::CLOSE,
                (*channel).id,
                0,
                &raw mut (*channel).receive_waiters,
            );
            (*channel).closing_sender = false;
            if (*channel).receiver == Receiver::Gone {
                drop(Box::from_raw(channel));
            }
        }
    }
}
/// Close the receiver and begin cleaning what is queued. From here only this
/// cleanup touches the queue: a sender sees the receiver closed.
unsafe fn receiver_begin(context: *mut Context, channel: *mut Channel) -> *mut Holder {
    // SAFETY: the caller transfers the sole receiver.
    unsafe {
        (*context).cancel_shield += 1;
        let _commit = tasks::before_commit();
        (*channel).receiver = Receiver::Cleaning;
        tasks::event_wake_waiters(
            tasks::CLOSE,
            (*channel).id,
            1,
            &raw mut (*channel).send_waiters,
        );
        (*channel).cursor = 0;
        (*channel).holder.as_mut_ptr()
    }
}
/// Clean the queued items, last published first, each to the end before the
/// next (CONC-9); a failing item never stops the rest (FAIL-10). Then the
/// receiver is gone, and the last end of the channel frees it.
unsafe fn receiver_step(context: *mut Context, holder: *mut Holder, list: *mut Worklist) -> bool {
    // SAFETY: the frame is the Cleaning receiver's; storage outlives the items.
    unsafe {
        let channel = holder
            .cast::<u8>()
            .sub(std::mem::offset_of!(Channel, holder))
            .cast::<Channel>();
        while (*channel).len != 0 {
            let newest = (*channel).len - 1;
            let rest = (*channel).capacity - (*channel).head;
            let at = if newest >= rest {
                newest - rest
            } else {
                (*channel).head + newest
            };
            let item = (*channel).storage.as_ptr().add(at * (*channel).item.bytes);
            if let Some(clean) = (*channel).item.clean
                && !clean(context, item, &mut (*channel).cursor, list)
            {
                return false;
            }
            (*channel).cursor = 0;
            (*channel).len = newest;
        }
        let _commit = crate::sequencer::enter();
        (*channel).receiver = Receiver::Gone;
        if (*channel).senders == 0 && !(*channel).closing_sender {
            drop(Box::from_raw(channel));
        }
        (*context).cancel_shield -= 1;
        true
    }
}
/// # Safety
/// Transfers the sole receiver. Each queued item is cleaned in reverse publication
/// order, under cancellation shielding. Its own prepaid resource cells add causes;
/// a failing item never prevents the remaining queued items from being cleaned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_receiver_drop(
    context: *mut Context,
    channel: *mut Channel,
) -> u8 {
    // SAFETY: as receiver_begin; the drain runs the cleanup to its end.
    unsafe {
        let holder = receiver_begin(context, channel);
        u8::from(counted::drain_holder(context, holder, receiver_step, 0))
    }
}
/// Release the sole receiver inside a running drain: begin its cleanup and
/// hand it over at `point`. Always stages, so always returns true.
/// # Safety
/// As hirsel_core_receiver_drop; `list` is the running drain's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_receiver_release(
    context: *mut Context,
    channel: *mut Channel,
    list: *mut Worklist,
    point: u64,
) -> bool {
    // SAFETY: as hirsel_core_receiver_drop.
    unsafe {
        let holder = receiver_begin(context, channel);
        (*list).stage_holder(holder, receiver_step, point);
        true
    }
}

#[cfg(test)]
#[path = "channel_rule_stop_tests.rs"]
mod rule_stop_tests;

#[cfg(any(test, feature = "charge-observation", feature = "prof"))]
impl Drop for Channel {
    fn drop(&mut self) {
        crate::backing_observation::observe!(free, (self as *const Self).cast(), size_of::<Self>());
    }
}
