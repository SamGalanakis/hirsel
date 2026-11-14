//! Own bytes have one native payer word. Only graph nodes reserve a resumable
//! drop frame; physical layouts are emitter operands, never logical charges.
use crate::allocator::allocate as native_allocate;
use crate::{Capacity, Context, limit::Payer};
use std::alloc::Layout;
use std::ptr::NonNull;

pub const PLAIN: u8 = crate::abi_layout::NATIVE_PLAIN;
pub const GRAPH: u8 = crate::abi_layout::NATIVE_GRAPH;
pub const RESOURCE: u8 = crate::abi_layout::NATIVE_RESOURCE;
pub const STATIC: u8 = crate::abi_layout::NATIVE_STATIC;
/// Immutable generated metadata for one concrete fixed owning shape.
#[repr(C)]
pub struct Descriptor {
    pub recipe: crate::counted::Recipe,
    pub charge: u64,
    pub payload_offset: u64,
}
#[repr(C)]
struct StaticDead {
    next: Option<NonNull<u8>>,
    descriptor: *const Descriptor,
    cursor: u64,
}
const _: () =
    assert!(size_of::<StaticDead>() + size_of::<Payer>() == crate::abi_layout::STATIC_PREFIX_BYTES);
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
/// The emitter has established capacity != 0; a static carrier is never enqueued.
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
/// # Safety
/// Exclusive dead STATIC block; descriptor lives for the compiled program.
pub(crate) unsafe fn push_static(
    list: &mut crate::counted::Worklist,
    node: NonNull<u8>,
    descriptor: *const Descriptor,
) {
    // SAFETY: STATIC reserves these three frame words before the payer word.
    unsafe {
        let frame = node.as_ptr().sub(prefix(STATIC)).cast::<StaticDead>();
        frame.write(StaticDead {
            next: list.head,
            descriptor,
            cursor: 0,
        });
        list.head = Some(node);
    }
}
/// # Safety
/// Node is the initialized dead head of an exclusive worklist. Returns its
/// borrowed recipe state; the frame remains live until that recipe completes.
pub(crate) unsafe fn resume_state(
    node: NonNull<u8>,
) -> (
    crate::counted::Recipe,
    *mut u8,
    *mut u64,
    Option<NonNull<u8>>,
    u64,
) {
    // SAFETY: the native kind fixes the initialized frame and descriptor layout.
    unsafe {
        if funding(node).native_tag() & 3 == STATIC as usize {
            let frame = &mut *node.as_ptr().sub(prefix(STATIC)).cast::<StaticDead>();
            let descriptor = &*frame.descriptor;
            (
                descriptor.recipe,
                node.as_ptr().add(descriptor.payload_offset as usize),
                &mut frame.cursor,
                frame.next,
                descriptor.charge,
            )
        } else {
            let frame = &mut *dead(node);
            (
                frame.recipe,
                frame.payload,
                &mut frame.cursor,
                frame.next,
                frame.charge,
            )
        }
    }
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
