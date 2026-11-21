//! Units and connections in the language (CONC-15, CONC-17): the entry points
//! the emitters call for `conn.*` and `task.spawn_unit`, over the unit ledger.
//!
//! Each run of a task scheduler is one unit. Its ledger context (`LangUnit`)
//! lives beside the scheduler; the unit's port readies the scheduler, which
//! takes every arrival at a scheduler turn (`pump`) and wakes the tasks that
//! wait on it. A helper unit is a run of its own on its own OS thread.
//!
//! Byte accounting has one authority: the program's limits (`limit.rs`).
//! A connection's or helper's charge is held as a block admitted from the
//! creating task's limit. The ledger keeps the unit graph (ports, connection
//! words, room balances, settlements, helper ends) with roots it never refuses
//! from; a ledger refusal after the program admitted is a bug (`UnitLedger`).
//!
//! An item crosses by copy. Its data is the item's inline bytes followed by
//! the content of each flat owner in it (a `List` of plain elements, `String`),
//! at length capacity. The item's charge, which the room holds, is the sum of
//! those owners' charges. Clients inside an item move as handles.
use crate::{
    Capacity, Context,
    block::{self, Block},
    causes::{self, Cause, Causes, Cell},
    tasks::{self, Body, Readies, TaskLayout, Waiters},
    units::ledger::conn::{Client, Inbox, MAX_CAP, MAX_ROOM, Received, Sent},
    units::ledger::{ItemBuf, Took, UnitCtx, Value},
};
use std::{collections::HashMap, ptr, sync::Arc};

/// Keyed by addresses and slot ids; a fixed seed keeps lookups cheap.
type Map<K, V> = HashMap<K, V, foldhash::fast::FixedState>;
fn map<K, V>() -> Map<K, V> {
    HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0))
}

/// The ledger roots never refuse: the program's limits decide every admission.
const UNREFUSED: u64 = 1 << 62;
/// A seq owner: pointer, length, capacity.
const SEQ: u64 = 0;
/// A client handle.
const CLIENT: u64 = 1;

/// The owners of a `cross` type, as the emitter writes them: per owner
/// `[offset, kind, element bytes, element logical size]`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Cross {
    pub bytes: u64,
    pub owners: *const [u64; 4],
    pub count: u64,
}

impl Cross {
    fn owners(&self) -> &[[u64; 4]] {
        if self.count == 0 {
            return &[];
        }
        // SAFETY: the emitter's descriptor holds `count` rows for this call.
        unsafe { std::slice::from_raw_parts(self.owners, self.count as usize) }
    }
    fn owned(&self) -> Vec<[u64; 4]> {
        self.owners().to_vec()
    }
}

fn round8(n: u64) -> u64 {
    n.div_ceil(8) * 8
}

/// The charge of a seq of `len` elements at length capacity.
fn seq_charge(len: u64, logical: u64) -> u64 {
    round8(len * logical)
}

unsafe fn word(at: *const u8) -> u64 {
    // SAFETY: the caller's value holds a word here.
    unsafe { at.cast::<u64>().read_unaligned() }
}
unsafe fn set_word(at: *mut u8, v: u64) {
    // SAFETY: as `word`.
    unsafe { at.cast::<u64>().write_unaligned(v) }
}

/// The charge `x` carries across: its owners' content at length capacity.
unsafe fn item_charge(value: *const u8, owners: &[[u64; 4]]) -> u64 {
    let mut total = 0u64;
    for o in owners {
        if o[1] == SEQ {
            // SAFETY: the owner's length word is at offset 8.
            let len = unsafe { word(value.add(o[0] as usize + 8)) };
            total += seq_charge(len, o[3]);
        }
    }
    total
}

/// Copy `value` and its owners' content into one buffer.
unsafe fn pack(value: *const u8, bytes: u64, owners: &[[u64; 4]]) -> Vec<u8> {
    let mut data = Vec::new();
    // SAFETY: as `pack_into`.
    unsafe { pack_into(&mut data, value, bytes, owners) };
    data
}

/// `pack` into `data`, reusing its buffer.
unsafe fn pack_into(data: &mut Vec<u8>, value: *const u8, bytes: u64, owners: &[[u64; 4]]) {
    // SAFETY: `value` is an initialized item of `bytes` bytes.
    unsafe {
        let content = |o: &[u64; 4]| {
            let at = value.add(o[0] as usize);
            (word(at) as *const u8, (word(at.add(8)) * o[2]) as usize)
        };
        let seqs = owners.iter().filter(|o| o[1] == SEQ);
        data.clear();
        data.reserve(bytes as usize + seqs.clone().map(|o| content(o).1).sum::<usize>());
        data.extend_from_slice(std::slice::from_raw_parts(value, bytes as usize));
        for o in seqs {
            let (p, n) = content(o);
            if n > 0 {
                data.extend_from_slice(std::slice::from_raw_parts(p, n));
            }
        }
    }
}

/// Item buffers a unit keeps for its next sends: a received item's buffer
/// goes back here, so a steady exchange allocates none. Only small buffers
/// are kept, so the spares stay under 64 × 4 KiB of runtime memory.
const SPARE_ITEMS: usize = 64;
const SPARE_BYTES: usize = 4096;

/// Free the seq buffers of a value whose content has been copied out; its
/// clients have moved and are not touched.
unsafe fn free_owners(value: *const u8, owners: &[[u64; 4]]) {
    // SAFETY: the value owns each nonempty buffer, charged at its capacity.
    unsafe {
        for o in owners {
            if o[1] == SEQ {
                let at = value.add(o[0] as usize);
                let cap = word(at.add(16));
                if cap != 0 {
                    let p = word(at) as *mut u8;
                    block::free(ptr::NonNull::new_unchecked(p), round8(cap * o[3]));
                }
            }
        }
    }
}

/// Rebuild an item from `data` into `dest`: each seq gets a fresh buffer at
/// length capacity from `payer`, whose charge was admitted by the caller.
unsafe fn unpack(
    context: *mut Context,
    payer: crate::limit::Payer,
    data: &[u8],
    dest: *mut u8,
    bytes: u64,
    owners: &[[u64; 4]],
) {
    // SAFETY: `dest` is writable for `bytes`; `data` was made by `pack` for
    // the same layout.
    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), dest, bytes as usize);
        let mut at = bytes as usize;
        for o in owners {
            if o[1] != SEQ {
                continue;
            }
            let slot = dest.add(o[0] as usize);
            let len = word(slot.add(8));
            let n = (len * o[2]) as usize;
            if len == 0 {
                set_word(slot, 0);
                set_word(slot.add(16), 0);
                continue;
            }
            let p = block::allocate_admitted(
                &*context,
                payer,
                n,
                o[2].clamp(1, 16) as usize,
                crate::abi_layout::NATIVE_PLAIN,
            );
            ptr::copy_nonoverlapping(data.as_ptr().add(at), p.as_ptr(), n);
            at += n;
            set_word(slot, p.as_ptr() as u64);
            set_word(slot.add(16), len);
        }
    }
}

/// Drop the clients inside a packed item that no one received.
unsafe fn drop_packed_clients(unit: &mut LangUnit, data: &[u8], owners: &[[u64; 4]]) {
    for o in owners {
        if o[1] == CLIENT {
            // SAFETY: `pack` copied the inline handle word.
            let h = unsafe { word(data.as_ptr().add(o[0] as usize)) } as *mut ClientH;
            if !h.is_null() {
                // SAFETY: the item owned this client.
                unit.ctx.drop_client(unsafe { Box::from_raw(h) }.client);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The unit beside a scheduler
// ---------------------------------------------------------------------------

/// A task waiting on one waiting send's settlement.
struct SendWait {
    waiters: Waiters,
    settled: Option<Took>,
}

pub(crate) struct LangUnit {
    pub(crate) ctx: Box<UnitCtx>,
    inboxes: Map<usize, *mut InboxH>,
    sends: Map<u32, *mut SendWait>,
    helpers: Map<usize, *mut HelperH>,
    /// Header and unused room of cleaned connections, until the last release.
    retained: Map<usize, Block>,
    /// Tasks waiting on something another unit settles: the scheduler does
    /// not call such a wait a deadlock.
    pub(crate) waits: usize,
    /// The run's root task, cancelled by a cancel arrival (helpers).
    pub(crate) root: *mut tasks::Task,
    helper: Option<Arc<HelperShared>>,
    spare: Vec<ItemBuf>,
}

impl LangUnit {
    fn new(
        ctx: Box<UnitCtx>,
        readies: &Arc<Readies>,
        helper: Option<Arc<HelperShared>>,
    ) -> Box<Self> {
        ctx.port
            .set_waker(Arc::into_raw(readies.clone()) as *mut ());
        ctx.port.arm();
        Box::new(LangUnit {
            ctx,
            inboxes: map(),
            sends: map(),
            helpers: map(),
            retained: map(),
            waits: 0,
            spare: Vec::new(),
            root: ptr::null_mut(),
            helper,
        })
    }
}

impl Drop for LangUnit {
    fn drop(&mut self) {
        if self.helper.is_none() {
            self.ctx.end_root();
        }
    }
}

/// A publish to an armed port readies its scheduler.
pub(crate) fn port_ready(w: *mut ()) {
    // SAFETY: the port holds one count of this scheduler's waiter.
    unsafe { (*(w as *const Readies)).ready_port() }
}

pub(crate) fn port_waker_drop(w: *mut ()) {
    // SAFETY: made by Arc::into_raw in `LangUnit::new`.
    drop(unsafe { Arc::from_raw(w as *const Readies) });
}

/// A helper about to start its run, and where the run's unit is handed back.
type Pending = Option<(Box<UnitCtx>, Arc<HelperShared>, *mut *mut LangUnit)>;
std::thread_local! {
    /// The helper unit this thread is about to run, for its `run_root`. Set
    /// and taken on one thread before the run's first switch, so a pool unit
    /// that later resumes on another worker never reads it again.
    static PENDING: std::cell::RefCell<Pending> = const { std::cell::RefCell::new(None) };
}

/// For `run_root`: the helper unit this thread is starting, bound to the
/// run's scheduler, or null (borrowed: the helper's body owns it).
pub(crate) fn adopt(readies: &Arc<Readies>) -> *mut LangUnit {
    let Some((ctx, shared, out)) = PENDING.with(|p| p.borrow_mut().take()) else {
        return ptr::null_mut();
    };
    let u = Box::into_raw(LangUnit::new(ctx, readies, Some(shared)));
    // SAFETY: `out` is on the helper body's own stack, live across its run.
    unsafe { *out = u };
    u
}

/// A root unit, made at the first unit operation of a run that is not a helper.
pub(crate) fn root_unit(readies: &Arc<Readies>) -> *mut LangUnit {
    Box::into_raw(LangUnit::new(
        Box::new(UnitCtx::root(UNREFUSED)),
        readies,
        None,
    ))
}

/// # Safety
/// The scheduler owns this root unit and no task runs.
pub(crate) unsafe fn drop_root_unit(u: *mut LangUnit) {
    // SAFETY: made by `root_unit`.
    drop(unsafe { Box::from_raw(u) });
}

/// Take every arrival and wake what waits on it; then arm the port so the
/// next publish readies the scheduler. Runs in a commit.
pub(crate) unsafe fn pump(u: &mut LangUnit) {
    loop {
        while let Some((took, _a)) = u.ctx.take() {
            let id = u.ctx.took_ident;
            // SAFETY: every registered frame and handle is pinned while registered.
            unsafe {
                match took {
                    Took::Item | Took::Close | Took::SealAnswer => {
                        if let Some(h) = u.inboxes.get(&id) {
                            Waiters::wake_all(&raw mut (**h).waiters);
                        }
                    }
                    Took::Accepted(s) | Took::Closed(s) | Took::Withdrawn(s) => {
                        if let Took::Closed(s) = took
                            && let Some(v) = u.ctx.closed_value(s)
                        {
                            u.ctx.drop_value(v);
                        }
                        if let Some(w) = u.sends.remove(&s) {
                            (*w).settled = Some(took);
                            Waiters::wake_all(&raw mut (*w).waiters);
                        }
                    }
                    Took::HelperEnd => {
                        if let Some(h) = u.helpers.get(&id) {
                            (**h).ended = true;
                            Waiters::wake_all(&raw mut (**h).waiters);
                        }
                    }
                    Took::LastRelease => {
                        u.retained.remove(&id);
                    }
                    Took::Credits => {}
                    Took::Cancel => {
                        if !u.root.is_null() {
                            tasks::hirsel_core_cancel(ptr::null_mut(), u.root);
                        }
                    }
                }
            }
        }
        if u.ctx.port.arm() {
            return;
        }
    }
}

/// # Safety
/// A task of this run is running.
unsafe fn unit() -> &'static mut LangUnit {
    // SAFETY: the scheduler pins its unit for the run.
    unsafe { &mut *tasks::lang_unit() }
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

pub struct InboxH {
    inbox: Inbox,
    waiters: Waiters,
    owners: Vec<[u64; 4]>,
    charge: Option<Block>,
}

pub struct ClientH {
    client: Client,
}

#[repr(C)]
pub struct ConnNew {
    pub client: *mut ClientH,
    pub inbox: *mut InboxH,
    pub available: u64,
    /// 0 made or Capacity (null handles), 1 failed (`Bounds` recorded).
    pub status: u8,
}

/// `conn.new[T](cap, bytes b)`: admits `64 + cap × size(T) + b`.
/// # Safety
/// Live task context; `cross` is T's descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_new(
    context: *mut Context,
    cap: u64,
    b: u64,
    item: *const Cross,
    logical: u64,
    site: u64,
) -> ConnNew {
    // SAFETY: the task owns its context; the descriptor lives for the call.
    unsafe {
        let refused = |available| ConnNew {
            client: ptr::null_mut(),
            inbox: ptr::null_mut(),
            available,
            status: 0,
        };
        if cap == 0 {
            causes::record(&mut *context, Cause::new(causes::BOUNDS, site));
            return ConnNew {
                client: ptr::null_mut(),
                inbox: ptr::null_mut(),
                available: 0,
                status: 1,
            };
        }
        let charge = cap
            .checked_mul(logical)
            .and_then(|s| s.checked_add(64))
            .and_then(|s| s.checked_add(b));
        // A connection the connection word cannot hold is refused as one no
        // limit admits.
        let Some(charge) = charge.filter(|_| cap <= MAX_CAP && b <= MAX_ROOM) else {
            return refused(0);
        };
        let held = match Block::new(&*context, Some(charge), 1, 8) {
            Ok(held) => held,
            Err(e) => return refused(e.available),
        };
        let _commit = tasks::before_commit();
        let u = unit();
        let (inbox, client) = u
            .ctx
            .conn_new(cap, logical, b)
            .unwrap_or_else(|_| crate::process::stop(b"UnitLedger"));
        let key = Arc::as_ptr(&inbox.0) as usize;
        let ih = Box::into_raw(Box::new(InboxH {
            inbox,
            waiters: Waiters::default(),
            owners: (*item).owned(),
            charge: Some(held),
        }));
        u.inboxes.insert(key, ih);
        ConnNew {
            client: Box::into_raw(Box::new(ClientH { client })),
            inbox: ih,
            available: 0,
            status: 0,
        }
    }
}

/// `conn.client(c)`: one more client; admits nothing.
/// # Safety
/// A live client borrow.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_client(client: *mut ClientH) -> *mut ClientH {
    // SAFETY: the borrow pins the client.
    unsafe {
        Box::into_raw(Box::new(ClientH {
            client: (*client).client.clone_client(),
        }))
    }
}

/// # Safety
/// Transfers the client.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_client_drop(_context: *mut Context, client: *mut ClientH) {
    // SAFETY: the caller transfers the handle.
    unsafe {
        let h = Box::from_raw(client);
        let _commit = tasks::before_commit();
        unit().ctx.drop_client(h.client);
    }
}

#[repr(C)]
pub struct SendOutcome {
    /// 0 accepted (x moved), 1 closed, 2 full; x stays the caller's unless
    /// accepted.
    pub outcome: u8,
    /// 1: the task failed (`Cancelled` at the wait, or `Bounds`), x still its.
    pub status: u8,
}

/// `conn.send` (`wait`) and `conn.try_send`.
/// # Safety
/// A live client borrow and an initialized item of the descriptor's layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_send(
    context: *mut Context,
    client: *mut ClientH,
    value: *mut u8,
    item: *const Cross,
    site: u64,
    wait: bool,
) -> SendOutcome {
    let out = |outcome| SendOutcome { outcome, status: 0 };
    let failed = SendOutcome {
        outcome: 0,
        status: 1,
    };
    // SAFETY: the caller pins the client, the value and the descriptor.
    unsafe {
        let item = &*item;
        let owners = item.owners();
        let mut commit = tasks::before_commit();
        if wait && tasks::check_cancel(context, site) != 0 {
            return failed;
        }
        let u = unit();
        let charge = item_charge(value, owners);
        let mut v = u
            .ctx
            .alloc_block(charge)
            .unwrap_or_else(|_| crate::process::stop(b"UnitLedger"));
        let mut data = u.spare.pop().unwrap_or_else(ItemBuf::new);
        pack_into(data.bytes_mut(), value, item.bytes, owners);
        v.set_item_buf(data);
        let id = match u.ctx.send(&(*client).client, v, wait) {
            Sent::Accepted => {
                free_owners(value, owners);
                return out(0);
            }
            Sent::Bounds => {
                causes::record(&mut *context, Cause::new(causes::BOUNDS, site));
                return failed;
            }
            Sent::Closed(v) => {
                u.ctx.drop_value(v);
                return out(1);
            }
            Sent::Full(v) => {
                u.ctx.drop_value(v);
                return out(2);
            }
            Sent::Waiting(id) => id,
        };
        let mut frame = SendWait {
            waiters: Waiters::default(),
            settled: None,
        };
        u.sends.insert(id, &raw mut frame);
        u.waits += 1;
        let mut withdrawn = false;
        loop {
            if frame.settled.is_none() {
                pump(u);
            }
            match frame.settled.take() {
                Some(Took::Accepted(_)) => {
                    u.waits -= 1;
                    free_owners(value, owners);
                    return out(0);
                }
                Some(Took::Closed(_)) => {
                    u.waits -= 1;
                    return out(1);
                }
                Some(_) => {
                    u.waits -= 1;
                    return failed;
                }
                None => {}
            }
            if !withdrawn && tasks::cancel_pending(context) {
                // A withdrawal that wins settles Withdrawn; one that loses
                // waits for the acceptance or close on its way.
                withdrawn = true;
                if u.ctx.withdraw(&(*client).client, id) {
                    tasks::check_cancel(context, site);
                }
                continue;
            }
            drop(commit);
            if tasks::park_unit_alone() {
                commit = tasks::before_commit();
                continue;
            }
            commit = tasks::park(&raw mut frame.waiters);
        }
    }
}

#[repr(C)]
pub struct ReceiveOutcome {
    /// 0 received, 1 closed, 2 Capacity (the item stays queued), 3 empty.
    pub outcome: u8,
    pub requested: u64,
    pub available: u64,
    /// 1: the task failed with `Cancelled` at the wait.
    pub status: u8,
}

/// `conn.receive` (`wait`) and `conn.try_receive`.
/// # Safety
/// The inbox borrow and a writable item destination.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_receive(
    context: *mut Context,
    inbox: *mut InboxH,
    dest: *mut u8,
    item: *const Cross,
    site: u64,
    wait: bool,
) -> ReceiveOutcome {
    let out = |outcome, requested, available| ReceiveOutcome {
        outcome,
        requested,
        available,
        status: 0,
    };
    // SAFETY: the inbox borrow pins the handle and its connection.
    unsafe {
        let item = &*item;
        let owners = item.owners();
        let h = &mut *inbox;
        let mut commit = tasks::before_commit();
        let mut waited = false;
        loop {
            if wait && tasks::check_cancel(context, site) != 0 {
                if waited {
                    unit().waits -= 1;
                }
                return ReceiveOutcome {
                    status: 1,
                    ..out(0, 0, 0)
                };
            }
            let u = unit();
            let side = h.inbox.0.side();
            if side.taken.is_empty() {
                pump(u);
            }
            if let Some(bytes) = h.inbox.front_bytes() {
                let payer = (*context).payer();
                if bytes > 0
                    && let Err(e) = block::admit_storage(payer, Some(bytes))
                {
                    if waited {
                        u.waits -= 1;
                    }
                    return out(2, e.requested, e.available);
                }
                let Received::Ok(v) = u.ctx.receive(&h.inbox) else {
                    crate::process::stop(b"UnitLedger");
                };
                let mut v = v;
                unpack(context, payer, v.item(), dest, item.bytes, owners);
                if u.spare.len() < SPARE_ITEMS
                    && let Some(data) = v.take_item_buf()
                    && data.capacity() <= SPARE_BYTES
                {
                    u.spare.push(data);
                }
                u.ctx.drop_value(v);
                if waited {
                    u.waits -= 1;
                }
                return out(0, 0, 0);
            }
            if side.closed {
                if waited {
                    u.waits -= 1;
                }
                return out(1, 0, 0);
            }
            if !wait {
                return out(3, 0, 0);
            }
            if !waited {
                waited = true;
                u.waits += 1;
            }
            drop(commit);
            if tasks::park_unit_alone() {
                commit = tasks::before_commit();
                continue;
            }
            commit = tasks::park(&raw mut h.waiters);
        }
    }
}

/// Clean an inbox: seal, wait (shielded) for the seal answer, drop every
/// item not received, and keep the retained charge until the last release.
/// # Safety
/// Transfers the inbox.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_inbox_drop(context: *mut Context, inbox: *mut InboxH) -> u8 {
    // SAFETY: the caller transfers the handle; it stays pinned while it waits.
    unsafe {
        let h = &mut *inbox;
        let mut commit = tasks::before_commit();
        (*context).cancel_shield += 1;
        let u = unit();
        u.ctx.seal(&h.inbox);
        u.waits += 1;
        loop {
            pump(u);
            if h.inbox.0.side().seal_taken {
                break;
            }
            drop(commit);
            if tasks::park_unit_alone() {
                commit = tasks::before_commit();
                continue;
            }
            commit = tasks::park(&raw mut h.waiters);
        }
        u.waits -= 1;
        (*context).cancel_shield -= 1;
        let side = h.inbox.0.side();
        for v in side.taken.iter() {
            let data = v.item().to_vec();
            drop_packed_clients(u, &data, &h.owners);
        }
        let key = Arc::as_ptr(&h.inbox.0) as usize;
        u.inboxes.remove(&key);
        u.ctx.clean(&h.inbox);
        let mut h = Box::from_raw(inbox);
        if let Some(held) = h.charge.take() {
            u.retained.insert(key, held);
        }
        drop(h);
        // The last release may already be on the port.
        pump(u);
        drop(commit);
        0
    }
}

// ---------------------------------------------------------------------------
// Helper units
// ---------------------------------------------------------------------------

/// What a helper's owner and its thread share.
struct HelperShared {}

pub struct HelperH {
    k: usize,
    waiters: Waiters,
    ended: bool,
    port: Arc<crate::units::ledger::Port>,
    /// The spawn's charge: the task charge with `n`.
    charge: Option<Block>,
    result: ValueOut,
}

#[derive(Clone)]
struct ValueOut {
    bytes: u64,
    owners: Vec<[u64; 4]>,
}

/// What the helper's thread hands back in the ledger's result value.
struct Ended {
    failed: bool,
    causes: Vec<(u8, u64)>,
}

/// A tagged task handle names a helper.
pub(crate) fn is_helper(task: *mut tasks::Task) -> bool {
    task as usize & 1 != 0
}
fn helper_of(task: *mut tasks::Task) -> *mut HelperH {
    (task as usize & !1) as *mut HelperH
}

struct SendPtr<T>(T);
// SAFETY: moved whole to the helper's thread.
unsafe impl<T> Send for SendPtr<T> {}

#[repr(C)]
pub struct SpawnUnit {
    /// Null when refused: `Capacity { requested, available }`, x still the caller's.
    pub task: *mut tasks::Task,
    pub requested: u64,
    pub available: u64,
    pub status: u8,
}

/// `task.spawn_unit(tasks, bytes n, take x, f)`.
/// # Safety
/// As `hirsel_core_spawn`, with the descriptors of X and R.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_spawn_unit(
    context: *mut Context,
    _tasks: u64,
    n: u64,
    input: *mut u8,
    body: Body,
    layout: *const TaskLayout,
    x: *const Cross,
    r: *const Cross,
    _site: u64,
) -> SpawnUnit {
    // SAFETY: the spawner owns its context and `input` until the spawn commits.
    unsafe {
        let layout = *layout;
        let x = &*x;
        let rx = &*r;
        let refused = |requested, available| SpawnUnit {
            task: ptr::null_mut(),
            requested,
            available,
            status: 0,
        };
        let r_size = layout.result.logical as u64;
        let t = 256u64
            .checked_add(r_size)
            .and_then(|s| s.checked_add(32))
            .and_then(|s| s.checked_add(n));
        let Some(t) = t else {
            return refused(u64::MAX, 0);
        };
        let held = match Block::new(&*context, Some(t), 1, 8) {
            Ok(b) => b,
            Err(e) => return refused(e.requested, e.available),
        };
        let xs = item_charge(input, x.owners());
        if xs > n {
            drop(held);
            return refused(xs, n);
        }
        let _commit = tasks::before_commit();
        let u = unit();
        let (k, hctx, _) = u
            .ctx
            .spawn_helper(n, r_size, None)
            .unwrap_or_else(|_| crate::process::stop(b"UnitLedger"));
        let data = pack(input, x.bytes, x.owners());
        free_owners(input, x.owners());
        let shared = Arc::new(HelperShared {});
        let h = Box::into_raw(Box::new(HelperH {
            k,
            waiters: Waiters::default(),
            ended: false,
            port: hctx.port.clone(),
            charge: Some(held),
            result: ValueOut {
                bytes: rx.bytes,
                owners: rx.owned(),
            },
        }));
        u.helpers.insert(k, h);
        u.waits += 1;
        let run = (*context).run;
        let job = SendPtr((
            hctx,
            data,
            x.bytes,
            x.owned(),
            rx.bytes,
            rx.owned(),
            run,
            body,
            layout,
            n,
            shared,
        ));
        let start = move || {
            let job = job;
            let (hctx, data, xb, xo, rb, ro, run, body, layout, n, shared) = job.0;
            helper_main(hctx, data, xb, xo, rb, ro, run, body, layout, n, shared);
        };
        // On the pool the helper is a unit beside its owner; without one it
        // has a thread of its own.
        let start: crate::units::Job = Box::new(start);
        if let Err(start) = crate::units::spawn_job(start) {
            std::thread::Builder::new()
                .name("hirsel-unit".into())
                .spawn(start)
                .unwrap_or_else(|_| crate::process::stop(b"HostThreads"));
        }
        SpawnUnit {
            task: (h as usize | 1) as *mut tasks::Task,
            requested: 0,
            available: 0,
            status: 0,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn helper_main(
    hctx: Box<UnitCtx>,
    data: Vec<u8>,
    xb: u64,
    xo: Vec<[u64; 4]>,
    rb: u64,
    ro: Vec<[u64; 4]>,
    run: *const crate::process::Config,
    body: Body,
    layout: TaskLayout,
    n: u64,
    shared: Arc<HelperShared>,
) {
    // SAFETY: this thread owns the helper's context and run for its life.
    unsafe {
        // The task record and primary cell are the owner's (outside n).
        let mut context = Context::new(n + 256 + 32);
        context.run = run;
        let mut files = crate::host_file::Files::default();
        context.host_files = &mut files;
        if causes::task_start(&mut context).is_err() {
            crate::process::stop(b"UnitLedger");
        }
        let mut input = vec![0u8; (xb as usize).max(layout.input.bytes)];
        let payer = context.payer();
        let xs = item_charge(data.as_ptr(), &xo);
        if xs > 0 && block::admit_storage(payer, Some(xs)).is_err() {
            crate::process::stop(b"UnitLedger");
        }
        unpack(&mut context, payer, &data, input.as_mut_ptr(), xb, &xo);
        drop(data);
        let mut adopted: *mut LangUnit = ptr::null_mut();
        PENDING.with(|p| *p.borrow_mut() = Some((hctx, shared, &raw mut adopted)));
        let mut result = vec![0u8; layout.result.bytes.max(rb as usize)];
        let provider = if run.is_null() {
            tasks::SchedulerProvider::default()
        } else {
            (*run).scheduler
        };
        let failed = tasks::run_root(
            &mut context,
            body,
            input.as_mut_ptr(),
            result.as_mut_ptr(),
            layout.input.bytes,
            layout.result.bytes,
            provider,
        );
        let lu = adopted;
        if lu.is_null() {
            crate::process::stop(b"UnitLedger");
        }
        let mut lu = Box::from_raw(lu);
        let ended = Ended {
            failed: failed != 0,
            causes: context.causes.iter().map(|c| (c.kind(), c.site)).collect(),
        };
        let mut ret = lu
            .ctx
            .alloc_block(0)
            .unwrap_or_else(|_| crate::process::stop(b"UnitLedger"));
        if failed == 0 {
            ret.set_item(pack(result.as_ptr(), rb, &ro));
            free_owners(result.as_ptr(), &ro);
        }
        RESULTS
            .lock()
            .unwrap()
            .insert(Arc::as_ptr(&lu.ctx.port) as usize, ended);
        files.close_all(&mut context);
        drop(context);
        let ctx = std::mem::replace(&mut lu.ctx, Box::new(UnitCtx::root(0)));
        drop(lu);
        ctx.end_helper(Some(ret), false);
    }
}

/// Each ended helper's outcome, by its port, until its owner joins.
static RESULTS: std::sync::Mutex<Option<HashMap<usize, Ended>>> = std::sync::Mutex::new(None);
trait ResultsExt {
    fn insert(&mut self, k: usize, e: Ended);
    fn take(&mut self, k: usize) -> Option<Ended>;
}
impl ResultsExt for std::sync::MutexGuard<'_, Option<HashMap<usize, Ended>>> {
    fn insert(&mut self, k: usize, e: Ended) {
        self.get_or_insert_with(HashMap::new).insert(k, e);
    }
    fn take(&mut self, k: usize) -> Option<Ended> {
        self.as_mut().and_then(|m| m.remove(&k))
    }
}

/// Wait for a helper's end take. Shielded waits ignore cancellation.
unsafe fn wait_end(context: *mut Context, h: *mut HelperH, site: u64, shielded: bool) -> bool {
    // SAFETY: the handle is pinned by its owner through the wait.
    unsafe {
        let mut commit = tasks::before_commit();
        loop {
            if !shielded && tasks::check_cancel(context, site) != 0 {
                return false;
            }
            let u = unit();
            if !(*h).ended {
                pump(u);
            }
            if (*h).ended {
                return true;
            }
            drop(commit);
            commit = tasks::park(&raw mut (*h).waiters);
        }
    }
}

/// Consume an ended helper: its result into `dest` (when it succeeded and
/// `dest` is not null), its causes as a chain. The spawn's charge is released
/// except what the result's owners keep.
unsafe fn consume(context: *mut Context, h: *mut HelperH, dest: *mut u8) -> (bool, Causes) {
    // SAFETY: the owner consumes the sole handle once.
    unsafe {
        let u = unit();
        u.helpers.remove(&(*h).k);
        u.waits -= 1;
        let mut hb = Box::from_raw(h);
        let ended = RESULTS
            .lock()
            .unwrap()
            .take(Arc::as_ptr(&hb.port) as usize)
            .unwrap_or_else(|| crate::process::stop(b"UnitLedger"));
        let ret = u.ctx.consume_helper(hb.k, false);
        let mut held = hb.charge.take().unwrap();
        let mut chain = Causes::default();
        if !ended.failed {
            if let Some(v) = ret.as_ref()
                && !dest.is_null()
            {
                let s = item_charge(v.item().as_ptr(), &hb.result.owners);
                let payer = held.partition(s);
                unpack(
                    context,
                    payer,
                    v.item(),
                    dest,
                    hb.result.bytes,
                    &hb.result.owners,
                );
            } else if let Some(v) = ret.as_ref() {
                drop_packed_clients(u, &v.item().to_vec(), &hb.result.owners);
            }
        } else {
            for (kind, site) in &ended.causes {
                if held.charge() < 32 {
                    break;
                }
                let mut cell = Cell::prepaid(held.partition(32));
                cell.cause = if *kind == causes::FAIL {
                    Cause::fail(Default::default(), *site)
                } else {
                    Cause::new(*kind, *site)
                };
                chain.append(cell);
            }
        }
        if let Some(v) = ret {
            u.ctx.drop_value(v);
        }
        drop(held);
        (ended.failed, chain)
    }
}

/// `join` on a helper's handle.
pub(crate) unsafe fn join(
    context: *mut Context,
    task: *mut tasks::Task,
    result: *mut u8,
    site: u64,
) -> tasks::Join {
    let h = helper_of(task);
    // SAFETY: as `hirsel_core_join`.
    unsafe {
        if !wait_end(context, h, site, false) {
            return tasks::Join {
                kind: 0,
                causes: ptr::null_mut(),
                status: 1,
            };
        }
        let _commit = tasks::before_commit();
        let (failed, chain) = consume(context, h, result);
        let cancelled = chain
            .iter()
            .next()
            .is_some_and(|c| c.kind() == causes::CANCELLED);
        tasks::Join {
            kind: if cancelled { 2 } else { u8::from(failed) },
            causes: chain.into_raw(),
            status: 0,
        }
    }
}

/// Ask a helper to cancel: an arrival at its port, taken in its own order.
pub(crate) unsafe fn cancel(task: *mut tasks::Task) {
    let h = helper_of(task);
    // SAFETY: the owner's handle pins the helper's port.
    unsafe {
        if !(*h).ended {
            (*h).port.publish(crate::units::ledger::ArrivalRef::boxed(
                crate::units::ledger::Body::Cancel,
            ));
        }
    }
}

/// Drop a helper's handle: cancel, wait for its end (shielded), drop the result.
pub(crate) unsafe fn abandon(context: *mut Context, task: *mut tasks::Task) -> u8 {
    // SAFETY: the caller transfers the handle.
    unsafe {
        cancel(task);
        wait_end(context, helper_of(task), 0, true);
        let _commit = tasks::before_commit();
        let (_, chain) = consume(context, helper_of(task), ptr::null_mut());
        drop(chain);
        0
    }
}

#[doc(hidden)]
#[path = "units_lang_bench.rs"]
pub mod bench;

#[cfg(test)]
#[path = "units_lang_tests.rs"]
mod tests;
