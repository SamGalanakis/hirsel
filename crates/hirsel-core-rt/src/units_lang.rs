//! Units and connections in the language (CONC-15, CONC-17): the entry points
//! the emitters call for `conn.*` and `task.spawn_unit`, over the unit ledger.
//!
//! Each run of a task scheduler is one unit. Its ledger context (`LangUnit`)
//! lives beside the scheduler; the unit's port readies the scheduler, which
//! takes every arrival at a scheduler turn (`pump`) and wakes the tasks that
//! wait on it. A helper unit is a run of its own on its own OS thread.
//!
//! Byte accounting has one authority: the program's limits (`limit.rs`).
//! A connection's or helper's charge is a `Held` admitted from the creating
//! task's limit. A helper's end take splits its charge: the owner gets back
//! the task record, the unused part of `n` and the unused cell, keeps
//! `size(R)` and the result's content until the join, and holds each
//! connection the helper left behind until that connection's last release.
//! The ledger keeps the unit graph (ports, connection words, room balances,
//! settlements, helper ends) and no books of its own: a language unit is
//! unfunded, and every byte it moves is a `Held` or a block's own charge.
//!
//! An item's flat bytes occupy its reserved arrival slot. Sequence content
//! (a `List` of plain elements or a `String`) occupies the room at length
//! capacity. Clients move as handles. Sockets move with their registrations detached.
use crate::{
    Capacity, Context,
    block,
    limit::Payer,
    causes::{self, Cause, Causes, Cell},
    net::{CrossedSocket, CrossingSockets, SocketCrossing},
    tasks::{self, Body, Readies, TaskLayout, Waiters},
    units::ledger::conn::{Client, Inbox, MAX_CAP, MAX_ROOM, Sent},
    units::ledger::{Released, Took, UnitCtx, Value},
};
use hirsel_core_layout::cross::{CLIENT, SEQ, SOCKET};
use std::{
    collections::HashMap,
    mem::MaybeUninit,
    ptr,
    sync::{Arc, Mutex},
};

/// Bytes admitted from `payer` and held for the unit runtime; the limit
/// record stays live while they are outstanding. Dropping a part refunds it
/// at the unit.
struct Held {
    payer: Payer,
    bytes: u64,
}

impl Held {
    fn admit(context: &Context, bytes: u64) -> Result<Self, Capacity> {
        let payer = context.payer();
        payer.admit(Some(bytes))?;
        Ok(Held { payer, bytes })
    }

    /// A part of at most `bytes`, refunded on its own.
    fn split(&mut self, bytes: u64) -> Held {
        let bytes = bytes.min(self.bytes);
        self.bytes -= bytes;
        Held {
            payer: self.payer,
            bytes,
        }
    }

    /// Hand `bytes` to an owner that refunds them itself.
    fn partition(&mut self, bytes: u64) -> Payer {
        self.bytes -= bytes;
        self.payer
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.payer.settle_at_unit(self.bytes);
        }
    }
}

/// Keyed by addresses and slot ids; a fixed seed keeps lookups cheap.
type Map<K, V> = HashMap<K, V, foldhash::fast::FixedState>;
fn map<K, V>() -> Map<K, V> {
    HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0))
}

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

fn socket_offsets(owners: &[[u64; 4]]) -> impl Iterator<Item = u64> {
    owners.iter().filter(|o| o[1] == SOCKET).map(|o| o[0])
}

unsafe fn detach_sockets(
    context: *mut Context,
    value: *const u8,
    offsets: impl Iterator<Item = u64>,
) -> Option<CrossingSockets> {
    let sockets: Vec<_> = offsets
        .map(|offset| {
            // SAFETY: the item exclusively owns each socket in this unit.
            unsafe {
                let key = word(value.add(offset as usize));
                CrossedSocket::detach(&mut *(*context).host_files, key)
            }
        })
        .collect();
    (!sockets.is_empty()).then(|| Arc::new(Mutex::new(SocketCrossing::InTransit(sockets))))
}

/// Install the socket keys in a received value or give a refused send its
/// sockets back.
#[inline(always)]
unsafe fn attach_sockets(
    context: *mut Context,
    value: *mut u8,
    offsets: impl Iterator<Item = u64>,
    sockets: Option<&CrossingSockets>,
) {
    if let Some(sockets) = sockets {
        // SAFETY: forwarded.
        unsafe { attach_crossing(context, value, offsets, sockets) }
    }
}
#[inline(never)]
unsafe fn attach_crossing(
    context: *mut Context,
    value: *mut u8,
    offsets: impl Iterator<Item = u64>,
    sockets: &CrossingSockets,
) {
    // SAFETY: the caller owns the destination and runs in its registry's unit.
    unsafe {
        let files = &mut *(*context).host_files;
        let keys = sockets.lock().unwrap().keys(files);
        for (offset, key) in offsets.zip(keys) {
            set_word(value.add(offset as usize), key);
        }
        if tasks::in_task()
            && let Some(poller) = files.sockets.active_poller()
        {
            tasks::attach_net(&raw mut files.sockets, poller);
        }
    }
}

/// Copy `value` and its owners' content into one buffer.
unsafe fn pack(value: *const u8, bytes: u64, owners: &[[u64; 4]]) -> Vec<u8> {
    // SAFETY: `value` is an initialized item of `bytes` bytes.
    unsafe {
        let content = |o: &[u64; 4]| {
            let at = value.add(o[0] as usize);
            (word(at) as *const u8, (word(at.add(8)) * o[2]) as usize)
        };
        let seqs = owners.iter().filter(|o| o[1] == SEQ);
        let mut data =
            Vec::with_capacity(bytes as usize + seqs.clone().map(|o| content(o).1).sum::<usize>());
        data.extend_from_slice(std::slice::from_raw_parts(value, bytes as usize));
        for o in seqs {
            let (p, n) = content(o);
            if n > 0 {
                data.extend_from_slice(std::slice::from_raw_parts(p, n));
            }
        }
        data
    }
}

/// A send can wait for a slot. Its call frame pins this source and its
/// owners until the send has published or its settlement is taken.
pub(crate) struct CrossSend {
    value: *const u8,
    pub(crate) sockets: Option<CrossingSockets>,
    #[cfg(test)]
    pub(crate) tag: u64,
}

impl CrossSend {
    pub(crate) fn new(value: *const u8) -> Self {
        Self {
            value,
            sockets: None,
            #[cfg(test)]
            tag: 0,
        }
    }

    /// The connection owns the destination slot. Only the flat bytes go
    /// there; sequence contents occupy the connection's room, in `spare`
    /// when it is large enough (it is then taken).
    pub(crate) unsafe fn copy_into(
        &self,
        slot: *mut u8,
        stride: usize,
        seqs: &[[u64; 4]],
        charge: u64,
        spare: &mut Box<[MaybeUninit<u64>]>,
    ) -> Box<[MaybeUninit<u64>]> {
        // SAFETY: the reserved slot has the connection's item layout. The
        // sender pins every source buffer through publication.
        unsafe {
            if stride != 0 {
                ptr::copy_nonoverlapping(self.value, slot, stride);
            }
            let content = |o: &[u64; 4]| {
                let at = self.value.add(o[0] as usize);
                (word(at) as *const u8, (word(at.add(8)) * o[2]) as usize)
            };
            if seqs.is_empty() {
                return Box::default();
            }
            let bytes: usize = seqs.iter().map(|o| content(o).1).sum();
            if bytes == 0 && charge == 0 {
                return Box::default();
            }
            let words = 1 + bytes.div_ceil(std::mem::size_of::<u64>());
            let mut room = if spare.len() >= words {
                std::mem::take(spare)
            } else {
                Box::new_uninit_slice(words)
            };
            room[0].write(charge);
            let to = room.as_mut_ptr().add(1).cast::<u8>();
            let mut at = 0;
            for o in seqs {
                let (p, n) = content(o);
                if n != 0 {
                    ptr::copy_nonoverlapping(p, to.add(at), n);
                    at += n;
                }
            }
            room
        }
    }
}

/// Free the seq buffers of a value whose content has been copied out; its
/// clients have moved and are not touched. `context` is the running task's
/// when the caller has it.
unsafe fn free_owners(context: Option<&Context>, value: *const u8, owners: &[[u64; 4]]) {
    // SAFETY: the value owns each nonempty buffer, charged at its capacity.
    unsafe {
        for o in owners {
            if o[1] == SEQ {
                let at = value.add(o[0] as usize);
                let cap = word(at.add(16));
                if cap != 0 {
                    let p = word(at) as *mut u8;
                    block::free_with(ptr::NonNull::new_unchecked(p), round8(cap * o[3]), context);
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
    sockets: Option<&CrossingSockets>,
    dest: *mut u8,
    bytes: u64,
    owners: &[[u64; 4]],
) {
    // SAFETY: `data` was packed for the same layout and `dest` is writable.
    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), dest, bytes as usize);
        attach_sockets(context, dest, socket_offsets(owners), sockets);
        unpack_owners(
            context,
            payer,
            data.as_ptr().add(bytes as usize),
            dest,
            owners,
        );
    }
}

/// Rebuild the sequence owners after the item's flat bytes have been copied.
unsafe fn unpack_owners(
    context: *mut Context,
    payer: crate::limit::Payer,
    content: *const u8,
    dest: *mut u8,
    owners: &[[u64; 4]],
) {
    // SAFETY: the receiver admitted the owners and the flat bytes give their lengths.
    unsafe {
        let mut at = 0;
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
            ptr::copy_nonoverlapping(content.add(at), p.as_ptr(), n);
            at += n;
            set_word(slot, p.as_ptr() as u64);
            set_word(slot.add(16), len);
        }
    }
}

/// Drop the moved owners of an item that no one received.
unsafe fn drop_packed_owners(
    context: *mut Context,
    unit: &mut LangUnit,
    flat: *const u8,
    owners: &[[u64; 4]],
    sockets: Option<&CrossingSockets>,
) {
    if let Some(sockets) = sockets {
        let keys = sockets.lock().unwrap().discard();
        for key in keys {
            // SAFETY: the item owned this socket in the current unit.
            unsafe { crate::net::hirsel_core_socket_drop(context, key) };
        }
    }
    for o in owners {
        if o[1] == CLIENT {
            // SAFETY: the item holds this inline handle and owns the client.
            let h = unsafe { word(flat.add(o[0] as usize)) } as *mut ClientH;
            if !h.is_null() {
                unit.drop_client(unsafe { Box::from_raw(h) }.client);
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
    pub(crate) socket_registry: *mut hirsel_core_net::Sockets,
    inboxes: Map<usize, *mut InboxH>,
    sends: Map<u32, *mut SendWait>,
    helpers: Map<usize, *mut HelperH>,
    /// Header and unused room of cleaned connections, until the last release.
    retained: Map<usize, Held>,
    /// Tasks waiting on something another unit settles: the scheduler does
    /// not call such a wait a deadlock.
    pub(crate) waits: usize,
    /// The run's root task, cancelled by a cancel arrival (helpers).
    pub(crate) root: *mut tasks::Task,
    helper: Option<Arc<HelperShared>>,
}

impl LangUnit {
    /// Drop a client. The owner's own release of a cleaned connection takes
    /// through its notice; each take wakes what waits on it, as in `pump`.
    fn drop_client(&mut self, client: Client) {
        let took = self.ctx.drop_client(client);
        if !took.is_empty() {
            self.on_released(took);
        }
    }

    /// Every caller holds a commit. A release that took drained the port and
    /// may have left later arrivals reached but untaken; pump takes them and
    /// rearms the port, so no task parks on work the unit already holds.
    #[cold]
    #[inline(never)]
    fn on_released(&mut self, took: Vec<Released>) {
        let wake = self.waits > 0;
        for (t, id, _) in took {
            // SAFETY: a release runs in a commit that owns the unit.
            unsafe { on_take(self, t, id, wake) };
        }
        // SAFETY: as above.
        unsafe { pump(self) };
    }

    fn refund_retained(&mut self, id: usize) {
        self.retained.remove(&id);
    }

    pub(crate) fn has_peer(&self) -> bool {
        self.helper.is_some() || !self.helpers.is_empty()
    }

    fn new(
        ctx: Box<UnitCtx>,
        readies: &Arc<Readies>,
        helper: Option<Arc<HelperShared>>,
    ) -> Box<Self> {
        ctx.port.set_waker(readies);
        ctx.port.arm();
        Box::new(LangUnit {
            ctx,
            socket_registry: ptr::null_mut(),
            inboxes: map(),
            sends: map(),
            helpers: map(),
            retained: map(),
            waits: 0,
            root: ptr::null_mut(),
            helper,
        })
    }
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
        Box::new(UnitCtx::unfunded()),
        readies,
        None,
    ))
}

/// # Safety
/// The scheduler owns this root unit and no task runs.
pub(crate) unsafe fn drop_root_unit(u: *mut LangUnit) {
    // SAFETY: made by `root_unit`.
    let mut u = unsafe { Box::from_raw(u) };
    u.ctx.end_root();
}

/// Take every arrival and wake what waits on it; then arm the port so the
/// next publish readies the scheduler. Runs in a commit.
pub(crate) unsafe fn pump(u: &mut LangUnit) {
    // SAFETY: the caller owns the unit for this commit.
    unsafe { pump_for(u, false) };
}

unsafe fn pump_for(u: &mut LangUnit, receiving: bool) {
    let wake_inboxes = u.waits > usize::from(receiving);
    loop {
        while let Some((took, _a)) = u.ctx.take() {
            let id = u.ctx.took_ident;
            // SAFETY: as in `on_take`.
            unsafe { on_take(u, took, id, wake_inboxes) };
        }
        if u.ctx.port.arm() {
            return;
        }
    }
}

/// Wake what waits on one take.
/// # Safety
/// The caller owns the unit for this commit; every registered frame and
/// handle is pinned while registered.
#[inline(always)]
unsafe fn on_take(u: &mut LangUnit, took: Took, id: usize, wake_inboxes: bool) {
    // SAFETY: by the contract.
    unsafe {
        match took {
            Took::Item | Took::Close | Took::SealAnswer => {
                if let Some(&h) = u.inboxes.get(&id) {
                    if took == Took::Item {
                        take_sockets(u, &*h);
                    }
                    if wake_inboxes {
                        Waiters::wake_all(&raw mut (*h).waiters);
                    }
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
                if let Some(&h) = u.helpers.get(&id) {
                    end_take(u, &mut *h);
                    (*h).ended = true;
                    Waiters::wake_all(&raw mut (*h).waiters);
                }
            }
            Took::LastRelease => u.refund_retained(id),
            Took::Credits => {}
            Took::Cancel => {
                if !u.root.is_null() {
                    tasks::hirsel_core_cancel(ptr::null_mut(), u.root);
                }
            }
        }
    }
}

/// Register sockets when the receiving unit takes an arrival.
#[inline(always)]
unsafe fn take_sockets(u: &mut LangUnit, h: &InboxH) {
    // Only an item type with socket fields carries sockets.
    if !h.inbox.0.socket_offsets.is_empty() {
        // SAFETY: forwarded.
        unsafe { take_crossing(u, h) }
    }
}
#[inline(never)]
unsafe fn take_crossing(u: &mut LangUnit, h: &InboxH) {
    if let Some(sockets) = h
        .inbox
        .0
        .side()
        .taken
        .back()
        .and_then(|item| item.value().item_sockets())
    {
        // SAFETY: the inbox belongs to this unit, which owns its host files.
        let files = unsafe { &mut *h.files };
        sockets.lock().unwrap().take(files);
        if files.sockets.active_poller().is_some() {
            u.socket_registry = &raw mut files.sockets;
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
    files: *mut crate::host_file::Files,
    inbox: Inbox,
    waiters: Waiters,
    owners: Vec<[u64; 4]>,
    charge: Option<Held>,
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
        let held = match Held::admit(&*context, charge) {
            Ok(held) => held,
            Err(e) => return refused(e.available),
        };
        let _commit = tasks::before_commit();
        let u = unit();
        let owners = (*item).owners();
        let (inbox, client) = u
            .ctx
            .conn_open(cap, logical, b, (*item).bytes as usize, owners);
        let key = Arc::as_ptr(&inbox.0) as usize;
        let ih = Box::into_raw(Box::new(InboxH {
            files: (*context).host_files,
            inbox,
            waiters: Waiters::default(),
            owners: owners.iter().copied().filter(|o| o[1] == CLIENT).collect(),
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
        unit().drop_client(h.client);
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
/// A live client borrow and an initialized item of the connection's layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_send(
    context: *mut Context,
    client: *mut ClientH,
    value: *mut u8,
    site: u64,
    wait: bool,
) -> SendOutcome {
    let out = |outcome| SendOutcome { outcome, status: 0 };
    let failed = SendOutcome {
        outcome: 0,
        status: 1,
    };
    // SAFETY: the caller pins the client and the value.
    unsafe {
        let client = &(*client).client;
        let owners = &client.0.seqs;
        let task = tasks::current_task();
        let mut commit = tasks::before_commit_for(task);
        if wait && tasks::check_cancel_for(context, site, task) != 0 {
            return failed;
        }
        let charge = item_charge(value, owners);
        let offsets = || client.0.socket_offsets.iter().copied();
        let sockets = if client.0.socket_offsets.is_empty() {
            None
        } else {
            detach_sockets(context, value, offsets())
        };
        let mut source = CrossSend::new(value);
        source.sockets = sockets.clone();
        let id = match UnitCtx::send_cross(client, source, charge, wait, || {
            &mut (*tasks::lang_unit_for(task)).ctx
        }) {
            Sent::Accepted => {
                free_owners(Some(&*context), value, owners);
                return out(0);
            }
            Sent::Bounds => {
                attach_sockets(context, value, offsets(), sockets.as_ref());
                causes::record(&mut *context, Cause::new(causes::BOUNDS, site));
                return failed;
            }
            Sent::Closed(_) => {
                attach_sockets(context, value, offsets(), sockets.as_ref());
                return out(1);
            }
            Sent::Full(_) => {
                attach_sockets(context, value, offsets(), sockets.as_ref());
                return out(2);
            }
            Sent::Waiting(id) => id,
        };
        let unit = tasks::lang_unit_for(task);
        let mut frame = SendWait {
            waiters: Waiters::default(),
            settled: None,
        };
        (*unit).sends.insert(id, &raw mut frame);
        (*unit).waits += 1;
        let mut withdrawn = false;
        loop {
            let u = &mut *unit;
            if frame.settled.is_none() {
                pump(u);
            }
            match frame.settled.take() {
                Some(Took::Accepted(_)) => {
                    u.waits -= 1;
                    free_owners(Some(&*context), value, owners);
                    return out(0);
                }
                Some(Took::Closed(_)) => {
                    attach_sockets(context, value, offsets(), sockets.as_ref());
                    u.waits -= 1;
                    return out(1);
                }
                Some(_) => {
                    attach_sockets(context, value, offsets(), sockets.as_ref());
                    u.waits -= 1;
                    return failed;
                }
                None => {}
            }
            if !withdrawn && tasks::cancel_pending_for(context, task) {
                // A withdrawal that wins settles Withdrawn; one that loses
                // waits for the acceptance or close on its way.
                withdrawn = true;
                if u.ctx.withdraw(client, id) {
                    tasks::check_cancel_for(context, site, task);
                }
                continue;
            }
            drop(commit);
            if tasks::park_unit_alone(task, || {
                (*unit).ctx.port.spin_before_park(Some(client.receiver_port()))
            }) {
                commit = tasks::before_commit_for(task);
                continue;
            }
            commit = tasks::park_for(task, &raw mut frame.waiters);
        }
    }
}

#[repr(C)]
pub struct ReceiveOutcome {
    /// The low byte holds the outcome and the next byte holds the failure
    /// status. Requested fits in the remaining bits because MAX_ROOM is 40 bits.
    pub code_and_requested: u64,
    pub available: u64,
}

const _: () = assert!(MAX_ROOM <= u64::MAX >> 16);

/// `conn.receive` (`wait`) and `conn.try_receive`.
/// # Safety
/// The inbox borrow and a writable item destination.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_receive(
    context: *mut Context,
    inbox: *mut InboxH,
    dest: *mut u8,
    site: u64,
    wait: bool,
) -> ReceiveOutcome {
    let out = |outcome: u64, requested: u64, available| ReceiveOutcome {
        code_and_requested: (requested << 16) | outcome,
        available,
    };
    // SAFETY: the inbox borrow pins the handle and its connection.
    unsafe {
        let input = &(*inbox).inbox;
        let c = &input.0;
        let task = tasks::current_task();
        let mut commit = tasks::before_commit_for(task);
        let unit = tasks::lang_unit_for(task);
        let mut waited = false;
        loop {
            let u = &mut *unit;
            if wait && tasks::check_cancel_for(context, site, task) != 0 {
                if waited {
                    u.waits -= 1;
                }
                return out(1 << 8, 0, 0);
            }
            if c.side().taken.is_empty() && u.ctx.port.pending() {
                // A lone item for this inbox is all the port holds, so no
                // other wait of this unit (a helper, another task) has
                // anything to take from it.
                if u.ctx.take_single_item(c) {
                    take_sockets(u, &*inbox);
                } else {
                    pump_for(u, waited);
                }
            }
            let received = input.receive_cross(|bytes, flat, content, sockets| {
                if c.seqs.is_empty() {
                    if c.stride != 0 {
                        ptr::copy_nonoverlapping(flat, dest, c.stride);
                    }
                } else {
                    let payer = (*context).payer();
                    if bytes != 0 {
                        block::admit_storage(payer, Some(bytes))?;
                    }
                    ptr::copy_nonoverlapping(flat, dest, c.stride);
                    unpack_owners(context, payer, content, dest, &c.seqs);
                }
                attach_sockets(context, dest, c.socket_offsets.iter().copied(), sockets);
                Ok::<_, Capacity>(())
            });
            if let Some(result) = received {
                if waited {
                    u.waits -= 1;
                }
                return match result {
                    Ok(()) => out(0, 0, 0),
                    Err(e) => out(2, e.requested, e.available),
                };
            }
            if c.side().closed {
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
            if tasks::park_unit_alone(task, || (*unit).ctx.port.spin_before_park(None)) {
                commit = tasks::before_commit_for(task);
                continue;
            }
            commit = tasks::park_for(task, &raw mut (*inbox).waiters);
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
            if tasks::park_unit_alone(tasks::current_task(), || false) {
                commit = tasks::before_commit();
                continue;
            }
            commit = tasks::park(&raw mut h.waiters);
        }
        u.waits -= 1;
        (*context).cancel_shield -= 1;
        let side = h.inbox.0.side();
        for v in side.taken.iter().rev() {
            drop_packed_owners(
                context,
                u,
                h.inbox.flat_ptr(v),
                &h.owners,
                v.value().item_sockets(),
            );
        }
        let key = Arc::as_ptr(&h.inbox.0) as usize;
        u.inboxes.remove(&key);
        let mut h = Box::from_raw(inbox);
        if let Some(held) = h.charge.take() {
            u.retained.insert(key, held);
        }
        // With no client left the cleaning is the owner's own release: it
        // takes through the notice, which refunds the retained charge.
        let (_, took) = u.ctx.clean(&h.inbox);
        drop(h);
        if !took.is_empty() {
            u.on_released(took);
        }
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
    /// The spawn's charge, `256 + size(R) + 32 + n`, less what its end take
    /// returned or handed on.
    charge: Option<Held>,
    r_size: u64,
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
    /// The result's content.
    result: u64,
    /// The cleaned connections it left behind with clients elsewhere: each
    /// one's key and its retained charge, part of `n`.
    survivors: Vec<(usize, u64)>,
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
        let held = match Held::admit(&*context, t) {
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
        let (k, hctx) = u.ctx.spawn_unit();
        let data = pack(input, x.bytes, x.owners());
        let sockets = detach_sockets(context, input, socket_offsets(x.owners()));
        free_owners(None, input, x.owners());
        let shared = Arc::new(HelperShared {});
        let h = Box::into_raw(Box::new(HelperH {
            k,
            waiters: Waiters::default(),
            ended: false,
            port: hctx.port.clone(),
            charge: Some(held),
            r_size,
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
            sockets,
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
            let (hctx, data, sockets, xb, xo, rb, ro, run, body, layout, n, shared) = job.0;
            helper_main(
                hctx, data, sockets, xb, xo, rb, ro, run, body, layout, n, shared,
            );
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
    sockets: Option<CrossingSockets>,
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
        let mut context = Context::with_unit_heap(n + 256 + 32);
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
        unpack(
            &mut context,
            payer,
            &data,
            sockets.as_ref(),
            input.as_mut_ptr(),
            xb,
            &xo,
        );
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
        // Each connection still retained has its last client in another unit;
        // the owner holds its charge from the end take on. Its own copy here
        // goes back to this helper's limit, which ends with it.
        let survivors = lu.retained.drain().map(|(k, h)| (k, h.bytes)).collect();
        let ended = Ended {
            failed: failed != 0,
            causes: context.causes.iter().map(|c| (c.kind(), c.site)).collect(),
            result: if failed == 0 {
                item_charge(result.as_ptr(), &ro)
            } else {
                0
            },
            survivors,
        };
        let mut ret = Value::empty();
        if failed == 0 {
            let data = pack(result.as_ptr(), rb, &ro);
            let sockets = detach_sockets(&mut context, result.as_ptr(), socket_offsets(&ro));
            ret.set_item(data, sockets);
            free_owners(None, result.as_ptr(), &ro);
        }
        RESULTS
            .lock()
            .unwrap()
            .insert(Arc::as_ptr(&lu.ctx.port) as usize, ended);
        files.close_all(&mut context);
        drop(context);
        let LangUnit { ctx, .. } = *lu;
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

/// A helper's end take in its owner: the survivors' charges move into the
/// owner's retained charges under their connections' keys, `size(R)`, the
/// result's content and a failure's cells stay for the join, and the rest
/// comes back now.
/// # Safety
/// The owner runs the take in a commit; the handle is live.
unsafe fn end_take(u: &mut LangUnit, h: &mut HelperH) {
    let results = RESULTS.lock().unwrap();
    let ended = results
        .as_ref()
        .and_then(|m| m.get(&(Arc::as_ptr(&h.port) as usize)))
        .unwrap_or_else(|| crate::process::stop(b"UnitLedger"));
    let held = h.charge.as_mut().unwrap();
    for &(key, bytes) in &ended.survivors {
        u.retained.insert(key, held.split(bytes));
    }
    let cells = if ended.failed {
        32 * ended.causes.len() as u64
    } else {
        0
    };
    let keep = h.r_size + ended.result + cells;
    drop(held.split(held.bytes.saturating_sub(keep)));
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
                    v.item_sockets(),
                    dest,
                    hb.result.bytes,
                    &hb.result.owners,
                );
            } else if let Some(v) = ret.as_ref() {
                drop_packed_owners(
                    context,
                    u,
                    v.item().as_ptr(),
                    &hb.result.owners,
                    v.item_sockets(),
                );
            }
        } else {
            for (kind, site) in &ended.causes {
                if held.bytes < 32 {
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
