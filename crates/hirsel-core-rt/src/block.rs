//! Own bytes have one native payer word. Only graph nodes reserve a resumable
//! drop frame; physical layouts are emitter operands, never logical charges.
use crate::allocator::allocate as native_allocate;
use crate::{Capacity, Context, limit::Payer};
use std::alloc::Layout;
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
pub const GRAPH_PREFIX_BYTES: usize = size_of::<Payer>() + size_of::<Frame>();
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
pub(crate) fn prefix(kind: u8) -> usize {
    crate::abi_layout::own_prefix(kind)
}
#[inline]
fn physical_layout(bytes: usize, align: usize, kind: u8) -> (Layout, usize) {
    let alignment = align.max(8);
    let extra = usize::from(alignment > 512) * 8;
    let offset = (prefix(kind) + extra).next_multiple_of(alignment);
    let layout = Layout::from_size_align(
        offset
            .checked_add(bytes.max(1))
            .unwrap_or_else(|| crate::process::stop(b"HostMemory")),
        alignment,
    )
    .unwrap_or_else(|_| crate::process::stop(b"HostMemory"));
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
    // The cold ABI also receives constant-layout ordinary blocks from Rust and
    // uncommon emitted branches. Avoid generic alignment rounding/tag selection
    // for this same frozen plain/GRAPH shape.
    let ordinary_prefix = if kind == PLAIN {
        crate::abi_layout::PLAIN_PREFIX_BYTES
    } else {
        crate::abi_layout::GRAPH_PREFIX_BYTES
    };
    if align <= 8 && kind <= GRAPH && bytes <= isize::MAX as usize - ordinary_prefix {
        // SAFETY: the size bound and fixed alignment form a nonzero valid Layout;
        // admission already retained the canonical payer and zero-byte pin.
        unsafe {
            let layout = Layout::from_size_align_unchecked(bytes.max(1) + ordinary_prefix, 8);
            let base = native_allocate(layout);
            let payload = NonNull::new_unchecked(base.as_ptr().add(ordinary_prefix));
            let word = if kind == PLAIN {
                payer
            } else {
                payer.with_tag(kind as usize)
            };
            payload
                .as_ptr()
                .sub(crate::abi_layout::OWN_PAYER_BACK)
                .cast::<Payer>()
                .write(word);
            return Ok(payload);
        }
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
        let tag = (index << 2) | kind as usize;
        // Admission took an untagged canonical payer; ordinary plain/align8
        // blocks need no tag operation at all.
        payload.as_ptr().sub(8).cast::<Payer>().write(if tag == 0 {
            payer
        } else {
            payer.with_tag(tag)
        });
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
    // SAFETY: caller supplies exclusive cleaned storage and its exact charge.
    unsafe { free_with(payload, charge, None) }
}
#[inline(always)]
unsafe fn free_with(payload: NonNull<u8>, charge: u64, context: Option<&Context>) {
    // SAFETY: native kind/alignment are encoded in the sole payer word.
    unsafe {
        let payer = *funding(payload);
        let tag = payer.native_tag();
        if tag == 0 {
            // Plain align-8 blocks, the common case: decode nothing.
            if let Some(context) = context {
                payer.settle_in(context, charge);
            } else {
                payer.settle(charge);
            }
            crate::allocator::deallocate_raw(
                payload.as_ptr().sub(crate::abi_layout::PLAIN_PREFIX_BYTES),
            );
            return;
        }
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
        // Alignment is a power of two (allocate), so round with a mask: the
        // drain frees every fixed block through here and a division is its
        // dearest step.
        let offset =
            (prefix(kind) + usize::from(index == 7) * 8 + alignment - 1) & !(alignment - 1);
        if charge != 0 || kind != RESOURCE {
            if let Some(context) = context {
                payer.with_tag(0).settle_in(context, charge);
            } else {
                payer.settle(charge);
            }
        }
        crate::allocator::deallocate_raw(payload.as_ptr().sub(offset));
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
        Self::paid(context.payer(), charge, bytes, align, PLAIN)
    }
    pub fn graph(
        context: &Context,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        Self::paid(context.payer(), charge, bytes, align, GRAPH)
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
        let pointer = allocate(
            context.payer(),
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
        payer.settle_in(context, charge);
        let (_, offset) = physical_layout(bytes, align, PLAIN);
        crate::allocator::deallocate_raw(payload.as_ptr().sub(offset));
    }
}

/// # Safety
/// Exclusive cleaned PLAIN payload with its exact original native operands.
#[inline(always)]
pub(crate) unsafe fn deallocate_plain(payload: NonNull<u8>, bytes: usize, align: usize) {
    let (_, offset) = physical_layout(bytes, align, PLAIN);
    // SAFETY: native operands identify the original allocation base.
    unsafe {
        crate::allocator::deallocate_raw(payload.as_ptr().sub(offset));
    }
}
