//! Prepaid task/resource cause cells. Recording, linking, delivering and dropping
//! a chain allocate nothing. Message ownership moves into the cell (FAIL-10).
//! Loaded-release literal and source metadata are carried through an aligned
//! counted owner word at offset 40 (PACK-9.7, L2).

#[path = "causes_owner.rs"]
mod owner;
pub use crate::abi_sites::format_cell;
use owner::PAIR_TAG;
pub use owner::{
    CauseData, CauseDataRef, OwnerPair, StaticFrozen, drop_frozen_bytes, hirsel_core_cause_owner,
    hirsel_core_release_static_bytes, hirsel_core_static_frozen, hirsel_core_static_string,
};

use crate::{
    Capacity, Context,
    block::{self, Block},
    limit::{Funding, Payer},
};
use hirsel_core_layout::charge;
use std::{
    mem::{ManuallyDrop, MaybeUninit},
    ptr::{self, NonNull},
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

/// Prepaid cell (PACK-9.7):
/// - offset `0..32`: public 32-byte [`Cause`]
/// - offset `32..40`: next-cell pointer
/// - offset `40..48`: aligned 64-bit counted data-owner word (`0` on reservation
///   or immortal/moved-only; even non-zero = `*const CauseData`; odd non-zero =
///   `&self.pair | 1` for two different origins)
/// - offset `48+`: funding, prepaid pair record, and reservation flags.
/// Logical charge remains 32 (MEM-24).
#[repr(C)]
pub struct Cell {
    pub(crate) cause: Cause,
    pub(crate) next: *mut Cell,
    pub(crate) owner: usize,
    funding: Payer,
    pair: OwnerPair,
    pub(crate) reserved: bool,
    /// The cell has no charge against its funding record.
    startup: bool,
}

const _: () = {
    assert!(std::mem::offset_of!(Cell, cause) == crate::abi_layout::CAUSE_CELL_VALUE);
    assert!(std::mem::offset_of!(Cell, next) == crate::abi_layout::CAUSE_CELL_NEXT);
    assert!(std::mem::offset_of!(Cell, owner) == crate::abi_layout::CAUSE_CELL_OWNER);
    assert!(std::mem::offset_of!(Cell, funding) == 48);
    assert!(std::mem::align_of::<CauseData>() >= 8);
    assert!(std::mem::align_of::<OwnerPair>() >= 8);
};

/// Exclusive raw-pointer owner of a heap-allocated [`Cell`], avoiding `Box`
/// noalias retagging over the inline [`OwnerPair`] address stored in `owner`.
pub(crate) struct OwnedCell(NonNull<Cell>);

impl OwnedCell {
    fn alloc(cell: Cell) -> Self {
        let raw = Box::into_raw(Box::new(cell));
        // SAFETY: `Box::into_raw` is non-null.
        Self(unsafe { NonNull::new_unchecked(raw) })
    }

    pub(crate) fn as_ptr(&self) -> *mut Cell {
        self.0.as_ptr()
    }

    pub(crate) fn into_raw(self) -> *mut Cell {
        let ptr = self.0.as_ptr();
        std::mem::forget(self);
        ptr
    }

    pub(crate) unsafe fn from_raw(ptr: *mut Cell) -> Self {
        // SAFETY: caller transfers an exclusively owned `Cell` allocation.
        Self(unsafe { NonNull::new_unchecked(ptr) })
    }
}

impl std::ops::Deref for OwnedCell {
    type Target = Cell;
    fn deref(&self) -> &Cell {
        // SAFETY: `self` exclusively owns this live `Cell`.
        unsafe { self.0.as_ref() }
    }
}

impl std::ops::DerefMut for OwnedCell {
    fn deref_mut(&mut self) -> &mut Cell {
        // SAFETY: `self` exclusively owns this live `Cell`.
        unsafe { self.0.as_mut() }
    }
}

impl Drop for OwnedCell {
    fn drop(&mut self) {
        // SAFETY: `self` owns this `Box`-allocated `Cell` exclusively.
        unsafe { drop(Box::from_raw(self.0.as_ptr())) };
    }
}

/// Reusable uninitialized heap storage for an isolate's reserved [`Cell`].
pub(crate) struct ReservedStorage(NonNull<MaybeUninit<Cell>>);

impl ReservedStorage {
    pub(crate) fn new_uninit() -> Self {
        let raw = Box::into_raw(Box::new_uninit());
        // SAFETY: `Box::into_raw` is non-null.
        Self(unsafe { NonNull::new_unchecked(raw) })
    }

    pub(crate) fn write(self, cell: Cell) -> OwnedCell {
        let ptr = self.0.as_ptr().cast::<Cell>();
        std::mem::forget(self);
        // SAFETY: `ptr` is valid, aligned, exclusively owned uninitialized `Cell` storage.
        unsafe { ptr.write(cell) };
        // SAFETY: `ptr` is non-null and now initialized.
        OwnedCell(unsafe { NonNull::new_unchecked(ptr) })
    }
}

impl Drop for ReservedStorage {
    fn drop(&mut self) {
        // SAFETY: `self` owns this `Box`-allocated `MaybeUninit<Cell>` exclusively.
        unsafe { drop(Box::from_raw(self.0.as_ptr())) };
    }
}

impl Cell {
    pub(crate) fn prepaid(funding: Payer) -> OwnedCell {
        let cell = OwnedCell::alloc(Self {
            cause: Cause::default(),
            next: ptr::null_mut(),
            owner: 0,
            funding,
            pair: OwnerPair::EMPTY,
            reserved: false,
            startup: false,
        });
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::allocated(cell.as_ptr().cast(), size_of::<Self>());
        #[cfg(feature = "prof")]
        crate::mem_prof::alloc(
            cell.as_ptr().cast(),
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
            cause: Cause::default(),
            next: ptr::null_mut(),
            owner: 0,
            funding,
            pair: OwnerPair::EMPTY,
            reserved: true,
            startup: false,
        }
    }

    pub(crate) fn startup(funding: Payer) -> OwnedCell {
        OwnedCell::alloc(Self {
            cause: Cause::default(),
            next: ptr::null_mut(),
            owner: 0,
            funding,
            pair: OwnerPair::EMPTY,
            reserved: false,
            startup: true,
        })
    }

    #[cfg(feature = "testing")]
    pub(crate) fn exclude_startup_from_test_limit(&mut self) {
        if !self.reserved && !self.startup {
            self.funding.refund(charge::CAUSE_CELL);
            self.startup = true;
        }
    }

    /// The raw aligned owner word at offset 40.
    #[inline]
    pub fn owner_word(&self) -> usize {
        self.owner
    }

    /// The public 32-byte cause value.
    #[inline]
    pub fn cause(&self) -> &Cause {
        &self.cause
    }

    /// The data owner supplying `self.cause.site`'s source directory, or null
    /// for process-lifetime/host sites.
    #[inline]
    pub fn site_owner(&self) -> *const CauseData {
        if self.owner == 0 {
            ptr::null()
        } else if self.owner & PAIR_TAG != 0 {
            self.pair.site
        } else {
            std::ptr::with_exposed_provenance(self.owner)
        }
    }

    /// The data owner backing a static `Fail` literal in `self.cause.message`,
    /// or null for moved heap messages, non-`Fail` causes, or host literals.
    #[inline]
    pub fn literal_owner(&self) -> *const CauseData {
        if self.owner == 0 {
            ptr::null()
        } else if self.owner & PAIR_TAG != 0 {
            self.pair.literal
        } else if self.cause.kind() == FAIL
            && self.cause.message.capacity == 0
            && self.cause.message.len != 0
        {
            std::ptr::with_exposed_provenance(self.owner)
        } else {
            ptr::null()
        }
    }

    /// Detached owned copy of a `Fail` message's bytes (PACK-9.7: detached
    /// copies own their backing, with zero truncation).
    pub fn detached_message_bytes(&self) -> Option<Vec<u8>> {
        let msg = self.cause.message()?;
        if msg.len == 0 {
            return Some(Vec::new());
        }
        // SAFETY: live cell keeps either the moved buffer, CauseData R/NX bytes,
        // or immortal host literal bytes alive for `msg.len`.
        Some(unsafe { std::slice::from_raw_parts(msg.data, msg.len as usize) }.to_vec())
    }

    /// Install `(site_owner, literal_owner)` into a fresh recorded `cell`
    /// without allocating or admitting. `site_retained` / `literal_retained`
    /// say whether the caller is already transferring one owned count on that
    /// pointer; otherwise this retains it.
    ///
    /// # Safety
    /// `cell` is a live, exclusively owned cell with `owner == 0`. Non-null
    /// owners must point to live [`CauseData`] allocations.
    pub(crate) unsafe fn install_owners(
        cell: *mut Cell,
        site_owner: *const CauseData,
        literal_owner: *const CauseData,
        site_retained: bool,
        literal_retained: bool,
    ) {
        // SAFETY: `cell` is exclusively owned and live; owners are live when non-null.
        unsafe {
            debug_assert_eq!((*cell).owner, 0);
            match (site_owner.is_null(), literal_owner.is_null()) {
                (true, true) => {
                    (*cell).owner = 0;
                    (*cell).pair = OwnerPair::EMPTY;
                }
                (false, true) => {
                    if !site_retained {
                        CauseData::retain(site_owner);
                    }
                    (*cell).owner = site_owner.expose_provenance();
                    (*cell).pair = OwnerPair::EMPTY;
                }
                (false, false) if std::ptr::eq(site_owner, literal_owner) => {
                    // Single origin: hold exactly one count on the data owner.
                    match (site_retained, literal_retained) {
                        (false, false) => CauseData::retain(site_owner),
                        (true, true) => CauseData::release(literal_owner),
                        (true, false) | (false, true) => {}
                    }
                    (*cell).owner = site_owner.expose_provenance();
                    (*cell).pair = OwnerPair::EMPTY;
                }
                _ => {
                    // Two distinct origins (or host site + unloadable literal):
                    // record the prepaid inline pair `(site_owner, literal_owner)`.
                    if !site_owner.is_null() && !site_retained {
                        CauseData::retain(site_owner);
                    }
                    if !literal_owner.is_null() && !literal_retained {
                        CauseData::retain(literal_owner);
                    }
                    (*cell).pair = OwnerPair {
                        site: site_owner,
                        literal: literal_owner,
                    };
                    let pair_addr = (&raw const (*cell).pair).expose_provenance();
                    debug_assert_eq!(pair_addr & PAIR_TAG, 0);
                    (*cell).owner = pair_addr | PAIR_TAG;
                }
            }
        }
    }

    pub(crate) unsafe fn release_owner(&mut self) {
        let word = self.owner;
        if word == 0 {
            return;
        }
        self.owner = 0;
        if word & PAIR_TAG != 0 {
            debug_assert_eq!(word & !PAIR_TAG, (&raw const self.pair).addr());
            let pair = self.pair;
            self.pair = OwnerPair::EMPTY;
            // SAFETY: cell owned one count on each non-null pointer in `pair`.
            unsafe {
                if !pair.site.is_null() {
                    CauseData::release(pair.site);
                }
                if !pair.literal.is_null() {
                    CauseData::release(pair.literal);
                }
            }
        } else {
            let owner: *const CauseData = std::ptr::with_exposed_provenance(word);
            // SAFETY: cell owned one count on `owner`.
            unsafe { CauseData::release(owner) };
        }
    }
}
impl Drop for Cell {
    fn drop(&mut self) {
        // PACK-9.7 destruction order: clean message -> release owner (both
        // counts for a pair) -> settle original cell funding.
        // SAFETY: this cell owns its inline String; unused cells have capacity zero.
        unsafe { drop_string(&self.cause.message) };
        // SAFETY: this cell exclusively owns its counted data-owner word.
        unsafe { self.release_owner() };
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
    pub(crate) fn append(&mut self, cell: OwnedCell) {
        let cell = cell.into_raw();
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
        self.iter_cells().map(|cell| &cell.cause)
    }
    pub fn iter_cells(&self) -> impl Iterator<Item = &Cell> {
        let mut next = self.head;
        std::iter::from_fn(move || {
            if next.is_null() {
                return None;
            }
            // SAFETY: self owns all live cells; the iterator's borrow pins the chain.
            let cell = unsafe { &*next };
            next = cell.next;
            Some(cell)
        })
    }
    pub(crate) fn take_first(&mut self) -> Option<OwnedCell> {
        if self.head.is_null() {
            return None;
        }
        // SAFETY: the chain owns this live first cell exclusively.
        let mut cell = unsafe { OwnedCell::from_raw(self.head) };
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
        let mut tail = head;
        if !tail.is_null() {
            // SAFETY: caller transferred a valid null-terminated chain.
            unsafe {
                while !(*tail).next.is_null() {
                    tail = (*tail).next;
                }
            }
        }
        Self { head, tail }
    }
}
impl Drop for Causes {
    fn drop(&mut self) {
        while !self.head.is_null() {
            // SAFETY: take each exclusively owned cell before dropping it.
            let cell = unsafe { OwnedCell::from_raw(self.head) };
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
pub unsafe fn record(context: &mut Context, mut cause: Cause) {
    let site_owner = if cause.site == 0 {
        ptr::null()
    } else {
        context.site_owner
    };
    let (literal_owner, literal_retained) = if cause.kind() == FAIL && cause.message.capacity == 0 {
        if cause.message.len == 0 {
            if let Some(owner) = CauseData::find_literal_owner(cause.message.data) {
                // SAFETY: a truncated-to-empty loaded static string transfers its CauseData count.
                unsafe { CauseData::release(owner) };
            }
            cause.message.data = ptr::null_mut();
            (ptr::null(), false)
        } else if let Some(hit) =
            crate::code::locate_literal(cause.message.data.addr(), cause.message.len as usize)
        {
            cause.message.data = hit.relocated.cast_mut();
            (hit.owner, true)
        } else if let Some(owner) = CauseData::find_literal_owner(cause.message.data) {
            // Owned loaded static String already holds +1 count on `owner`; move it into the cell.
            (owner, true)
        } else {
            (ptr::null(), false)
        }
    } else {
        (ptr::null(), false)
    };
    let cell = take_cell(context);
    let raw = cell.as_ptr();
    // SAFETY: `raw` is exclusively owned, unused, and `site_owner`/`literal_owner`
    // are pinned by the active execution or already retained by `locate_literal`.
    unsafe {
        (*raw).cause = cause;
        Cell::install_owners(raw, site_owner, literal_owner, false, literal_retained);
    }
    context.causes.append(cell);
}

/// Record a cause with explicit site and literal [`CauseData`] owners.
/// Allocates and admits nothing.
/// # Safety
/// Same as [`record`]; any non-null owner must point to a live [`CauseData`].
pub unsafe fn record_with_owners(
    context: &mut Context,
    mut cause: Cause,
    site_owner: *const CauseData,
    literal_owner: *const CauseData,
) {
    let site_owner = if cause.site == 0 {
        ptr::null()
    } else {
        site_owner
    };
    let literal_owner = if cause.kind() == FAIL && cause.message.capacity == 0 {
        if cause.message.len == 0 {
            cause.message.data = ptr::null_mut();
            ptr::null()
        } else {
            literal_owner
        }
    } else {
        ptr::null()
    };
    let cell = take_cell(context);
    let raw = cell.as_ptr();
    // SAFETY: `raw` is exclusively owned and unused; non-null owners are live.
    unsafe {
        (*raw).cause = cause;
        Cell::install_owners(raw, site_owner, literal_owner, false, false);
    }
    context.causes.append(cell);
}

/// Record `Capacity(c)` for an admission the current limit refused (MEM-21);
/// every admission charges the current limit (MEM-26). The report names a
/// refusal by the root limit (PROG-3).
/// # Safety
/// As for [`record`].
pub unsafe fn record_capacity(context: &mut Context, capacity: Capacity, site: u64) {
    let root = context.current.is_root();
    let site_owner = if site == 0 {
        ptr::null()
    } else {
        context.site_owner
    };
    let cell = take_cell(context);
    let raw = cell.as_ptr();
    // SAFETY: `raw` is exclusively owned and unused; `site_owner` is pinned.
    unsafe {
        (*raw).cause = Cause::capacity(capacity, site);
        Cell::install_owners(raw, site_owner, ptr::null(), false, false);
        if root {
            context.root_refusal = &raw const (*raw).cause;
        }
    }
    context.causes.append(cell);
}

fn take_cell(context: &mut Context) -> OwnedCell {
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
    pub(crate) cell: Option<OwnedCell>,
    grant: Option<Funding>,
    creator: u64,
    previous: Option<Funding>,
    previous_resource: *mut Resource,
    /// The resource's frame while it cleans through a drain (LANG-19): its
    /// storage's recipe, the recipe's cursor, and the logical storage refunded
    /// once the recipe is done. The storage itself never holds a frame word.
    holder: std::mem::MaybeUninit<crate::counted::Holder>,
    origin: Option<crate::code::ExecutionPin>,
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
        origin: None,
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

/// Associate the construction image's recipe with this resource before publishing it.
///
/// # Safety
/// The caller owns a freshly admitted resource and supplies its storage recipe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_resource_origin(
    pointer: std::ptr::NonNull<u8>,
    recipe: crate::counted::Recipe,
) {
    // SAFETY: the freshly admitted resource owns its unpublished state.
    let resource = unsafe { &mut *block::resource_state(pointer) };
    resource.recipe = Some(recipe);
    resource.origin = crate::code::ExecutionPin::pin_pc(None, recipe as usize);
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
        resource.recipe.get_or_insert(recipe);
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
