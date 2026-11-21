//! Own blocks keep funding in their allocation page. Graph nodes reserve a
//! resumable drop frame. Physical layout is separate from logical charge.
use crate::{Capacity, Context, limit::Payer, task_heap};
use std::ptr::NonNull;

pub const PLAIN: u8 = crate::abi_layout::NATIVE_PLAIN;
pub const GRAPH: u8 = crate::abi_layout::NATIVE_GRAPH;
pub const RESOURCE: u8 = crate::abi_layout::NATIVE_RESOURCE;
/// Immutable generated metadata for one concrete fixed owning shape, followed
/// inline by its suspension [`Point`]s (abi_layout::fixed_point).
#[repr(C)]
pub struct Descriptor {
    pub recipe: crate::counted::Recipe,
    pub charge: u64,
    pub payload_offset: u64,
}
/// Where a suspended fixed block keeps its frame, payload-relative.
#[repr(C)]
pub struct Point {
    /// Distance back to the descriptor.
    pub back: u64,
    pub cursor: u64,
    pub a: u64,
    pub b: u64,
    pub index: u64,
}
const _: () = assert!(size_of::<Descriptor>() == crate::abi_layout::FIXED_DESCRIPTOR_BYTES);
const _: () = assert!(size_of::<Point>() == crate::abi_layout::FIXED_POINT_BYTES);
const _: () = assert!(std::mem::offset_of!(Point, a) == crate::abi_layout::FIXED_POINT_A);
const _: () = assert!(std::mem::offset_of!(Point, index) == crate::abi_layout::FIXED_POINT_INDEX);
pub const GRAPH_PREFIX_BYTES: usize = size_of::<Frame>();
#[repr(C)]
struct Frame {
    dead: std::mem::MaybeUninit<Dead>,
}
#[repr(C)]
pub(crate) struct Dead {
    /// A worklist link (counted::Worklist).
    pub(crate) next: usize,
    pub(crate) charge: u64,
    pub(crate) recipe: crate::counted::Recipe,
    pub(crate) payload: *mut u8,
    pub(crate) cursor: u64,
}
#[inline]
unsafe fn frame(payload: NonNull<u8>) -> *mut Frame {
    // SAFETY: a GRAPH allocation reserves a Frame immediately before the payload.
    unsafe { payload.as_ptr().sub(size_of::<Frame>()).cast() }
}
pub(crate) unsafe fn set_resource(payload: NonNull<u8>, resource: *mut crate::causes::Resource) {
    // SAFETY: fresh RESOURCE allocation has this native pointer slot.
    unsafe {
        payload
            .as_ptr()
            .sub(size_of::<usize>())
            .cast::<*mut crate::causes::Resource>()
            .write(resource);
    }
}
pub(crate) unsafe fn resource(payload: NonNull<u8>) -> *mut crate::causes::Resource {
    // SAFETY: the tagged native word identifies whether a state slot exists.
    unsafe {
        if task_heap::kind(payload) != RESOURCE {
            std::ptr::null_mut()
        } else {
            payload
                .as_ptr()
                .sub(size_of::<usize>())
                .cast::<*mut crate::causes::Resource>()
                .read()
        }
    }
}
/// Write the frame of buffer storage whose recipe borrows a descriptor in its
/// live parent; the drain links it.
/// # Safety
/// The buffer is exclusively owned and not already queued, and its parent pins
/// the payload until this child finishes. The emitter has established
/// capacity != 0; a static carrier is never enqueued.
pub(crate) unsafe fn dead_buffer(
    block: NonNull<u8>,
    charge: u64,
    recipe: crate::counted::Recipe,
    payload: *mut u8,
) {
    // SAFETY: the buffer owns its prepaid GRAPH frame.
    unsafe {
        (*frame(block)).dead.write(Dead {
            next: 0,
            charge,
            recipe,
            payload,
            cursor: 0,
        });
    }
}
pub(crate) unsafe fn dead(payload: NonNull<u8>) -> *mut Dead {
    // SAFETY: dead_buffer initialized this uniquely owned dead graph node's frame.
    unsafe { (*frame(payload)).dead.as_mut_ptr() }
}
#[inline]
pub(crate) fn allocate(
    context: &Context,
    payer: Payer,
    charge: Option<u64>,
    bytes: usize,
    align: usize,
    kind: u8,
) -> Result<NonNull<u8>, Capacity> {
    admit_storage(payer, charge)?;
    Ok(allocate_admitted(context, payer, bytes, align, kind))
}

#[inline]
pub(crate) fn admit_storage(payer: Payer, charge: Option<u64>) -> Result<(), Capacity> {
    payer.admit(charge)?;
    if charge == Some(0) {
        payer.pin_zero();
    }
    Ok(())
}

#[inline]
pub(crate) fn allocate_admitted(
    context: &Context,
    payer: Payer,
    bytes: usize,
    align: usize,
    kind: u8,
) -> NonNull<u8> {
    task_heap::Heap::allocate(
        context.heap.get_or_init(std::sync::Arc::default),
        payer,
        bytes,
        align,
        kind,
        false,
    )
}
/// # Safety
/// A live block pins its original payer until its logical refund.
#[inline]
pub(crate) unsafe fn funding<'a>(payload: NonNull<u8>) -> &'a Payer {
    // SAFETY: the live block pins its page and original payer.
    unsafe { task_heap::payer(payload) }
}

/// # Safety
/// Exclusive cleaned block and exact remaining logical charge.
#[inline]
pub(crate) unsafe fn free(payload: NonNull<u8>, charge: u64) {
    // SAFETY: caller supplies exclusive cleaned storage and its exact charge.
    unsafe { free_with(payload, charge, None) }
}
#[inline(always)]
unsafe fn free_with(payload: NonNull<u8>, charge: u64, context: Option<&Context>) {
    // SAFETY: this live block pins page metadata. Resolve the logical refund
    // first. The page remains live until the physical slot is returned.
    unsafe {
        let page = task_heap::page_of(payload);
        let payer = *page.funding();
        if charge != 0 || page.kind() != RESOURCE {
            if let Some(context) = context {
                payer.settle_in(context, charge);
            } else {
                payer.settle(charge);
            }
        }
        task_heap::release_page(page, payload);
    }
}
/// # Safety
/// The worklist owns a cleaned node, its exact charge and a live task context.
#[inline(always)]
pub(crate) unsafe fn free_dead_in(context: &Context, payload: NonNull<u8>, charge: u64) {
    // SAFETY: caller transfers exclusive storage; the original payer stays pinned.
    unsafe { free_with(payload, charge, Some(context)) }
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
        Self::paid(context, context.payer(), charge, bytes, align, PLAIN)
    }
    pub fn graph(
        context: &Context,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        Self::paid(context, context.payer(), charge, bytes, align, GRAPH)
    }
    #[inline]
    pub(crate) fn paid(
        context: &Context,
        payer: Payer,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
        kind: u8,
    ) -> Result<Self, Capacity> {
        let pointer = allocate(context, payer, charge, bytes, align, kind)?;
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
        let pointer = allocate(
            context,
            context.payer(),
            charge,
            bytes,
            align,
            // SAFETY: self retains the original allocation.
            unsafe { task_heap::kind(self.pointer) },
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
/// Exclusive cleaned block and exact remaining logical charge. Foreign or
/// forwarded payers follow the ordered refund path.
#[inline(always)]
pub(crate) unsafe fn free_in(context: &Context, payload: NonNull<u8>, charge: u64) {
    // SAFETY: caller transfers this live exclusive slot and its charge.
    unsafe { free_with(payload, charge, Some(context)) };
}
