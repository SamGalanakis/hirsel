//! Exclusive user-space instruction attribution for `--timings` (compile-gap
//! 611, "The single counted run needed first").
//!
//! Each compiler thread that enters a scope while timings record opens one
//! pinned `instructions:u` counter for itself (`perf_event_open`, this thread
//! only, kernel and hypervisor excluded). Every scope transition reads it and
//! charges the delta since the previous read to the scope that was active,
//! then changes the active scope. A delta therefore lands in exactly one
//! [`Owner`]; a nested scope's work is never also its parent's. Work a thread
//! does outside every scope goes to [`Owner::Driver`] on the thread that
//! started the request and to [`Owner::Outside`] on any other thread.
//!
//! Per thread, `last read - first read` is that thread's counted total, read
//! independently of the charges; the sum of every owner equals the sum of
//! those totals exactly (the reconciliation law). Instructions a thread ran
//! before its first scope, and the process's wrapper, are the residual against
//! an external process-tree count.
//!
//! The accounting is refused, not approximated, when it cannot be exact: the
//! counter was multiplexed (enabled time differs from running time), a pinned
//! counter fell into its error state, the PMU is unavailable, a scope exited
//! out of order, or the fixed stack overflowed. Storage is fixed: a static
//! owner table, a fixed per-thread stack and a census table allocated once
//! per request through the global allocator, so the memory limit charges it.
use super::Phase;
use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering::Relaxed};

/// The exclusive owner of a counted instruction delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Owner {
    /// The request thread outside every scope: driver and setup work.
    Driver,
    /// Any other counted thread outside every scope: pool and scheduling.
    Outside,
    /// A thread waiting for others ([`super::wait`]).
    Wait,
    /// An existing `--timings` phase, outside every finer scope inside it.
    Phase(Phase),
    /// Actual structural checking (H).
    StructuralProduction,
    /// A structural-cache hit: recipe probe, replay and restoration.
    StructuralHit,
    /// Receipt normalization, completion and binding of read rows.
    ReceiptBinding,
    /// Full independent V over an artifact.
    VExecution,
    /// Exact V receipt reuse: lookup and read re-observation.
    VReuse,
    /// Store/product transport: encoding, publication and lookup.
    Store,
    /// Content hashing.
    Hash,
    /// This diagnostic's own volume counting.
    Diagnostic,
}

const FINE: [Owner; 10] = [
    Owner::StructuralProduction,
    Owner::StructuralHit,
    Owner::ReceiptBinding,
    Owner::VExecution,
    Owner::VReuse,
    Owner::Store,
    Owner::Hash,
    Owner::Diagnostic,
    Owner::Driver,
    Owner::Outside,
];
const PHASES: usize = Phase::ALL.len();
/// Owner rows: Wait, every phase, then the fine owners.
const OWNERS: usize = 1 + PHASES + FINE.len();

impl Owner {
    fn index(self) -> usize {
        match self {
            Owner::Wait => 0,
            Owner::Phase(phase) => 1 + phase as usize,
            fine => {
                1 + PHASES
                    + FINE
                        .iter()
                        .position(|&f| f == fine)
                        .expect("every fine owner has a row")
            }
        }
    }
    fn all() -> impl Iterator<Item = Owner> {
        std::iter::once(Owner::Wait)
            .chain(Phase::ALL.iter().map(|&p| Owner::Phase(p)))
            .chain(FINE)
    }
    /// The stable name the report prints.
    pub fn name(self) -> String {
        match self {
            Owner::Driver => "driver".into(),
            Owner::Outside => "worker outside scopes".into(),
            Owner::Wait => "wait".into(),
            Owner::Phase(phase) => format!("phase {}", phase.name()),
            Owner::StructuralProduction => "structural production".into(),
            Owner::StructuralHit => "structural cache hit".into(),
            Owner::ReceiptBinding => "receipt normalization/binding".into(),
            Owner::VExecution => "V execution".into(),
            Owner::VReuse => "V exact reuse".into(),
            Owner::Store => "store/transport".into(),
            Owner::Hash => "hash".into(),
            Owner::Diagnostic => "diagnostic volume counting".into(),
        }
    }
}

/// How a structural production relates to earlier ones for its definition in
/// this request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Census {
    /// No earlier production of this definition in this mode.
    First,
    /// Its definition was invalidated by a signature read since the last one.
    SignatureInvalidated,
    /// The checked input (text and declared signature) changed.
    ChangedInput,
    /// Same input, no recorded invalidation: an unexplained repeat.
    Repeated,
    /// A structural-cache hit restored the result instead.
    Restored,
}

impl Census {
    pub const ALL: [Census; 5] = [
        Census::First,
        Census::SignatureInvalidated,
        Census::ChangedInput,
        Census::Repeated,
        Census::Restored,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Census::First => "first",
            Census::SignatureInvalidated => "signature-invalidation-triggered",
            Census::ChangedInput => "changed-input",
            Census::Repeated => "repeated unchanged input",
            Census::Restored => "restored",
        }
    }
}

/// A scalar volume of semantic or transport data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Volume {
    /// TIR instructions plus terminators in bodies V executed.
    TirNodes,
    /// CFG successor edges in bodies V executed.
    CfgEdges,
    /// Blocks in bodies V executed.
    TirBlocks,
    /// Independent uniqueness proof contexts V executed.
    ProofContexts,
    /// Bytes fed to content hashes.
    HashedBytes,
    /// Content hash computations.
    Hashes,
    /// Encoded product bytes published to a store or product domain.
    EncodedBytes,
    /// V forward ownership and backward result-permission block transfers.
    VTransferVisits,
    /// Rows V visited while joining flow states.
    VJoinRows,
    /// Ownership (Modes) forward and liveness node transfers.
    OwnershipTransferVisits,
    /// Ownership value joins.
    OwnershipJoins,
    /// Temporaries carried by ownership's transferred states and live sets.
    OwnershipStateRows,
}

impl Volume {
    pub const ALL: [Volume; 12] = [
        Volume::TirNodes,
        Volume::CfgEdges,
        Volume::TirBlocks,
        Volume::ProofContexts,
        Volume::HashedBytes,
        Volume::Hashes,
        Volume::EncodedBytes,
        Volume::VTransferVisits,
        Volume::VJoinRows,
        Volume::OwnershipTransferVisits,
        Volume::OwnershipJoins,
        Volume::OwnershipStateRows,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Volume::TirNodes => "TIR nodes validated",
            Volume::CfgEdges => "CFG edges validated",
            Volume::TirBlocks => "TIR blocks validated",
            Volume::ProofContexts => "V proof contexts",
            Volume::HashedBytes => "hashed bytes",
            Volume::Hashes => "hash computations",
            Volume::EncodedBytes => "encoded store bytes",
            Volume::VTransferVisits => "V transfer visits",
            Volume::VJoinRows => "V join rows",
            Volume::OwnershipTransferVisits => "ownership transfer visits",
            Volume::OwnershipJoins => "ownership value joins",
            Volume::OwnershipStateRows => "ownership state rows",
        }
    }
}

/// Fixed census capacity in definitions; a larger id refuses the census.
const CENSUS_DEFINITIONS: usize = 1 << 17;
/// Fixed per-thread scope depth; deeper nesting refuses the diagnostic.
const DEPTH: usize = 64;

struct Totals {
    owners: [AtomicU64; OWNERS],
    entries: [AtomicU64; OWNERS],
    census: [AtomicU64; Census::ALL.len()],
    volumes: [AtomicU64; Volume::ALL.len()],
    /// Sum over threads of each thread's independent `last - first` reads.
    threads_total: AtomicU64,
    threads: AtomicU64,
    reads: AtomicU64,
    census_overflow: AtomicU64,
}

static TOTALS: Totals = Totals {
    owners: [const { AtomicU64::new(0) }; OWNERS],
    entries: [const { AtomicU64::new(0) }; OWNERS],
    census: [const { AtomicU64::new(0) }; Census::ALL.len()],
    volumes: [const { AtomicU64::new(0) }; Volume::ALL.len()],
    threads_total: AtomicU64::new(0),
    threads: AtomicU64::new(0),
    reads: AtomicU64::new(0),
    census_overflow: AtomicU64::new(0),
};
static ACTIVE: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU32 = AtomicU32::new(0);
/// Why accounting is invalid, once it is.
static REFUSAL: Mutex<Option<String>> = Mutex::new(None);
/// Instructions one counter read costs, calibrated at request start.
static READ_COST: AtomicU64 = AtomicU64::new(0);

/// Per definition: bit 0 produced, bit 1 signature dirty; then the low 32
/// bits of the last produced input.
struct CensusTable {
    flags: Box<[AtomicU8]>,
    inputs: Box<[AtomicU32]>,
}
static CENSUS: Mutex<Option<std::sync::Arc<CensusTable>>> = Mutex::new(None);

fn refuse(reason: impl Into<String>) {
    let mut refusal = REFUSAL.lock().unwrap_or_else(|e| e.into_inner());
    refusal.get_or_insert_with(|| reason.into());
    ACTIVE.store(false, Relaxed);
}

/// One thread's counter and scope stack.
struct Thread {
    counter: Option<Counter>,
    epoch: u32,
    driver: bool,
    first: u64,
    last: u64,
    /// The span `last - first` already added to the thread totals.
    published: u64,
    stack: [Owner; DEPTH],
    depth: usize,
}

impl Thread {
    fn top(&self) -> Owner {
        if self.depth == 0 {
            if self.driver {
                Owner::Driver
            } else {
                Owner::Outside
            }
        } else {
            self.stack[self.depth - 1]
        }
    }
    /// Read once and charge the delta since the last read to the active
    /// owner. False when accounting must stop.
    fn charge(&mut self) -> bool {
        let epoch = EPOCH.load(Relaxed);
        if self.counter.is_none() || self.epoch != epoch {
            // A thread new to this request starts its own count here.
            if self.counter.is_none() {
                match Counter::open() {
                    Ok(counter) => self.counter = Some(counter),
                    Err(reason) => {
                        refuse(reason);
                        return false;
                    }
                }
            }
            let Some(now) = self.read() else {
                return false;
            };
            if self.epoch != epoch {
                self.epoch = epoch;
                self.driver = false;
                self.depth = 0;
                TOTALS.threads.fetch_add(1, Relaxed);
            }
            self.first = now;
            self.last = now;
            self.published = 0;
            return true;
        }
        let Some(now) = self.read() else {
            return false;
        };
        TOTALS.owners[self.top().index()].fetch_add(now - self.last, Relaxed);
        self.last = now;
        // The thread total comes from the raw first and current reads only.
        let span = now - self.first;
        TOTALS
            .threads_total
            .fetch_add(span - self.published, Relaxed);
        self.published = span;
        true
    }
    fn read(&mut self) -> Option<u64> {
        TOTALS.reads.fetch_add(1, Relaxed);
        match self.counter.as_ref().expect("opened counter").read() {
            Ok(value) => Some(value),
            Err(reason) => {
                refuse(reason);
                None
            }
        }
    }
}

thread_local! {
    static THREAD: RefCell<Thread> = const { RefCell::new(Thread {
        counter: None,
        epoch: u32::MAX,
        driver: false,
        first: 0,
        last: 0,
        published: 0,
        stack: [Owner::Driver; DEPTH],
        depth: 0,
    }) };
}

/// Whether counted attribution is running.
#[inline]
pub fn active() -> bool {
    ACTIVE.load(Relaxed)
}

/// Start counting for a request on its driving thread. Refusal leaves the
/// rest of `--timings` intact and is reported instead of a table.
pub(super) fn start_request() {
    ACTIVE.store(false, Relaxed);
    for cell in TOTALS
        .owners
        .iter()
        .chain(&TOTALS.entries)
        .chain(&TOTALS.census)
        .chain(&TOTALS.volumes)
        .chain([
            &TOTALS.threads_total,
            &TOTALS.threads,
            &TOTALS.reads,
            &TOTALS.census_overflow,
        ])
    {
        cell.store(0, Relaxed);
    }
    *REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let table = CensusTable {
        flags: (0..CENSUS_DEFINITIONS).map(|_| AtomicU8::new(0)).collect(),
        inputs: (0..CENSUS_DEFINITIONS).map(|_| AtomicU32::new(0)).collect(),
    };
    *CENSUS.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::new(table));
    EPOCH.fetch_add(1, Relaxed);
    THREAD.with(|t| {
        let mut thread = t.borrow_mut();
        // Calibrate one read's own user-space cost before counting starts.
        if thread.counter.is_none() {
            match Counter::open() {
                Ok(counter) => thread.counter = Some(counter),
                Err(reason) => return refuse(reason),
            }
        }
        let counter = thread.counter.as_ref().expect("opened counter");
        let mut samples = [0u64; 33];
        for sample in &mut samples {
            match counter.read() {
                Ok(value) => *sample = value,
                Err(reason) => return refuse(reason),
            }
        }
        READ_COST.store((samples[32] - samples[0]) / 32, Relaxed);
        ACTIVE.store(true, Relaxed);
        if thread.charge() {
            thread.driver = true;
        }
    });
}

/// Charge this thread's work up to now, then stop counting.
pub(super) fn finish_request() {
    if active() {
        THREAD.with(|t| t.borrow_mut().charge());
    }
    ACTIVE.store(false, Relaxed);
}

/// Attribute this thread's instructions to `owner` until the guard drops.
#[inline]
#[must_use = "the scope ends when the guard drops"]
pub fn scope(owner: Owner) -> Scope {
    if !active() {
        return Scope(None);
    }
    THREAD.with(|t| {
        let mut thread = t.borrow_mut();
        if !thread.charge() {
            return Scope(None);
        }
        if thread.depth == DEPTH {
            refuse("instruction scope stack exceeds its fixed depth");
            return Scope(None);
        }
        let depth = thread.depth;
        thread.stack[depth] = owner;
        thread.depth += 1;
        TOTALS.entries[owner.index()].fetch_add(1, Relaxed);
        Scope(Some((owner, thread.epoch)))
    })
}

/// This thread's active owner, for a helper thread to [`carry`].
pub fn current() -> Option<Owner> {
    if !active() {
        return None;
    }
    THREAD.with(|t| {
        let thread = t.borrow();
        (thread.epoch == EPOCH.load(Relaxed) && thread.counter.is_some()).then(|| thread.top())
    })
}

/// Attribute a helper thread's work to the owner its spawner had.
pub fn carry(owner: Option<Owner>) -> Scope {
    owner.map_or(Scope(None), scope)
}

/// An active attribution scope; dropping it charges the scope and resumes
/// the one it interrupted.
pub struct Scope(Option<(Owner, u32)>);

impl Scope {
    /// A scope that attributes nothing.
    pub fn none() -> Scope {
        Scope(None)
    }
}

impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {
        let Some((owner, epoch)) = self.0 else {
            return;
        };
        if !active() {
            return;
        }
        THREAD.with(|t| {
            let mut thread = t.borrow_mut();
            if thread.epoch != epoch {
                return;
            }
            if !thread.charge() {
                return;
            }
            if thread.depth == 0 || thread.stack[thread.depth - 1] != owner {
                refuse(format!(
                    "instruction scope {} exited out of order",
                    owner.name()
                ));
                return;
            }
            thread.depth -= 1;
        });
    }
}

/// Record how a structural production of `definition` (by session index)
/// with checked-input digest `input` relates to earlier ones.
pub fn census_production(definition: u32, input: u32) {
    if !active() {
        return;
    }
    let Some(table) = CENSUS.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return;
    };
    let index = definition as usize;
    if index >= CENSUS_DEFINITIONS {
        TOTALS.census_overflow.fetch_add(1, Relaxed);
        return;
    }
    let flags = table.flags[index].fetch_or(1, Relaxed);
    let previous = table.inputs[index].swap(input, Relaxed);
    let census = if flags & 1 == 0 {
        Census::First
    } else if table.flags[index].fetch_and(!2, Relaxed) & 2 != 0 {
        Census::SignatureInvalidated
    } else if previous != input {
        Census::ChangedInput
    } else {
        Census::Repeated
    };
    TOTALS.census[census as usize].fetch_add(1, Relaxed);
}

/// Record a structural-cache restoration instead of a production.
pub fn census_restored() {
    if active() {
        TOTALS.census[Census::Restored as usize].fetch_add(1, Relaxed);
    }
}

/// Note that `definition`'s memo was invalidated by a signature read.
pub fn census_signature_invalidated(definition: u32) {
    if !active() {
        return;
    }
    let Some(table) = CENSUS.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return;
    };
    if let Some(flags) = table.flags.get(definition as usize) {
        flags.fetch_or(2, Relaxed);
    }
}

/// Add `n` to a volume.
#[inline]
pub fn volume(volume: Volume, n: u64) {
    if active() {
        TOTALS.volumes[volume as usize].fetch_add(n, Relaxed);
    }
}

/// Volumes this thread's deterministic work counters accumulate between
/// [`WorkWindow::open`] and [`WorkWindow::close`].
pub struct WorkWindow(Option<[u64; WINDOW.len()]>);

const WINDOW: [(crate::work::Counter, Volume); 7] = {
    use crate::work::Counter as C;
    [
        (C::ValidateFlowSteps, Volume::VTransferVisits),
        (C::ValidateResultDemandSteps, Volume::VTransferVisits),
        (C::ValidateJoinRows, Volume::VJoinRows),
        (C::OwnershipFlowSteps, Volume::OwnershipTransferVisits),
        (C::OwnershipLivenessSteps, Volume::OwnershipTransferVisits),
        (C::OwnershipValueJoins, Volume::OwnershipJoins),
        (C::OwnershipStateRows, Volume::OwnershipStateRows),
    ]
};

impl WorkWindow {
    pub fn open() -> WorkWindow {
        WorkWindow(active().then(|| WINDOW.map(|(counter, _)| crate::work::read(counter))))
    }
    pub fn close(self) {
        let Some(start) = self.0 else { return };
        for ((counter, volume), start) in WINDOW.iter().zip(start) {
            self::volume(*volume, crate::work::read(*counter) - start);
        }
    }
}

/// The counted request.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// Why the diagnostic was refused; nothing else is meaningful then.
    pub refused: Option<String>,
    /// Exclusive instructions and scope entries per owner.
    pub owners: Vec<(Owner, u64, u64)>,
    /// Sum over threads of each thread's independently read total.
    pub threads_total: u64,
    pub threads: u64,
    /// Counter reads and the calibrated instructions one read costs.
    pub reads: u64,
    pub read_cost: u64,
    /// Structural-production census.
    pub census: Vec<(Census, u64)>,
    pub census_overflow: u64,
    pub volumes: Vec<(Volume, u64)>,
}

pub(super) fn report() -> Report {
    let refused = REFUSAL.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Report {
        refused,
        owners: Owner::all()
            .map(|owner| {
                (
                    owner,
                    TOTALS.owners[owner.index()].load(Relaxed),
                    TOTALS.entries[owner.index()].load(Relaxed),
                )
            })
            .collect(),
        threads_total: TOTALS.threads_total.load(Relaxed),
        threads: TOTALS.threads.load(Relaxed),
        reads: TOTALS.reads.load(Relaxed),
        read_cost: READ_COST.load(Relaxed),
        census: Census::ALL
            .iter()
            .map(|&c| (c, TOTALS.census[c as usize].load(Relaxed)))
            .collect(),
        census_overflow: TOTALS.census_overflow.load(Relaxed),
        volumes: Volume::ALL
            .iter()
            .map(|&v| (v, TOTALS.volumes[v as usize].load(Relaxed)))
            .collect(),
    }
}

use crate::instructions::InstructionCounter as Counter;

#[cfg(test)]
mod tests {
    use super::*;

    fn spin(n: u64) -> u64 {
        let mut x = 0u64;
        for i in 0..n {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
        x
    }

    // The reconciliation law: owners partition the counted thread totals
    // exactly. A nested scope whose delta also stayed in its parent would make
    // the owner sum exceed the independently read totals.
    #[test]
    fn exclusive_owners_sum_to_counted_thread_totals() {
        start_request();
        if let Some(reason) = report().refused {
            eprintln!("skipped: {reason}");
            return;
        }
        {
            let _check = scope(Owner::Phase(Phase::Check));
            spin(200_000);
            {
                let _v = scope(Owner::VExecution);
                spin(400_000);
                let _hash = scope(Owner::Hash);
                spin(100_000);
            }
            spin(100_000);
        }
        std::thread::spawn(|| {
            let _store = scope(Owner::Store);
            spin(300_000);
        })
        .join()
        .unwrap();
        finish_request();
        let report = report();
        assert_eq!(report.refused, None);
        let owned: u64 = report.owners.iter().map(|(_, n, _)| n).sum();
        assert_eq!(owned, report.threads_total);
        let get = |o: Owner| report.owners.iter().find(|r| r.0 == o).unwrap().1;
        // Each nested owner keeps only its own work: V's 400k-iteration spin
        // is at least twice the parent's exclusive 300k minus loop noise.
        assert!(get(Owner::VExecution) > get(Owner::Hash));
        assert!(get(Owner::VExecution) > get(Owner::Phase(Phase::Check)));
        assert!(get(Owner::Store) > 0 && report.threads >= 2);
    }
}
