//! A test-only evaluator, written from the op documentation (SEM-1, SEM-2, LIR's op
//! docs; for the FORM v1 draft FAIL-8..14, MEM-19..29 and CONC-3..9) so the
//! samples' hand-derived run lines can be checked. Memory is blocks of bytes laid
//! out by the layout recipes. Limits keep exact `used` totals and hand over by
//! forwarding. A spawned child runs to its end at the spawn, so the event order is
//! the program order and nothing waits. A delivered `Causes` owns the child's used
//! cells until it is dropped. Sequences, maps, regions' compact contents and static
//! data are not modelled and panic.

use crate::*;
use hirsel_core_layout::{
    Body as Shaped, FieldTy, Handle, Layout, Recipe, Repr, TypeRef, charge, compute, field_size,
};
use std::collections::VecDeque;

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
    /// The first u64 field of each resource, in the order its cleanup started.
    pub trace: Vec<u64>,
}

const FN_BASE: u64 = 0x1000;
const REGION_BASE: u64 = 0x10_0000;
const TASK_BASE: u64 = 0x20_0000;
/// Sender of channel `i` is `CHAN_BASE + 2i`, its receiver `CHAN_BASE + 2i + 1`.
const CHAN_BASE: u64 = 0x30_0000;
const CAUSES_BASE: u64 = 0x40_0000;

/// Why an instruction stopped: a new cause, or a callee's failure already chained.
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
    chain: Vec<String>,
    /// Used cause cells beyond the primary, with the limit each is charged to.
    cells: Vec<(usize, u64)>,
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
    regions: Vec<Vec<usize>>,
    children: Vec<Child>,
    chans: Vec<Chan>,
    /// Each delivered `Causes`: the cells it owns, None once dropped.
    causes: Vec<Option<Vec<(usize, u64)>>>,
    /// Every inline array field type of the module: element and length.
    arrays: Vec<(FieldTy, u64)>,
    trace: Vec<u64>,
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
    let mut mach = Machine {
        m,
        recipes: compute(&m.types).expect("layouts"),
        mem: Vec::new(),
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
        causes: Vec::new(),
        arrays: Vec::new(),
        trace: Vec::new(),
    };
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
    };
    let outcome = match mach.call(&mut task, f, args) {
        Ok(r) => Outcome::Returned(r),
        Err(_) => Outcome::Failed(task.chain),
    };
    Report {
        outcome,
        used: mach.limits[0].used,
        trace: mach.trace,
    }
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
    fn block(&mut self, size: u64, limit: Option<usize>, charge: u64, res: Option<Res>) -> u64 {
        self.mem.push(Blk {
            bytes: vec![0; size as usize],
            live: true,
            limit,
            charge,
            res,
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
        self.mem[b].bytes[o..o + bytes.len()].copy_from_slice(bytes);
    }
    fn store(&mut self, p: u64, n: usize, v: u64) {
        self.put(p, &v.to_le_bytes()[..n]);
    }
    /// Free a block and refund its charge, less a settled grant and a used cause
    /// cell, which left with the finalizer's hand-over and with the chain.
    fn free(&mut self, p: u64) {
        let (b, o) = split(p);
        assert!(o == 0 && self.mem[b].live, "free of a non-block");
        let blk = &mut self.mem[b];
        blk.live = false;
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
        if n <= room {
            lim.used += n;
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
    fn fixed(&self, t: TypeRef) -> &hirsel_core_layout::Fixed {
        match &self.recipes[t.index()].layout {
            Layout::Fixed(f) => f,
            Layout::Compact(_) => panic!("compact values are not modelled"),
        }
    }
    fn is_resource(&self, t: TypeRef) -> bool {
        self.shape(t).repr == Repr::Resource
    }
    /// Bytes a holder spends on a value of `t`: a resource is a pointer to its block.
    fn held(&self, t: TypeRef) -> u64 {
        if self.is_resource(t) {
            8
        } else {
            self.fixed(t).size
        }
    }
    fn logical(&self, t: TypeRef) -> u64 {
        if self.is_resource(t) {
            8
        } else {
            self.recipes[t.index()].logical_size.expect("fixed")
        }
    }
    fn at_of(&self, t: TypeRef) -> At {
        if self.is_resource(t) {
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
        self.drop_fields(t, at, ty)
    }
    fn drop_fields(&mut self, t: &mut Task, base: u64, ty: TypeRef) -> bool {
        let row = match self.fixed(ty).tag {
            Some(tag) => self.load(base + tag.offset, tag.width as usize) as usize,
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
            // The provider closes the file and ignores the outcome (MEM-24).
            FieldTy::Handle(Handle::HostFile) => false,
            FieldTy::Box(x) => {
                let p = self.load(at, 8);
                let failed = self.drop_value(t, p, *x);
                self.free(p);
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
                for (l, n) in self.settle_child(i) {
                    self.refund(l, n);
                }
                failed
            }
            FieldTy::Rc(x) | FieldTy::Arc(x) => {
                let node = self.load(at, 8);
                self.release(node, *x);
                false
            }
            FieldTy::Handle(Handle::Causes) => {
                let i = (self.load(at, 8) - CAUSES_BASE) as usize;
                let cells = self.causes[i].take().expect("Causes dropped twice");
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
        if self.mem[b].bytes.len() >= 8 {
            self.trace.push(self.load(blk, 8));
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
        self.free(blk);
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
        let count = self.load(node, 8) - 1;
        self.store(node, 8, count);
        if count == 0 {
            let mut scratch = Task {
                limits: Vec::new(),
                chain: Vec::new(),
                cells: Vec::new(),
            };
            assert!(
                !self.drop_value(&mut scratch, node + 8, ty),
                "a counted payload failed"
            );
            self.free(node);
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
                };
                let f = FuncId((a[2] - FN_BASE) as u32);
                let ok = self.call(&mut child, f, &[rb, xb]).is_ok();
                self.end_limit(lim);
                let status = match child.chain.first().map(String::as_str) {
                    _ if ok => 0,
                    Some("Cancelled") => 2,
                    _ => 1,
                };
                let mut cells = child.cells;
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
                });
                vec![TASK_BASE + self.children.len() as u64 - 1, 0]
            }
            RtOp::Join { r } => {
                let i = (a[0] - TASK_BASE) as usize;
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
                    self.causes.push(Some(cells));
                    vec![
                        u64::from(status),
                        CAUSES_BASE + self.causes.len() as u64 - 1,
                    ]
                }
            }
            RtOp::Cancel | RtOp::CheckCancel => vec![], // every child has already ended
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
    fn call(&mut self, t: &mut Task, f: FuncId, args: &[u64]) -> X<Vec<u64>> {
        let m = self.m;
        let b = m.functions[f.index()]
            .body
            .as_ref()
            .expect("external functions do not run here");
        let depth = t.limits.len();
        let mut vals = vec![0u64; b.values.len()];
        let slots: Vec<u64> = b
            .slots
            .iter()
            .map(|s| self.block(self.place_size(*s), None, 0, None))
            .collect();
        let mut places = vec![(0u64, At::Other); b.addresses.len()];
        let pool = |r: PoolRange<ValueId>| r.get(&b.pools.values).unwrap().to_vec();
        let mut block = BlockId(0);
        let mut incoming = args.to_vec();
        'blocks: loop {
            let blk = &b.blocks[block.index()];
            for (p, a) in pool(blk.params).into_iter().zip(&incoming) {
                vals[p.index()] = *a;
            }
            for inst in blk.insts.get(&b.insts).unwrap() {
                let r = self.inst(t, b, &slots, &mut vals, &mut places, inst);
                if let Err(stop) = r {
                    if let Stop::Cause(c) = stop {
                        t.chain.push(c.into());
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
                    t.chain.push(cause(*c).into());
                    match cleanup {
                        Some(c) => (*c, Vec::new()),
                        None => break 'blocks,
                    }
                }
                Terminator::Propagate => break 'blocks,
                Terminator::Unreachable => panic!("a never call returned"),
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
            Inst::ReadStatic { .. } => panic!("static data is not modelled"),
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
                    let division = matches!(
                        op,
                        BinaryOp::Div
                            | BinaryOp::Rem
                            | BinaryOp::DivFloor
                            | BinaryOp::Mod
                            | BinaryOp::DivExact
                    );
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
                let (r, fl) = match mode {
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
                    ArithOp::Mul => Some(a * c),
                    ArithOp::Div => (c != 0).then(|| a / c),
                };
                let fits = exact.and_then(|e| fit(t, e));
                vals[result.index()] = match mode {
                    ArithMode::Checked if op == ArithOp::Div => fits.unwrap_or(0),
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
                let tag = self.fixed(ty).tag.expect("a variant");
                vals[result.index()] = self.load(p + tag.offset, tag.width as usize);
            }
            Inst::SetCase { address, case } => {
                let (p, At::Decl(ty)) = places[address.index()] else {
                    panic!("set_case of an untyped place")
                };
                let tag = self.fixed(ty).tag.expect("a variant");
                self.store(p + tag.offset, tag.width as usize, u64::from(case));
            }
            Inst::Read {
                result, address, ..
            } => {
                let n = bytes_of(ty(result));
                vals[result.index()] = self.load(places[address.index()].0, n);
            }
            Inst::Store { address, value } => {
                let n = bytes_of(ty(value));
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
                let r = match site.callee {
                    Callee::Direct(f) => self.call(t, f, &args),
                    Callee::Indirect(v) => {
                        self.call(t, FuncId((vals[v.index()] - FN_BASE) as u32), &args)
                    }
                    Callee::Runtime(op) => self.rt(t, op, &args),
                }?;
                for (v, x) in site.results.get(&b.pools.values).unwrap().iter().zip(r) {
                    vals[v.index()] = x;
                }
            }
            Inst::Alloc {
                result,
                available,
                kind,
                place,
                charge,
                size,
                ..
            } => {
                let top = *t.limits.last().unwrap();
                let l = match place {
                    // N03: inside a finalizer the grant takes precedence.
                    AllocPlace::Like(_) if self.limits[top].grant => top,
                    AllocPlace::Like(v) => {
                        let (blk, _) = split(vals[v.index()]);
                        self.find(self.mem[blk].limit.expect("a charged block"))
                    }
                    AllocPlace::Current | AllocPlace::Region(_) => *t.limits.last().unwrap(),
                };
                let n = vals[charge.index()];
                match self.admit(l, n) {
                    Err(_) if self.limits[l].grant => return Err(Stop::Cause("Capacity")),
                    Err(room) => (vals[result.index()], vals[available.index()]) = (0, room),
                    Ok(()) => {
                        let res = match kind {
                            AllocKind::Resource { grant } => Some(Res {
                                grant,
                                settled: false,
                                cell_used: false,
                            }),
                            _ => None,
                        };
                        let p = self.block(size, Some(l), n, res);
                        if kind == AllocKind::Counted {
                            self.store(p, 8, 1);
                        }
                        if let AllocPlace::Region(r) = place {
                            self.regions[(vals[r.index()] - REGION_BASE) as usize].push(split(p).0);
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
                if self.drop_value(t, places[address.index()].0, TypeRef(ty.0)) {
                    return Err(Stop::Chained);
                }
            }
            Inst::Copy { to, from, bytes } => {
                let n = vals[bytes.index()] as usize;
                let src = self.get(places[from.index()].0, n).to_vec();
                self.put(places[to.index()].0, &src);
            }
            Inst::Share { node, .. } => {
                let p = vals[node.index()];
                let c = self.load(p, 8);
                self.store(p, 8, c + 1);
            }
            Inst::Release { node, ty, .. } => self.release(vals[node.index()], TypeRef(ty.0)),
            Inst::TryUnwrap { result, node, .. } => {
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
        BinaryOp::DivFloor => fit(
            t,
            x.checked_div_euclid(y)
                .map(|q| if y < 0 && x % y != 0 { q + 1 } else { q })?,
        ),
        BinaryOp::Mod => (y != 0).then(|| mask(t, (((x % y) + y) % y) as u64)),
        BinaryOp::DivExact => {
            if y != 0 && x % y == 0 {
                fit(t, x / y)
            } else {
                None
            }
        }
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
