//! Sockets that cross units in a value: detached from the sending unit's
//! registry with the value, attached in the receiving unit's at its take.
use super::*;

pub(super) fn socket_offsets(owners: &[[u64; 4]]) -> impl Iterator<Item = u64> + Clone {
    owners.iter().filter(|o| o[1] == SOCKET).map(|o| o[0])
}

pub(super) unsafe fn detach_sockets(
    context: *mut Context,
    value: *const u8,
    offsets: impl Iterator<Item = u64> + Clone,
) -> Option<CrossingSockets> {
    for offset in offsets.clone() {
        // SAFETY: the initialized item pins each socket word during the preflight.
        unsafe {
            let key = word(value.add(offset as usize));
            CrossedSocket::check(&*(*context).host_files, key);
        }
    }
    let sockets: Vec<_> = offsets
        .map(|offset| {
            // SAFETY: the item exclusively owns each socket in this unit.
            unsafe {
                let key = word(value.add(offset as usize));
                CrossedSocket::detach(&mut *(*context).host_files, key)
            }
        })
        .collect();
    (!sockets.is_empty()).then(|| Arc::new(Mutex::new(SocketCrossing::InTransit(sockets))))
}

/// Install the socket keys in a received value or give a refused send its
/// sockets back.
#[inline(always)]
pub(super) unsafe fn attach_sockets(
    context: *mut Context,
    value: *mut u8,
    offsets: impl Iterator<Item = u64>,
    sockets: Option<&CrossingSockets>,
) {
    if let Some(sockets) = sockets {
        // SAFETY: forwarded.
        unsafe { attach_crossing(context, value, offsets, sockets) }
    }
}
#[inline(never)]
unsafe fn attach_crossing(
    context: *mut Context,
    value: *mut u8,
    offsets: impl Iterator<Item = u64>,
    sockets: &CrossingSockets,
) {
    // SAFETY: the caller owns the destination and runs in its registry's unit.
    unsafe {
        let files = &mut *(*context).host_files;
        let keys = sockets.lock().unwrap().keys(files);
        for (offset, key) in offsets.zip(keys) {
            set_word(value.add(offset as usize), key);
        }
        if tasks::in_task()
            && let Some(poller) = files.sockets.active_poller()
        {
            tasks::attach_net(&raw mut files.sockets, poller);
        }
    }
}

/// Register sockets when the receiving unit takes an arrival.
#[inline(always)]
pub(super) unsafe fn take_sockets(u: &mut LangUnit, h: &InboxH) {
    // Only an item type with socket fields carries sockets.
    if !h.inbox.0.socket_offsets.is_empty() {
        // SAFETY: forwarded.
        unsafe { take_crossing(u, h) }
    }
}
#[inline(never)]
unsafe fn take_crossing(u: &mut LangUnit, h: &InboxH) {
    if let Some(sockets) = h
        .inbox
        .0
        .side()
        .taken
        .back()
        .and_then(|item| item.value().item_sockets())
    {
        // SAFETY: the inbox belongs to this unit, which owns its host files.
        let files = unsafe { &mut *h.files };
        sockets.lock().unwrap().take(files);
        if files.sockets.active_poller().is_some() {
            u.socket_registry = &raw mut files.sockets;
        }
    }
}
