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
    _item_pin: Option<crate::code::ExecutionPin>,
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
        let count = usize::try_from(capacity).unwrap_or_else(|_| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostMemory"))
        });
        let bytes = count.checked_mul(item.bytes).unwrap_or_else(|| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostMemory"))
        });
        tasks::before_commit();
        let id = tasks::identity();
        let channel = crate::task_heap::custody::RawStorage::into_guest(Box::new(Channel {
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
            _item_pin: crate::code::ExecutionPin::recipe(&*context, item.clean),
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
        tasks::before_commit();
        (*channel).senders = (*channel).senders.checked_add(1).unwrap_or_else(|| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"ChannelSenders"))
        });
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
/// # Safety
/// Borrowed live sender and initialized T. Result 0 transfers it, 1 Full and 2
/// Closed preserve it. No cancellation point, including when the queue is full.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_try_send(channel: *mut Channel, value: *mut u8) -> u8 {
    // SAFETY: no suspension or outside access between deciding and committing.
    unsafe {
        tasks::before_commit();
        let result = publish(channel, value);
        tasks::before_commit();
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
        loop {
            tasks::before_commit();
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
                _ => tasks::park(
                    &raw mut (*channel).send_waiters,
                    #[cfg(feature = "prof")]
                    (crate::prof::Reason::ChannelSend, (*channel).id),
                ),
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
        (*channel).len -= 1;
        let next = (*channel).head + 1;
        (*channel).head = if (*channel).len == 0 || next == (*channel).capacity {
            0
        } else {
            next
        };
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
    // SAFETY: forwarded.
    unsafe { receive::<false>(context, ptr::null_mut(), channel, destination, site) }
}

/// `hirsel_core_receive` that may park with the stack given back
/// (`RtOp::ReceiveRec`). A zero word makes the receive; a nonzero one resumes
/// it after a returnable park, at the commit loop: a delegate serves only a
/// requester blocked in its call, so the resume makes no second offer. A park
/// leaves the word 1 and initializes nothing; the frames above return to the
/// task's entry, and the wake makes the call again.
///
/// # Safety
/// As for hirsel_core_receive; `word` is the caller's site word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_receive_rec(
    context: *mut Context,
    word: *mut u64,
    channel: *mut Channel,
    destination: *mut u8,
    site: u64,
) -> Transfer {
    // SAFETY: forwarded.
    unsafe { receive::<true>(context, word, channel, destination, site) }
}

/// # Safety
/// As for hirsel_core_receive; with REC, `word` as hirsel_core_receive_rec.
#[inline(always)]
unsafe fn receive<const REC: bool>(
    context: *mut Context,
    word: *mut u64,
    channel: *mut Channel,
    destination: *mut u8,
    site: u64,
) -> Transfer {
    // SAFETY: receiver keeps the queue alive across each suspension.
    unsafe {
        if REC {
            *word = 0;
        }
        let returnable = REC && tasks::returnable_waits();
        loop {
            tasks::before_commit();
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
            if returnable {
                tasks::park_returning(
                    tasks::current_task(),
                    &raw mut (*channel).receive_waiters,
                    #[cfg(feature = "prof")]
                    (crate::prof::Reason::ChannelRecv, (*channel).id),
                );
                *word = 1;
                return Transfer {
                    transferred: false,
                    status: 0,
                };
            }
            tasks::park(
                &raw mut (*channel).receive_waiters,
                #[cfg(feature = "prof")]
                (crate::prof::Reason::ChannelRecv, (*channel).id),
            );
        }
    }
}

#[repr(C)]
pub struct ReceiveUntil {
    pub kind: u8,
    pub status: u8,
}

/// # Safety
/// Sole receiver borrow, writable T destination and a live Clock when present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_receive_until(
    context: *mut Context,
    channel: *mut Channel,
    destination: *mut u8,
    present: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> ReceiveUntil {
    // SAFETY: the capability and receiver pin their owners across the wait.
    unsafe {
        if present == 0 {
            let result = hirsel_core_receive(context, channel, destination, site);
            return ReceiveUntil {
                kind: if result.transferred { 0 } else { 2 },
                status: result.status,
            };
        }

        let mut deadline = ReceiveDeadline::new(clock, at);
        let kind = loop {
            tasks::before_commit();
            if tasks::check_cancel(context, site) != 0 {
                return ReceiveUntil { kind: 0, status: 1 };
            }
            if take(channel, destination) {
                break 0;
            }
            let id = (*channel).id;
            if (*channel).senders == 0 {
                tasks::event(
                    tasks::RECEIVE,
                    id,
                    hirsel_core_layout::event::CHANNEL_CLOSED,
                );
                break 2;
            }
            match deadline.park(context, &raw mut (*channel).receive_waiters, id, site) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(status) => return ReceiveUntil { kind: 0, status },
            }
            tasks::event(tasks::RECEIVE, id, hirsel_core_layout::event::CHANNEL_EMPTY);
            break 1;
        };
        ReceiveUntil { kind, status: 0 }
    }
}

/// One timed receive, shared by channels and inboxes. A provider outside the
/// runtime can use the same request token on each retry.
pub(crate) struct ReceiveDeadline {
    clock: u64,
    at: u64,
    token: u64,
}

impl ReceiveDeadline {
    pub(crate) fn new(clock: u64, at: u64) -> Self {
        Self {
            clock,
            at,
            token: 0,
        }
    }

    /// False means the deadline has passed. True means a wake retired the
    /// registrations and the caller must poll again. An error is a host failure.
    /// # Safety
    /// The running task pins its context, clock and wait queue across the call.
    pub(crate) unsafe fn park(
        &mut self,
        context: *mut Context,
        queue: *mut tasks::Waiters,
        id: u64,
        site: u64,
    ) -> Result<bool, u8> {
        // SAFETY: the clock and wait queue remain live until the call returns.
        unsafe {
            let provider = *crate::entry::hirsel_core_capability_provider(context, self.clock);
            let handle = crate::entry::hirsel_core_capability_handle(context, self.clock);
            #[cfg(feature = "record")]
            let recorded = crate::tape::owns(&provider);
            #[cfg(not(feature = "record"))]
            let recorded = false;
            if recorded {
                let observed = crate::host_ops::hirsel_core_host_clock(
                    context,
                    crate::provider::MONO_NS,
                    self.clock,
                    ptr::null_mut(),
                    0,
                    site,
                );
                if observed.status != 0 {
                    return Err(observed.status);
                }
                if observed.value >= self.at {
                    return Ok(false);
                }
            }
            #[cfg(feature = "record")]
            let source = if recorded {
                crate::tape::source(&provider, handle)
            } else {
                Some((provider, handle))
            };
            #[cfg(not(feature = "record"))]
            let source = Some((provider, handle));
            let deadline = channel_deadline(source, self.at, recorded, &mut self.token);
            Ok(deadline.is_some_and(|deadline| tasks::park_channel(queue, id, deadline, recorded)))
        }
    }
}

/// # Safety
/// A non-null source pins its Clock provider and local handle.
unsafe fn channel_deadline(
    source: Option<(crate::provider::Provider, u64)>,
    at: u64,
    recorded: bool,
    token: &mut u64,
) -> Option<tasks::ChannelDeadline> {
    use tasks::ChannelDeadline;
    // SAFETY: the source and its handle are live until retirement.
    unsafe {
        let Some((provider, handle)) = source else {
            #[cfg(feature = "record")]
            return Some(ChannelDeadline::Recorded);
            #[cfg(not(feature = "record"))]
            unreachable!("an untaped clock always has a provider");
        };
        if let Some(clock) = crate::provide::ChannelClock::of(&provider, handle) {
            return Some(ChannelDeadline::Cell(clock, at));
        }
        if crate::os::owns(&provider) {
            let at = crate::os::epoch(provider.state).saturating_add(at);
            return (recorded || at > crate::os::monotonic_ns())
                .then_some(ChannelDeadline::Host(at));
        }

        let mut bytes = at.to_ne_bytes();
        let mut request = crate::provider::Request {
            operation: crate::provider::SLEEP_UNTIL,
            mode: 0,
            handle,
            path: ptr::null(),
            path_len: 0,
            buffer: crate::provider::Buffer {
                data: bytes.as_mut_ptr(),
                len: bytes.len(),
                capacity: bytes.len(),
            },
            site: 0,
        };
        if *token == 0 {
            *token = tasks::operation_token();
        }
        let wait = (provider.wait)(provider.state, &mut request, *token);
        match wait.kind {
            crate::provider::WAIT_READY if recorded => {
                Some(ChannelDeadline::Host(crate::os::monotonic_ns()))
            }
            crate::provider::WAIT_READY => None,
            crate::provider::WAIT_UNTIL => Some(ChannelDeadline::Host(wait.deadline_ns)),
            _ => crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnsupportedClock")),
        }
    }
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
        tasks::before_commit();
        let status = if take(channel, destination) {
            0
        } else if (*channel).senders == 0 {
            2
        } else {
            1
        };
        tasks::before_commit();
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
        tasks::before_commit();
        (*channel).senders -= 1;
        let last = (*channel).senders == 0;
        (*channel).closing_sender = last;
        tasks::event(tasks::SENDERS, (*channel).id, (*channel).senders);
        if last {
            tasks::before_commit();
            tasks::event_wake_waiters(
                tasks::CLOSE,
                (*channel).id,
                0,
                &raw mut (*channel).receive_waiters,
            );
            (*channel).closing_sender = false;
            if (*channel).receiver == Receiver::Gone {
                drop(crate::task_heap::custody::RawStorage::from_guest(channel));
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
        tasks::before_commit();
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
        (*channel).head = 0;
        (*channel).receiver = Receiver::Gone;
        if (*channel).senders == 0 && !(*channel).closing_sender {
            drop(crate::task_heap::custody::RawStorage::from_guest(channel));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks_tests::{run, value};

    unsafe extern "C" fn drain_refill(context: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
        // SAFETY: the root owns both endpoints and passes valid u64 places.
        unsafe {
            let queue = hirsel_core_channel_new(context, 4, value::<u64>(8), 1);
            assert!(!queue.sender.is_null());
            let mut next = 1u64;
            let mut got = 0u64;
            assert_eq!(
                hirsel_core_try_send(queue.sender, (&mut next as *mut u64).cast()),
                0
            );
            assert_eq!(
                hirsel_core_try_receive(queue.receiver, (&mut got as *mut u64).cast()),
                0
            );
            assert_eq!(got, 1);
            let drained_head = (*queue.receiver).head;

            for mut sent in [2u64, 3, 4, 5] {
                assert_eq!(
                    hirsel_core_try_send(queue.sender, (&mut sent as *mut u64).cast()),
                    0
                );
            }
            for expected in [2, 3, 4, 5] {
                assert_eq!(
                    hirsel_core_try_receive(queue.receiver, (&mut got as *mut u64).cast()),
                    0
                );
                assert_eq!(got, expected);
            }
            assert_eq!(drained_head, 0);
            hirsel_core_sender_drop(queue.sender);
            assert_eq!(hirsel_core_receiver_drop(context, queue.receiver), 0);
        }
        0
    }

    #[test]
    fn drained_fifo_reuses_slot_zero_and_refills_in_order() {
        run(drain_refill, ());
    }
}

#[cfg(any(test, feature = "charge-observation", feature = "prof"))]
impl Drop for Channel {
    fn drop(&mut self) {
        crate::backing_observation::observe!(free, (self as *const Self).cast(), size_of::<Self>());
    }
}
