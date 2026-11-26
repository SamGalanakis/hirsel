//! An item's flat bytes and sequence contents, packed for a crossing and
//! rebuilt where it arrives.
use super::*;
use crate::units::ledger::conn::ConnCore;
use hirsel_core_layout::cross::{self, ELEMENT, PADDING};
use std::{borrow::Cow, ops::Range};

/// The rows of `rows` that hold for the value at `value`: a variant's owner
/// words and padding for its active case only (`cross::live`). Read before
/// anything rewrites an owner word, since a variant's tag can sit in one (a
/// sequence capacity's niche).
///
/// # Safety
/// `value` holds an initialized value laid out as `rows` describe.
pub(crate) unsafe fn live_rows<'r>(value: *const u8, rows: &'r [[u64; 4]]) -> Cow<'r, [[u64; 4]]> {
    if !cross::guarded(rows) {
        return Cow::Borrowed(rows);
    }
    Cow::Owned(cross::live(rows, &|at, width| {
        let mut tag = [0; 8];
        // SAFETY: a guard names the initialized tag word of a variant in the value.
        unsafe {
            ptr::copy_nonoverlapping(value.add(at as usize), tag.as_mut_ptr(), width as usize)
        };
        u64::from_le_bytes(tag)
    }))
}

/// The charge of a seq of `len` elements at length capacity.
pub(super) fn seq_charge(len: u64, logical: u64) -> u64 {
    round8(len * logical)
}

/// The charge `x` carries across: its owners' content at length capacity.
pub(super) unsafe fn item_charge(value: *const u8, owners: &[[u64; 4]]) -> u64 {
    let mut total = 0u64;
    for o in owners {
        if o[1] == SEQ {
            // SAFETY: the owner's length word is at offset 8.
            let len = unsafe { word(value.add(o[0] as usize + 8)) };
            total += seq_charge(len, o[3]);
        }
    }
    total
}

/// `item_charge` over the rows that hold for the value (`live_rows`).
pub(super) unsafe fn live_charge(value: *const u8, rows: &[[u64; 4]]) -> u64 {
    // SAFETY: as `live_rows` and `item_charge`.
    unsafe { item_charge(value, &live_rows(value, rows)) }
}

/// The owner rows an inbox keeps to drop the items no one received: clients
/// and `Shared` holders, and every row of a guarded (variant) item, so the
/// drop resolves each item's case.
pub(super) fn inbox_owners(owners: &[[u64; 4]]) -> Vec<[u64; 4]> {
    if cross::guarded(owners) {
        return owners.to_vec();
    }
    owners
        .iter()
        .copied()
        .filter(|o| o[1] == CLIENT || o[1] == SHARED)
        .collect()
}

/// Copy a value's flat span, zeroing the padding in the destination.
pub(crate) unsafe fn copy_flat(
    from: *const u8,
    to: *mut u8,
    at: u64,
    bytes: usize,
    rows: &[[u64; 4]],
) {
    if bytes != 0 {
        // SAFETY: the caller supplies disjoint spans and the layout's rows.
        unsafe {
            ptr::copy_nonoverlapping(from.add(at as usize), to, bytes);
            clear_padding(from, to, at..at + bytes as u64, rows, false);
        }
    }
}

/// Copy a sequence's plain elements, zeroing each element's own padding.
pub(crate) unsafe fn copy_elements(
    from: *const u8,
    to: *mut u8,
    bytes: usize,
    stride: usize,
    rows: &[[u64; 4]],
) {
    // SAFETY: the spans hold complete elements and are disjoint.
    unsafe {
        ptr::copy_nonoverlapping(from, to, bytes);
        if rows.iter().any(|r| r[1] == (PADDING | ELEMENT)) {
            for offset in (0..bytes).step_by(stride) {
                clear_padding(
                    from.add(offset),
                    to.add(offset),
                    0..stride as u64,
                    rows,
                    true,
                );
            }
        }
    }
}

unsafe fn clear_padding(
    from: *const u8,
    to: *mut u8,
    copied: Range<u64>,
    rows: &[[u64; 4]],
    element: bool,
) {
    cross::padding(
        rows,
        element,
        &|at, width| {
            let mut tag = [0; 8];
            // SAFETY: a case row reads only its initialized tag.
            unsafe {
                ptr::copy_nonoverlapping(from.add(at as usize), tag.as_mut_ptr(), width as usize)
            };
            u64::from_le_bytes(tag)
        },
        |at, bytes| {
            // A tape entry copies just its input field. The wrapper can
            // have its own padding outside that field.
            let start = at.max(copied.start);
            let end = (at + bytes).min(copied.end);
            if start < end {
                // SAFETY: the intersection lies within the destination.
                unsafe {
                    to.add((start - copied.start) as usize)
                        .write_bytes(0, (end - start) as usize)
                };
            }
        },
    );
}

/// Append a native copy that initializes all `bytes` in the spare capacity.
/// The copy may move uninitialized padding, but clears it before returning.
///
/// # Safety
/// `copy` must fully initialize the destination.
pub(crate) unsafe fn append(data: &mut Vec<u8>, bytes: usize, copy: impl FnOnce(*mut u8)) {
    data.reserve(bytes);
    copy(data.spare_capacity_mut().as_mut_ptr().cast());
    // SAFETY: the copy initialized the new bytes.
    unsafe { data.set_len(data.len() + bytes) };
}

/// The source and physical length of a sequence's elements.
unsafe fn content(value: *const u8, [offset, _, stride, _]: &[u64; 4]) -> (*const u8, usize) {
    // SAFETY: the value holds this sequence carrier.
    unsafe {
        let at = value.add(*offset as usize);
        (word(at) as *const u8, (word(at.add(8)) * stride) as usize)
    }
}

/// Copy `value` and its owners' content into one buffer.
pub(super) unsafe fn pack(value: *const u8, bytes: u64, owners: &[[u64; 4]]) -> Vec<u8> {
    // SAFETY: `value` holds the live fields and sequences of the item.
    unsafe {
        let seqs = cross::sequences(owners);
        let bytes = bytes as usize;
        let mut data = Vec::with_capacity(
            bytes
                + seqs
                    .clone()
                    .map(|(o, _)| content(value, o).1)
                    .sum::<usize>(),
        );
        append(&mut data, bytes, |to| {
            copy_flat(value, to, 0, bytes, owners)
        });
        for (o, rows) in seqs {
            let (p, n) = content(value, o);
            if n > 0 {
                append(&mut data, n, |to| {
                    copy_elements(p, to, n, o[2] as usize, rows)
                });
            }
        }
        data
    }
}

impl ConnCore {
    /// The rows that hold for the item at `value`, when the rows are guarded.
    ///
    /// # Safety
    /// `value` holds an initialized item of this connection.
    pub(crate) unsafe fn live(&self, value: *const u8) -> Option<Vec<[u64; 4]>> {
        // SAFETY: the caller's item is laid out as `rows` describe.
        self.guarded
            .then(|| unsafe { live_rows(value, &self.rows) }.into_owned())
    }
}

/// A send can wait for a slot. Its call frame pins this source and its
/// owners until the send has published or its settlement is taken.
pub(crate) struct CrossSend {
    value: *const u8,
    /// The rows that hold for the value, when its connection's are guarded.
    pub(crate) rows: Option<Vec<[u64; 4]>>,
    pub(crate) sockets: Option<CrossingSockets>,
    #[cfg(test)]
    pub(crate) tag: u64,
}

impl CrossSend {
    pub(crate) fn new(value: *const u8) -> Self {
        Self {
            value,
            rows: None,
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
        conn: &ConnCore,
        charge: u64,
        spare: &mut Box<[MaybeUninit<u64>]>,
    ) -> Box<[MaybeUninit<u64>]> {
        // SAFETY: the reserved slot has the connection's item layout. The
        // sender pins every source buffer through publication.
        unsafe {
            copy_flat(self.value, slot, 0, conn.stride, &conn.rows);
            if conn.seqs.is_empty() {
                return Box::default();
            }
            let rows = self.rows.as_deref().unwrap_or(&conn.rows);
            let bytes: usize = cross::sequences(rows)
                .map(|(o, _)| content(self.value, o).1)
                .sum();
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
            for (o, rows) in cross::sequences(rows) {
                let (p, n) = content(self.value, o);
                if n != 0 {
                    copy_elements(p, to.add(at), n, o[2] as usize, rows);
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
        let owners = &*live_rows(data.as_ptr(), owners);
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
    // SAFETY: the flat bytes hold the item as it was packed.
    let owners = &*unsafe { live_rows(flat, owners) };
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
        } else if o[1] == MEDIA {
            // SAFETY: the packed item still owns the live media word.
            unsafe {
                crate::media::hirsel_core_media_drop(context, word(flat.add(o[0] as usize)));
            }
        } else if o[1] == SHARED {
            // SAFETY: the item moved this holder here and no one received it.
            unsafe {
                crate::shared::release(unit, context, word(flat.add(o[0] as usize)) as *mut u8)
            };
        }
    }
}
