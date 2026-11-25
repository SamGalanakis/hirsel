//! An item's flat bytes and sequence contents, packed for a crossing and
//! rebuilt where it arrives.
use super::*;

/// Copy `value` and its owners' content into one buffer.
pub(super) unsafe fn pack(value: *const u8, bytes: u64, owners: &[[u64; 4]]) -> Vec<u8> {
    // SAFETY: `value` is an initialized item of `bytes` bytes.
    unsafe {
        let content = |o: &[u64; 4]| {
            let at = value.add(o[0] as usize);
            (word(at) as *const u8, (word(at.add(8)) * o[2]) as usize)
        };
        let seqs = owners.iter().filter(|o| o[1] == SEQ);
        let mut data =
            Vec::with_capacity(bytes as usize + seqs.clone().map(|o| content(o).1).sum::<usize>());
        data.extend_from_slice(std::slice::from_raw_parts(value, bytes as usize));
        for o in seqs {
            let (p, n) = content(o);
            if n > 0 {
                data.extend_from_slice(std::slice::from_raw_parts(p, n));
            }
        }
        data
    }
}

/// A send can wait for a slot. Its call frame pins this source and its
/// owners until the send has published or its settlement is taken.
pub(crate) struct CrossSend {
    value: *const u8,
    pub(crate) sockets: Option<CrossingSockets>,
    #[cfg(test)]
    pub(crate) tag: u64,
}

impl CrossSend {
    pub(crate) fn new(value: *const u8) -> Self {
        Self {
            value,
            sockets: None,
            #[cfg(test)]
            tag: 0,
        }
    }

    /// The connection owns the destination slot. Only the flat bytes go
    /// there; sequence contents occupy the connection's room, in `spare`
    /// when it is large enough (it is then taken).
    pub(crate) unsafe fn copy_into(
        &self,
        slot: *mut u8,
        stride: usize,
        seqs: &[[u64; 4]],
        charge: u64,
        spare: &mut Box<[MaybeUninit<u64>]>,
    ) -> Box<[MaybeUninit<u64>]> {
        // SAFETY: the reserved slot has the connection's item layout. The
        // sender pins every source buffer through publication.
        unsafe {
            if stride != 0 {
                ptr::copy_nonoverlapping(self.value, slot, stride);
            }
            let content = |o: &[u64; 4]| {
                let at = self.value.add(o[0] as usize);
                (word(at) as *const u8, (word(at.add(8)) * o[2]) as usize)
            };
            if seqs.is_empty() {
                return Box::default();
            }
            let bytes: usize = seqs.iter().map(|o| content(o).1).sum();
            if bytes == 0 && charge == 0 {
                return Box::default();
            }
            let words = 1 + bytes.div_ceil(std::mem::size_of::<u64>());
            let mut room = if spare.len() >= words {
                std::mem::take(spare)
            } else {
                Box::new_uninit_slice(words)
            };
            room[0].write(charge);
            let to = room.as_mut_ptr().add(1).cast::<u8>();
            let mut at = 0;
            for o in seqs {
                let (p, n) = content(o);
                if n != 0 {
                    ptr::copy_nonoverlapping(p, to.add(at), n);
                    at += n;
                }
            }
            room
        }
    }
}

/// Free the seq buffers of a value whose content has been copied out; its
/// clients have moved and are not touched. `context` is the running task's
/// when the caller has it.
pub(super) unsafe fn free_owners(context: Option<&Context>, value: *const u8, owners: &[[u64; 4]]) {
    // SAFETY: the value owns each nonempty buffer, charged at its capacity.
    unsafe {
        for o in owners {
            if o[1] == SEQ {
                let at = value.add(o[0] as usize);
                let cap = word(at.add(16));
                if cap != 0 {
                    let p = word(at) as *mut u8;
                    block::free_with(ptr::NonNull::new_unchecked(p), round8(cap * o[3]), context);
                }
            }
        }
    }
}

/// Rebuild an item from `data` into `dest`: each seq gets a fresh buffer at
/// length capacity from `payer`, whose charge was admitted by the caller.
pub(super) unsafe fn unpack(
    context: *mut Context,
    payer: crate::limit::Payer,
    data: &[u8],
    sockets: Option<&CrossingSockets>,
    dest: *mut u8,
    bytes: u64,
    owners: &[[u64; 4]],
) {
    // SAFETY: `data` was packed for the same layout and `dest` is writable.
    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), dest, bytes as usize);
        attach_sockets(context, dest, socket_offsets(owners), sockets);
        unpack_owners(
            context,
            payer,
            data.as_ptr().add(bytes as usize),
            dest,
            owners,
        );
    }
}

/// Rebuild the sequence owners after the item's flat bytes have been copied.
pub(super) unsafe fn unpack_owners(
    context: *mut Context,
    payer: crate::limit::Payer,
    content: *const u8,
    dest: *mut u8,
    owners: &[[u64; 4]],
) {
    // SAFETY: the receiver admitted the owners and the flat bytes give their lengths.
    unsafe {
        let mut at = 0;
        for o in owners {
            if o[1] != SEQ {
                continue;
            }
            let slot = dest.add(o[0] as usize);
            let len = word(slot.add(8));
            let n = (len * o[2]) as usize;
            if len == 0 {
                set_word(slot, 0);
                set_word(slot.add(16), 0);
                continue;
            }
            let p = block::allocate_admitted(
                &*context,
                payer,
                n,
                o[2].clamp(1, 16) as usize,
                crate::abi_layout::NATIVE_PLAIN,
            );
            ptr::copy_nonoverlapping(content.add(at), p.as_ptr(), n);
            at += n;
            set_word(slot, p.as_ptr() as u64);
            set_word(slot.add(16), len);
        }
    }
}

/// Drop the moved owners of an item that no one received.
pub(super) unsafe fn drop_packed_owners(
    context: *mut Context,
    unit: &mut LangUnit,
    flat: *const u8,
    owners: &[[u64; 4]],
    sockets: Option<&CrossingSockets>,
) {
    if let Some(sockets) = sockets {
        let keys = sockets.lock().unwrap().discard();
        for key in keys {
            // SAFETY: the item owned this socket in the current unit.
            unsafe { crate::net::hirsel_core_socket_drop(context, key) };
        }
    }
    for o in owners {
        if o[1] == CLIENT {
            // SAFETY: the item holds this inline handle and owns the client.
            let h = unsafe { word(flat.add(o[0] as usize)) } as *mut ClientH;
            if !h.is_null() {
                unit.drop_client(unsafe { Box::from_raw(h) }.client);
            }
        } else if o[1] == SHARED {
            // SAFETY: the item moved this holder here and no one received it.
            unsafe {
                crate::shared::release(unit, context, word(flat.add(o[0] as usize)) as *mut u8)
            };
        }
    }
}
