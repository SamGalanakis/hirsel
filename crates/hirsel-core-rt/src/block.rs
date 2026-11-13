//! Own bytes have one native payer word. Only graph nodes reserve a resumable
//! drop frame; physical layouts are emitter operands, never logical charges.
use crate::allocator::allocate as native_allocate;
use crate::{Capacity, Context, limit::Payer};
use std::alloc::Layout;
use std::ptr::NonNull;

pub const PLAIN: u8 = 0;
pub const GRAPH: u8 = 1;
pub const RESOURCE: u8 = 2;
pub const GRAPH_PREFIX_BYTES: usize = size_of::<Payer>() + size_of::<Frame>();
#[repr(C)]
struct Frame {
    dead: std::mem::MaybeUninit<Dead>,
}
#[repr(C)]
pub(crate) struct Dead {
    pub(crate) next: Option<NonNull<u8>>,
    pub(crate) charge: u64,
    pub(crate) recipe: crate::counted::Recipe,
    pub(crate) payload: *mut u8,
    pub(crate) cursor: u64,
}
#[inline]
pub(crate) fn prefix(kind: u8) -> usize {
    size_of::<Payer>()
        + match kind {
            PLAIN => 0,
            GRAPH => size_of::<Frame>(),
            RESOURCE => size_of::<*mut crate::causes::Resource>(),
            _ => unreachable!(),
        }
}
#[inline]
fn physical_layout(bytes: usize, align: usize, kind: u8) -> (Layout, usize) {
    let alignment = align.max(8);
    let extra = usize::from(alignment > 512) * 8;
    let offset = (prefix(kind) + extra).next_multiple_of(alignment);
    let layout = Layout::from_size_align(
        offset
            .checked_add(bytes.max(1))
            .unwrap_or_else(|| std::process::abort()),
        alignment,
    )
    .unwrap_or_else(|_| std::process::abort());
    (layout, offset)
}
#[inline]
unsafe fn frame(payload: NonNull<u8>) -> *mut Frame {
    // SAFETY: a GRAPH allocation reserves a Frame immediately before the payer word.
    unsafe {
        payload
            .as_ptr()
            .sub(size_of::<Payer>() + size_of::<Frame>())
            .cast()
    }
}
pub(crate) unsafe fn set_resource(payload: NonNull<u8>, resource: *mut crate::causes::Resource) {
    // SAFETY: fresh RESOURCE allocation has this native pointer slot.
    unsafe {
        payload
            .as_ptr()
            .sub(16)
            .cast::<*mut crate::causes::Resource>()
            .write(resource);
    }
}
pub(crate) unsafe fn resource(payload: NonNull<u8>) -> *mut crate::causes::Resource {
    // SAFETY: the tagged native word identifies whether a state slot exists.
    unsafe {
        if funding(payload).native_tag() & 3 != RESOURCE as usize {
            std::ptr::null_mut()
        } else {
            payload
                .as_ptr()
                .sub(16)
                .cast::<*mut crate::causes::Resource>()
                .read()
        }
    }
}
pub(crate) unsafe fn push_dead(
    list: &mut crate::counted::Worklist,
    payload: NonNull<u8>,
    charge: u64,
    recipe: crate::counted::Recipe,
    payload_offset: usize,
) {
    // SAFETY: the last holder owns the GRAPH allocation and its prepaid frame.
    unsafe {
        (*frame(payload)).dead.write(Dead {
            next: list.head,
            charge,
            recipe,
            payload: payload.as_ptr().add(payload_offset),
            cursor: 0,
        });
    }
    list.head = Some(payload);
}
/// Enqueue buffer storage whose recipe borrows a descriptor in its live parent.
/// # Safety
/// The list is depth first. The parent frame owns the descriptor until this
/// child finishes, and the buffer is exclusively owned and not already queued.
pub(crate) unsafe fn push_buffer(
    list: &mut crate::counted::Worklist,
    block: NonNull<u8>,
    charge: u64,
    recipe: crate::counted::Recipe,
    payload: *mut u8,
) {
    // SAFETY: the buffer owns its native frame and its parent pins the payload.
    unsafe {
        push_dead(list, block, charge, recipe, 0);
        (*dead(block)).payload = payload;
    }
}
pub(crate) unsafe fn dead(payload: NonNull<u8>) -> *mut Dead {
    // SAFETY: push_dead initialized this uniquely owned dead graph node's frame.
    unsafe { (*frame(payload)).dead.as_mut_ptr() }
}
#[inline]
pub(crate) fn allocate(
    payer: Payer,
    charge: Option<u64>,
    bytes: usize,
    align: usize,
    kind: u8,
) -> Result<NonNull<u8>, Capacity> {
    payer.admit(charge)?;
    if charge == Some(0) {
        payer.pin_zero();
    }
    let (layout, offset) = physical_layout(bytes, align, kind);
    // SAFETY: new nonzero allocation owns the native prefix and disjoint payload.
    unsafe {
        let base = native_allocate(layout);
        let payload = NonNull::new_unchecked(base.as_ptr().add(offset));
        let alignment = align.max(8);
        let index = if alignment <= 512 {
            alignment.trailing_zeros() as usize - 3
        } else {
            7
        };
        payload
            .as_ptr()
            .sub(8)
            .cast::<Payer>()
            .write(payer.with_tag((index << 2) | kind as usize));
        if index == 7 {
            payload
                .as_ptr()
                .sub(prefix(kind) + 8)
                .cast::<usize>()
                .write(alignment);
        }
        Ok(payload)
    }
}
/// Growth alone borrows its original payer. The single-worker scheduler grants
/// the exclusive table lease for this non-suspending operation.
#[inline]
pub(crate) fn allocate_like(
    context: &Context,
    like: Option<&Payer>,
    charge: Option<u64>,
    bytes: usize,
    align: usize,
    kind: u8,
) -> Result<NonNull<u8>, Capacity> {
    if context.finalizing != 0 || like.is_none() {
        return allocate(context.payer(None), charge, bytes, align, kind);
    }
    let payer = *like.unwrap();
    if payer.record_pointer() == context.limit_record() {
        return allocate(context.payer(None), charge, bytes, align, kind);
    }
    crate::abi_tasks::payer_lease(|| {
        let funding = payer.growth_funding();
        let result = allocate(funding.payer, charge, bytes, align, kind);
        if result.is_ok() && funding.owner() != crate::tasks::current_id() {
            crate::tasks::event(crate::tasks::CHARGE, funding.owner(), charge.unwrap());
        }
        result
    })
}
/// # Safety
/// Live own, graph or resource payload; its native payer word stays initialized.
#[inline]
pub(crate) unsafe fn funding<'a>(payload: NonNull<u8>) -> &'a Payer {
    // SAFETY: every independent block has its payer immediately before the payload.
    unsafe { &*payload.as_ptr().sub(size_of::<Payer>()).cast::<Payer>() }
}
/// # Safety
/// Exclusive cleaned block; native size/alignment/kind and logical charge are exact.
#[inline]
pub(crate) unsafe fn free(payload: NonNull<u8>, charge: u64) {
    // SAFETY: native kind/alignment are encoded in the sole payer word.
    unsafe {
        let payer = *funding(payload);
        let tag = payer.native_tag();
        let kind = (tag & 3) as u8;
        let index = tag >> 2;
        let alignment = if index == 7 {
            payload
                .as_ptr()
                .sub(prefix(kind) + 8)
                .cast::<usize>()
                .read()
        } else {
            8usize << index
        };
        let offset = (prefix(kind) + usize::from(index == 7) * 8).next_multiple_of(alignment);
        if charge != 0 || kind != RESOURCE {
            payer.settle(charge);
        }
        crate::allocator::deallocate_raw(payload.as_ptr().sub(offset));
    }
}
/// # Safety
/// The dead GRAPH node's fields are completely cleaned.
pub(crate) unsafe fn free_graph(payload: NonNull<u8>, charge: u64) {
    // SAFETY: caller transfers the exclusively owned dead graph block.
    unsafe {
        free(payload, charge);
    }
}
/// Raw-byte Rust owner. Native layout operands correspond to emitter constants
/// or storage lengths; payload cleanup belongs to the emitter.
pub struct Block {
    pointer: NonNull<u8>,
    charge: u64,
}
impl Block {
    #[inline]
    pub fn new(
        context: &Context,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        Self::paid(context.payer(None), charge, bytes, align, PLAIN)
    }
    pub fn graph(
        context: &Context,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        Self::paid(context.payer(None), charge, bytes, align, GRAPH)
    }
    #[inline]
    pub(crate) fn paid(
        payer: Payer,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
        kind: u8,
    ) -> Result<Self, Capacity> {
        let pointer = allocate(payer, charge, bytes, align, kind)?;
        Ok(Self {
            pointer,
            charge: charge.unwrap(),
        })
    }
    #[inline]
    pub fn pointer(&self) -> NonNull<u8> {
        self.pointer
    }
    #[inline]
    pub fn charge(&self) -> u64 {
        self.charge
    }
    pub(crate) fn partition(&mut self, bytes: u64) -> Payer {
        self.charge -= bytes;
        *self.payer()
    }
    #[inline]
    pub(crate) fn payer(&self) -> &Payer {
        // SAFETY: this owner pins the live block and native payer.
        unsafe { funding(self.pointer) }
    }
    #[inline]
    pub fn replacement(
        &self,
        context: &Context,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        let pointer = allocate_like(
            context,
            Some(self.payer()),
            charge,
            bytes,
            align,
            self.payer().native_tag() as u8 & 3,
        )?;
        Ok(Self {
            pointer,
            charge: charge.unwrap(),
        })
    }
    pub fn into_raw(self) -> NonNull<u8> {
        let pointer = self.pointer;
        std::mem::forget(self);
        pointer
    }
    /// # Safety
    /// Exclusive live block from into_raw; every layout and charge operand is exact.
    pub unsafe fn from_raw(pointer: NonNull<u8>, charge: u64) -> Self {
        Self { pointer, charge }
    }
}
impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: this owner releases its cleaned storage and exact charge once.
        unsafe {
            free(self.pointer, self.charge);
        }
    }
}

/// # Safety
/// Exclusive cleaned PLAIN block, exact native layout and logical charge; live
/// task context. Foreign or merged payers go through the ordered refund path.
#[inline(always)]
pub(crate) unsafe fn free_in(
    context: &Context,
    payload: NonNull<u8>,
    charge: u64,
    bytes: usize,
    align: usize,
) {
    // SAFETY: recover the live block's payer before deallocating its native bytes.
    unsafe {
        let payer = *funding(payload);
        if charge != 0 && payer.record_pointer() == context.limit_record() {
            // The task pins its current canonical record and owns these local writes.
            (*context.limit_record()).available =
                (*context.limit_record()).available.wrapping_add(charge);
            (*context.limit_record()).outstanding =
                (*context.limit_record()).outstanding.wrapping_sub(charge);
        } else {
            payer.settle(charge);
        }
        let (_, offset) = physical_layout(bytes, align, PLAIN);
        crate::allocator::deallocate_raw(payload.as_ptr().sub(offset));
    }
}
