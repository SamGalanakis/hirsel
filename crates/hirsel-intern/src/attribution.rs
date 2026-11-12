//! Which unit of work is running on this thread, and the reads between units.
//!
//! An installation producer records every store row it touches. With a trace
//! installed, the runtime names the query it runs around each task and reports
//! each read edge, including reads served from memory. A row is attributed to
//! the work running when it was touched, and the rows a part of the work needs
//! are those attributed to everything it reaches, whichever reader touched a
//! shared row first.
//!
//! Only the work that publishes products is traced. Elsewhere naming work
//! and reporting reads cost a thread-local read; a store use outside any
//! trace is reported as untraced, so a publishing path the trace does not
//! reach is visible rather than lost.
use std::cell::{Cell, RefCell};
use std::sync::Arc;

/// A unit of work: a query kind and address, or a caller-defined part of one
/// query (a kind no runtime query uses).
pub type Node = (u32, [u8; 32]);

/// The kind of a part of a published product: the unit its uses are
/// summarized by. No query has this kind.
pub const PART: u32 = u32::MAX;

/// The traced operation itself, outside any query or part.
pub const ROOT: Node = (FILL, [0; 32]);

/// Receives the edges of the work traced on the threads that carry it.
pub trait Trace: Send + Sync {
    fn edge(&self, reader: Node, dependency: Node);
}

thread_local! {
    static TRACE: RefCell<Option<Arc<dyn Trace>>> = const { RefCell::new(None) };
    static RUNNING: Cell<Option<Node>> = const { Cell::new(None) };
    /// Edges this thread already reported to its current trace, direct-mapped:
    /// a hit re-reads the same entries many times.
    static REPORTED: RefCell<Vec<Option<(Node, Node)>>> = const { RefCell::new(Vec::new()) };
}
const REPORTED_SLOTS: usize = 4096;

fn forget_reported() {
    REPORTED.with_borrow_mut(Vec::clear);
}

/// FxHash: the trace's keys are already hashes, serials or session ids.
#[derive(Default)]
pub struct Fx(u64);
impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }
    fn write_u64(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_u32(&mut self, word: u32) {
        self.write_u64(u64::from(word));
    }
    fn write_u16(&mut self, word: u16) {
        self.write_u64(u64::from(word));
    }
    fn write_usize(&mut self, word: usize) {
        self.write_u64(word as u64);
    }
}
pub type FxBuild = std::hash::BuildHasherDefault<Fx>;

/// The work running on this thread, if a trace is installed.
#[inline]
pub fn running() -> Option<Node> {
    RUNNING.get()
}

/// Whether this thread's work is traced.
#[inline]
pub fn tracing() -> bool {
    TRACE.with_borrow(Option::is_some)
}

/// Record that the running work read `dependency`.
#[inline]
pub fn read(dependency: Node) {
    let Some(reader) = RUNNING.get() else {
        return;
    };
    let slot = {
        use std::hash::BuildHasher;
        FxBuild::default().hash_one((reader.0, &reader.1[..8], dependency.0, &dependency.1[..8]))
            as usize
            % REPORTED_SLOTS
    };
    let fresh = REPORTED.with_borrow_mut(|reported| {
        if reported.is_empty() {
            reported.resize(REPORTED_SLOTS, None);
        }
        let entry = Some((reader, dependency));
        (reported[slot] != entry)
            .then(|| reported[slot] = entry)
            .is_some()
    });
    if !fresh {
        return;
    }
    TRACE.with_borrow(|trace| {
        if let Some(trace) = trace {
            trace.edge(reader, dependency);
        }
    });
}

/// Run `node` until the guard drops, then resume the work it interrupted.
/// Without an installed trace this names nothing.
#[inline]
pub fn run_as(node: Node) -> Running {
    if !tracing() {
        return Running(None);
    }
    Running(Some(RUNNING.replace(Some(node))))
}

pub struct Running(Option<Option<Node>>);

impl Drop for Running {
    #[inline]
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            RUNNING.set(previous);
        }
    }
}

/// The kinds of cache fills: `FILL | layer`. No query kind has these bits.
pub const FILL: u32 = 0xfffe_0000;

/// Every engine layer that serves a value without running its query.
pub mod layer {
    /// A definition's signature header row (`HeaderProducts` slot).
    pub const HEADER_PRODUCT: u16 = 1;
    /// A snapshot's validation universe (`SignatureSnapshot::validation_universe`).
    pub const VALIDATION_UNIVERSE: u16 = 2;
    /// A type component written or decoded once per `TypeRecords` table.
    pub const TYPE_COMPONENT: u16 = 3;
    /// A row of a validation `FactTable` (verifier replays, execution and
    /// read facts, snapshot overlays, structural bodies), per table and key.
    pub const FACT_TABLE: u16 = 4;
}

/// The work that filled one cached entry. An engine cache that serves a
/// value without running the query that produced it keeps this beside the
/// entry, and every hit reads it, so a part that is served from the cache
/// reaches the rows the fill used, whichever part filled it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Filled(Option<Node>);

impl Filled {
    /// The entry of cache `layer` for `key`, when it is one node per key
    /// for the whole run (a table whose slot is filled once).
    #[inline]
    pub fn keyed(layer: u16, key: impl std::hash::Hash) -> Self {
        if !tracing() {
            return Self(None);
        }
        use std::hash::BuildHasher;
        let hash = FxBuild::default().hash_one((layer, key));
        let mut address = [0; 32];
        address[..8].copy_from_slice(&hash.to_le_bytes());
        Self(Some((FILL | u32::from(layer), address)))
    }
    /// A caller-named unit of work (a kind no query uses), entered like a fill.
    #[inline]
    pub fn of(node: Node) -> Self {
        Self(tracing().then_some(node))
    }
    /// Run `fill` as this entry's work, read by the work that asked for it.
    #[inline]
    pub fn filling<R>(self, fill: impl FnOnce() -> R) -> R {
        let Some(node) = self.0 else {
            return fill();
        };
        read(node);
        let _running = run_as(node);
        fill()
    }
    /// Record that the running work was served this entry.
    #[inline]
    pub fn hit(self) {
        if let Some(node) = self.0 {
            read(node);
        }
    }
}

/// Trace this thread's work into `trace` until the guard drops.
pub fn traced(trace: Arc<dyn Trace>) -> Traced {
    forget_reported();
    Traced(Some((TRACE.replace(Some(trace)), RUNNING.replace(None))))
}

/// The trace and running work a spawned helper thread continues.
#[derive(Clone)]
pub struct Carried(Option<(Arc<dyn Trace>, Option<Node>)>);

pub fn carried() -> Carried {
    Carried(
        TRACE
            .with_borrow(|trace| trace.clone())
            .map(|trace| (trace, RUNNING.get())),
    )
}

impl Carried {
    /// Continue the spawner's trace on this thread until the guard drops.
    pub fn enter(self) -> Traced {
        match self.0 {
            None => Traced(None),
            Some((trace, running)) => {
                forget_reported();
                Traced(Some((TRACE.replace(Some(trace)), RUNNING.replace(running))))
            }
        }
    }
}

pub struct Traced(Option<(Option<Arc<dyn Trace>>, Option<Node>)>);

impl Drop for Traced {
    fn drop(&mut self) {
        if let Some((trace, running)) = self.0.take() {
            forget_reported();
            TRACE.set(trace);
            RUNNING.set(running);
        }
    }
}

/// The distinct edges of a traced run, over interned work.
#[derive(Default)]
pub struct Edges(std::sync::Mutex<EdgeSet>);

#[derive(Default)]
pub struct EdgeSet {
    pub work: std::collections::HashMap<Node, u32, FxBuild>,
    pub edges: std::collections::HashSet<(u32, u32), FxBuild>,
}

impl EdgeSet {
    pub fn intern(&mut self, node: Node) -> u32 {
        let next = u32::try_from(self.work.len()).unwrap_or(u32::MAX);
        *self.work.entry(node).or_insert(next)
    }
}

impl Edges {
    pub fn take(&self) -> EdgeSet {
        std::mem::take(&mut self.0.lock().unwrap_or_else(|error| error.into_inner()))
    }
}

impl Trace for Edges {
    fn edge(&self, reader: Node, dependency: Node) {
        let mut set = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let edge = (set.intern(reader), set.intern(dependency));
        set.edges.insert(edge);
    }
}
