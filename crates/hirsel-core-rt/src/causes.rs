//! Prepaid task/resource cause cells. Recording, linking, delivering and dropping
//! a chain allocate nothing. Message ownership moves into the cell (FAIL-10).

use crate::{
    Capacity, Context,
    block::{self, Block},
    limit::{Funding, Payer},
};
use hirsel_core_layout::charge;
use std::{mem::ManuallyDrop, ptr};

pub const OVERFLOW: u8 = 0;
pub const BOUNDS: u8 = 1;
pub const DIVISION: u8 = 2;
pub const UNWRAP: u8 = 3;
pub const FAIL: u8 = 4;
pub const CANCELLED: u8 = 5;
pub const CAPACITY: u8 = 6;

/// Core's 32-byte Cause: `Fail`'s String at 0, then the site at 24. `CauseKind` is
/// that String's niche (FORM #75): every other kind is a capacity at or above 2^63
/// ([`crate::abi_layout::cause_kind_word`]), so the chain owns a message buffer
/// exactly when the capacity is nonzero and below 2^63.
#[derive(Default)]
#[repr(C)]
pub struct Cause {
    pub(crate) message: StringMessage,
    pub site: u64,
}
impl Cause {
    /// A cause of `kind`, which is not [`FAIL`] (a failure has a message).
    pub fn new(kind: u8, site: u64) -> Self {
        assert_ne!(kind, FAIL, "a Fail cause carries its message");
        Self {
            message: StringMessage {
                capacity: crate::abi_layout::cause_kind_word(kind),
                ..Default::default()
            },
            site,
        }
    }
    pub fn fail(message: StringMessage, site: u64) -> Self {
        Self { message, site }
    }
    pub fn kind(&self) -> u8 {
        crate::abi_layout::cause_kind(self.message.capacity)
    }
    /// `Fail`'s message.
    pub fn message(&self) -> Option<&StringMessage> {
        (self.kind() == FAIL).then_some(&self.message)
    }
}

/// An owned String carrier retained in a prepaid cause cell. The buffer keeps
/// its original payer until the delivered chain is consumed.
#[derive(Default)]
#[repr(C)]
pub struct StringMessage {
    pub data: *mut u8,
    pub len: u64,
    pub capacity: u64,
}
unsafe fn drop_string(message: &StringMessage) {
    // FORM #48: capacity alone says whether the moved carrier owns a block; one at
    // or above 2^63 is another cause kind (FORM #75).
    if message.capacity != 0 && message.capacity < crate::abi_layout::CAUSE_KIND_START {
        // SAFETY: positive capacity guarantees the transferred live owned buffer.
        unsafe {
            block::free(
                std::ptr::NonNull::new_unchecked(message.data),
                charge::seq(message.capacity, 1).unwrap(),
            )
        };
    }
}

/// Prepaid cell: core Cause followed immediately by the next-cell pointer.
/// Native funding follows that prefix; logical charge remains 32.
#[repr(C)]
pub struct Cell {
    pub(crate) cause: Cause,
    pub(crate) next: *mut Cell,
    funding: Payer,
    pub(crate) reserved: bool,
    /// The root task's startup cell is outside the program limit. Its charge
    /// remains held by the task record until context teardown.
    pub(crate) startup: bool,
}
impl Cell {
    pub(crate) fn prepaid(funding: Payer) -> Box<Self> {
        let cell = Box::new(Self {
            funding,
            reserved: false,
            startup: false,
            cause: Cause::default(),
            next: ptr::null_mut(),
        });
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::allocated((&*cell as *const Self).cast(), size_of::<Self>());
        cell
    }

    /// A reservation has already moved its 32-byte charge into this payer's
    /// outstanding balance. It therefore must not call `prepaid`, which creates
    /// a child scope and transfers a parent charge.
    pub(crate) fn reserved(funding: Payer) -> Box<Self> {
        Box::new(Self {
            funding,
            reserved: true,
            startup: false,
            cause: Cause::default(),
            next: ptr::null_mut(),
        })
    }

    pub(crate) fn startup(funding: Payer) -> Box<Self> {
        Box::new(Self {
            funding,
            reserved: false,
            startup: true,
            cause: Cause::default(),
            next: ptr::null_mut(),
        })
    }
}
impl Drop for Cell {
    fn drop(&mut self) {
        // SAFETY: this cell owns its inline String; unused cells have capacity zero.
        unsafe { drop_string(&self.cause.message) };
        if !self.startup {
            self.funding.refund(charge::CAUSE_CELL);
        }
        if self.reserved {
            self.funding.finish_reserved();
        }
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::deallocated((self as *const Self).cast());
    }
}

#[derive(Default)]
pub struct Causes {
    head: *mut Cell,
    tail: *mut Cell,
}
impl Causes {
    fn append(&mut self, cell: Box<Cell>) {
        let cell = Box::into_raw(cell);
        if self.tail.is_null() {
            self.head = cell;
        } else {
            // SAFETY: the chain exclusively owns its live final cell.
            unsafe {
                (*self.tail).next = cell;
            }
        }
        self.tail = cell;
    }
    pub fn iter(&self) -> impl Iterator<Item = &Cause> {
        let mut next = self.head;
        std::iter::from_fn(move || {
            if next.is_null() {
                return None;
            }
            // SAFETY: self owns all live cells; the iterator's borrow pins the chain.
            let cell = unsafe { &*next };
            next = cell.next;
            Some(&cell.cause)
        })
    }
    pub fn into_raw(self) -> *mut Cell {
        ManuallyDrop::new(self).head
    }

    pub(crate) fn append_chain(&mut self, other: Causes) {
        let mut other = ManuallyDrop::new(other);
        if other.head.is_null() {
            return;
        }
        if self.head.is_null() {
            self.head = other.head;
            self.tail = other.tail;
        } else {
            // SAFETY: both lists are exclusively owned and their tails are live.
            unsafe { (*self.tail).next = other.head };
            self.tail = other.tail;
        }
        other.head = ptr::null_mut();
        other.tail = ptr::null_mut();
    }
    /// # Safety
    /// The caller transfers an entire exclusively owned chain from into_raw.
    pub unsafe fn from_raw(head: *mut Cell) -> Self {
        Self {
            head,
            tail: ptr::null_mut(),
        }
    }
}
impl Drop for Causes {
    fn drop(&mut self) {
        while !self.head.is_null() {
            // SAFETY: take each exclusively owned cell before dropping it.
            let cell = unsafe { Box::from_raw(self.head) };
            self.head = cell.next;
            drop(cell);
        }
    }
}

/// Start a task at the root: its record and primary cell are one admission of
/// 288. Child task providers instead pre-admit the complete spawn and attach
/// this same storage through their custody integration.
pub fn task_start(context: &mut Context) -> Result<(), Capacity> {
    let mut record = Block::new(
        context,
        Some(charge::TASK_RECORD + charge::CAUSE_CELL),
        1,
        8,
    )?;
    let funding = record.partition(charge::CAUSE_CELL);
    context.primary = Some(Cell::prepaid(funding));
    context.task_record = Some(record);
    Ok(())
}

/// # Safety
/// `cause` transfers its message to the current task/resource's unused prepaid
/// cell. The emitter abandons the failing body and never records into that
/// obligation again. A non-finalizer failure requires a started task.
pub unsafe fn record(context: &mut Context, cause: Cause) {
    let mut cell = take_cell(context);
    cell.cause = cause;
    context.causes.append(cell);
}

fn take_cell(context: &mut Context) -> Box<Cell> {
    let cell = if context.active_resource.is_null() {
        context.primary.take().unwrap()
    } else {
        // SAFETY: active_resource is the live, exclusively running finalizer's state.
        unsafe { (*context.active_resource).cell.take().unwrap() }
    };
    if let Some(reservation) = context.isolates.last_mut()
        && reservation.obligation == context.active_resource
    {
        reservation.used = true;
    }
    cell
}

/// Retain a moved String without allocating after failure or keeping a frame
/// address. Taking the prepaid cell is the same operation as primitive raise.
/// # Safety
/// The caller transfers an owned buffer with positive capacity or a non-owning
/// carrier with capacity zero. A non-owning data pointer may be static and nonnull
/// at any length, including zero. It owns the current task/resource's unused cause cell.
pub unsafe fn record_string(context: &mut Context, site: u64, message: StringMessage) {
    // SAFETY: the caller transfers the String into its unused obligation cell.
    unsafe { record(context, Cause::fail(message, site)) };
}

pub(crate) struct Resource {
    pub(crate) cell: Option<Box<Cell>>,
    grant: Option<Funding>,
    previous: Option<Funding>,
    previous_resource: *mut Resource,
    previous_grant_depth: usize,
}

/// Admit the complete container before moving fields into it.
/// # Safety
/// The caller owns the returned container and ends it exactly once through
/// finalization or whole destructure followed by resource_free.
pub unsafe fn resource_new(
    context: &Context,
    storage: u64,
    grant: u64,
    bytes: usize,
    align: usize,
) -> Result<std::ptr::NonNull<u8>, Capacity> {
    let block = Block::paid(
        context.payer(None),
        charge::resource(storage, grant),
        bytes,
        align,
        block::RESOURCE,
    )?;
    let payer = block.payer();
    let state = Box::new(Resource {
        cell: Some(Cell::prepaid(*payer)),
        grant: Some(payer.prepaid(grant)),
        previous: None,
        previous_resource: ptr::null_mut(),
        previous_grant_depth: usize::MAX,
    });
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::allocated((&*state as *const Resource).cast(), size_of::<Resource>());
    // SAFETY: fresh resource storage receives its single prepaid metadata record.
    unsafe {
        block::set_resource(block.pointer(), Box::into_raw(state));
    }
    Ok(block.into_raw())
}

/// # Safety
/// `pointer` is a live resource whose finalizer has not started; context is its
/// task's exclusive context. Beginning a finalizer never admits or allocates.
pub unsafe fn finalizer_begin(context: &mut Context, pointer: std::ptr::NonNull<u8>) {
    // SAFETY: the emitter supplies the live resource's uniquely owned metadata.
    let resource = unsafe { &mut *block::resource(pointer) };
    resource.previous_resource = context.active_resource;
    resource.previous_grant_depth = context.grant_depth;
    context.grant_depth = context.bound_depth;
    resource.previous = Some(std::mem::replace(
        &mut context.current,
        resource.grant.take().unwrap(),
    ));
    // SAFETY: the state stays alive through finalizer_end and field cleanup.
    context.active_resource = unsafe { block::resource(pointer) };
    crate::abi_tasks::payer_lease(|| {
        context.current.drain();
        context.current.normalize();
        context.current.task_owner(crate::tasks::current_id());
        context.current.bind();
    });
    context.finalizing += 1;
}

/// # Safety
/// This resource's finalizer has returned or failed; all scopes it opened have
/// ended. The emitter next cleans fields, then calls resource_free.
pub unsafe fn finalizer_end(context: &mut Context, pointer: std::ptr::NonNull<u8>) {
    // SAFETY: this is the state installed by the matching finalizer_begin.
    let resource = unsafe { &mut *block::resource(pointer) };
    // An isolate cell installed into this obligation is funded by the active
    // grant. Return it before handing that grant back to its parent.
    if resource.cell.as_ref().is_some_and(|cell| cell.reserved) {
        drop(resource.cell.take());
    }
    context.current.handover();
    context.current = resource.previous.take().unwrap();
    context.active_resource = resource.previous_resource;
    context.grant_depth = resource.previous_grant_depth;
    context.current.normalize();
    context.current.bind();
    context.finalizing -= 1;
}

/// # Safety
/// The resource is exclusively owned; finalizer is inactive and fields have
/// been cleaned or wholly transferred. Storage is its logical field storage.
/// Whole destructure uses this directly, without finalizer_begin/end (W5).
pub unsafe fn resource_free(pointer: std::ptr::NonNull<u8>, storage: u64) {
    // SAFETY: take ownership of the resource's native metadata exactly once.
    let resource = unsafe { Box::from_raw(block::resource(pointer)) };
    if let Some(grant) = &resource.grant {
        grant.end();
    }
    drop(resource); // unused cell refunds here; a used cell belongs to Causes.
    // SAFETY: grant/cell charges are settled separately; refund all container storage.
    unsafe {
        block::free(pointer, storage);
    }
}

/// Free a wholly destructured value, including unused resource funding.
/// # Safety
/// The caller owns a live block and has moved or cleaned all fields. A resource
/// has not begun finalization, and `charge` is its complete remaining charge.
pub unsafe fn free_value(pointer: std::ptr::NonNull<u8>, charge: u64) {
    // SAFETY: the caller owns the block and its initialized allocation metadata.
    unsafe {
        let resource = block::resource(pointer);
        if resource.is_null() {
            block::free(pointer, charge);
        } else {
            let storage = charge - 32 - (*resource).grant.as_ref().unwrap().available();
            resource_free(pointer, storage);
        }
    }
}

/// # Safety
/// A live borrowed chain cell, held by an unmodified live Causes owner.
pub unsafe fn next(cell: *const Cell) -> *const Cell {
    // SAFETY: the caller keeps the chain cell live.
    unsafe { (*cell).next }
}
/// # Safety
/// A live borrowed chain cell, held by an unmodified live Causes owner.
pub unsafe fn value<'a>(cell: *const Cell) -> &'a Cause {
    // SAFETY: a delivered cell always holds an initialized cause.
    unsafe { &(*cell).cause }
}

#[cfg(any(test, feature = "charge-observation"))]
impl Drop for Resource {
    fn drop(&mut self) {
        crate::peak::deallocated((self as *const Self).cast());
    }
}
