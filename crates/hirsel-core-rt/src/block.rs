//! Own bytes have one native payer word. Only graph nodes reserve a resumable
//! drop frame; physical layouts are emitter operands, never logical charges.
use crate::allocator::{allocate as native_allocate, deallocate as dealloc};
use crate::{Capacity, Context, limit::Payer};
use std::alloc::Layout;
use std::ptr::NonNull;

pub const PLAIN: u8 = 0;
pub const GRAPH: u8 = 1;
pub const RESOURCE: u8 = 2;
pub const GRAPH_PREFIX_BYTES: usize = size_of::<Payer>() + size_of::<Frame>();
#[repr(C)]
struct Frame {
    size: usize,
    alignment: usize,
    offset: usize,
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
    let offset = prefix(kind).next_multiple_of(alignment);
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
    // SAFETY: live RESOURCE allocation has an initialized resource-state pointer.
    unsafe {
        payload
            .as_ptr()
            .sub(16)
            .cast::<*mut crate::causes::Resource>()
            .read()
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
        payload
            .as_ptr()
            .sub(size_of::<Payer>())
            .cast::<Payer>()
            .write(payer);
        if kind == GRAPH {
            frame(payload).write(Frame {
                size: layout.size(),
                alignment: layout.align(),
                offset,
                dead: std::mem::MaybeUninit::uninit(),
            });
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
pub(crate) unsafe fn free(payload: NonNull<u8>, charge: u64, bytes: usize, align: usize, kind: u8) {
    // SAFETY: take the live payer before matching deallocation of the original layout.
    unsafe {
        let payer = *funding(payload);
        let (layout, offset) = physical_layout(bytes, align, kind);
        payer.settle(charge);
        dealloc(payload.as_ptr().sub(offset), layout);
    }
}
/// # Safety
/// The graph node's fields have completed; its prepaid native frame is live.
pub(crate) unsafe fn free_graph(payload: NonNull<u8>, charge: u64) {
    // SAFETY: the dead frame retains the graph allocation's original physical layout.
    unsafe {
        let frame = &*frame(payload);
        let layout = Layout::from_size_align_unchecked(frame.size, frame.alignment);
        let offset = frame.offset;
        let payer = *funding(payload);
        payer.settle(charge);
        dealloc(payload.as_ptr().sub(offset), layout);
    }
}
/// Raw-byte Rust owner. Native layout operands correspond to emitter constants
/// or storage lengths; payload cleanup belongs to the emitter.
pub struct Block {
    pointer: NonNull<u8>,
    charge: u64,
    bytes: usize,
    align: usize,
    kind: u8,
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
            bytes,
            align,
            kind,
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
        Self::paid(
            context.payer(Some(self.payer())),
            charge,
            bytes,
            align,
            self.kind,
        )
    }
    pub fn into_raw(self) -> NonNull<u8> {
        let pointer = self.pointer;
        std::mem::forget(self);
        pointer
    }
    /// # Safety
    /// Exclusive live block from into_raw; every layout and charge operand is exact.
    pub unsafe fn from_raw(
        pointer: NonNull<u8>,
        charge: u64,
        bytes: usize,
        align: usize,
        kind: u8,
    ) -> Self {
        Self {
            pointer,
            charge,
            bytes,
            align,
            kind,
        }
    }
}
impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: this owner releases its cleaned storage and exact charge once.
        unsafe {
            free(self.pointer, self.charge, self.bytes, self.align, self.kind);
        }
    }
}
