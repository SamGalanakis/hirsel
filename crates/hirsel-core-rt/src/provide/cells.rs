//! MEM-24: slots are pinned for the run. A withdrawal retires the cell's
//! designations before its slot can serve another generation. A port slot
//! keeps its vocabulary so parked callers can retire their typed deadlines.
use super::*;
use crate::limit::Payer;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A cell, the generation its live designations carry (0 once withdrawn),
/// and its provider, which a grant resolves to so no wait points at the grant.
pub(crate) struct Slot {
    generation: AtomicU64,
    pub(super) cell: Cell,
    provider: Provider,
    kind: usize,
}

/// The run's slots, each charged cell's payer and charge, and the withdrawn
/// slots of each kind.
#[derive(Default)]
pub(crate) struct Table {
    slots: Vec<Box<Slot>>,
    charged: HashMap<usize, (Payer, u64)>,
    words: HashMap<usize, crate::host_file::CellWords>,
    free: [Vec<usize>; 7],
    next: u64,
}

/// The run's cell slots, each charged cell's payer and amount (`cells`).
/// Boxed so each provider's state pointer stays put.
#[derive(Default)]
pub(crate) struct Cells(pub(super) Mutex<ManuallyDrop<Table>>);
// SAFETY: a payer designates a limit record the run pins until teardown,
// which refunds it under the run's ownership.
unsafe impl Send for Cells {}
// SAFETY: as Send; every access goes through the mutex.
unsafe impl Sync for Cells {}

impl Drop for Cells {
    fn drop(&mut self) {
        if let Some(hooks) = SCHEDULER_HOOKS.get() {
            (hooks.drop_cells)(self);
        }
    }
}

pub(super) unsafe fn cell<'a>(state: *mut u8) -> &'a Cell {
    // SAFETY: the run pins every slot until teardown.
    unsafe { &slot(state).cell }
}

/// The cells of every run by the address of the limit record
/// they are charged to, with their run's table.
static BY_LIMIT: Mutex<BTreeMap<usize, Vec<(usize, usize)>>> = Mutex::new(BTreeMap::new());
/// How many entries `BY_LIMIT` holds: a limit end looks it up only when some
/// cell is withdrawable.
static WITHDRAWABLE: AtomicUsize = AtomicUsize::new(0);

/// A reusable cell's kind: the free list its withdrawn slot joins.
fn kind(cell: &Cell) -> usize {
    match cell {
        Cell::Clock(_) => 0,
        Cell::Random(_) => 1,
        Cell::Process(_) => 2,
        Cell::Port(port) => 3 + lock(port).vocab as usize,
    }
}

/// The slot a cell provider's state designates.
/// # Safety
/// `state` is a cell provider's: the run pins every slot until teardown.
pub(super) unsafe fn slot<'a>(state: *mut u8) -> &'a Slot {
    // SAFETY: by the contract.
    unsafe { &*state.cast::<Slot>() }
}

/// Whether a designation with `handle` still designates the slot's cell. Read
/// under the cell's own lock, which every withdrawal and reuse holds.
pub(super) fn live(state: *mut u8, handle: u64) -> bool {
    // SAFETY: the caller holds a cell provider's state.
    unsafe { slot(state) }.generation.load(Ordering::Relaxed) == handle
}

/// The outcome of an operation on a withdrawn cell, which `admit` turns into
/// `Bounds` at issue.
pub(crate) const WITHDRAWN: Outcome = Outcome {
    value: 0,
    error: IoError {
        tag: random::TAPE_BOUNDS,
        code: 0,
    },
    written: 0,
    status: 0,
    kind: 0,
};

impl Cells {
    /// Admit a cell's charge to the current limit and place the cell, reusing
    /// a withdrawn slot of its kind; answers its provider and the generation
    /// its designations carry. A refusal answers value 0 and the available
    /// bytes in `written`, from which the caller raises Capacity.
    pub(super) unsafe fn add(
        &self,
        context: *mut Context,
        make: impl FnOnce() -> Cell,
        charge: u64,
    ) -> Result<(Provider, u64), Outcome> {
        // SAFETY: a running context has a current payer.
        let payer = unsafe { (*context).payer() };
        if let Err(capacity) = payer.admit_run_owned(Some(charge)) {
            return Err(Outcome {
                written: capacity.available,
                ..Outcome::default()
            });
        }
        install_hooks();
        let cell = make();
        let kind = kind(&cell);
        let mut table = lock(&self.0);
        if table.next == PORT_ROOT - 1 {
            drop(table);
            drop(cell);
            payer.refund_run_owned(charge);
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostHandles"));
        }
        table.next += 1;
        let generation = if let Cell::Port(p) = &cell {
            let root = PORT_ROOT | table.next;
            lock(p).root = root;
            root
        } else {
            table.next
        };
        let reused = table.free[kind].pop();
        let address = match reused {
            Some(address) => {
                // SAFETY: the table owns the slot; its kind matches.
                let slot = unsafe { slot(address as *mut u8) };
                refill(slot, cell, generation);
                address
            }
            None => {
                let mut slot = Box::new(Slot {
                    generation: AtomicU64::new(generation),
                    cell,
                    kind,
                    provider: PROVIDER,
                });
                let address = &raw const *slot as usize;
                slot.provider.state = address as *mut u8;
                table.slots.push(slot);
                address
            }
        };
        table.charged.insert(address, (payer, charge));
        drop(table);
        lock(&BY_LIMIT)
            .entry(payer.address())
            .or_default()
            .push((address, std::ptr::from_ref(self) as usize));
        WITHDRAWABLE.fetch_add(1, Ordering::Relaxed);
        Ok((
            Provider {
                state: address as *mut u8,
                ..PROVIDER
            },
            generation,
        ))
    }

    /// Grant a new cell's designations: its word, and a held hand or trigger
    /// closed by `close_op` when there is one. A withdrawal retires both.
    pub(super) fn designate(
        &self,
        files: &crate::host_file::Files,
        (provider, generation): (Provider, u64),
        close_op: Option<u8>,
    ) -> Outcome {
        self.designate_with_terminal(
            files,
            (provider, generation),
            close_op,
            crate::host_file::Terminal::No,
        )
    }

    pub(super) fn designate_with_terminal(
        &self,
        files: &crate::host_file::Files,
        (provider, generation): (Provider, u64),
        close_op: Option<u8>,
        terminal: crate::host_file::Terminal,
    ) -> Outcome {
        let value = files
            .registry
            .grant_with_terminal(provider, generation, terminal);
        let held = close_op.map_or(0, |op| files.registry.held(provider, generation, op));
        lock(&self.0).words.insert(
            provider.state as usize,
            files.registry.cell_words(value, held),
        );
        Outcome {
            value,
            written: held,
            ..Outcome::default()
        }
    }

    /// Withdraw one cell: its designations stop designating it, its waits
    /// are readied (and fail at issue), its state is freed and its charge
    /// refunded to its limit, and its slot waits for the next cell of its
    /// kind. A port's calls settle and its `next` waits are readied, each
    /// failing with Bounds.
    fn withdraw(&self, address: usize) {
        // SAFETY: the table owns the slot until teardown.
        let slot = unsafe { slot(address as *mut u8) };
        let mut tokens = Vec::new();
        match &slot.cell {
            Cell::Clock(clock) => {
                let mut clock = lock(clock);
                slot.generation.store(0, Ordering::Relaxed);
                tokens.extend(clock.timers.iter().map(|&(_, _, t)| t));
                tokens.extend(clock.wall_timers.iter().map(|&(_, t)| t));
                tokens.extend(clock.idle.iter().map(|&(t, _)| t));
                while clock.channel_timers.first().is_some() {
                    // SAFETY: the clock lock pins the nonempty arm list.
                    unsafe { clock.channel_timers.fire_first() };
                }
                *clock = ClockCell::default();
            }
            Cell::Random(random) => {
                let mut random = lock(random);
                slot.generation.store(0, Ordering::Relaxed);
                *random = RandomStream::tape(Vec::new());
            }
            Cell::Process(process) => {
                let mut process = lock(process);
                slot.generation.store(0, Ordering::Relaxed);
                tokens = std::mem::take(&mut process.waiters);
                *process = ProcessCell::default();
            }
            Cell::Port(port) => {
                let retired;
                {
                    let mut p = lock(port);
                    slot.generation.store(0, Ordering::Relaxed);
                    (tokens, retired) = p.withdraw();
                }
                drop(retired);
            }
        }
        lock(&IDLE).retain(|_, (cell, _)| *cell != address);
        lock(&SERVERS).retain(|_, (cell, _)| *cell != address);
        let charged = {
            let mut table = lock(&self.0);
            if let Some(words) = table.words.remove(&address) {
                words.retire();
            }
            table.free[slot.kind].push(address);
            table.charged.remove(&address)
        };
        ready_all(tokens);
        if let Some((payer, charge)) = charged {
            payer.refund_run_owned(charge);
        }
        WITHDRAWABLE.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Port {
    /// Withdraw the port (MEM-24): seal it, settle every unsettled call with
    /// Bounds and free what the room held. A `Call` the server holds stays
    /// until it is closed or replied to, as a settled call's does. Answers
    /// the callers and `next` waits to ready, and the UDP retirements to drop
    /// once the port is unlocked.
    pub(super) fn withdraw(
        &mut self,
    ) -> (Vec<u64>, BTreeMap<u64, crate::udp_net::served::Retirement>) {
        self.sealed = true;
        self.withdrawn = true;
        let ids: Vec<u64> = self.calls.keys().copied().collect();
        let mut tokens: Vec<u64> = ids
            .iter()
            .filter_map(|&id| self.settle(id, WITHDRAWN))
            .collect();
        for id in ids {
            self.free_staged(id);
            self.prune(id);
        }
        tokens.extend(
            std::mem::take(&mut self.servers)
                .into_iter()
                .map(|(token, _)| token),
        );
        for words in std::mem::take(&mut self.derived_words) {
            words.retire();
        }
        self.grants.clear();
        self.handles = BTreeMap::new();
        self.keys = BTreeMap::new();
        self.used = 0;
        (tokens, std::mem::take(&mut self.udp_retired))
    }
}

/// A live slot holding `cell` with generation 0, as a request with handle 0
/// designates it.
#[cfg(test)]
pub(super) fn test_slot(cell: Cell) -> Box<Slot> {
    Box::new(Slot {
        generation: AtomicU64::new(0),
        kind: kind(&cell),
        cell,
        provider: PROVIDER,
    })
}

/// The slot-pinned provider a provide cell's grant resolves to.
pub(crate) fn pinned(provider: &Provider) -> Option<*const Provider> {
    // SAFETY: an owned provider's state is a slot the run pins.
    owns(provider).then(|| unsafe { &raw const slot(provider.state).provider })
}

/// What a retired word designates: its closes succeed and every other
/// operation fails with Bounds at issue.
pub(crate) struct Retired(pub(crate) Provider);
// SAFETY: the provider has no state.
unsafe impl Sync for Retired {}
pub(crate) static RETIRED: Retired = Retired(Provider {
    state: ptr::null_mut(),
    wait: retired_wait,
    issue: retired_issue,
    resume: retired_issue,
    abort: retired_abort,
    deliver: retired_deliver,
});
unsafe extern "C" fn retired_wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
    Wait::default()
}
unsafe extern "C" fn retired_issue(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: the caller pins the request.
    match unsafe { (*request).operation } {
        CLOSE | CLOSE_CELL | CLOSE_SERVED | CLOSE_CALL | RELEASE => done(Outcome::default()),
        _ => done(WITHDRAWN),
    }
}
unsafe extern "C" fn retired_abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    Outcome::default()
}
unsafe extern "C" fn retired_deliver(_: *mut u8, _: *mut Request, outcome: Outcome) -> Outcome {
    outcome
}

/// Reinitialize a withdrawn slot with a new cell of its kind.
fn refill(slot: &Slot, cell: Cell, generation: u64) {
    match (&slot.cell, cell) {
        (Cell::Clock(old), Cell::Clock(new)) => {
            let mut old = lock(old);
            *old = new.into_inner().unwrap_or_else(|e| e.into_inner());
            slot.generation.store(generation, Ordering::Relaxed);
        }
        (Cell::Random(old), Cell::Random(new)) => {
            let mut old = lock(old);
            *old = new.into_inner().unwrap_or_else(|e| e.into_inner());
            slot.generation.store(generation, Ordering::Relaxed);
        }
        (Cell::Process(old), Cell::Process(new)) => {
            let mut old = lock(old);
            *old = new.into_inner().unwrap_or_else(|e| e.into_inner());
            slot.generation.store(generation, Ordering::Relaxed);
        }
        (Cell::Port(old), Cell::Port(new)) => {
            let mut old = lock(old);
            let mut new = new.into_inner().unwrap_or_else(|e| e.into_inner());
            new.next_id = old.next_id;
            new.first_call = old.next_id;
            new.calls = std::mem::take(&mut old.calls);
            *old = new;
            slot.generation.store(generation, Ordering::Relaxed);
        }
        _ => unreachable!("a free slot is reused by its own kind"),
    }
}

/// The limit whose record is `payer` ends (MEM-20): withdraw the cells
/// charged to it before its unused bytes are counted. Returns whether it
/// refunded any.
pub(crate) fn withdraw_limit(payer: Payer) -> bool {
    if WITHDRAWABLE.load(Ordering::Relaxed) == 0 {
        return false;
    }
    let Some(cells) = lock(&BY_LIMIT).remove(&payer.address()) else {
        return false;
    };
    for (address, table) in cells {
        // SAFETY: a run's table outlives its limits; teardown removes its entries.
        unsafe { (*(table as *const Cells)).withdraw(address) };
    }
    true
}

/// Run teardown: forget the run's idle and server waits, refund every charge
/// still held and drop its withdrawable entries.
pub(super) fn forget_cells(cells: &mut Cells) {
    let own = std::ptr::from_ref(&*cells) as usize;
    let table = cells.0.get_mut().unwrap_or_else(|e| e.into_inner());
    let states: Vec<usize> = table
        .slots
        .iter()
        .map(|s| &raw const **s as usize)
        .collect();
    lock(&IDLE).retain(|_, (cell, _)| !states.contains(cell));
    lock(&SERVERS).retain(|_, (cell, _)| !states.contains(cell));
    let mut dropped = 0;
    lock(&BY_LIMIT).retain(|_, entries| {
        let before = entries.len();
        entries.retain(|&(_, table)| table != own);
        dropped += before - entries.len();
        !entries.is_empty()
    });
    WITHDRAWABLE.fetch_sub(dropped, Ordering::Relaxed);
    for (_, (payer, charge)) in table.charged.drain() {
        payer.refund(charge);
    }
}

pub(super) fn drop_cells(cells: &mut Cells) {
    // SAFETY: Cells owns the table. Its Drop runs once, and its constructor
    // installs this callback before putting any cell in the table.
    unsafe { ManuallyDrop::drop(cells.0.get_mut().unwrap_or_else(|e| e.into_inner())) };
}

#[cfg(test)]
#[path = "port_slot_tests.rs"]
mod port_slot_tests;

/// `provide.stdout/stderr/dir(bytes n)`: a port with room `n`, admitted with
/// its cell's charge from the current limit until that limit ends. `value`
/// is the capability's grant (id 0, a Dir port's root) and `written` the
/// `Served` key; a refusal answers as `Cells::add`.
/// # Safety
/// Context is running.
pub(super) unsafe fn provide_port(
    context: *mut Context,
    n: u64,
    vocab: Vocab,
    terminal: crate::host_file::Terminal,
) -> Outcome {
    // SAFETY: forwarded.
    let files = unsafe { files(context) };
    // A sealed port answers Closed (IoError tag 4), or for a Net Reset
    // (NetError tag 5).
    let closed = if matches!(vocab, Vocab::Net | Vocab::Exec) {
        5
    } else {
        4
    };
    let mut port = Port {
        vocab,
        closed: Outcome {
            error: IoError {
                tag: closed,
                code: 0,
            },
            ..Outcome::default()
        },
        room: n,
        ..Port::default()
    };
    if vocab == Vocab::Dir {
        port.handles.insert(0, Handle::new(Kind::Dir));
    }
    let charge = PROVIDE_CELL.saturating_add(n);
    // SAFETY: forwarded.
    let provider = match unsafe {
        files.registry.run_grants().cells.add(
            context,
            || Cell::Port(Box::new(Mutex::new(port))),
            charge,
        )
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    files.registry.run_grants().cells.designate_with_terminal(
        files,
        provider,
        Some(CLOSE_SERVED),
        terminal,
    )
}
