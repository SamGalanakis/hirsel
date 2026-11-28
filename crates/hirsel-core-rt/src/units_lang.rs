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
    Capacity, Context, block,
    causes::{self, Cause, Causes, Cell},
    net::{CrossedSocket, CrossingSockets, SocketCrossing},
    tasks::{self, Body, Readies, TaskLayout, Waiters},
    units::ledger::conn::{Client, Inbox, MAX_CAP, MAX_ROOM, Sent},
    units::ledger::{Released, Took, UnitCtx, Value},
};
use hirsel_core_layout::{
    charge,
    cross::{CLIENT, MEDIA, SEQ, SHARED, SOCKET},
};
use std::{
    collections::HashMap,
    mem::MaybeUninit,
    ptr,
    sync::{Arc, Mutex},
};

mod held;
mod inbox;
use held::Held;
pub use inbox::{
    ReceiveOutcome, hirsel_core_conn_receive, hirsel_core_conn_receive_rec,
    hirsel_core_conn_receive_until, hirsel_core_inbox_drop,
};
mod crossing;
mod ending;
pub use crossing::{Cross, SpawnEntry};
use crossing::{TapeHelper, tape_group};
use ending::{RESULTS, ResultsExt, end_take, wait_end};
pub(crate) use ending::{abandon, cancel};
pub(crate) mod packing;
#[cfg(feature = "record")]
pub(crate) mod replay;
mod sockets;
use sockets::{attach_sockets, detach_sockets, socket_offsets, take_sockets};
pub(crate) mod takes;
pub(crate) use packing::CrossSend;
use packing::{
    drop_packed_owners, free_owners, inbox_owners, item_charge, live_charge, live_rows, pack,
    round8, set_word, unpack, unpack_owners, word,
};

/// Keyed by addresses and slot ids; a fixed seed keeps lookups cheap.
type Map<K, V> = HashMap<K, V, foldhash::fast::FixedState>;
fn map<K, V>() -> Map<K, V> {
    HashMap::with_hasher(foldhash::fast::FixedState::with_seed(0))
}

// The unit beside a scheduler

/// A task waiting on one waiting send's settlement.
struct SendWait {
    waiters: Waiters,
    settled: Option<Took>,
}

struct SchedulerHooks {
    pump: unsafe fn(&mut LangUnit),
    drop_root: unsafe fn(*mut LangUnit),
}

static SCHEDULER_HOOKS: SchedulerHooks = SchedulerHooks {
    pump,
    drop_root: drop_root_unit,
};

pub(crate) struct LangUnit {
    hooks: &'static SchedulerHooks,
    pub(crate) ctx: Box<UnitCtx>,
    pub(crate) socket_registry: *mut hirsel_core_net::Sockets,
    inboxes: Map<usize, *mut InboxH>,
    sends: Map<u32, *mut SendWait>,
    helpers: Map<usize, *mut HelperH>,
    /// Header and unused room of cleaned connections, until the last release;
    /// likewise the bytes of an ended helper's live `Shared` nodes.
    retained: Map<usize, Held>,
    /// Each live `Shared` node made here, with the bytes it holds (CONC-15).
    pub(crate) homed: Map<usize, u64>,
    /// Run-owned grants paid from helper reservations, held until this unit ends.
    run_grants: Vec<Held>,
    /// Tasks waiting on something another unit settles: the scheduler does
    /// not call such a wait a deadlock.
    pub(crate) waits: usize,
    /// The run's root task, cancelled by a cancel arrival (helpers).
    pub(crate) root: *mut tasks::Task,
    helper: Option<Arc<HelperShared>>,
    /// The unit's recording: its run's, or its helper stream's.
    #[cfg(feature = "record")]
    tape: Option<crate::tape::schedule::Group>,
    /// Takes now are at a scheduler pump (point kind 1).
    #[cfg(feature = "record")]
    pumping: bool,
    /// A replaying unit's helpers, unstarted until their recorded end take.
    #[cfg(feature = "record")]
    unstarted: Map<usize, Box<UnitCtx>>,
    /// The host files of the context that bound the unit's recording, whose
    /// run's `Secret`s an arriving one joins on replay.
    #[cfg(feature = "record")]
    files: *mut crate::host_file::Files,
    /// Replay: the own connection whose `LastRelease` was just admitted at
    /// this pump, so `inbox_drop` knows whether `on_released` ran at record.
    #[cfg(feature = "record")]
    last_released: Option<usize>,
}

impl LangUnit {
    pub(crate) fn refund_retained(&mut self, id: usize) {
        self.retained.remove(&id);
    }

    pub(crate) fn run_view(&self) -> Option<Arc<tasks::RunView>> {
        self.helper.as_ref().map(|h| h.0.view())
    }

    pub(crate) fn attach(&self, readies: &Arc<Readies>) {
        self.ctx.port.set_waker(readies);
        self.ctx.port.arm();
    }

    /// # Safety
    /// The scheduler owns the unit for this commit.
    pub(crate) unsafe fn pump_ready(&mut self) {
        self.set_pumping(true);
        // SAFETY: forwarded to the installed unit hook.
        unsafe { (self.hooks.pump)(self) }
        self.set_pumping(false);
    }

    /// # Safety
    /// The scheduler owns the root allocated by `root_unit`; no task runs.
    pub(crate) unsafe fn end_root(unit: *mut Self) {
        // SAFETY: forwarded to the installed unit hook.
        unsafe { ((*unit).hooks.drop_root)(unit) }
    }

    fn new(ctx: Box<UnitCtx>, helper: Option<Arc<HelperShared>>) -> Box<Self> {
        Box::new(LangUnit {
            hooks: &SCHEDULER_HOOKS,
            ctx,
            socket_registry: ptr::null_mut(),
            inboxes: map(),
            sends: map(),
            helpers: map(),
            retained: map(),
            homed: map(),
            run_grants: Vec::new(),
            waits: 0,
            root: ptr::null_mut(),
            helper,
            #[cfg(feature = "record")]
            tape: None,
            #[cfg(feature = "record")]
            pumping: false,
            #[cfg(feature = "record")]
            unstarted: map(),
            #[cfg(feature = "record")]
            files: ptr::null_mut(),
            #[cfg(feature = "record")]
            last_released: None,
        })
    }
}

/// A root unit, made at the first unit operation of a run that is not a helper.
pub(crate) fn root_unit(readies: &Arc<Readies>) -> *mut LangUnit {
    let unit = LangUnit::new(Box::new(UnitCtx::unfunded()), None);
    unit.attach(readies);
    Box::into_raw(unit)
}

/// # Safety
/// The scheduler owns this root unit and no task runs.
unsafe fn drop_root_unit(u: *mut LangUnit) {
    // SAFETY: made by `root_unit`.
    let mut u = unsafe { Box::from_raw(u) };
    #[cfg(feature = "record")]
    crate::tape::units::replay::drain_survivors(u.tape);
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
        #[cfg(feature = "record")]
        let took_replay = unsafe { replay::take_one(u) };
        while let Some((took, _a)) = u.ctx.take() {
            let id = u.ctx.took_ident;
            unsafe { on_take(u, took, id, wake_inboxes) };
        }
        #[cfg(feature = "record")]
        if took_replay {
            continue;
        }
        #[cfg(any(test, feature = "testing", feature = "record"))]
        u.ctx.hold_published();
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
        takes::take(u, &took, id);
        match took {
            Took::Item | Took::Close | Took::SealAnswer => {
                if let Some(&h) = u.inboxes.get(&id) {
                    if took == Took::Item {
                        take_sockets(u, &*h);
                    }
                    if wake_inboxes {
                        Waiters::wake_all(
                            &raw mut (*h).waiters,
                            #[cfg(feature = "prof")]
                            (crate::prof::WakeSource::Unknown, ptr::null_mut(), 0),
                        );
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
                    Waiters::wake_all(
                        &raw mut (*w).waiters,
                        #[cfg(feature = "prof")]
                        (crate::prof::WakeSource::Unknown, ptr::null_mut(), 0),
                    );
                }
            }
            Took::HelperEnd => {
                if let Some(&h) = u.helpers.get(&id) {
                    end_take(u, &mut *h);
                    (*h).ended = true;
                    Waiters::wake_all(
                        &raw mut (*h).waiters,
                        #[cfg(feature = "prof")]
                        (crate::prof::WakeSource::Unknown, ptr::null_mut(), 0),
                    );
                }
            }
            Took::LastRelease => u.refund_retained(id),
            Took::Credits => {}
            Took::SharedRelease => crate::shared::clean(u, tasks::current_context(), id as *mut u8),
            Took::Cancel => {
                if !u.root.is_null() {
                    tasks::hirsel_core_cancel(ptr::null_mut(), u.root);
                }
            }
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
    pub(crate) client: Client,
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
            .and_then(|s| s.checked_add(charge::CONNECTION_BASE))
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
        let (u, owners, bytes) = (unit(), (*item).owners(), (*item).bytes as usize);
        let (inbox, client) = u.ctx.conn_open(cap, logical, b, bytes, owners);
        #[cfg(not(feature = "standalone"))]
        crate::embed::maybe_capture_dialer(&client);
        let key = Arc::as_ptr(&inbox.0) as usize;
        let ih = Box::into_raw(Box::new(InboxH {
            files: (*context).host_files,
            inbox,
            waiters: Waiters::default(),
            owners: inbox_owners(owners),
            charge: Some(held),
        }));
        u.inboxes.insert(key, ih);
        let client = Box::into_raw(Box::new(ClientH { client }));
        #[cfg(feature = "record")]
        takes::conn_new(u, key, client, &*item, (cap, b, logical));
        ConnNew {
            client,
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
        let client = Box::into_raw(Box::new(ClientH {
            client: (*client).client.clone_client(),
        }));
        #[cfg(feature = "record")]
        takes::client_new(unit(), client);
        client
    }
}

/// # Safety
/// Transfers the client.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_client_drop(_context: *mut Context, client: *mut ClientH) {
    // SAFETY: the caller transfers the handle.
    unsafe {
        let h = Box::from_raw(client);
        #[cfg(feature = "record")]
        takes::client_drop(unit(), client);
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
    // SAFETY: by the contract; the stackful form never parks returning.
    unsafe { conn_send::<false>(context, client, value, site, wait, false) }
        .unwrap_or_else(|| unreachable!())
}

/// The returnable form of `conn.send` (`RtOp::Returnable`): `word` is the
/// caller's child word, 1 while the send waits with the stack given back.
/// # Safety
/// As `hirsel_core_conn_send`, in a task that runs a returnable root; a
/// resume passes the same client and item place.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_send_rec(
    context: *mut Context,
    word: *mut u64,
    client: *mut ClientH,
    value: *mut u8,
    site: u64,
) -> SendOutcome {
    // SAFETY: by the contract.
    unsafe {
        let sent = conn_send::<true>(context, client, value, site, true, *word != 0);
        tasks::returned(
            word,
            sent,
            SendOutcome {
                outcome: 0,
                status: 0,
            },
        )
    }
}

/// A waiting send's state from its registration to its settlement: on the
/// stack in the stackful form, in the frame record in the returnable one.
pub(crate) struct SendRec {
    frame: SendWait,
    id: u32,
    withdrawn: bool,
    sockets: Option<CrossingSockets>,
}

/// `REC` parks with the stack given back (None) and keeps the waiting
/// send's state in the frame record; `resumed` continues that wait.
#[inline(always)]
unsafe fn conn_send<const REC: bool>(
    context: *mut Context,
    client: *mut ClientH,
    value: *mut u8,
    site: u64,
    wait: bool,
    resumed: bool,
) -> Option<SendOutcome> {
    let out = |outcome| Some(SendOutcome { outcome, status: 0 });
    let failed = Some(SendOutcome {
        outcome: 0,
        status: 1,
    });
    // SAFETY: the caller pins the client and the value; the record outlives
    // the wait and only this task's fiber touches it.
    unsafe {
        let handle = client;
        let client = &(*client).client;
        let live = client.0.live(value);
        let owners: &[[u64; 4]] = live.as_deref().unwrap_or(&client.0.seqs);
        let live_sockets: Option<Vec<u64>> = live.as_deref().map(|r| socket_offsets(r).collect());
        let task = tasks::current_task();
        let sockets_at = live_sockets.as_deref().unwrap_or(&client.0.socket_offsets);
        let offsets = || sockets_at.iter().copied();
        let mut local = MaybeUninit::<SendRec>::uninit();
        // The returnable form's state is in the record; the fast path that
        // never waits leaves it untouched.
        let mut state: *mut SendRec = local.as_mut_ptr();
        if REC && resumed {
            state = tasks::held_wait();
        } else {
            tasks::before_commit_for(task);
            if wait && tasks::check_cancel_for(context, site, task) != 0 {
                return failed;
            }
            let charge = item_charge(value, owners);
            let sockets = if offsets().next().is_none() {
                None
            } else {
                detach_sockets(context, value, offsets())
            };
            // The item as a value, read before the send moves it.
            let mut send = takes::SendNote::new(context, handle, value, wait, sockets.as_ref());
            // A replayed send delivers nothing: its outcome is the recorded one.
            let mut replayed_wait = None;
            if charge <= client.0.room()
                && let Some(outcome) = send.replayed(task)
            {
                match outcome {
                    Some(takes::ReplayedSend::Immediate(0)) => {
                        free_owners(Some(&*context), value, owners);
                        return out(0);
                    }
                    Some(takes::ReplayedSend::Waiting(id)) => {
                        replayed_wait = Some(id);
                    }
                    Some(takes::ReplayedSend::Immediate(1)) if !wait => {
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        return out(2);
                    }
                    Some(takes::ReplayedSend::Immediate(_)) => {
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        return out(1);
                    }
                    None => {
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        return failed;
                    }
                }
            }
            let id = if let Some(id) = replayed_wait {
                id
            } else {
                let mut source = CrossSend::new(value);
                source.rows = live.clone();
                source.sockets = sockets.clone();
                match UnitCtx::send_cross(client, source, charge, wait, || {
                    &mut (*tasks::lang_unit_for(task)).ctx
                }) {
                    Sent::Accepted => {
                        send.note(Some(0), None);
                        free_owners(Some(&*context), value, owners);
                        return out(0);
                    }
                    Sent::Bounds => {
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        causes::record(&mut *context, Cause::new(causes::BOUNDS, site));
                        return failed;
                    }
                    Sent::Closed(_) => {
                        send.note(Some(if wait { 1 } else { 2 }), None);
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        return out(1);
                    }
                    Sent::Full(_) => {
                        send.note(Some(1), None);
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        return out(2);
                    }
                    Sent::Waiting(id) => {
                        send.note(None, Some(id));
                        id
                    }
                }
            };
            if REC {
                state = tasks::held_wait();
            }
            state.write(SendRec {
                frame: SendWait {
                    waiters: Waiters::default(),
                    settled: None,
                },
                id,
                withdrawn: false,
                sockets,
            });
            let unit = tasks::lang_unit_for(task);
            (*unit).sends.insert(id, &raw mut (*state).frame);
            (*unit).waits += 1;
        }
        let unit = tasks::lang_unit_for(task);
        let st = &mut *state;
        // A retry after spinning is the same take-point position.
        let mut spun = false;
        loop {
            let u = &mut *unit;
            if st.frame.settled.is_none() {
                if !std::mem::take(&mut spun) {
                    u.reach();
                }
                pump(u);
            }
            let settled = st.frame.settled.take();
            if settled.is_some() {
                u.waits -= 1;
                let sockets = ptr::read(&st.sockets);
                #[cfg(feature = "record")]
                let replaying = u.ctx.parking;
                #[cfg(not(feature = "record"))]
                let replaying = false;
                return match settled {
                    Some(Took::Accepted(_)) => {
                        #[cfg(feature = "record")]
                        if replaying {
                            replay::departed(u, value, &client.0.rows);
                        }
                        free_owners(Some(&*context), value, owners);
                        out(0)
                    }
                    Some(Took::Closed(_)) => {
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        out(1)
                    }
                    _ => {
                        attach_sockets(context, value, offsets(), sockets.as_ref());
                        failed
                    }
                };
            }
            #[cfg(feature = "record")]
            let replaying = u.ctx.parking;
            #[cfg(not(feature = "record"))]
            let replaying = false;
            if !st.withdrawn && tasks::cancel_pending_for(context, task) {
                // A withdrawal that wins settles Withdrawn; one that loses
                // waits for the acceptance or close on its way.
                st.withdrawn = true;
                #[cfg(feature = "record")]
                let won = if replaying {
                    crate::tape::units::replay::withdraw_wins(u.tape(), st.id)
                } else {
                    u.ctx.withdraw(client, st.id)
                };
                #[cfg(not(feature = "record"))]
                let won = u.ctx.withdraw(client, st.id);
                if won {
                    tasks::check_cancel_for(context, site, task);
                }
                continue;
            }
            if !replaying
                && tasks::park_unit_alone(task, || {
                    (*unit)
                        .ctx
                        .port
                        .spin_before_park(Some(client.receiver_port()))
                })
            {
                spun = true;
                continue;
            }
            if REC {
                tasks::park_returning(
                    task,
                    &raw mut st.frame.waiters,
                    #[cfg(feature = "prof")]
                    (crate::prof::Reason::Join, 0),
                );
                return None;
            }
            tasks::park_for(
                task,
                &raw mut st.frame.waiters,
                #[cfg(feature = "prof")]
                (crate::prof::Reason::Join, 0),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Helper units
// ---------------------------------------------------------------------------

/// What a helper's owner and its thread share.
struct HelperShared(tasks::Member); // active in its run until its end is published

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
    /// MEM-30: the account that granted the helper its steps, kept open until
    /// the handle settles and the helper's residual goes back to it.
    steps: Option<(Arc<crate::steps::Account>, u64)>,
    /// The owner's host files, whose registry designates a capability the
    /// helper's result hands back.
    #[cfg(feature = "record")]
    files: *mut crate::host_file::Files,
}

#[derive(Clone)]
struct ValueOut {
    bytes: u64,
    owners: Vec<[u64; 4]>,
}

/// What the helper's thread hands back in the ledger's result value.
struct Ended {
    failed: bool,
    causes: Vec<CrossedCause>,
    /// The result's content.
    result: u64,
    /// The cleaned connections it left behind with clients elsewhere: each
    /// one's key and its retained charge, part of `n`.
    survivors: Vec<(usize, u64)>,
    run_grants: u64,
    /// Live charges for Secret designations and UDP pools paid by this helper.
    /// Its owner pays them from the end take on.
    charges: crate::secret::HandedOver,
    /// MEM-30: the unused part of its step grant, returned to the donor.
    residual: u64,
    /// Its outcome as the owner's end `TAKE` holds it, when it is recorded:
    /// the kind and body, or why it has none.
    #[cfg(feature = "record")]
    tape_end: Option<Result<crate::tape::helper::EndTake, &'static str>>,
    /// Replay: the survivors recorded on the owner's helper-end `TAKE`,
    /// partitioned from the helper's reservation at `end_take`.
    #[cfg(feature = "record")]
    replay_survivors: Vec<hirsel_tape_format::Survivor>,
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
/// As `hirsel_core_spawn`; `entry` is the link's static row for X and R.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_spawn_unit(
    context: *mut Context,
    _tasks: u64,
    n: u64,
    input: *mut u8,
    entry: *const SpawnEntry,
    _site: u64,
) -> SpawnUnit {
    // SAFETY: the spawner owns its context and `input` until the spawn commits.
    unsafe {
        let entry = &*entry;
        let (body, layout) = (entry.bridge, entry.layout);
        let x = &entry.x;
        let rx = &entry.r;
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
        let xl = live_rows(input, x.owners());
        let xs = item_charge(input, &xl);
        if xs > n {
            drop(held);
            return refused(xs, n);
        }
        tasks::before_commit();
        let u = unit();
        let (k, hctx) = u.ctx.spawn_unit();
        // MEM-30: the helper's private grant, debited now that it is admitted.
        let (donor, steps) = crate::steps::grant(&mut *context).unzip();
        // The entry writes each socket as it leaves: detached first.
        let sockets = detach_sockets(context, input, socket_offsets(&xl));
        #[cfg(feature = "record")]
        let replayed = replay::spawn_args(context, n, steps, input, entry, sockets.as_ref());
        #[cfg(feature = "record")]
        let tape = crate::tape::helper::spawn(context, n, steps, input, entry, sockets.as_ref());
        #[cfg(not(feature = "record"))]
        let tape = ();
        let data = pack(input, x.bytes, &xl);
        free_owners(None, input, &xl);
        let shared = Arc::new(HelperShared(tasks::Member::of_current()));
        let h = Box::into_raw(Box::new(HelperH {
            k,
            waiters: Waiters::default(),
            ended: false,
            port: hctx.port.clone(),
            charge: Some(held),
            r_size,
            steps: donor.zip(steps),
            result: ValueOut {
                bytes: rx.bytes,
                owners: rx.owned(),
            },
            #[cfg(feature = "record")]
            files: (*context).host_files,
        }));
        u.helpers.insert(k, h);
        let task = (h as usize | 1) as *mut tasks::Task;
        #[cfg(feature = "record")]
        if let Some(args) = replayed {
            u.waits += 1;
            replay::spawn(u, k, hctx, args, (input, x.owners()));
            return SpawnUnit {
                task,
                requested: 0,
                available: 0,
                status: 0,
            };
        }
        #[cfg(feature = "record")]
        if let Some(t) = &tape {
            crate::tape::units::spawn(tasks::current_group(), k, t.stream(), t.spawn_args());
        }
        u.waits += 1;
        let run = (*context).run;
        let foreign = (*context).foreign;
        let map_keys = (*context).map_keys();
        let registry = (*context)
            .host_files
            .as_ref()
            .map_or_else(crate::host_file::Registry::default, |files| {
                files.registry.for_unit()
            });
        // PACK-14: a Unit owns its loaded code generation from spawn through end
        // publication. Pin the bridge and the entry's code word if in the slab.
        let mut unit_pins = Vec::new();
        if let Some(pin) = crate::code::pin_pc(body as usize) {
            unit_pins.push(pin);
        }
        if x.bytes >= 8 {
            let code_word = input.cast::<usize>().read_unaligned();
            if let Some(pin) = crate::code::pin_pc(code_word) {
                unit_pins.push(pin);
            }
        }
        let job = SendPtr((
            hctx,
            data,
            sockets,
            x.bytes,
            x.owned(),
            rx.bytes,
            rx.owned(),
            run,
            foreign,
            map_keys,
            registry,
            body,
            layout,
            n,
            steps,
            shared,
            tape,
            unit_pins,
        ));
        let start = move || {
            let job = job;
            let (
                hctx,
                data,
                sockets,
                xb,
                xo,
                rb,
                ro,
                run,
                foreign,
                map_keys,
                registry,
                body,
                layout,
                n,
                steps,
                shared,
                tape,
                unit_pins,
            ) = job.0;
            helper_main(
                hctx, data, sockets, xb, xo, rb, ro, run, foreign, map_keys, registry, body,
                layout, n, steps, shared, tape,
            );
            drop(unit_pins);
        };
        // On the pool the helper is a unit beside its owner; without one it
        // has a thread of its own.
        let start: crate::units::Job = Box::new(start);
        crate::units::spawn_language_job(start);
        SpawnUnit {
            task,
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
    foreign: *const crate::foreign::Run,
    map_keys: crate::MapKeys,
    registry: crate::host_file::Registry,
    body: Body,
    layout: TaskLayout,
    n: u64,
    steps: Option<u64>,
    shared: Arc<HelperShared>,
    tape: TapeHelper,
) {
    // SAFETY: this thread owns the helper's context and run for its life.
    unsafe {
        // The task record and primary cell are the owner's (outside n).
        let mut context = Context::with_unit_heap(n + 256 + 32);
        let grant = crate::steps::install_grant(&mut context, steps);
        context.run = run;
        context.foreign = foreign;
        context.set_map_keys(map_keys);
        let mut files = crate::host_file::Files::with_registry(registry);
        #[cfg(feature = "record")]
        replay::bind_helper_files(&tape, &mut files);
        context.host_files = &mut files;
        if causes::task_start(&mut context).is_err() {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitLedger"));
        }
        let mut input = vec![0u8; (xb as usize).max(layout.input.bytes)];
        let payer = context.payer();
        let xs = live_charge(data.as_ptr(), &xo);
        if xs > 0 && block::admit_storage(payer, Some(xs)).is_err() {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitLedger"));
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
        let mut lu = LangUnit::new(hctx, Some(shared));
        #[cfg(feature = "record")]
        {
            lu.files = context.host_files;
            lu.set_tape(tape_group(&tape));
        }
        let mut result = vec![0u8; layout.result.bytes.max(rb as usize)];
        let provider = if run.is_null() {
            tasks::SchedulerProvider::default()
        } else {
            (*run).scheduler
        };
        let outcome = tasks::run_root_with_unit_contained(
            &mut context,
            body,
            input.as_mut_ptr(),
            result.as_mut_ptr(),
            layout.input.bytes,
            layout.result.bytes,
            provider,
            Some(&mut lu),
            tape_group(&tape),
        );
        let failed = match outcome {
            Ok(failed) => failed,
            Err(end) => {
                if let crate::run_stop::EndReason::RunStop(stop) = end {
                    crate::run_stop::record(stop);
                }
                files.close_all(&mut context);
                return;
            }
        };
        // Each connection still retained has its last client in another unit;
        // the owner holds its charge from the end take on. Its own copy here
        // goes back to this helper's limit, which ends with it.
        // So does each `Shared` node made here that another unit still holds.
        let survivors = lu
            .retained
            .drain()
            .map(|(k, h)| (k, h.bytes))
            .chain(lu.homed.drain())
            .collect();
        let charges = files.registry.run_grants().hand_over(context.payer());
        let residual = grant.as_deref().map_or(0, crate::steps::residual);
        let rl = live_rows(result.as_ptr(), if failed == 0 { &ro } else { &[] });
        // The END writes each socket in the result as it leaves: detached first.
        let sockets = if failed == 0 {
            detach_sockets(&mut context, result.as_ptr(), socket_offsets(&rl))
        } else {
            None
        };
        let ended = Ended {
            failed: failed != 0,
            causes: CrossedCause::pack_chain(&context.causes),
            result: if failed == 0 {
                item_charge(result.as_ptr(), &rl)
            } else {
                0
            },
            survivors,
            run_grants: context.payer().run_owned()
                + lu.run_grants.drain(..).map(|held| held.bytes).sum::<u64>(),
            charges,
            residual,
            #[cfg(feature = "record")]
            tape_end: tape.as_ref().map(|tape| {
                let causes: Vec<_> = context.causes.iter().collect();
                tape.end(
                    failed != 0,
                    &causes,
                    result.as_ptr(),
                    rb,
                    &ro,
                    &raw const context,
                    sockets.as_ref(),
                    residual,
                )
            }),
            #[cfg(feature = "record")]
            replay_survivors: Vec::new(),
        };
        let mut ret = Value::empty();
        if failed == 0 {
            let data = pack(result.as_ptr(), rb, &rl);
            ret.set_item(data, sockets);
            free_owners(None, result.as_ptr(), &rl);
        }
        RESULTS
            .lock()
            .unwrap()
            .insert(Arc::as_ptr(&lu.ctx.port) as usize, ended);
        files.close_all(&mut context);
        drop(files);
        #[cfg(feature = "record")]
        crate::tape::units::replay::drain_survivors(lu.tape);
        // Its registry designated through the helper's sources until here.
        drop(tape);
        drop(context);
        let LangUnit { ctx, .. } = *lu; // its helper stays active to the end
        ctx.end_helper(Some(ret), false);
    }
}

/// Consume an ended helper. A join delivers the result or the cause chain.
/// The spawn's charge is released except what the delivered owners keep.
unsafe fn consume(context: *mut Context, h: *mut HelperH, dest: Option<*mut u8>) -> (bool, Causes) {
    // SAFETY: the owner consumes the sole handle once.
    unsafe {
        let u = unit();
        u.helpers.remove(&(*h).k);
        u.waits -= 1;
        let mut hb = Box::from_raw(h);
        let ended = { RESULTS.lock().unwrap().take(Arc::as_ptr(&hb.port) as usize) }
            .unwrap_or_else(|| {
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitLedger"))
            });
        let ret = u.ctx.consume_helper(hb.k, false);
        if let Some((donor, _)) = hb.steps.take() {
            crate::steps::settle(donor, ended.residual);
        }
        let mut held = hb.charge.take().unwrap();
        let mut chain = Causes::default();
        if !ended.failed {
            if let Some(v) = ret.as_ref()
                && let Some(dest) = dest
                && !dest.is_null()
            {
                let s = live_charge(v.item().as_ptr(), &hb.result.owners);
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
        } else if dest.is_some() {
            for cause in ended.causes {
                let cell = Cell::prepaid(held.partition(charge::CAUSE_CELL));
                cause.unpack_into(&*context, &mut held, cell.as_ptr());
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

/// `join` on a helper's handle; `REC` and `resumed` as `wait_end`'s.
#[inline(always)]
pub(crate) unsafe fn join<const REC: bool>(
    context: *mut Context,
    task: *mut tasks::Task,
    result: *mut u8,
    site: u64,
    resumed: bool,
) -> Option<tasks::Join> {
    let h = helper_of(task);
    // SAFETY: as `hirsel_core_join`.
    unsafe {
        if !wait_end::<REC>(context, h, site, false, resumed)? {
            return Some(tasks::Join {
                kind: 0,
                causes: ptr::null_mut(),
                status: 1,
            });
        }
        let (failed, chain) = consume(context, h, Some(result));
        let cancelled = chain
            .iter()
            .next()
            .is_some_and(|c| c.kind() == causes::CANCELLED);
        Some(tasks::Join {
            kind: if cancelled { 2 } else { u8::from(failed) },
            causes: chain.into_raw(),
            status: 0,
        })
    }
}

#[doc(hidden)]
#[path = "units_lang_cause.rs"]
mod cause;
use cause::CrossedCause;
#[path = "units_lang_bench.rs"]
pub mod bench;

#[cfg(test)]
#[path = "units_lang_tests.rs"]
mod tests;

#[cfg(any(test, feature = "testing"))]
#[path = "units_lang_hold.rs"]
pub(crate) mod hold;
