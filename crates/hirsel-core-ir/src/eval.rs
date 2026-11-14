//! A test-only evaluator, written from the op documentation (SEM-1, SEM-2, LIR's op
//! docs; for the FORM v1 draft FAIL-8..14, MEM-19..29 and CONC-3..9) so the
//! samples' hand-derived run lines can be checked. Memory is blocks of bytes laid
//! out by the layout recipes. Limits keep exact `used` totals and hand over by
//! forwarding. A spawned child runs to its end at the spawn, so the event order is
//! the program order and nothing waits. A delivered `Causes` owns the child's used
//! cells until it is dropped. Compact data is region bytes read and written through
//! `byte` steps, as a producer lowers it. Each static is a read-only block made
//! at the start of the run: a store into one, or a `free` of one, stops the run.
//! Sequence storage is cleaned by its carrier; list and map operations are the
//! producer's code. Provider rows run against a modelled provider ([`World`]).

use crate::*;
use hirsel_core_layout::{
    Body as Shaped, Cleanup, FieldTy, Handle, Layout, Recipe, Repr, TypeRef, charge, cleanup_class,
    compute, field_size, map_entry,
};
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Returned(Vec<u64>),
    /// The run failed with this cause chain, primary cause first.
    Failed(Vec<String>),
}

pub struct Report {
    pub outcome: Outcome,
    /// The root limit's `used` after the run (MEM-19).
    pub used: u64,
    /// Root peak reservation (MEM-19), including bound/task/grant reservations.
    pub peak_reserved: u64,
    /// The first u64 field of each resource, in the order its cleanup started.
    pub trace: Vec<u64>,
    pub admissions: usize,
    /// Bytes the program wrote to its `Stdout` and `Stderr`.
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// The site of each cause of a failed run, parallel to its chain (SITES.md):
    /// the anchor of the raising operation, or of the call that reached it when
    /// that operation is core's or unanchored; `None` when no such anchor exists.
    pub sites: Vec<Option<Anchor>>,
}

/// What the run's oracle checks covered (crate docs, "Loans").
#[derive(Debug, Default, PartialEq)]
pub struct Checked {
    /// Lent places: each `view` and `take` place frozen for its call, each `mut`
    /// place and destination checked against the call's other places.
    pub loans: u64,
    /// Accesses through an address with a storage role, each checked against the
    /// bytes the activation's other roles touched (crate docs, "Roles").
    pub roles: u64,
}

/// The modelled provider (CAP-5), deterministic so runs can be derived by hand:
/// - `Args` is the run's argument table: each argument a run-owned `String`
///   carrier of capacity 0; the handle's value is not read.
/// - `stdout` and `stderr` writes are captured whole: `written` is the length.
/// - A clock read returns the number of clock reads before it, both clocks alike.
/// - Directories hold flat names: a key is (`Dir` handle, path bytes). `open`
///   with `Read` needs a file (`NotFound`), `CreateNew` needs no entry (`Exists`),
///   `Write` truncates or creates, `Append` creates and writes at the end; a
///   directory is `Denied`. `read` fills the buffer's spare capacity from the
///   file's cursor. `try_lock` takes the lock unless another open file of the
///   same key holds it (then 0). `mkdtemp` appends `t<k>` to the name, k from 0,
///   and returns a new `Dir`; it fails with `Bounds` when the name has fewer than
///   32 spare bytes. `mkdir` of an existing key is `Exists`; `remove` and
///   `remove_tree` of a missing one `NotFound`.
/// - Nothing is ever cancelled.
#[derive(Default)]
struct World {
    args: Vec<u64>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    clock: u64,
    nodes: std::collections::BTreeMap<(u64, Vec<u8>), Node>,
    files: Vec<Option<OpenFile>>,
    temporaries: u64,
}
enum Node {
    File(Vec<u8>),
    Dir,
}
struct OpenFile {
    key: (u64, Vec<u8>),
    at: usize,
    locked: bool,
}
/// The model host's largest block: an admitted allocation above it stops the run.
const HOST_BYTES: u64 = 1 << 32;
const FILE_BASE: u64 = 0x50_0000;
const DIR_BASE: u64 = 0x60_0000;

const FN_BASE: u64 = 0x1000;
const REGION_BASE: u64 = 0x10_0000;
const TASK_BASE: u64 = 0x20_0000;
/// Sender of channel `i` is `CHAN_BASE + 2i`, its receiver `CHAN_BASE + 2i + 1`.
const CHAN_BASE: u64 = 0x30_0000;

/// One cause of a chain: its `CauseKind` case and, for `Fail`, the message's
/// `String` carrier `{data, length, capacity}`, which the cause owns (FAIL-10).
#[derive(Clone, Copy, Debug)]
struct Cause {
    kind: &'static str,
    message: [u64; 3],
    site: Option<Anchor>,
}
/// `CauseKind` in declaration order, its case ordinals.
const CAUSE_KINDS: [&str; 7] = [
    "Overflow",
    "Bounds",
    "Division",
    "Unwrap",
    "Fail",
    "Cancelled",
    "Capacity",
];
/// A cause cell: core's `Cause` (32 bytes: the `Fail` carrier at 0, `Site` at 24),
/// then the next cell's pointer. `CauseKind` is the carrier's capacity niche (FORM
/// #75): another kind `k` is the capacity `CAUSE_KIND_START + k`.
const CELL_NEXT: u64 = 32;
const CAUSE_CAPACITY: u64 = 16;
const CAUSE_SITE: u64 = 24;

/// A cause's `Site` word (SITES.md): `(item + 1) << 32 | start` for an anchor in
/// item `item` starting `start` bytes into it, 0 for none.
fn site_word(site: Option<Anchor>) -> u64 {
    site.map_or(0, |a| {
        ((a.item.index() as u64 + 1) << 32) | u64::from(a.start)
    })
}
const CAUSE_KIND_START: u64 = 1 << 63;

/// Why an instruction stopped: a new cause, or a callee's failure already chained.
#[derive(Debug)]
enum Stop {
    Cause(&'static str),
    Chained,
}
type X<T> = Result<T, Stop>;

struct Blk {
    bytes: Vec<u8>,
    live: bool,
    /// The limit the block is charged to; None for frame slots and engine records.
    limit: Option<usize>,
    charge: u64,
    res: Option<Res>,
    /// A counted node's kind, fixed by its `alloc`; its operations must name it.
    counted: Option<Counted>,
    /// The cleanup class its `alloc` named (crate docs, "Cleanup classes"): `None`
    /// for a block no `alloc` row made; `Some(None)` for region storage.
    class: Option<Option<Cleanup>>,
    /// Static data (`static dN`): read, never written or freed.
    fixed: bool,
    /// The liveness shadow of `mark` (crate docs, "Slot kernels"): each marked
    /// slot's offset, whether it holds a value, and its size.
    marks: BTreeMap<usize, (bool, usize)>,
}
struct Res {
    grant: u64,
    settled: bool,
    cell_used: bool,
}
struct Limit {
    cap: u64,
    used: u64,
    parent: Option<usize>,
    forward: Option<usize>,
    grant: bool,
}
struct Task {
    limits: Vec<usize>,
    chain: Vec<Cause>,
    /// Used cause cells beyond the primary, with the limit each is charged to.
    cells: Vec<(usize, u64)>,
    /// The anchor each running activation of the task is at, innermost last.
    at: Vec<Option<Anchor>>,
}
struct Child {
    status: u8,
    result: u64,
    r: TypeRef,
    charge: u64,
    n: u64,
    limit: usize,
    joined: bool,
    /// The child's used cells: its primary and each failed finalizer's.
    cells: Vec<(usize, u64)>,
    /// Its causes, primary first, while no `Causes` holds them.
    chain: Vec<Cause>,
}
struct Chan {
    item: TypeRef,
    cap: u64,
    queue: VecDeque<Vec<u8>>,
    senders: u64,
    receiver: bool,
    charge: u64,
    limit: usize,
}

#[derive(Clone, Copy)]
enum At {
    Decl(TypeRef),
    Ptr(TypeRef),
    /// Holds an Rc/Arc node pointer; the payload is at node + 8.
    Counted(TypeRef),
    /// An inline array: its element type and length are `Machine::arrays[i]`.
    Array(usize),
    Other,
}

struct Machine<'m> {
    m: &'m Module,
    recipes: Vec<Recipe>,
    mem: Vec<Blk>,
    limits: Vec<Limit>,
    peak_reserved: u64,
    regions: Vec<Vec<usize>>,
    children: Vec<Child>,
    chans: Vec<Chan>,
    /// Each live delivered `Causes`, by its head cell: the charged cells it owns.
    causes: HashMap<u64, Vec<(usize, u64)>>,
    /// Every inline array field type of the module: element and length.
    arrays: Vec<(FieldTy, u64)>,
    trace: Vec<u64>,
    /// The last destination identity handed out (`rt.dest_id`).
    dests: u64,
    admissions: usize,
    injection: Option<(usize, bool)>,
    world: World,
    /// The `view` places of the calls running now, as (block, offset, size).
    frozen: Vec<(usize, usize, usize)>,
    loans: u64,
    /// Each function's derived roles, filled on its first call.
    roles: Vec<Option<std::rc::Rc<[Option<roles::Role>]>>>,
    /// Per running activation: its roles, and by block the byte spans each role
    /// touched (`lo -> (hi, role)`, disjoint, a role's adjacent spans merged).
    footprints: Vec<Footprint>,
    role_checks: u64,
}

struct Footprint {
    roles: std::rc::Rc<[Option<roles::Role>]>,
    spans: HashMap<usize, BTreeMap<usize, (usize, roles::Role)>>,
}

fn collect_arrays(ty: &FieldTy, out: &mut Vec<(FieldTy, u64)>) {
    if let FieldTy::Array(e, n) = ty {
        if !out.iter().any(|(x, k)| x == &**e && k == n) {
            out.push(((**e).clone(), *n));
        }
        collect_arrays(e, out);
    }
}

pub fn run(m: &Module, f: FuncId, args: &[u64], limit: u64) -> Report {
    run_injected(m, f, args, limit, None)
}
/// A run, with what its oracle checks covered.
pub fn run_checked(m: &Module, f: FuncId, args: &[u64], limit: u64) -> (Report, Checked) {
    run_world(m, f, args, limit, None, &[])
}
/// A run whose `Args` table holds `argv`.
pub fn run_args(m: &Module, f: FuncId, args: &[u64], limit: u64, argv: &[&str]) -> Report {
    run_world(m, f, args, limit, None, argv).0
}
/// Deterministic admission refusal for native/evaluator agreement fixtures.
pub fn run_injected(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    injection: Option<(usize, bool)>,
) -> Report {
    run_world(m, f, args, limit, injection, &[]).0
}
fn run_world(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    injection: Option<(usize, bool)>,
    argv: &[&str],
) -> (Report, Checked) {
    let mut mach = Machine {
        m,
        recipes: compute(&m.types).expect("layouts"),
        mem: Vec::new(),
        peak_reserved: 0,
        limits: vec![Limit {
            cap: limit,
            used: 0,
            parent: None,
            forward: None,
            grant: false,
        }],
        regions: Vec::new(),
        children: Vec::new(),
        chans: Vec::new(),
        causes: HashMap::new(),
        arrays: Vec::new(),
        trace: Vec::new(),
        dests: 0,
        admissions: 0,
        injection,
        world: World::default(),
        frozen: Vec::new(),
        loans: 0,
        roles: vec![None; m.functions.len()],
        footprints: Vec::new(),
        role_checks: 0,
    };
    for s in &m.statics {
        let p = mach.block(s.bytes.len() as u64, None, 0, None);
        mach.mem[split(p).0].bytes.clone_from(&s.bytes);
        mach.mem[split(p).0].fixed = true;
    }
    for a in argv {
        let data = mach.block(a.len() as u64, None, 0, None);
        mach.mem[split(data).0].bytes = a.as_bytes().to_vec();
        let carrier = mach.block(24, None, 0, None);
        mach.store(carrier, 8, data);
        mach.store(carrier + 8, 8, a.len() as u64);
        mach.mem[split(data).0].fixed = true;
        mach.mem[split(carrier).0].fixed = true;
        mach.world.args.push(carrier);
    }
    for shape in &m.types {
        let fields: Vec<&hirsel_core_layout::Field> = match &shape.body {
            Shaped::Record(f) => f.iter().collect(),
            Shaped::Variant(c) => c.iter().flat_map(|c| &c.fields).collect(),
        };
        for f in fields {
            collect_arrays(&f.ty, &mut mach.arrays);
        }
    }
    let mut task = Task {
        limits: vec![0],
        chain: Vec::new(),
        cells: Vec::new(),
        at: Vec::new(),
    };
    let outcome = match mach.call(&mut task, f, args) {
        Ok(r) => Outcome::Returned(r),
        Err(_) => Outcome::Failed(task.chain.iter().map(|c| c.kind.into()).collect()),
    };
    let sites = task.chain.iter().map(|c| c.site).collect();
    let checked = Checked {
        loans: mach.loans,
        roles: mach.role_checks,
    };
    let report = Report {
        outcome,
        used: mach.limits[0].used,
        peak_reserved: mach.peak_reserved,
        trace: mach.trace,
        admissions: mach.admissions,
        stdout: mach.world.stdout,
        stderr: mach.world.stderr,
        sites,
    };
    (report, checked)
}

fn ptr(blk: usize, off: u64) -> u64 {
    ((blk as u64 + 1) << 32) | off
}
fn split(p: u64) -> (usize, usize) {
    assert!(p >> 32 != 0, "{p:#x} is not a memory pointer");
    (((p >> 32) - 1) as usize, (p & 0xffff_ffff) as usize)
}
fn bytes_of(t: Ty) -> usize {
    (width(t) / 8) as usize
}
fn cause(r: RaiseCause) -> &'static str {
    match r {
        RaiseCause::Unwrap => "Unwrap",
        RaiseCause::Bounds => "Bounds",
        RaiseCause::Fail(_) => "Fail",
    }
}

impl Machine<'_> {
    // ---- memory
    /// Rc and Arc counts differ in atomicity only, which one task cannot observe;
    /// what the evaluator checks is that every operation names its node's kind.
    /// A `String` or `Bytes` carrier's bytes, data pointer, length and capacity.
    fn carrier(&self, c: u64) -> (Vec<u8>, u64, u64, u64) {
        let (data, len, cap) = (self.load(c, 8), self.load(c + 8, 8), self.load(c + 16, 8));
        let bytes = match len {
            0 => Vec::new(),
            n => self.get(data, n as usize).to_vec(),
        };
        (bytes, data, len, cap)
    }
    /// A provider row against [`World`]: an outcome `value, error, code, written`.
    fn host(&mut self, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        const NOT_FOUND: u64 = 1;
        const DENIED: u64 = 2;
        const EXISTS: u64 = 3;
        let ok = |value: u64, written: u64| vec![value, 0, 0, written];
        let error = |e: u64| vec![0, e, 0, 0];
        let file = |h: u64| (h - FILE_BASE) as usize;
        Ok(match op {
            HostOp::WallMs | HostOp::MonoNs => {
                self.world.clock += 1;
                vec![self.world.clock - 1]
            }
            HostOp::Stdout | HostOp::Stderr => {
                let (bytes, ..) = self.carrier(a[1]);
                let n = bytes.len() as u64;
                match op {
                    HostOp::Stdout => self.world.stdout.extend(bytes),
                    _ => self.world.stderr.extend(bytes),
                }
                ok(0, n)
            }
            HostOp::Open => {
                let key = (a[0], self.carrier(a[1]).0);
                let w = &mut self.world;
                let at = match (w.nodes.get(&key), a[2]) {
                    (Some(Node::Dir), _) => return Ok(error(DENIED)),
                    (None, 0) => return Ok(error(NOT_FOUND)),
                    (Some(_), 3) => return Ok(error(EXISTS)),
                    (Some(Node::File(c)), 2) => c.len(),
                    (Some(_), 0) => 0,
                    _ => {
                        let keep = a[2] == 2;
                        let n = w.nodes.entry(key.clone()).or_insert(Node::File(Vec::new()));
                        if let Node::File(c) = n
                            && !keep
                        {
                            c.clear();
                        }
                        0
                    }
                };
                w.files.push(Some(OpenFile {
                    key,
                    at,
                    locked: false,
                }));
                ok(FILE_BASE + w.files.len() as u64 - 1, 0)
            }
            HostOp::Read => {
                let (_, data, len, cap) = self.carrier(a[1]);
                let i = file(a[0]);
                let f = self.world.files[i].as_ref().expect("an open file");
                let Some(Node::File(c)) = self.world.nodes.get(&f.key) else {
                    return Ok(error(NOT_FOUND));
                };
                let n = (cap.saturating_sub(len) as usize).min(c.len() - f.at);
                let bytes = c[f.at..f.at + n].to_vec();
                if n > 0 {
                    self.put(data + len, &bytes);
                    self.store(a[1] + 8, 8, len + n as u64);
                }
                self.world.files[i].as_mut().unwrap().at += n;
                ok(n as u64, 0)
            }
            HostOp::Write => {
                let (bytes, ..) = self.carrier(a[1]);
                let i = file(a[0]);
                let f = self.world.files[i].as_mut().expect("an open file");
                let Some(Node::File(c)) = self.world.nodes.get_mut(&f.key) else {
                    return Ok(error(NOT_FOUND));
                };
                let end = f.at + bytes.len();
                if c.len() < end {
                    c.resize(end, 0);
                }
                c[f.at..end].copy_from_slice(&bytes);
                f.at = end;
                ok(0, bytes.len() as u64)
            }
            HostOp::Close => {
                let i = file(a[0]);
                self.world.files[i] = None;
                ok(0, 0)
            }
            HostOp::TryLock | HostOp::Unlock => {
                let i = file(a[0]);
                let key = self.world.files[i]
                    .as_ref()
                    .expect("an open file")
                    .key
                    .clone();
                let held =
                    self.world.files.iter().enumerate().any(|(j, f)| {
                        j != i && f.as_ref().is_some_and(|f| f.locked && f.key == key)
                    });
                let take = op == HostOp::TryLock;
                if take && held {
                    return Ok(ok(0, 0));
                }
                self.world.files[i].as_mut().unwrap().locked = take;
                ok(u64::from(take), 0)
            }
            HostOp::Mkdir => {
                let key = (a[0], self.carrier(a[1]).0);
                if self.world.nodes.contains_key(&key) {
                    return Ok(error(EXISTS));
                }
                self.world.nodes.insert(key, Node::Dir);
                ok(0, 0)
            }
            HostOp::Remove | HostOp::RemoveTree => {
                let key = (a[0], self.carrier(a[1]).0);
                match self.world.nodes.remove(&key) {
                    Some(_) => ok(0, 0),
                    None => error(NOT_FOUND),
                }
            }
            HostOp::Mkdtemp => {
                let (_, data, len, cap) = self.carrier(a[1]);
                if len + 32 > cap {
                    return Err(Stop::Cause("Bounds"));
                }
                let k = self.world.temporaries;
                self.world.temporaries += 1;
                let name = format!("t{k}").into_bytes();
                self.put(data + len, &name);
                self.store(a[1] + 8, 8, len + name.len() as u64);
                self.world.nodes.insert((a[0], name), Node::Dir);
                ok(DIR_BASE + k, 0)
            }
        })
    }
    fn kind(&self, p: u64, kind: Counted) {
        let made = self.mem[split(p).0].counted;
        assert_eq!(
            made,
            Some(kind),
            "a counted operation names another kind than its alloc"
        );
    }
    fn block(&mut self, size: u64, limit: Option<usize>, charge: u64, res: Option<Res>) -> u64 {
        // An admitted block the host cannot provide is a run stop, as for
        // `hirsel_core_native_alloc`, never `Capacity`: a refusal is the limit's
        // (give the run `limit N`).
        assert!(
            size <= HOST_BYTES,
            "host out of memory: a block of {size} bytes was admitted (a run stop, not Capacity)"
        );
        self.mem.push(Blk {
            bytes: vec![0; size as usize],
            live: true,
            limit,
            charge,
            res,
            counted: None,
            class: None,
            fixed: false,
            marks: BTreeMap::new(),
        });
        ptr(self.mem.len() - 1, 0)
    }
    fn get(&self, p: u64, n: usize) -> &[u8] {
        let (b, o) = split(p);
        assert!(self.mem[b].live, "use of a freed block");
        &self.mem[b].bytes[o..o + n]
    }
    fn load(&self, p: u64, n: usize) -> u64 {
        let mut x = [0u8; 8];
        x[..n].copy_from_slice(self.get(p, n));
        u64::from_le_bytes(x)
    }
    fn put(&mut self, p: u64, bytes: &[u8]) {
        let (b, o) = split(p);
        assert!(self.mem[b].live, "use of a freed block");
        assert!(!self.mem[b].fixed, "a store into static data");
        if let Some(&(_, at, n)) = self
            .frozen
            .iter()
            .find(|&&(fb, fo, fn_)| fb == b && o < fo + fn_ && fo < o + bytes.len())
        {
            panic!(
                "a write of +{o}..+{} into a lent view or take place +{at}..+{} while its call runs",
                o + bytes.len(),
                at + n
            );
        }
        self.mem[b].bytes[o..o + bytes.len()].copy_from_slice(bytes);
    }
    fn store(&mut self, p: u64, n: usize, v: u64) {
        self.put(p, &v.to_le_bytes()[..n]);
    }
    /// A producer's read of `n` bytes at `p` (`read`, or `copy` from `p`) must not
    /// touch a slot marked empty. Marked slots do not overlap, so walking back from
    /// the read's end stops at the first slot that ends at or before its start.
    fn read_marked(&self, p: u64, n: usize) {
        let (b, o) = split(p);
        for (&at, &(live, size)) in self.mem[b].marks.range(..o + n).rev() {
            if at + size <= o {
                break;
            }
            assert!(
                live,
                "read of the empty slot at +{at}: a slot kernel read a slot it took"
            );
        }
    }
    /// `mark`: a put (`live`) needs a slot not marked live, a take (`empty`) one not
    /// marked empty. An unmarked slot may be either (storage a `List` handed over).
    /// A zero-size slot has no storage to tell it from its neighbours, so it is not
    /// shadowed.
    fn mark(&mut self, p: u64, live: bool, size: usize) {
        if size == 0 {
            return;
        }
        let (b, o) = split(p);
        assert!(self.mem[b].live, "mark of a freed block");
        let was = self.mem[b].marks.insert(o, (live, size)).map(|m| m.0);
        match (was, live) {
            (Some(true), true) => panic!("put over the live slot at +{o}"),
            (Some(false), false) => panic!("take of the empty slot at +{o}"),
            _ => {}
        }
    }
    /// The slot at `p` leaves the shadow in the state the kernel claims; a slot never
    /// marked may be either, so it passes.
    fn unmark(&mut self, p: u64, live: bool, size: usize) {
        if size == 0 {
            return;
        }
        let (b, o) = split(p);
        assert!(self.mem[b].live, "unmark of a freed block");
        match (self.mem[b].marks.remove(&o).map(|m| m.0), live) {
            (Some(false), true) => {
                panic!("unmark live of the empty slot at +{o}: a hole handed back as a value")
            }
            (Some(true), false) => {
                panic!("unmark empty of the live slot at +{o}: a value left behind")
            }
            _ => {}
        }
    }
    /// A `drop` of a value, or cleanup of a `List` element, at a marked slot is a
    /// take: the slot must be marked live, and is then marked empty. Cleanup that
    /// reaches a slot a kernel emptied is the bug this catches.
    fn drop_marked(&mut self, p: u64, size: usize) {
        if size == 0 {
            return;
        }
        let (b, o) = split(p);
        if let Some(m) = self.mem[b].marks.get_mut(&o) {
            assert!(
                m.0,
                "drop of the empty slot at +{o}: cleanup reached a slot a kernel took"
            );
            m.0 = false;
        }
    }
    /// The block an owning edge cleans: its native prefix was fixed by the class its
    /// `alloc` named, and cleanup through the edge reads the edge's class, so the two
    /// must agree (crate docs, "Cleanup classes").
    fn edge(&self, p: u64, edge: &FieldTy, what: &str) {
        if let Some(made) = self.mem[split(p).0].class {
            let cleaned = cleanup_class(&self.m.types, &self.recipes, edge);
            assert!(
                made == cleaned,
                "{what} was allocated as {made:?} storage, but its edge ({edge:?}) cleans \
                 {cleaned:?} storage: the block's prefix does not fit its cleanup"
            );
        }
    }
    /// Cleanup glue's free: native glue refunds the charge it computes from the
    /// recipe, not the one the block was admitted with, so a block allocated with
    /// another charge (or, given `size`, another native size) than the recipe's is a
    /// fault here, where the evaluator would otherwise refund what was admitted and
    /// hide the difference.
    fn free_as(&mut self, p: u64, charge: Option<u64>, size: Option<u64>, what: &str) {
        let blk = &self.mem[split(p).0];
        let charge = charge.expect("a cleanup charge in range");
        assert_eq!(
            blk.charge, charge,
            "cleanup of {what} refunds {charge}, but its block was charged {}: \
             the allocation and the recipe disagree",
            blk.charge
        );
        if let Some(size) = size {
            assert_eq!(
                blk.bytes.len() as u64,
                size,
                "cleanup of {what} frees a block of the recipe's {size} bytes, but it was \
                 allocated with {}",
                blk.bytes.len()
            );
        }
        self.free(p);
    }
    /// Free a block and refund its charge, less a settled grant and a used cause
    /// cell, which left with the finalizer's hand-over and with the chain.
    fn free(&mut self, p: u64) {
        let (b, o) = split(p);
        assert!(o == 0 && self.mem[b].live, "free of a non-block");
        assert!(
            !self.frozen.iter().any(|f| f.0 == b),
            "free of a block holding a lent view or take place while its call runs"
        );
        assert!(!self.mem[b].fixed, "a free of static data");
        if let Some((at, _)) = self.mem[b].marks.iter().find(|(_, m)| m.0) {
            panic!("free of a block whose slot at +{at} is live: a slot kernel left a value in it");
        }
        let blk = &mut self.mem[b];
        blk.live = false;
        blk.marks.clear();
        let mut n = blk.charge;
        if let Some(r) = &blk.res {
            n -= if r.settled { r.grant } else { 0 };
            n -= if r.cell_used { charge::CAUSE_CELL } else { 0 };
        }
        if let Some(l) = blk.limit {
            self.refund(l, n);
        }
    }

    // ---- limits
    fn find(&self, mut l: usize) -> usize {
        while let Some(f) = self.limits[l].forward {
            l = f;
        }
        l
    }
    /// Admit `n` against `l`, or Err(the room left). A successful admission reports
    /// no room: `available` is 0 then.
    fn admit(&mut self, l: usize, n: u64) -> Result<(), u64> {
        let lim = &mut self.limits[l];
        let room = lim.cap - lim.used;
        self.admissions += 1;
        let refused = self
            .injection
            .is_some_and(|(nth, later)| self.admissions == nth || (later && self.admissions > nth));
        if n <= room && !refused {
            lim.used += n;
            self.peak_reserved = self.peak_reserved.max(self.limits[0].used);
            Ok(())
        } else {
            Err(room)
        }
    }
    fn refund(&mut self, l: usize, n: u64) {
        let l = self.find(l);
        self.limits[l].used -= n;
    }
    fn open_limit(&mut self, cap: u64, parent: usize, grant: bool) -> usize {
        self.limits.push(Limit {
            cap,
            used: 0,
            parent: Some(parent),
            forward: None,
            grant,
        });
        self.limits.len() - 1
    }
    /// MEM-20: refund the unused part to the parent and forward the record to it.
    fn end_limit(&mut self, l: usize) {
        let p = self.find(self.limits[l].parent.expect("not the root"));
        let unused = self.limits[l].cap - self.limits[l].used;
        self.limits[p].used -= unused;
        self.limits[l].forward = Some(p);
    }

    // ---- types
    fn shape(&self, t: TypeRef) -> &Shape {
        &self.m.types[t.index()]
    }
    /// The case of the variant of type `ty` at `base`.
    fn case_at(&self, base: u64, ty: TypeRef) -> u32 {
        let tag = self.fixed(ty).tag.expect("a variant");
        hirsel_core_layout::case_of(&tag, self.load(base + tag.offset, tag.width as usize))
    }
    fn fixed(&self, t: TypeRef) -> &hirsel_core_layout::Fixed {
        match &self.recipes[t.index()].layout {
            Layout::Fixed(f) => f,
            Layout::Compact(_) => panic!("compact values are not modelled"),
        }
    }
    fn is_resource(&self, t: TypeRef) -> bool {
        self.shape(t).repr == Repr::Resource
    }
    /// A value of `t` is held as a pointer to its block: a resource, or a stable
    /// value, which never moves once published (MEM-5).
    fn by_block(&self, t: TypeRef) -> bool {
        matches!(self.shape(t).repr, Repr::Resource | Repr::Stable)
    }
    /// Bytes a holder spends on a value of `t`: a resource or stable value is a
    /// pointer to its block.
    fn held(&self, t: TypeRef) -> u64 {
        if self.by_block(t) {
            8
        } else {
            self.fixed(t).size
        }
    }
    fn logical(&self, t: TypeRef) -> u64 {
        if self.is_resource(t) {
            8
        } else if self.by_block(t) {
            16
        } else {
            self.recipes[t.index()].logical_size.expect("fixed")
        }
    }
    fn at_of(&self, t: TypeRef) -> At {
        if self.by_block(t) {
            At::Ptr(t)
        } else {
            At::Decl(t)
        }
    }
    /// What a place of field type `f` holds, for the steps that follow it.
    fn at_field(&self, f: &FieldTy) -> At {
        match f {
            FieldTy::Inline(x) => self.at_of(*x),
            FieldTy::Box(x) => At::Ptr(*x),
            FieldTy::Rc(x) | FieldTy::Arc(x) => At::Counted(*x),
            FieldTy::Array(e, n) => At::Array(
                self.arrays
                    .iter()
                    .position(|(x, k)| x == &**e && k == n)
                    .expect("collected"),
            ),
            _ => At::Other,
        }
    }
    fn stride(&self, e: &FieldTy) -> u64 {
        field_size(&self.m.types, &self.recipes, e).0
    }
    fn place_size(&self, p: PlaceTy) -> u64 {
        match p {
            PlaceTy::Value(t) => bytes_of(t) as u64,
            PlaceTy::Declared(t) => self.held(TypeRef(t.0)),
        }
    }

    // ---- cleanup (FAIL-8, FAIL-12): each returns whether a finalizer failed
    fn drop_value(&mut self, t: &mut Task, at: u64, ty: TypeRef) -> bool {
        if self.recipes[ty.index()].drop.is_none() {
            return false;
        }
        if self.is_resource(ty) {
            let blk = self.load(at, 8);
            return self.drop_resource(t, blk, ty);
        }
        // An own row's core finalizer (crate docs, "Slot kernels") gets the value's
        // address first, with no grant and no cause cell. wf refuses one that may fail,
        // so it only moves slots; the fields' cleanup does the dropping.
        if let Some(f) = self.shape(ty).finalizer {
            let _ = self.call(t, FuncId(f.0), &[at]);
        }
        self.drop_fields(t, at, ty)
    }
    fn drop_fields(&mut self, t: &mut Task, base: u64, ty: TypeRef) -> bool {
        let row = match self.fixed(ty).tag {
            Some(_) => self.case_at(base, ty) as usize,
            None => 0,
        };
        let fields = self.recipes[ty.index()].drop.as_ref().unwrap().fields[row].clone();
        let mut failed = false;
        for f in fields {
            failed |= self.drop_field(t, base + f.offset, &f.ty);
        }
        failed
    }
    fn drop_field(&mut self, t: &mut Task, at: u64, ty: &FieldTy) -> bool {
        match ty {
            FieldTy::Scalar(_)
            | FieldTy::Fn
            | FieldTy::Stable(_)
            | FieldTy::Locator(_)
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Handle(Handle::Capability) => false,
            FieldTy::Inline(x) => self.drop_value(t, at, *x),
            // SEM-9: an inline array is cleaned from index 0 up.
            FieldTy::Array(e, n) => {
                let stride = self.stride(e);
                let mut failed = false;
                for i in 0..*n {
                    failed |= self.drop_field(t, at + i * stride, e);
                }
                failed
            }
            // The provider closes the file and ignores the outcome (core.md,
            // MEM-24); the trace records the close by its handle.
            FieldTy::Handle(Handle::HostFile) => {
                let h = self.load(at, 8);
                self.trace.push(h);
                if let Some(file) = h
                    .checked_sub(FILE_BASE)
                    .and_then(|i| self.world.files.get_mut(i as usize))
                {
                    *file = None;
                }
                false
            }
            FieldTy::Box(x) => {
                let p = self.load(at, 8);
                self.edge(p, ty, "a box");
                let failed = self.drop_value(t, p, *x);
                let size = (!self.is_resource(*x)).then(|| self.held(*x));
                self.free_as(p, charge::boxed(self.logical(*x)), size, "a box");
                failed
            }
            FieldTy::Handle(Handle::Task) => {
                // CONC-8: the child has already ended; drop its result unjoined.
                let i = (self.load(at, 8) - TASK_BASE) as usize;
                let (status, result, r) = {
                    let c = &self.children[i];
                    (c.status, c.result, c.r)
                };
                // CONC-8: an abandoned handle cleans the result or the chain.
                let failed = status == 0 && self.drop_value(t, result, r);
                for c in std::mem::take(&mut self.children[i].chain) {
                    if c.message[2] != 0 {
                        self.free(c.message[0]);
                    }
                }
                for (l, n) in self.settle_child(i) {
                    self.refund(l, n);
                }
                failed
            }
            FieldTy::Seq(element) => {
                let block = self.load(at, 8);
                let length = self.load(at + 8, 8);
                let capacity = self.load(at + 16, 8);
                let stride = self.stride(element);
                if stride == 0 {
                    // SEM-4: no element storage, nothing to clean (capacity 2^64 - 1).
                    return false;
                }
                if capacity != 0 {
                    self.edge(block, ty, "list storage");
                }
                let mut failed = false;
                for i in 0..length {
                    self.drop_marked(block + i * stride, stride as usize);
                    failed |= self.drop_field(t, block + i * stride, element);
                }
                // Capacity 0 has no storage: empty, or static data the carrier does
                // not own (core.md "Literals").
                if capacity != 0 {
                    let element = field_size(&self.m.types, &self.recipes, element).2;
                    self.free_as(block, charge::seq(capacity, element), None, "list storage");
                }
                failed
            }
            // SEM-5, SEM-9: live entries in insertion order, each key before its
            // value (`map_entry`); removed entries (state 0) were cleaned by
            // `map.remove`. Storage exists exactly when the capacity is not 0.
            FieldTy::Map(k, v) => {
                let block = self.load(at, 8);
                let slots = self.load(at + 8, 8);
                let capacity = self.load(at + 16, 8);
                let e = map_entry(&self.m.types, &self.recipes, k, v);
                if capacity != 0 {
                    self.edge(block, ty, "map storage");
                }
                let mut failed = false;
                for i in 0..slots {
                    let entry = block + i * e.stride;
                    if self.load(entry + 8, 8) == 1 {
                        failed |= self.drop_field(t, entry + e.key, k);
                        failed |= self.drop_field(t, entry + e.value, v);
                    }
                }
                if capacity != 0 {
                    let key = field_size(&self.m.types, &self.recipes, k).2;
                    let value = field_size(&self.m.types, &self.recipes, v).2;
                    self.free_as(
                        block,
                        charge::map(capacity, key, value),
                        None,
                        "map storage",
                    );
                }
                failed
            }
            FieldTy::OwnFn => {
                let environment = self.load(at + 8, 8);
                if environment != 0 {
                    let glue = FuncId((self.load(environment, 8) - FN_BASE) as u32);
                    self.call(t, glue, &[environment])
                        .expect("mechanical closure cleanup");
                }
                false
            }
            FieldTy::Rc(x) | FieldTy::Arc(x) => {
                let node = self.load(at, 8);
                self.release(node, *x);
                false
            }
            FieldTy::Handle(Handle::Causes) => {
                let head = self.load(at, 8);
                let cells = self.causes.remove(&head).expect("Causes dropped twice");
                let mut cell = head;
                while cell != 0 {
                    let next = self.load(cell + CELL_NEXT, 8);
                    let capacity = self.load(cell + CAUSE_CAPACITY, 8);
                    if capacity != 0 && capacity < CAUSE_KIND_START {
                        let data = self.load(cell, 8);
                        self.free(data);
                    }
                    self.free(cell);
                    cell = next;
                }
                for (l, n) in cells {
                    self.refund(l, n);
                }
                false
            }
            FieldTy::Handle(Handle::Sender) => {
                let i = ((self.load(at, 8) - CHAN_BASE) / 2) as usize;
                self.chans[i].senders -= 1;
                self.chan_freed(i);
                false
            }
            FieldTy::Handle(Handle::Receiver) => {
                let i = ((self.load(at, 8) - CHAN_BASE) / 2) as usize;
                self.chans[i].receiver = false;
                let mut failed = false;
                // CONC-9: last published first; a failure appends and the rest go on.
                while let Some(bytes) = self.chans[i].queue.pop_back() {
                    let tmp = self.block(bytes.len() as u64, None, 0, None);
                    self.put(tmp, &bytes);
                    failed |= self.drop_value(t, tmp, self.chans[i].item);
                }
                self.chan_freed(i);
                failed
            }
            other => panic!("cleanup of {other:?} is not modelled"),
        }
    }
    fn drop_resource(&mut self, t: &mut Task, blk: u64, ty: TypeRef) -> bool {
        let (b, _) = split(blk);
        // Report::trace names a declared u64 field, never carrier/pointer bytes.
        if let Shaped::Record(fields) = &self.shape(ty).body
            && let Some(i) = fields
                .iter()
                .position(|f| f.ty == FieldTy::Scalar(Scalar::U64))
        {
            let offset = self.fixed(ty).offsets[0][i];
            self.trace.push(self.load(blk + offset, 8));
        }
        let mut failed = false;
        if let Some(f) = self.shape(ty).finalizer {
            let grant = self.mem[b].res.as_ref().expect("a resource block").grant;
            let g = self.open_limit(grant, self.mem[b].limit.expect("charged"), true);
            t.limits.push(g);
            failed = self.call(t, FuncId(f.0), &[blk]).is_err();
            t.limits.pop();
            self.end_limit(g);
            let res = self.mem[b].res.as_mut().unwrap();
            res.settled = true;
            res.cell_used |= failed;
            if failed {
                // FAIL-10: the resource's cell now holds a cause and leaves with the chain.
                let l = self.mem[b].limit.unwrap();
                t.cells.push((l, charge::CAUSE_CELL));
            }
        }
        failed |= self.drop_fields(t, blk, ty);
        let storage = self.recipes[ty.index()]
            .logical_size
            .expect("resource storage");
        let charge = charge::resource(storage, self.shape(ty).grant);
        self.free_as(blk, charge, None, "a resource");
        failed
    }
    /// Refund a joined or abandoned task's record, result slot and primary cell; the
    /// reserved `n` came back when the child's limit ended. A failed child's primary
    /// cell is among its used cells, which are returned to go with its `Causes`.
    fn settle_child(&mut self, i: usize) -> Vec<(usize, u64)> {
        let c = &mut self.children[i];
        assert!(!c.joined, "task handle used twice");
        c.joined = true;
        let cell = if c.status == 0 { 0 } else { charge::CAUSE_CELL };
        let (l, n) = (c.limit, c.charge - c.n - cell);
        let cells = std::mem::take(&mut c.cells);
        self.refund(l, n);
        cells
    }
    /// MEM-16: drop one holder; at zero clean the payload and free the node. The
    /// payload holds no resource, so nothing here fails or runs user code.
    fn release(&mut self, node: u64, ty: TypeRef) {
        self.edge(node, &FieldTy::Rc(ty), "a counted node");
        let count = self.load(node, 8) - 1;
        self.store(node, 8, count);
        if count == 0 {
            let mut scratch = Task {
                limits: Vec::new(),
                chain: Vec::new(),
                cells: Vec::new(),
                at: Vec::new(),
            };
            assert!(
                !self.drop_value(&mut scratch, node + 8, ty),
                "a counted payload failed"
            );
            let charge = charge::counted(self.recipes[ty.index()].logical_size.expect("fixed"));
            self.free_as(node, charge, None, "a counted node");
        }
    }
    fn chan_freed(&mut self, i: usize) {
        let c = &self.chans[i];
        if c.senders == 0 && !c.receiver {
            let (l, n) = (c.limit, c.charge);
            self.refund(l, n);
        }
    }

    // ---- engine operations (CONC-3..6)
    fn rt(&mut self, t: &mut Task, op: RtOp, a: &[u64]) -> X<Vec<u64>> {
        let cur = *t.limits.last().unwrap();
        Ok(match op {
            RtOp::Spawn { x, r } => {
                let x = TypeRef(x.0);
                let r = TypeRef(r.0);
                let n = a[0];
                let charge = charge::task(self.logical(r), n).unwrap_or(u64::MAX);
                if let Err(room) = self.admit(cur, charge) {
                    return Ok(vec![0, room]); // x stays the caller's
                }
                let lim = self.open_limit(n, cur, false);
                let xb = self.block(self.held(x), None, 0, None);
                let bytes = self.get(a[1], self.held(x) as usize).to_vec();
                self.put(xb, &bytes);
                let rb = self.block(self.held(r), None, 0, None);
                let mut child = Task {
                    limits: vec![lim],
                    chain: Vec::new(),
                    cells: Vec::new(),
                    at: Vec::new(),
                };
                let f = FuncId((a[2] - FN_BASE) as u32);
                let xc = self.m.types[x.index()].carried;
                let rc = self.m.types[r.index()].carried;
                let mut args = Vec::with_capacity(3);
                if matches!(rc, Carried::Address) {
                    args.push(rb)
                }
                args.push(0); // A plain callable's environment.
                match xc {
                    Carried::Address => args.push(xb),
                    Carried::Value(t) => args.push(self.load(xb, bytes_of(t))),
                    Carried::Nothing => {}
                }
                let returned = self.call(&mut child, f, &args);
                if let (Ok(values), Carried::Value(t)) = (&returned, rc) {
                    self.store(rb, bytes_of(t), values[0]);
                }
                let ok = returned.is_ok();
                self.end_limit(lim);
                let status = match child.chain.first().map(|c| c.kind) {
                    _ if ok => 0,
                    Some("Cancelled") => 2,
                    _ => 1,
                };
                let (mut cells, chain) = (child.cells, child.chain);
                if status != 0 {
                    cells.insert(0, (cur, charge::CAUSE_CELL));
                }
                self.children.push(Child {
                    status,
                    result: rb,
                    r,
                    charge,
                    n,
                    limit: cur,
                    joined: false,
                    cells,
                    chain,
                });
                vec![TASK_BASE + self.children.len() as u64 - 1, 0]
            }
            RtOp::Join { r } => {
                let i = a[0].checked_sub(TASK_BASE).unwrap_or_else(|| {
                    panic!(
                        "join of a task handle that was never spawned ({:#x}): a refused \
                         spawn's null handle reached Join",
                        a[0]
                    )
                }) as usize;
                let status = self.children[i].status;
                if status == 0 {
                    let n = self.held(TypeRef(r.0)) as usize;
                    let bytes = self.get(self.children[i].result, n).to_vec();
                    self.put(a[1], &bytes);
                }
                let cells = self.settle_child(i);
                if status == 0 {
                    vec![0, 0]
                } else {
                    // The chain as the run time delivers it: one cell per cause, each
                    // a `Cause` in core's layout then the next cell (crate docs, "Causes").
                    let chain = std::mem::take(&mut self.children[i].chain);
                    let mut head = 0;
                    for c in chain.iter().rev() {
                        let cell = self.block(CELL_NEXT + 8, None, 0, None);
                        let tag = CAUSE_KINDS.iter().position(|k| *k == c.kind).unwrap();
                        if c.kind == "Fail" {
                            for (j, w) in c.message.into_iter().enumerate() {
                                self.store(cell + 8 * j as u64, 8, w);
                            }
                        } else {
                            self.store(cell + CAUSE_CAPACITY, 8, CAUSE_KIND_START + tag as u64);
                        }
                        self.store(cell + CAUSE_SITE, 8, site_word(c.site));
                        self.store(cell + CELL_NEXT, 8, head);
                        head = cell;
                    }
                    self.causes.insert(head, cells);
                    vec![u64::from(status), head]
                }
            }
            RtOp::CauseNext => vec![self.load(a[0] + CELL_NEXT, 8)],
            RtOp::CauseValue => vec![a[0]],
            RtOp::Cancel | RtOp::CheckCancel => vec![], // every child has already ended
            RtOp::DestId => {
                self.dests += 1;
                vec![self.dests]
            }
            RtOp::ArgsLen => vec![self.world.args.len() as u64],
            RtOp::ArgsGet => vec![self.world.args.get(a[1] as usize).copied().unwrap_or(0)],
            RtOp::Host(op) => self.host(op, a)?,
            RtOp::CompareBytes => {
                let range = |p: u64, n: u64| match n {
                    0 => Vec::new(),
                    n => self.get(p, n as usize).to_vec(),
                };
                let order = range(a[0], a[1]).cmp(&range(a[2], a[3]));
                vec![order as i8 as u8 as u64]
            }
            RtOp::HashBytes => {
                let bytes = match a[1] {
                    0 => Vec::new(),
                    n => self.get(a[0], n as usize).to_vec(),
                };
                vec![crate::hash_bytes(&bytes)]
            }
            RtOp::ChannelNew { item } => {
                let item = TypeRef(item.0);
                if a[0] == 0 {
                    return Err(Stop::Cause("Bounds"));
                }
                let charge = charge::channel(a[0], self.logical(item)).unwrap_or(u64::MAX);
                match self.admit(cur, charge) {
                    Err(room) => vec![0, 0, room],
                    Ok(()) => {
                        self.chans.push(Chan {
                            item,
                            cap: a[0],
                            queue: VecDeque::new(),
                            senders: 1,
                            receiver: true,
                            charge,
                            limit: cur,
                        });
                        let s = CHAN_BASE + 2 * (self.chans.len() as u64 - 1);
                        vec![s, s + 1, 0]
                    }
                }
            }
            RtOp::ChannelSender => {
                self.chans[((a[0] - CHAN_BASE) / 2) as usize].senders += 1;
                vec![a[0]]
            }
            RtOp::Send { item } | RtOp::TrySend { item } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                let n = self.held(TypeRef(item.0)) as usize;
                let status = if !self.chans[i].receiver {
                    2
                } else if self.chans[i].queue.len() as u64 == self.chans[i].cap {
                    assert!(matches!(op, RtOp::TrySend { .. }), "send would wait");
                    1
                } else {
                    let bytes = self.get(a[1], n).to_vec();
                    self.chans[i].queue.push_back(bytes);
                    0
                };
                match op {
                    RtOp::Send { .. } => vec![u64::from(status == 0)],
                    _ => vec![status],
                }
            }
            RtOp::Receive { .. } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                match self.chans[i].queue.pop_front() {
                    Some(bytes) => {
                        self.put(a[1], &bytes);
                        vec![1]
                    }
                    None if self.chans[i].senders == 0 => vec![0],
                    None => panic!("receive would wait"),
                }
            }
        })
    }

    // ---- functions
    /// Check a call's loans (crate docs, "Loans") and freeze its `view` places
    /// until the caller truncates `frozen` back. The destination is exclusive.
    fn lend(&mut self, sig: &Signature, args: &[u64]) {
        let places: Vec<(usize, usize, usize, Loan)> = sig
            .dest
            .map(|ty| Param::Address {
                place: PlaceTy::Declared(ty),
                loan: Loan::Mut,
            })
            .into_iter()
            .chain(sig.params.iter().copied())
            .zip(args)
            .filter_map(|(p, &a)| match p {
                Param::Address { place, loan } => {
                    let n = self.place_size(place) as usize;
                    (n > 0).then(|| {
                        let (b, o) = split(a);
                        (b, o, n, loan)
                    })
                }
                Param::Value(_) => None,
            })
            .collect();
        for (i, &(b, o, n, loan)) in places.iter().enumerate() {
            self.loans += 1;
            match loan {
                Loan::View | Loan::Take => {
                    self.frozen.push((b, o, n));
                    continue;
                }
                Loan::Mut => {}
            }
            for (j, &(b2, o2, n2, _)) in places.iter().enumerate() {
                assert!(
                    i == j || b != b2 || o + n <= o2 || o2 + n2 <= o,
                    "a {loan:?} place +{o}..+{} overlaps another argument's place +{o2}..+{}",
                    o + n,
                    o2 + n2
                );
            }
        }
    }
    fn call(&mut self, t: &mut Task, f: FuncId, args: &[u64]) -> X<Vec<u64>> {
        let m = self.m;
        let roles = self.roles[f.index()]
            .get_or_insert_with(|| roles::roles(m, &m.functions[f.index()]).into())
            .clone();
        self.footprints.push(Footprint {
            roles,
            spans: HashMap::new(),
        });
        let r = self.activation(t, f, args);
        self.footprints.pop();
        r
    }

    /// Note that an access through `a` touched `n` bytes at `p`, and stop the run
    /// when another role of this activation touched any of them (crate docs, "Roles").
    fn touch(&mut self, a: AddressId, p: u64, n: usize) {
        let Some(frame) = self.footprints.last_mut() else {
            return;
        };
        let Some(role) = frame.roles.get(a.index()).copied().flatten() else {
            return;
        };
        if n == 0 {
            return;
        }
        self.role_checks += 1;
        let (b, lo) = split(p);
        // Static bytes are never written, so any roles may read them together.
        if self.mem[b].fixed {
            return;
        }
        let hi = lo + n;
        let spans = frame.spans.entry(b).or_default();
        let near: Vec<(usize, usize, roles::Role)> = spans
            .range(..=hi)
            .rev()
            .take_while(|(_, (h, _))| *h >= lo)
            .map(|(&l, &(h, r))| (l, h, r))
            .collect();
        let (mut start, mut end) = (lo, hi);
        for (l, h, r) in near {
            if r == role {
                spans.remove(&l);
                (start, end) = (start.min(l), end.max(h));
            } else {
                assert!(
                    h <= lo || hi <= l,
                    "{role:?} touches +{}..+{} of a block that {r:?} touched",
                    lo.max(l),
                    hi.min(h)
                );
            }
        }
        spans.insert(start, (end, role));
    }

    /// Run one activation of `f`. Its slots are frame memory: they end when the
    /// activation does, by return or by failure, so a later use of an address into
    /// them is a stop (crate docs, "Values, slots and addresses").
    fn activation(&mut self, t: &mut Task, f: FuncId, args: &[u64]) -> X<Vec<u64>> {
        let b = self.m.functions[f.index()]
            .body
            .as_ref()
            .expect("external functions do not run here");
        let slots: Vec<u64> = b
            .slots
            .iter()
            .map(|s| self.block(self.place_size(*s), None, 0, None))
            .collect();
        let r = self.frame(t, b, args, &slots);
        for p in slots {
            let blk = &mut self.mem[split(p).0];
            blk.live = false;
            blk.marks.clear();
        }
        r
    }
    fn frame(&mut self, t: &mut Task, b: &Body, args: &[u64], slots: &[u64]) -> X<Vec<u64>> {
        t.at.push(None);
        let r = self.blocks(t, b, args, slots);
        t.at.pop();
        r
    }
    /// SITES.md: the innermost anchor of the task's activations that is not in a
    /// core@1 item. A core operation's cause, or an unanchored one's, takes the
    /// site of the call that reached it.
    fn site(&self, t: &Task) -> Option<Anchor> {
        t.at.iter()
            .rev()
            .flatten()
            .find(|a| !self.m.items[a.item.index()].package_is("core", "1"))
            .copied()
    }
    fn blocks(&mut self, t: &mut Task, b: &Body, args: &[u64], slots: &[u64]) -> X<Vec<u64>> {
        let depth = t.limits.len();
        let mut vals = vec![0u64; b.values.len()];
        let mut places = vec![(0u64, At::Other); b.addresses.len()];
        let pool = |r: PoolRange<ValueId>| r.get(&b.pools.values).unwrap().to_vec();
        let mut block = BlockId(0);
        let mut incoming = args.to_vec();
        'blocks: loop {
            let blk = &b.blocks[block.index()];
            for (p, a) in pool(blk.params).into_iter().zip(&incoming) {
                vals[p.index()] = *a;
            }
            let first = blk.insts.start as usize;
            for (k, inst) in blk.insts.get(&b.insts).unwrap().iter().enumerate() {
                *t.at.last_mut().unwrap() = b.inst_anchors[first + k];
                let r = self.inst(t, b, slots, &mut vals, &mut places, inst);
                if let Err(stop) = r {
                    if let Stop::Cause(c) = stop {
                        let site = self.site(t);
                        t.chain.push(Cause {
                            kind: c,
                            message: [0; 3],
                            site,
                        });
                    }
                    match inst.fail(b) {
                        Some(fb) => {
                            (block, incoming) = (fb, Vec::new());
                            continue 'blocks;
                        }
                        None => break 'blocks,
                    }
                }
            }
            let take = |e: &EdgeId, vals: &[u64]| {
                let e = &b.edges[e.index()];
                (
                    e.target,
                    pool(e.args).iter().map(|v| vals[v.index()]).collect(),
                )
            };
            (block, incoming) = match &b.terminators[block.index()] {
                Terminator::Jump(e) => take(e, &vals),
                Terminator::Branch { condition, yes, no } => take(
                    if vals[condition.index()] != 0 {
                        yes
                    } else {
                        no
                    },
                    &vals,
                ),
                Terminator::Switch {
                    value,
                    arms,
                    otherwise,
                } => {
                    let x = vals[value.index()];
                    let arms = arms.get(&b.pools.arms).unwrap();
                    take(
                        arms.iter()
                            .find(|a| a.value == x)
                            .map_or(otherwise, |a| &a.edge),
                        &vals,
                    )
                }
                Terminator::Return(r) => {
                    assert_eq!(t.limits.len(), depth, "a return left a bound scope open");
                    return Ok(pool(*r).iter().map(|v| vals[v.index()]).collect());
                }
                Terminator::Raise { cause: c, cleanup } => {
                    let mut message = [0; 3];
                    if let RaiseCause::Fail(a) = c {
                        let (p, _) = self.address(b, slots, &vals, &places, *a)?;
                        message = [0, 8, 16].map(|o| self.load(p + o, 8));
                    }
                    *t.at.last_mut().unwrap() = blk.anchor;
                    let site = self.site(t);
                    t.chain.push(Cause {
                        kind: cause(*c),
                        message,
                        site,
                    });
                    match cleanup {
                        Some(c) => (*c, Vec::new()),
                        None => break 'blocks,
                    }
                }
                Terminator::Propagate => break 'blocks,
                Terminator::Unreachable => panic!("an unreachable point was reached"),
            };
        }
        assert_eq!(t.limits.len(), depth, "a failure left a bound scope open");
        Err(Stop::Chained)
    }

    fn address(
        &self,
        b: &Body,
        slots: &[u64],
        vals: &[u64],
        places: &[(u64, At)],
        a: AddressId,
    ) -> X<(u64, At)> {
        let info = &b.addresses[a.index()];
        let (mut p, mut at) = match info.base {
            AddressBase::Slot(s) => match b.slots[s.index()] {
                PlaceTy::Declared(t) => (slots[s.index()], self.at_of(TypeRef(t.0))),
                PlaceTy::Value(_) => (slots[s.index()], At::Other),
            },
            // A pointer value names the storage it points at: a typed address with
            // no steps is that value itself (a resource's block, a box payload).
            AddressBase::Value(v) => match info.ty {
                PlaceTy::Declared(t) if info.steps.len == 0 => {
                    (vals[v.index()], At::Decl(TypeRef(t.0)))
                }
                _ => (vals[v.index()], At::Other),
            },
            AddressBase::Address(x) => places[x.index()],
        };
        for step in info.steps.get(&b.pools.steps).unwrap() {
            let (case, field) = match *step {
                Step::Deref => {
                    (p, at) = match at {
                        At::Ptr(t) => (self.load(p, 8), At::Decl(t)),
                        At::Counted(t) => (self.load(p, 8) + 8, self.at_of(t)),
                        _ => panic!("deref of an untyped place"),
                    };
                    continue;
                }
                Step::Field(f) => (None, f),
                Step::CaseField { case, field } => (Some(case), field),
                Step::Element(i) => {
                    let At::Array(k) = at else {
                        panic!("element of a place that is not an inline array")
                    };
                    let (e, n) = &self.arrays[k];
                    let i = vals[i.index()];
                    if i >= *n {
                        return Err(Stop::Cause("Bounds"));
                    }
                    p += i * self.stride(e);
                    at = self.at_field(e);
                    continue;
                }
                Step::Byte(n) => {
                    p += vals[n.index()];
                    at = At::Other;
                    continue;
                }
                Step::Index(i) => {
                    // Raw storage of elements of the address's type.
                    p += vals[i.index()] * self.place_size(info.ty);
                    at = match info.ty {
                        PlaceTy::Declared(t) => self.at_of(TypeRef(t.0)),
                        PlaceTy::Value(_) => At::Other,
                    };
                    continue;
                }
            };
            let At::Decl(t) = at else {
                panic!("field of an untyped place")
            };
            let off = self.fixed(t).offsets[case.unwrap_or(0) as usize][field as usize];
            let fty = &self.shape(t).fields(case).unwrap()[field as usize].ty;
            at = self.at_field(fty);
            p += off;
        }
        Ok((p, at))
    }

    fn inst(
        &mut self,
        t: &mut Task,
        b: &Body,
        slots: &[u64],
        vals: &mut [u64],
        places: &mut [(u64, At)],
        inst: &Inst,
    ) -> X<()> {
        let ty = |v: ValueId| b.values[v.index()];
        match *inst {
            Inst::Const { result, bits } => vals[result.index()] = bits,
            Inst::FunctionRef { result, function } => {
                vals[result.index()] = FN_BASE + u64::from(function.0)
            }
            // Statics are the first blocks, in module order.
            Inst::ReadStatic { result, data } => vals[result.index()] = ptr(data.index(), 0),
            Inst::Unary {
                result,
                op,
                operand,
                ..
            } => {
                let (t, x) = (ty(operand), vals[operand.index()]);
                vals[result.index()] = match op {
                    UnaryOp::Not => x ^ 1,
                    UnaryOp::BitNot => mask(t, !x),
                    UnaryOp::Neg if t.is_float() => bits(t, -float(t, x)),
                    UnaryOp::Neg => fit(t, -int(t, x)).ok_or(Stop::Cause("Overflow"))?,
                };
            }
            Inst::Binary {
                result,
                op,
                left,
                right,
                ..
            } => {
                let t = ty(left);
                let (a, c) = (vals[left.index()], vals[right.index()]);
                vals[result.index()] = if t.is_float() {
                    float_binary(t, op, a, c)
                } else {
                    let division = matches!(op, BinaryOp::Div | BinaryOp::Rem);
                    int_binary(t, op, a, c).ok_or(Stop::Cause(if division && c == 0 {
                        "Division"
                    } else {
                        "Overflow"
                    }))?
                };
            }
            Inst::Compare {
                result,
                op,
                left,
                right,
            } => {
                let t = ty(left);
                let (a, c) = (vals[left.index()], vals[right.index()]);
                let ord = if t.is_float() {
                    float(t, a).partial_cmp(&float(t, c))
                } else {
                    Some(int(t, a).cmp(&int(t, c)))
                };
                use std::cmp::Ordering::*;
                vals[result.index()] = u64::from(match op {
                    CompareOp::Eq => ord == Some(Equal),
                    CompareOp::Ne => ord != Some(Equal),
                    CompareOp::Lt => ord == Some(Less),
                    CompareOp::Le => matches!(ord, Some(Less | Equal)),
                    CompareOp::Gt => ord == Some(Greater),
                    CompareOp::Ge => matches!(ord, Some(Greater | Equal)),
                });
            }
            Inst::Convert {
                result,
                flag,
                operand,
                mode,
            } => {
                let (from, to, x) = (ty(operand), ty(result), vals[operand.index()]);
                let char_ty = Ty::Scalar(Scalar::Char);
                let (r, fl) = match mode {
                    ConvertMode::Widen if from == char_ty => (x, 0),
                    ConvertMode::TryNarrow if to == char_ty => {
                        let ok = (x < 0xD800) || (0xE000..=0x10FFFF).contains(&x);
                        (if ok { x } else { 0 }, u64::from(ok))
                    }
                    ConvertMode::Widen => (fit(to, int(from, x)).expect("widen fits"), 0),
                    ConvertMode::Truncate => (mask(to, x), 0),
                    ConvertMode::TryNarrow => {
                        (mask(to, x), u64::from(fit(to, int(from, x)).is_some()))
                    }
                    ConvertMode::FloatToInt => {
                        let f = float(from, x);
                        let (lo, hi) = range(to);
                        if f.is_nan() {
                            (0, 1)
                        } else if f.trunc() >= lo as f64 && f.trunc() < (hi + 1) as f64 {
                            (mask(to, f.trunc() as i128 as u64), 0)
                        } else {
                            (0, 2)
                        }
                    }
                    ConvertMode::IntToFloat => {
                        let v = int(from, x);
                        let r = if is_f32(to) {
                            u64::from((v as f32).to_bits())
                        } else {
                            (v as f64).to_bits()
                        };
                        (r, 0)
                    }
                    ConvertMode::Reinterpret if from.is_float() => (bits(from, float(from, x)), 0),
                    ConvertMode::Reinterpret => (bits(to, float(to, x)), 0),
                    ConvertMode::FloatResize => (bits(to, float(from, x)), 0),
                };
                vals[result.index()] = r;
                if let Some(f) = flag {
                    vals[f.index()] = fl;
                }
            }
            Inst::IntArith {
                result,
                flag,
                op,
                mode,
                left,
                right,
            } => {
                let t = ty(left);
                let (a, c) = (int(t, vals[left.index()]), int(t, vals[right.index()]));
                let exact = match op {
                    ArithOp::Add => Some(a + c),
                    ArithOp::Sub => Some(a - c),
                    ArithOp::Mul => Some(a.saturating_mul(c)),
                    ArithOp::Div => (c != 0).then(|| a / c),
                };
                let fits = exact.and_then(|e| fit(t, e));
                vals[result.index()] = match mode {
                    ArithMode::Checked if op == ArithOp::Div => fits.unwrap_or(0),
                    ArithMode::Wrapping | ArithMode::Checked if op == ArithOp::Mul => {
                        mask(t, vals[left.index()].wrapping_mul(vals[right.index()]))
                    }
                    ArithMode::Wrapping | ArithMode::Checked => mask(t, exact.unwrap() as u64),
                    ArithMode::Saturating => {
                        let (lo, hi) = range(t);
                        fit(t, exact.unwrap().clamp(lo, hi)).unwrap()
                    }
                };
                if let Some(f) = flag {
                    vals[f.index()] = u64::from(fits.is_none());
                }
            }
            Inst::MulAdd {
                result,
                left,
                right,
                addend,
            } => {
                let t = ty(result);
                let [a, c, d] = [left, right, addend].map(|v| vals[v.index()]);
                vals[result.index()] = if is_f32(t) {
                    let f = |x: u64| f32::from_bits(x as u32);
                    bits(t, f64::from(f(a).mul_add(f(c), f(d))))
                } else {
                    bits(
                        t,
                        f64::from_bits(a).mul_add(f64::from_bits(c), f64::from_bits(d)),
                    )
                };
            }
            Inst::ScalarUnary {
                result,
                op,
                operand,
            } => {
                let (t, x) = (ty(operand), vals[operand.index()]);
                let w = width(t);
                let f = float(t, x);
                vals[result.index()] = match op {
                    ScalarUnaryOp::Sqrt => bits(t, f.sqrt()),
                    ScalarUnaryOp::Floor => bits(t, f.floor()),
                    ScalarUnaryOp::Ceil => bits(t, f.ceil()),
                    ScalarUnaryOp::Trunc => bits(t, f.trunc()),
                    ScalarUnaryOp::Round => bits(t, f.round()),
                    ScalarUnaryOp::RoundEven => bits(t, f.round_ties_even()),
                    ScalarUnaryOp::Abs => bits(t, f.abs()),
                    ScalarUnaryOp::LeadingZeros => u64::from(x.leading_zeros() - (64 - w)),
                    ScalarUnaryOp::TrailingZeros => u64::from(x.trailing_zeros().min(w)),
                    ScalarUnaryOp::CountOnes => u64::from(x.count_ones()),
                    ScalarUnaryOp::SwapBytes => x.swap_bytes() >> (64 - w),
                    ScalarUnaryOp::ReverseBits => x.reverse_bits() >> (64 - w),
                };
            }
            Inst::Address { address, .. } => {
                places[address.index()] = self.address(b, slots, vals, places, address)?;
            }
            Inst::Case { result, address } => {
                let (p, At::Decl(ty)) = places[address.index()] else {
                    panic!("case of an untyped place")
                };
                vals[result.index()] = u64::from(self.case_at(p, ty));
            }
            Inst::SetCase { address, case } => {
                let (p, At::Decl(ty)) = places[address.index()] else {
                    panic!("set_case of an untyped place")
                };
                let tag = self.fixed(ty).tag.expect("a variant");
                // A niche variant's dataful case is made by its payload alone.
                if let Some(word) = hirsel_core_layout::case_word(&tag, case) {
                    self.store(p + tag.offset, tag.width as usize, word);
                }
            }
            Inst::Read {
                result, address, ..
            } => {
                let n = bytes_of(ty(result));
                self.touch(address, places[address.index()].0, n);
                self.read_marked(places[address.index()].0, n);
                vals[result.index()] = self.load(places[address.index()].0, n);
            }
            Inst::Store { address, value } => {
                let n = bytes_of(ty(value));
                self.touch(address, places[address.index()].0, n);
                self.store(places[address.index()].0, n, vals[value.index()]);
            }
            Inst::StorageLive { .. } | Inst::StorageDead { .. } => {}
            Inst::UpdateMask {
                result,
                mask: mv,
                field,
                initialized,
            } => {
                let x = vals[mv.index()];
                vals[result.index()] = if initialized {
                    x | (1 << field)
                } else {
                    x & !(1 << field)
                };
            }
            Inst::Call { call } => {
                let site = &b.calls[call.index()];
                let args: Vec<u64> = site
                    .args
                    .get(&b.pools.args)
                    .unwrap()
                    .iter()
                    .map(|a| match a {
                        Arg::Value(v) => vals[v.index()],
                        Arg::Address(a) => places[a.index()].0,
                    })
                    .collect();
                let held = self.frozen.len();
                self.lend(&self.m.signatures[site.signature.index()], &args);
                let r = match site.callee {
                    Callee::Direct(f) => self.call(t, f, &args),
                    Callee::Indirect(v) => {
                        self.call(t, FuncId((vals[v.index()] - FN_BASE) as u32), &args)
                    }
                    Callee::Runtime(op) => self.rt(t, op, &args),
                };
                self.frozen.truncate(held);
                let r = r?;
                for (v, x) in site.results.get(&b.pools.values).unwrap().iter().zip(r) {
                    vals[v.index()] = x;
                }
            }
            Inst::Alloc {
                result,
                available,
                region_entry,
                ref kind,
                place,
                charge,
                overflow,
                size,
                ..
            } => {
                let top = *t.limits.last().unwrap();
                let l = match place {
                    // N03, MEM-27: inside a finalizer growth admits from the current
                    // limit, the grant or a bound scope entered within it (MEM-25).
                    AllocPlace::Like(_) if t.limits.iter().any(|&l| self.limits[l].grant) => top,
                    AllocPlace::Like(v) => {
                        let (blk, _) = split(vals[v.index()]);
                        self.find(self.mem[blk].limit.expect("a charged block"))
                    }
                    AllocPlace::Current | AllocPlace::Region(_) => *t.limits.last().unwrap(),
                };
                if let Some(token) = region_entry {
                    vals[token.index()] = 0;
                }
                let n = vals[charge.index()];
                let admission = if overflow.is_some_and(|v| vals[v.index()] != 0) {
                    self.admissions += 1;
                    Err(self.limits[l].cap - self.limits[l].used)
                } else {
                    self.admit(l, n)
                };
                match admission {
                    Err(_) if self.limits[l].grant => return Err(Stop::Cause("Capacity")),
                    Err(room) => (vals[result.index()], vals[available.index()]) = (0, room),
                    Ok(()) => {
                        let res = match kind {
                            AllocKind::Resource { grant } => Some(Res {
                                grant: *grant,
                                settled: false,
                                cell_used: false,
                            }),
                            _ => None,
                        };
                        let p = self.block(vals[size.index()], Some(l), n, res);
                        if let Some(count) = kind.counted() {
                            self.mem[split(p).0].counted = Some(count);
                            self.store(p, 8, 1);
                        }
                        self.mem[split(p).0].class =
                            Some(kind.cleanup(&self.m.types, &self.recipes));
                        if let AllocPlace::Region(r) = place {
                            let region =
                                &mut self.regions[(vals[r.index()] - REGION_BASE) as usize];
                            if let Some(token) = region_entry {
                                vals[token.index()] = region.len() as u64;
                            }
                            region.push(split(p).0);
                        }
                        (vals[result.index()], vals[available.index()]) = (p, 0);
                    }
                }
            }
            Inst::Free { block, charge } => {
                let p = vals[block.index()];
                assert_eq!(
                    self.mem[split(p).0].charge,
                    vals[charge.index()],
                    "free refunds another charge"
                );
                self.free(p);
            }
            Inst::Pointer { result, address } => vals[result.index()] = places[address.index()].0,
            Inst::Trim {
                block,
                region,
                entry: _,
                previous,
                charge,
            } => {
                let (blk, o) = split(vals[block.index()]);
                let (n, old) = (vals[charge.index()], self.mem[blk].charge);
                assert_eq!(old, vals[previous.index()], "writer retained charge");
                assert!(self.regions[(vals[region.index()] - REGION_BASE) as usize].contains(&blk));
                assert!(
                    o == 0 && n <= old,
                    "trim of a non-block or above its charge"
                );
                self.mem[blk].charge = n;
                self.refund(self.mem[blk].limit.expect("a region block"), old - n);
            }
            Inst::RegionOpen { result } => {
                self.regions.push(Vec::new());
                vals[result.index()] = REGION_BASE + self.regions.len() as u64 - 1;
            }
            Inst::RegionClose { region } => {
                let blocks = std::mem::take(
                    &mut self.regions[(vals[region.index()] - REGION_BASE) as usize],
                );
                for blk in blocks {
                    self.free(ptr(blk, 0));
                }
            }
            Inst::LimitEnter {
                admitted,
                available,
                bytes,
            } => {
                let cur = *t.limits.last().unwrap();
                let n = vals[bytes.index()];
                let (ok, room) = match self.admit(cur, n) {
                    Ok(()) => (1, 0),
                    Err(room) => (0, room),
                };
                if ok == 1 {
                    let l = self.open_limit(n, cur, false);
                    t.limits.push(l);
                }
                (vals[admitted.index()], vals[available.index()]) = (ok, room);
            }
            Inst::LimitExit => {
                let l = t.limits.pop().unwrap();
                self.end_limit(l);
            }
            Inst::Drop { address, ty, .. } => {
                let at = places[address.index()].0;
                self.drop_marked(at, self.held(TypeRef(ty.0)) as usize);
                if self.drop_value(t, at, TypeRef(ty.0)) {
                    return Err(Stop::Chained);
                }
            }
            Inst::Copy { to, from, bytes } => {
                let n = vals[bytes.index()] as usize;
                if n == 0 {
                    // Zero bytes touch no storage; a zero-sized `List`'s null data is
                    // a valid operand (SEM-4).
                    return Ok(());
                }
                self.touch(from, places[from.index()].0, n);
                self.touch(to, places[to.index()].0, n);
                let (f, t) = (places[from.index()].0, places[to.index()].0);
                assert!(
                    f == t || split(f).0 != split(t).0 || f + n as u64 <= t || t + n as u64 <= f,
                    "copy of {n} bytes between overlapping ranges"
                );
                self.read_marked(f, n);
                let src = self.get(f, n).to_vec();
                self.put(places[to.index()].0, &src);
            }
            Inst::Share { node, kind } => {
                let p = vals[node.index()];
                self.kind(p, kind);
                let c = self.load(p, 8);
                self.store(p, 8, c + 1);
            }
            Inst::Release { node, ty, kind } => {
                self.kind(vals[node.index()], kind);
                self.release(vals[node.index()], TypeRef(ty.0))
            }
            Inst::Mark {
                address,
                count,
                live,
                ty,
            } => {
                let size = self.place_size(ty);
                for k in 0..vals[count.index()] {
                    self.mark(places[address.index()].0 + k * size, live, size as usize);
                }
            }
            Inst::Unmark {
                address,
                count,
                live,
                ty,
            } => {
                let size = self.place_size(ty);
                for k in 0..vals[count.index()] {
                    self.unmark(places[address.index()].0 + k * size, live, size as usize);
                }
            }
            Inst::TryUnwrap { result, node, kind } => {
                self.kind(vals[node.index()], kind);
                vals[result.index()] = u64::from(self.load(vals[node.index()], 8) == 1);
            }
        }
        Ok(())
    }
}

fn range(t: Ty) -> (i128, i128) {
    let (signed, w) = t.int().unwrap();
    if signed {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    }
}

/// A run-line literal of type `t`: decimal floats or the constant syntax.
pub fn scalar(t: Ty, s: &str) -> u64 {
    if t.is_float() && !s.starts_with("0x") {
        let f: f64 = s.parse().unwrap_or_else(|_| panic!("bad float `{s}`"));
        return if t == Ty::Scalar(Scalar::F32) {
            u64::from((f as f32).to_bits())
        } else {
            f.to_bits()
        };
    }
    crate::text::parse_constant(t, s).unwrap_or_else(|e| panic!("{e}"))
}

fn width(t: Ty) -> u32 {
    match t {
        Ty::Scalar(s) => s.size() as u32 * 8,
        Ty::Ptr => 64,
    }
}
fn mask(t: Ty, x: u64) -> u64 {
    let w = width(t);
    if w == 64 { x } else { x & ((1 << w) - 1) }
}
/// The integer value of `x` read at `t`.
fn int(t: Ty, x: u64) -> i128 {
    match t.int() {
        Some((true, w)) => i128::from(((x << (64 - w)) as i64) >> (64 - w)),
        _ => i128::from(x),
    }
}
/// `v` as bits of `t`, or None when it does not fit.
fn fit(t: Ty, v: i128) -> Option<u64> {
    let (signed, w) = t.int()?;
    let (lo, hi) = if signed {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    };
    (lo..=hi).contains(&v).then(|| mask(t, v as u64))
}
fn is_f32(t: Ty) -> bool {
    t == Ty::Scalar(Scalar::F32)
}
fn float(t: Ty, x: u64) -> f64 {
    if is_f32(t) {
        f64::from(f32::from_bits(x as u32))
    } else {
        f64::from_bits(x)
    }
}
/// A float result as bits, a NaN as the kind's one observable NaN (SEM-2).
fn bits(t: Ty, f: f64) -> u64 {
    match (is_f32(t), f.is_nan()) {
        (true, true) => 0x7fc0_0000,
        (true, false) => u64::from((f as f32).to_bits()),
        (false, true) => 0x7ff8_0000_0000_0000,
        (false, false) => f.to_bits(),
    }
}

fn int_binary(t: Ty, op: BinaryOp, a: u64, c: u64) -> Option<u64> {
    let (x, y) = (int(t, a), int(t, c));
    let w = width(t);
    match op {
        BinaryOp::Add => fit(t, x + y),
        BinaryOp::Sub => fit(t, x - y),
        BinaryOp::Mul => fit(t, x.checked_mul(y)?),
        BinaryOp::Div => fit(t, x.checked_div(y)?),
        BinaryOp::Rem => (y != 0).then(|| mask(t, (x % y) as u64)),
        BinaryOp::Shl | BinaryOp::Shr if !(0..i128::from(w)).contains(&y) => None,
        BinaryOp::Shl => Some(mask(t, a << y)),
        BinaryOp::Shr => Some(mask(t, (x >> y) as u64)),
        BinaryOp::BitAnd => Some(a & c),
        BinaryOp::BitOr => Some(a | c),
        BinaryOp::BitXor => Some(a ^ c),
        BinaryOp::Min => Some(if x <= y { a } else { c }),
        BinaryOp::Max => Some(if x >= y { a } else { c }),
        BinaryOp::MinNumber | BinaryOp::MaxNumber => unreachable!("float only"),
    }
}

fn float_binary(t: Ty, op: BinaryOp, a: u64, c: u64) -> u64 {
    let (x, y) = (float(t, a), float(t, c));
    let r = match op {
        BinaryOp::Add => x + y,
        BinaryOp::Sub => x - y,
        BinaryOp::Mul => x * y,
        BinaryOp::Div => x / y,
        BinaryOp::Min | BinaryOp::Max if x.is_nan() || y.is_nan() => f64::NAN,
        BinaryOp::Min | BinaryOp::MinNumber => x.minimum_or_number(y, op == BinaryOp::Min),
        BinaryOp::Max | BinaryOp::MaxNumber => -(-x).minimum_or_number(-y, op == BinaryOp::Max),
        _ => unreachable!("integer only"),
    };
    // f32 arithmetic done in f64 is exact before the one rounding in `bits` for
    // + - * (24-bit significands); `/` is not used on f32 by any sample.
    bits(t, r)
}

trait MinimumOrNumber {
    fn minimum_or_number(self, y: f64, propagate: bool) -> f64;
}
impl MinimumOrNumber for f64 {
    /// IEEE 754-2019 minimum (`propagate`) or minimumNumber; -0 < +0.
    fn minimum_or_number(self, y: f64, propagate: bool) -> f64 {
        let x = self;
        if x.is_nan() || y.is_nan() {
            return if propagate {
                f64::NAN
            } else if x.is_nan() {
                y
            } else {
                x
            };
        }
        if x == y {
            return if x.is_sign_negative() { x } else { y };
        }
        x.min(y)
    }
}
