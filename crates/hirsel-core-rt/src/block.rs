//! Own allocations. Native metadata is outside the emitter-visible payload.

use crate::{Capacity, Context, limit::Funding};
use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::ptr::{self, NonNull};

struct Header {
    funding: Funding,
    layout: Layout,
    dead: std::mem::MaybeUninit<Dead>,
    resource: *mut crate::causes::Resource,
}

pub(crate) struct Dead {
    pub(crate) next: Option<NonNull<u8>>,
    pub(crate) charge: u64,
    pub(crate) recipe: crate::counted::Recipe,
    pub(crate) payload_offset: usize,
    pub(crate) cursor: u64,
}

unsafe fn header(payload: NonNull<u8>) -> *mut Header {
    // SAFETY: caller pins a live block's initialized backpointer.
    unsafe {
        payload
            .as_ptr()
            .sub(size_of::<*mut Header>())
            .cast::<*mut Header>()
            .read()
    }
}

pub(crate) unsafe fn set_resource(payload: NonNull<u8>, resource: *mut crate::causes::Resource) {
    // SAFETY: caller exclusively initializes a fresh resource block's native state.
    unsafe {
        (*header(payload)).resource = resource;
    }
}
pub(crate) unsafe fn resource(payload: NonNull<u8>) -> *mut crate::causes::Resource {
    // SAFETY: caller pins a live resource whose metadata was installed once.
    unsafe { (*header(payload)).resource }
}

pub(crate) unsafe fn push_dead(
    list: &mut crate::counted::Worklist,
    payload: NonNull<u8>,
    charge: u64,
    recipe: crate::counted::Recipe,
    payload_offset: usize,
) {
    // SAFETY: the caller exclusively owns this dead allocation; its native
    // prefix supplies worklist space without touching initialized payload bytes.
    unsafe {
        (*header(payload)).dead.write(Dead {
            next: list.head,
            charge,
            recipe,
            payload_offset,
            cursor: 0,
        });
    }
    list.head = Some(payload);
}
pub(crate) unsafe fn dead(payload: NonNull<u8>) -> *mut Dead {
    // SAFETY: push_dead initialized the frame in this exclusively owned node.
    unsafe { (*header(payload)).dead.as_mut_ptr() }
}

fn physical_layout(bytes: usize, align: usize) -> (Layout, usize) {
    let prefix = Layout::from_size_align(
        size_of::<Header>() + size_of::<*mut Header>(),
        align_of::<Header>(),
    )
    .unwrap();
    let payload = Layout::from_size_align(bytes.max(1), align.max(align_of::<*mut Header>()))
        .unwrap_or_else(|_| std::process::abort());
    prefix
        .extend(payload)
        .map(|(l, n)| (l.pad_to_align(), n))
        .unwrap_or_else(|_| std::process::abort())
}

pub(crate) fn allocate(
    payer: Funding,
    charge: Option<u64>,
    bytes: usize,
    align: usize,
) -> Result<NonNull<u8>, Capacity> {
    payer.admit(charge)?;
    let (layout, offset) = physical_layout(bytes, align);
    // SAFETY: layout is nonzero; the new allocation owns its header and backpointer.
    unsafe {
        let base = NonNull::new(alloc(layout)).unwrap_or_else(|| handle_alloc_error(layout));
        let header = base.cast::<Header>().as_ptr();
        ptr::write(
            header,
            Header {
                funding: payer,
                layout,
                dead: std::mem::MaybeUninit::uninit(),
                resource: ptr::null_mut(),
            },
        );
        let payload = base.as_ptr().add(offset);
        payload
            .sub(size_of::<*mut Header>())
            .cast::<*mut Header>()
            .write(header);
        Ok(NonNull::new_unchecked(payload))
    }
}

/// # Safety
/// `payload` is a live allocation from this module, not a region pointer.
pub(crate) unsafe fn funding<'a>(payload: NonNull<u8>) -> &'a Funding {
    // SAFETY: the allocation's backpointer and header remain initialized until free.
    unsafe {
        &(*payload
            .as_ptr()
            .sub(size_of::<*mut Header>())
            .cast::<*mut Header>()
            .read())
        .funding
    }
}

/// # Safety
/// The caller owns the allocation exclusively, its payload has been moved or
/// cleaned, and `charge` is its remaining logical charge, refunded exactly once.
pub(crate) unsafe fn free(payload: NonNull<u8>, charge: u64) {
    // SAFETY: recover the live header before deallocating its complete original layout.
    unsafe {
        let header = payload
            .as_ptr()
            .sub(size_of::<*mut Header>())
            .cast::<*mut Header>()
            .read();
        let Header {
            funding, layout, ..
        } = header.read();
        funding.refund(charge);
        dealloc(header.cast(), layout);
    }
}

/// A Rust owner for raw bytes; payload cleanup is the emitter's responsibility.
pub struct Block {
    pointer: NonNull<u8>,
    charge: u64,
}
impl Block {
    pub fn new(
        context: &Context,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        Self::paid(context.payer(None), charge, bytes, align)
    }
    pub(crate) fn paid(
        payer: Funding,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        let pointer = allocate(payer, charge, bytes, align)?;
        Ok(Self {
            pointer,
            charge: charge.unwrap(),
        })
    }
    pub fn pointer(&self) -> NonNull<u8> {
        self.pointer
    }
    pub fn charge(&self) -> u64 {
        self.charge
    }
    /// Transfer an admitted subcharge to a separately owned prepaid cell.
    pub(crate) fn partition(&mut self, bytes: u64) -> Funding {
        self.charge -= bytes;
        self.payer().clone()
    }
    pub(crate) fn payer(&self) -> &Funding {
        // SAFETY: self owns the live block for this borrow.
        unsafe { funding(self.pointer) }
    }
    /// Allocate replacement storage while the original is still charged.
    /// The emitter copies the initialized prefix, installs it, then drops the old block.
    pub fn replacement(
        &self,
        context: &Context,
        charge: Option<u64>,
        bytes: usize,
        align: usize,
    ) -> Result<Self, Capacity> {
        Self::paid(context.payer(Some(self.payer())), charge, bytes, align)
    }
    pub fn into_raw(self) -> NonNull<u8> {
        let pointer = self.pointer;
        std::mem::forget(self);
        pointer
    }
    /// # Safety
    /// The pointer is exclusively owned, came from `into_raw`, and the charge is exact.
    pub unsafe fn from_raw(pointer: NonNull<u8>, charge: u64) -> Self {
        Self { pointer, charge }
    }
}
impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: this owner releases its allocation and authoritative charge once.
        unsafe {
            free(self.pointer, self.charge);
        }
    }
}
