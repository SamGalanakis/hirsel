//! Bounded FIFO ownership transfer. A queue admits its whole logical storage
//! before publication and never allocates at send, receive, close or cleanup.
use crate::{
    Context,
    block::Block,
    causes::{self, Cause},
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
    id: u64,
    senders: u64,
    receiver: Receiver,
    capacity: usize,
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
            causes::record(
                &mut *context,
                Cause {
                    kind: causes::BOUNDS,
                    site,
                    message: Default::default(),
                },
            );
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
        let id = tasks::identity();
        let channel = Box::into_raw(Box::new(Channel {
            id,
            senders: 1,
            receiver: Receiver::Open,
            capacity: count,
            head: 0,
            len: 0,
            item,
            storage: Storage::new(bytes, item.align),
            _charge: charge,
        }));
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
        (*channel).senders = (*channel)
            .senders
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"ChannelSenders"));
        tasks::event(tasks::SENDERS, (*channel).id, (*channel).senders);
        channel
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
        let rest = (*channel).capacity - (*channel).head;
        let at = if (*channel).len >= rest {
            (*channel).len - rest
        } else {
            (*channel).head + (*channel).len
        };
        ptr::copy_nonoverlapping(
            value,
            (*channel).storage.as_ptr().add(at * (*channel).item.bytes),
            (*channel).item.bytes,
        );
        (*channel).len += 1;
        tasks::event(tasks::PUBLISH, (*channel).id, 0);
        tasks::wake();
        0
    }
}
/// # Safety
/// Borrowed live sender and initialized T. Result 0 transfers it, 1 Full and 2
/// Closed preserve it. No cancellation point, including when the queue is full.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_try_send(channel: *mut Channel, value: *mut u8) -> u8 {
    // SAFETY: no suspension or outside access between deciding and committing.
    unsafe {
        let result = publish(channel, value);
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
                    return Transfer {
                        transferred: false,
                        status: 0,
                    };
                }
                _ => tasks::checkpoint(true),
            }
        }
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
        loop {
            if tasks::check_cancel(context, site) != 0 {
                return Transfer {
                    transferred: false,
                    status: 1,
                };
            }
            if (*channel).len != 0 {
                ptr::copy_nonoverlapping(
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
                tasks::event(tasks::RECEIVE, (*channel).id, 0);
                tasks::wake();
                return Transfer {
                    transferred: true,
                    status: 0,
                };
            }
            if (*channel).senders == 0 {
                return Transfer {
                    transferred: false,
                    status: 0,
                };
            }
            tasks::checkpoint(true);
        }
    }
}
/// # Safety
/// Transfers one live sender. No allocation and no finalizer failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_sender_drop(channel: *mut Channel) {
    // SAFETY: exact live sender count, with receiver retaining storage during cleanup.
    unsafe {
        (*channel).senders -= 1;
        tasks::event(tasks::SENDERS, (*channel).id, (*channel).senders);
        if (*channel).senders == 0 {
            tasks::event(tasks::CLOSE, (*channel).id, 0);
            tasks::wake();
        }
        if (*channel).senders == 0 && (*channel).receiver == Receiver::Gone {
            drop(Box::from_raw(channel));
        }
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
    // SAFETY: receiver retains queue storage throughout possibly blocking item drops.
    unsafe {
        (*context).cancel_shield += 1;
        (*channel).receiver = Receiver::Cleaning;
        tasks::event(tasks::CLOSE, (*channel).id, 1);
        tasks::wake();
        let mut failed = 0;
        while (*channel).len != 0 {
            (*channel).len -= 1;
            let rest = (*channel).capacity - (*channel).head;
            let at = if (*channel).len >= rest {
                (*channel).len - rest
            } else {
                (*channel).head + (*channel).len
            };
            failed |= ((*channel).item.drop)(
                context,
                (*channel).storage.as_ptr().add(at * (*channel).item.bytes),
            );
        }
        (*channel).receiver = Receiver::Gone;
        if (*channel).senders == 0 {
            drop(Box::from_raw(channel));
        }
        (*context).cancel_shield -= 1;
        failed
    }
}
