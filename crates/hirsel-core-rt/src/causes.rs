//! Prepaid task/resource cause cells. Recording, linking, delivering and dropping
//! a chain allocate nothing. Message ownership moves into the cell (FAIL-10).

use crate::{
    Capacity, Context,
    block::{self, Block},
    limit::Funding,
};
use hirsel_core_layout::charge;
use std::{
    mem::{ManuallyDrop, MaybeUninit},
    ptr,
};

pub const OVERFLOW: u8 = 0;
pub const BOUNDS: u8 = 1;
pub const DIVISION: u8 = 2;
pub const UNWRAP: u8 = 3;
pub const FAIL: u8 = 4;
pub const CANCELLED: u8 = 5;
pub const CAPACITY: u8 = 6;

/// A message is an owned emitter value. Its generated drop entry point accepts
/// the pointer and frees/refunds all of it without allocation or failure.
#[repr(C)]
pub struct Cause {
    pub kind: u8,
    pub site: u64,
    pub message: *mut u8,
    pub drop_message: Option<unsafe extern "C" fn(*mut u8)>,
}

/// An owned String carrier retained in a prepaid cause cell. The buffer keeps
/// its original payer until the delivered chain is consumed.
#[repr(C)]
pub struct StringMessage {
    pub data: *mut u8,
    pub len: u64,
    pub capacity: u64,
}
unsafe extern "C" fn drop_string(pointer: *mut u8) {
    // SAFETY: record_string points the callback at its cell-owned carrier.
    let message = unsafe { &*pointer.cast::<StringMessage>() };
    if let Some(data) = std::ptr::NonNull::new(message.data) {
        // SAFETY: the cell owns the live String buffer and its original charge.
        unsafe { block::free(data, charge::seq(message.capacity, 1).unwrap()) };
    }
}

/// Opaque native ABI handle. Its physical size is not its logical charge of 32.
pub struct Cell {
    funding: Funding,
    cause: Option<Cause>,
    next: *mut Cell,
    string: MaybeUninit<StringMessage>,
}
impl Cell {
    fn prepaid(funding: Funding) -> Box<Self> {
        Box::new(Self {
            funding,
            cause: None,
            next: ptr::null_mut(),
            string: MaybeUninit::uninit(),
        })
    }
}
impl Drop for Cell {
    fn drop(&mut self) {
        if let Some(cause) = &self.cause
            && let Some(drop_message) = cause.drop_message
        {
            // SAFETY: the used cell owns the message and its generated drop recipe.
            unsafe {
                drop_message(cause.message);
            }
        }
        self.funding.refund(charge::CAUSE_CELL);
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
            Some(cell.cause.as_ref().unwrap())
        })
    }
    pub fn into_raw(self) -> *mut Cell {
        ManuallyDrop::new(self).head
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
    cell.cause = Some(cause);
    context.causes.append(cell);
}

fn take_cell(context: &mut Context) -> Box<Cell> {
    if context.active_resource.is_null() {
        context.primary.take().unwrap()
    } else {
        // SAFETY: active_resource is the live, exclusively running finalizer's state.
        unsafe { (*context.active_resource).cell.take().unwrap() }
    }
}

/// Retain a moved String without allocating after failure or keeping a frame
/// address. Taking the prepaid cell is the same operation as primitive raise.
/// # Safety
/// The caller transfers a live String buffer, or null for an unreserved empty
/// String. It owns the current task/resource's unused cause cell.
pub unsafe fn record_string(context: &mut Context, site: u64, message: StringMessage) {
    let mut cell = take_cell(context);
    let carrier = cell.string.write(message) as *mut StringMessage;
    cell.cause = Some(Cause {
        kind: FAIL,
        site,
        message: carrier.cast(),
        drop_message: Some(drop_string),
    });
    context.causes.append(cell);
}

pub(crate) struct Resource {
    cell: Option<Box<Cell>>,
    grant: Option<Funding>,
    previous: Option<Funding>,
    previous_resource: *mut Resource,
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
    let block = Block::new(context, charge::resource(storage, grant), bytes, align)?;
    let payer = block.payer();
    let state = Box::new(Resource {
        cell: Some(Cell::prepaid(payer.clone())),
        grant: Some(payer.prepaid(grant)),
        previous: None,
        previous_resource: ptr::null_mut(),
    });
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
    resource.previous = Some(std::mem::replace(
        &mut context.current,
        resource.grant.take().unwrap(),
    ));
    // SAFETY: the state stays alive through finalizer_end and field cleanup.
    context.active_resource = unsafe { block::resource(pointer) };
    context.finalizing += 1;
}

/// # Safety
/// This resource's finalizer has returned or failed; all scopes it opened have
/// ended. The emitter next cleans fields, then calls resource_free.
pub unsafe fn finalizer_end(context: &mut Context, pointer: std::ptr::NonNull<u8>) {
    // SAFETY: this is the state installed by the matching finalizer_begin.
    let resource = unsafe { &mut *block::resource(pointer) };
    context.current.end();
    context.current = resource.previous.take().unwrap();
    context.active_resource = resource.previous_resource;
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
    unsafe { (*cell).cause.as_ref().unwrap() }
}
