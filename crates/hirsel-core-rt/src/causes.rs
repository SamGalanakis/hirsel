//! Prepaid task/resource cause cells. Recording, linking, delivering and dropping
//! a chain allocate nothing. Message ownership moves into the cell (FAIL-10).

use crate::{
    Capacity, Context,
    block::{self, Block},
    limit::{Funding, Payer},
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
pub const OUT_OF_TIME: u8 = 7;
/// FAIL-17: an activation of user code the stack allowance refused.
pub const STACK: u8 = 8;

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
        assert!(
            kind != FAIL,
            "assertion `left != right` failed: a Fail cause carries its message"
        );
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
    /// `Capacity(c)` (MEM-21): `c`'s words where `Fail`'s carrier has data and length.
    pub fn capacity(capacity: Capacity, site: u64) -> Self {
        Self {
            message: StringMessage {
                data: capacity.requested as *mut u8,
                len: capacity.available,
                capacity: crate::abi_layout::cause_kind_word(CAPACITY),
            },
            site,
        }
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
    /// The cell has no charge against its funding record.
    startup: bool,
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
        #[cfg(feature = "prof")]
        crate::mem_prof::alloc(
            (&*cell as *const Self).cast(),
            size_of::<Self>(),
            crate::mem_prof::Backing::CauseCell,
        );
        cell
    }

    /// A reservation has already moved its 32-byte charge into this payer's
    /// outstanding balance. It therefore must not call `prepaid`, which creates
    /// a child scope and transfers a parent charge.
    pub(crate) fn reserved(funding: Payer) -> Self {
        Self {
            funding,
            reserved: true,
            startup: false,
            cause: Cause::default(),
            next: ptr::null_mut(),
        }
    }

    pub(crate) fn recycle(cell: Box<Self>) -> Box<MaybeUninit<Self>> {
        // SAFETY: both boxes own the same allocation with the same layout.
        let mut storage = unsafe { Box::from_raw(Box::into_raw(cell).cast::<MaybeUninit<Self>>()) };
        // SAFETY: the cell is still initialized. Its destructor settles the
        // old payer before this allocation can be used for a new reservation.
        unsafe { storage.as_mut_ptr().drop_in_place() };
        storage
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

    #[cfg(feature = "testing")]
    pub(crate) fn exclude_startup_from_test_limit(&mut self) {
        if !self.reserved && !self.startup {
            self.funding.refund(charge::CAUSE_CELL);
            self.startup = true;
        }
    }
}
impl Drop for Cell {
    fn drop(&mut self) {
        // SAFETY: this cell owns its inline String; unused cells have capacity zero.
        unsafe { drop_string(&self.cause.message) };
        // A reservation's record still names the task that reserved it, which
        // may have ended (a caught failure's cell is dropped when its Context
        // retires): its handover returns the charge to the live limit the
        // record leads to (MEM-20).
        if self.reserved {
            self.funding.finish_reserved(charge::CAUSE_CELL);
        } else if !self.startup {
            self.funding.refund(charge::CAUSE_CELL);
        }
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::deallocated((self as *const Self).cast());
        #[cfg(feature = "prof")]
        if !self.startup && !self.reserved {
            crate::mem_prof::free((self as *const Self).cast(), size_of::<Self>());
        }
    }
}

#[derive(Default)]
pub struct Causes {
    head: *mut Cell,
    tail: *mut Cell,
}
impl Causes {
    pub(crate) fn is_empty(&self) -> bool {
        self.head.is_null()
    }
    pub(crate) fn append(&mut self, cell: Box<Cell>) {
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
    pub(crate) fn take_first(&mut self) -> Option<Box<Cell>> {
        if self.head.is_null() {
            return None;
        }
        // SAFETY: the chain owns this live first cell exclusively.
        let mut cell = unsafe { Box::from_raw(self.head) };
        self.head = cell.next;
        if self.head.is_null() {
            self.tail = ptr::null_mut();
        }
        cell.next = ptr::null_mut();
        Some(cell)
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

/// Record `Capacity(c)` for an admission the current limit refused (MEM-21);
/// every admission charges the current limit (MEM-26). The report names a
/// refusal by the root limit (PROG-3).
/// # Safety
/// As for [`record`].
pub unsafe fn record_capacity(context: &mut Context, capacity: Capacity, site: u64) {
    let root = context.current.is_root();
    let mut cell = take_cell(context);
    cell.cause = Cause::capacity(capacity, site);
    if root {
        context.root_refusal = &cell.cause;
    }
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
    creator: u64,
    previous: Option<Funding>,
    previous_resource: *mut Resource,
    /// The resource's frame while it cleans through a drain (LANG-19): its
    /// storage's recipe, the recipe's cursor, and the logical storage refunded
    /// once the recipe is done. The storage itself never holds a frame word.
    holder: std::mem::MaybeUninit<crate::counted::Holder>,
    recipe: Option<crate::counted::Recipe>,
    cursor: u64,
    storage: u64,
    payload: *mut u8,
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
        context,
        context.payer(),
        charge::resource(storage, grant),
        bytes,
        align,
        block::RESOURCE,
    )?;
    let payer = block.payer();
    let state = Box::new(Resource {
        cell: Some(Cell::prepaid(payer)),
        grant: Some(payer.prepaid(grant)),
        creator: crate::tasks::current_id(),
        previous: None,
        previous_resource: ptr::null_mut(),
        holder: std::mem::MaybeUninit::uninit(),
        recipe: None,
        cursor: 0,
        storage: 0,
        payload: ptr::null_mut(),
    });
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::allocated((&*state as *const Resource).cast(), size_of::<Resource>());
    #[cfg(feature = "prof")]
    crate::mem_prof::alloc(
        (&*state as *const Resource).cast(),
        size_of::<Resource>(),
        crate::mem_prof::Backing::Block,
    );
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
    context.current.task_owner(crate::tasks::current_id());
    context.current.bind();
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
    context.current.handover(resource.creator);
    context.current = resource.previous.take().unwrap();
    context.active_resource = resource.previous_resource;
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

/// Begin a resource's cleanup through a drain: `recipe` runs the trace hook
/// and finalizer as its step 0 and then cleans the fields, resumably; then the
/// resource is freed with `storage`, its logical field storage (LANG-19).
/// # Safety
/// The caller transfers the exclusively owned resource; the recipe is its
/// type's and lives as long as the compiled program.
pub(crate) unsafe fn resource_begin(
    pointer: std::ptr::NonNull<u8>,
    recipe: crate::counted::Recipe,
    storage: u64,
) -> *mut crate::counted::Holder {
    // SAFETY: the caller transfers the resource and so its native state.
    unsafe {
        let resource = &mut *block::resource(pointer);
        resource.recipe = Some(recipe);
        resource.cursor = 0;
        resource.storage = storage;
        resource.payload = pointer.as_ptr();
        resource.holder.as_mut_ptr()
    }
}

/// One step of a resource's cleanup: resume its recipe, and once it is done
/// free the resource (its frame goes with it).
/// # Safety
/// The holder is one resource_begin returned, linked by the running drain.
pub(crate) unsafe fn resource_step(
    context: *mut Context,
    holder: *mut crate::counted::Holder,
    list: *mut crate::counted::Worklist,
) -> bool {
    // SAFETY: the holder lives inside its resource's native state.
    unsafe {
        let resource = holder
            .cast::<u8>()
            .sub(std::mem::offset_of!(Resource, holder))
            .cast::<Resource>();
        let recipe = (*resource).recipe.expect("a begun resource");
        if !recipe(context, (*resource).payload, &mut (*resource).cursor, list) {
            return false;
        }
        let (payload, storage) = ((*resource).payload, (*resource).storage);
        resource_free(std::ptr::NonNull::new_unchecked(payload), storage);
        true
    }
}

/// Free a wholly destructured value, including unused resource funding.
/// # Safety
/// The caller owns a live block and has moved or cleaned all fields. A resource
/// has not begun finalization, and `charge` is its complete remaining charge.
pub unsafe fn free_value(context: &Context, pointer: std::ptr::NonNull<u8>, charge: u64) {
    // SAFETY: the caller owns the block and its initialized allocation metadata.
    unsafe {
        let page = crate::task_heap::page_of(pointer);
        if crate::task_heap::Page::kind(page) == block::RESOURCE {
            let resource = block::resource_state(pointer);
            if !resource.is_null() {
                free_unused_resource(pointer, resource, charge);
                return;
            }
        }
        block::free_on_page(page, pointer, charge, Some(context));
    }
}

#[cold]
#[inline(never)]
unsafe fn free_unused_resource(
    pointer: std::ptr::NonNull<u8>,
    resource: *mut Resource,
    charge: u64,
) {
    // SAFETY: the caller has exclusive custody of the resource and its grant.
    unsafe {
        let storage = charge - 32 - (*resource).grant.as_ref().unwrap().available();
        resource_free(pointer, storage);
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

#[cfg(any(test, feature = "charge-observation", feature = "prof"))]
impl Drop for Resource {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::deallocated((self as *const Self).cast());
        #[cfg(feature = "prof")]
        crate::mem_prof::free((self as *const Self).cast(), size_of::<Self>());
    }
}
