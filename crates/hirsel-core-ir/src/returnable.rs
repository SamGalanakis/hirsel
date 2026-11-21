//! Returnable waits: the owned network waits a task parks at with its stack
//! given back.
//!
//! A wait is returnable when every frame from the task's entry down to it is a
//! body of this module represented in the task's frame record. Each such body
//! gets a private variant that keeps its slots and the values it needs across
//! the wait in its part of the record: a site word, then the child area its
//! callee uses, then the homes. A parked callee leaves its site word nonzero;
//! its caller stores its own site number and returns, so the task's entry
//! returns to the dispatcher and the stack goes back to the pool. The runtime
//! resumes by entering the entry again with the same arguments on a pooled
//! stack: each variant reads its site word and jumps to that site's resume
//! block, which reloads what the call needs and makes it again. A wait's own
//! row continues the parked operation, so nothing before the wait runs twice.
//!
//! The rule is a function of the module alone. A function is represented when
//! it reaches an owned wait (`recv_bounded`, `send_pair`) through direct calls
//! of represented functions, has none of the refused forms (frame storage,
//! regions, limits, step scopes, isolation, vectors, `never`, recursion), and
//! every address it needs after a wait is computed from parameters and
//! constants alone. A task entry is represented when its spawn names the entry
//! adapter and a represented callee whose code pointer the spawning body
//! stores. Every other wait keeps the stackful drive.
use crate::extent::{operands, terminator_uses};
use crate::*;
use hirsel_core_ids::ContentHash;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Set to any value, the driver leaves every wait stackful.
pub const DISABLE: &str = "HIRSEL_NO_RETURNABLE";

const CHILD: u64 = 16;
const ALIGN: u64 = 16;

/// Why a function cannot be represented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Never,
    Vectors,
    FrameAlloc,
    PromoteFrame,
    Region,
    Limit,
    StepScope,
    Isolate,
    Recursive,
    NoSite,
    LiveAddress,
    SlotLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Site {
    Leaf(HostOp),
    Callee(FuncId),
}

fn leaf(callee: Callee) -> Option<HostOp> {
    match callee {
        Callee::Runtime(RtOp::Host(op @ (HostOp::RecvBounded | HostOp::SendPair))) => Some(op),
        _ => None,
    }
}

fn refusal(module: &Module, f: &Function, body: &Body) -> Option<Refusal> {
    let sig = &module.signatures[f.signature.index()];
    if sig.never {
        return Some(Refusal::Never);
    }
    if f.versioned || sig.has_vectors() || body.values.iter().any(|t| matches!(t, Ty::Vector(_)))
    {
        return Some(Refusal::Vectors);
    }
    for inst in &body.insts {
        let r = match *inst {
            Inst::Alloc {
                place: AllocPlace::Frame(_) | AllocPlace::LentFrame(_),
                ..
            }
            | Inst::FrameRefund { .. } => Some(Refusal::FrameAlloc),
            Inst::PromoteFrame { .. } => Some(Refusal::PromoteFrame),
            Inst::RegionOpen { .. } | Inst::RegionClose { .. } | Inst::Trim { .. } => {
                Some(Refusal::Region)
            }
            Inst::Alloc {
                place: AllocPlace::Region(_),
                ..
            } => Some(Refusal::Region),
            Inst::LimitEnter { .. } | Inst::LimitExit => Some(Refusal::Limit),
            Inst::Call { call } => match body.calls[call.index()].callee {
                Callee::Runtime(RtOp::StepEnter | RtOp::StepLeave | RtOp::StepCatch) => {
                    Some(Refusal::StepScope)
                }
                Callee::Runtime(RtOp::IsolateEnter | RtOp::IsolateLeave | RtOp::IsolateCatch) => {
                    Some(Refusal::Isolate)
                }
                _ => None,
            },
            _ => None,
        };
        if r.is_some() {
            return r;
        }
    }
    None
}

type Set = Vec<u64>;
fn has(s: &Set, k: usize) -> bool {
    s[k / 64] >> (k % 64) & 1 == 1
}
fn put(s: &mut Set, k: usize) {
    s[k / 64] |= 1 << (k % 64);
}
fn cut(s: &mut Set, k: usize) {
    s[k / 64] &= !(1 << (k % 64));
}
fn join(s: &mut Set, t: &Set) {
    for (x, y) in s.iter_mut().zip(t) {
        *x |= *y;
    }
}

fn successors(b: &Body, bi: usize) -> Vec<usize> {
    let edge = |e: EdgeId| b.edges[e.index()].target.index();
    match &b.terminators[bi] {
        Terminator::Jump(e) => vec![edge(*e)],
        Terminator::Branch { yes, no, .. } => vec![edge(*yes), edge(*no)],
        Terminator::Switch {
            arms, otherwise, ..
        } => arms
            .get(&b.pools.arms)
            .unwrap_or_default()
            .iter()
            .map(|a| edge(a.edge))
            .chain([edge(*otherwise)])
            .collect(),
        Terminator::Raise { cleanup, .. } => cleanup.iter().map(|c| c.index()).collect(),
        Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => vec![],
    }
}

fn params(b: &Body, bi: usize) -> &[ValueId] {
    b.blocks[bi].params.get(&b.pools.values).unwrap_or_default()
}

/// Names (values, then addresses) live just after each instruction in `wanted`.
fn live_after(b: &Body, wanted: &BTreeSet<usize>) -> BTreeMap<usize, Set> {
    let names = b.values.len() + b.addresses.len();
    let words = names.div_ceil(64).max(1);
    let n = b.blocks.len();
    let mut live_in: Vec<Set> = vec![vec![0; words]; n];
    let mut out = BTreeMap::new();
    let mut pass = |live_in: &mut Vec<Set>, record: bool| -> bool {
        let mut changed = false;
        for bi in (0..n).rev() {
            let mut now = vec![0; words];
            for t in successors(b, bi) {
                let mut s = live_in[t].clone();
                for p in params(b, t) {
                    cut(&mut s, p.index());
                }
                join(&mut now, &s);
            }
            for u in terminator_uses(b, bi) {
                put(&mut now, u);
            }
            let range = b.blocks[bi].insts;
            let first = range.start as usize;
            let insts = range.get(&b.insts).unwrap_or_default();
            for (k, inst) in insts.iter().enumerate().rev() {
                if record && wanted.contains(&(first + k)) {
                    // A site's failure continues with what its fail block needs.
                    let mut after = now.clone();
                    if let Some(f) = inst.fail(b) {
                        join(&mut after, &live_in[f.index()]);
                    }
                    out.insert(first + k, after);
                }
                let (uses, defs) = operands(b, inst);
                for d in defs {
                    cut(&mut now, d);
                }
                for u in uses {
                    put(&mut now, u);
                }
                if let Some(f) = inst.fail(b) {
                    let s = live_in[f.index()].clone();
                    join(&mut now, &s);
                }
            }
            if now != live_in[bi] {
                live_in[bi] = now;
                changed = true;
            }
        }
        changed
    };
    while pass(&mut live_in, false) {}
    pass(&mut live_in, true);
    out
}

/// The values a variant computes in its prologue, so that every resume has
/// them: parameters, constants, and pointers into its record or parameters
/// whose addresses cannot fail. Also the pointer instructions to compute
/// there, in an order that defines each operand first.
fn prologue_plan(body: &Body) -> (Vec<bool>, Vec<usize>) {
    let mut prologue = vec![false; body.values.len()];
    for p in params(body, 0) {
        prologue[p.index()] = true;
    }
    for inst in &body.insts {
        if let Some(v) = hoisted(inst) {
            prologue[v.index()] = true;
        }
    }
    let mut evaluations = vec![0u32; body.addresses.len()];
    for inst in &body.insts {
        if let Inst::Address { address, .. } = *inst {
            evaluations[address.index()] += 1;
        }
    }
    let fixed = |prologue: &[bool], a: AddressId| -> bool {
        let mut x = Some(a);
        while let Some(y) = x {
            let info = &body.addresses[y.index()];
            let steps = info.steps.get(&body.pools.steps).unwrap_or_default();
            if steps
                .iter()
                .any(|s| matches!(s, Step::Deref | Step::Element(_)))
            {
                return false;
            }
            let (values, base) = address_values(body, y);
            if values.iter().any(|v| !prologue[v.index()]) {
                return false;
            }
            x = base;
        }
        true
    };
    let mut pointers = vec![];
    loop {
        let mut changed = false;
        for (k, inst) in body.insts.iter().enumerate() {
            if let Inst::Pointer { result, address } = *inst
                && !prologue[result.index()]
                && fixed(&prologue, address)
            {
                prologue[result.index()] = true;
                pointers.push(k);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (prologue, pointers)
}

/// The address chain of `a`, bases first.
fn chain(body: &Body, a: AddressId) -> Vec<AddressId> {
    let mut out = vec![];
    let mut x = Some(a);
    while let Some(y) = x {
        out.push(y);
        x = address_values(body, y).1;
    }
    out.reverse();
    out
}

fn hoisted(inst: &Inst) -> Option<ValueId> {
    match *inst {
        Inst::Const { result, .. }
        | Inst::FunctionRef { result, .. }
        | Inst::ReadStatic { result, .. }
        | Inst::DescriptorRef { result, .. }
        | Inst::LayoutConst { result, .. } => Some(result),
        _ => None,
    }
}

/// The uses of `inst` mapped through `f`; results, addresses and blocks kept.
fn map_uses(inst: &Inst, f: &mut impl FnMut(ValueId) -> ValueId) -> Inst {
    let mut i = inst.clone();
    match &mut i {
        Inst::Const { .. }
        | Inst::ReadStatic { .. }
        | Inst::FunctionRef { .. }
        | Inst::DescriptorRef { .. }
        | Inst::RegionOpen { .. }
        | Inst::LayoutConst { .. }
        | Inst::Address { .. }
        | Inst::Read { .. }
        | Inst::Pointer { .. }
        | Inst::Case { .. }
        | Inst::SetCase { .. }
        | Inst::Drop { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::BuiltinUse { .. }
        | Inst::LimitExit
        | Inst::Step { .. }
        | Inst::PromoteFrame { .. }
        | Inst::Call { .. } => {}
        Inst::Simd { first, second, .. } => {
            *first = f(*first);
            if let Some(s) = second {
                *s = f(*s);
            }
        }
        Inst::PutLanes { vector, count, .. } => {
            *vector = f(*vector);
            *count = f(*count);
        }
        Inst::Unary { operand, .. }
        | Inst::CompactWidth { word: operand, .. }
        | Inst::ScalarUnary { operand, .. }
        | Inst::Convert { operand, .. } => *operand = f(*operand),
        Inst::Binary { left, right, .. }
        | Inst::Compare { left, right, .. }
        | Inst::IntArith { left, right, .. } => {
            *left = f(*left);
            *right = f(*right);
        }
        Inst::MulAdd {
            left,
            right,
            addend,
            ..
        } => {
            *left = f(*left);
            *right = f(*right);
            *addend = f(*addend);
        }
        Inst::Store { value, .. } => *value = f(*value),
        Inst::UpdateMask { mask, .. } => *mask = f(*mask),
        Inst::Alloc {
            place,
            charge,
            overflow,
            size,
            ..
        } => {
            if let AllocPlace::Like(x)
            | AllocPlace::Region(x)
            | AllocPlace::Frame(x)
            | AllocPlace::LentFrame(x) = place
            {
                *x = f(*x);
            }
            *charge = f(*charge);
            if let Some(o) = overflow {
                *o = f(*o);
            }
            *size = f(*size);
        }
        Inst::Free { block, charge } => {
            *block = f(*block);
            *charge = f(*charge);
        }
        Inst::FrameRefund { charge } => *charge = f(*charge),
        Inst::Copy { bytes, .. } => *bytes = f(*bytes),
        Inst::Grow {
            charge,
            overflow,
            size,
            capacity,
            moved,
            refund,
            ..
        } => {
            for v in [charge, size, capacity, moved, refund] {
                *v = f(*v);
            }
            if let Some(o) = overflow {
                *o = f(*o);
            }
        }
        Inst::RegionClose { region } => *region = f(*region),
        Inst::Trim {
            block,
            region,
            entry,
            previous,
            charge,
        } => {
            for v in [block, region, entry, previous, charge] {
                *v = f(*v);
            }
        }
        Inst::LimitEnter { bytes, .. } => *bytes = f(*bytes),
        Inst::Share { node, .. } | Inst::Release { node, .. } | Inst::TryUnwrap { node, .. } => {
            *node = f(*node)
        }
        Inst::Mark { count, .. } | Inst::Unmark { count, .. } => *count = f(*count),
    }
    i
}

fn map_fail(inst: &mut Inst, f: impl Fn(BlockId) -> BlockId) {
    match inst {
        Inst::Step { fail }
        | Inst::Unary { fail, .. }
        | Inst::Binary { fail, .. }
        | Inst::Simd { fail, .. }
        | Inst::Address { fail, .. }
        | Inst::Drop { fail, .. } => {
            if let Some(b) = fail {
                *b = f(*b);
            }
        }
        _ => {}
    }
}

/// The value operands of an address, and its base address.
fn address_values(b: &Body, a: AddressId) -> (Vec<ValueId>, Option<AddressId>) {
    let info = &b.addresses[a.index()];
    let mut values = vec![];
    let mut base = None;
    match info.base {
        AddressBase::Slot(_) => {}
        AddressBase::Value(v) => values.push(v),
        AddressBase::Address(x) => base = Some(x),
    }
    for s in info.steps.get(&b.pools.steps).unwrap_or_default() {
        if let Step::Index(x) | Step::Element(x) | Step::Byte(x) = *s {
            values.push(x);
        }
    }
    (values, base)
}

fn derefs(b: &Body, a: AddressId) -> bool {
    b.addresses[a.index()]
        .steps
        .get(&b.pools.steps)
        .unwrap_or_default()
        .contains(&Step::Deref)
}

/// What a function's variant needs, computed before any body is built.
#[derive(Clone, Debug)]
struct Shape {
    sites: BTreeMap<usize, Site>,
    /// Values kept in the record across a site.
    demoted: Vec<ValueId>,
    /// Addresses evaluated again at each resume, bases first.
    reevaluated: BTreeMap<usize, Vec<AddressId>>,
    /// Addresses only the site's call takes, built afresh at each resume.
    cloned: BTreeMap<usize, Vec<AddressId>>,
    /// Record offset of each slot.
    slot_at: Vec<u64>,
    value_at: HashMap<ValueId, u64>,
    /// The function's whole area, its own word first.
    area: u64,
}

fn place_layout(module: &Module, recipes: &[hirsel_core_layout::Recipe], t: PlaceTy) -> Option<(u64, u64)> {
    match t {
        PlaceTy::Value(Ty::Scalar(s)) => Some((s.size(), s.size())),
        PlaceTy::Value(Ty::Ptr) => Some((8, 8)),
        PlaceTy::Value(Ty::Vector(_)) => None,
        PlaceTy::Declared(t)
            if matches!(
                module.types[t.index()].repr,
                hirsel_core_layout::Repr::Resource | hirsel_core_layout::Repr::Region
            ) =>
        {
            Some((8, 8))
        }
        PlaceTy::Declared(t) => match &recipes.get(t.index())?.layout {
            hirsel_core_layout::Layout::Fixed(f) => Some((f.size, f.align.max(1))),
            _ => None,
        },
    }
}

fn round(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}

/// The shape of `body`'s variant whose sites are `sites`, with the callee
/// areas `area`.
fn shape(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    body: &Body,
    sites: BTreeMap<usize, Site>,
    area: &dyn Fn(FuncId) -> u64,
) -> Result<Shape, Refusal> {
    if sites.is_empty() {
        return Err(Refusal::NoSite);
    }
    let nv = body.values.len();
    let (prologue, _) = prologue_plan(body);
    let wanted: BTreeSet<usize> = sites.keys().copied().collect();
    let live = live_after(body, &wanted);
    let mut demote = vec![false; nv];
    let mut addresses = BTreeMap::new();
    // An address needed after a site is evaluated again at the resume, under
    // its own name, from parameters and constants alone.
    let needed = |a: AddressId, list: &mut Vec<AddressId>| -> Result<(), Refusal> {
        let mut stack = vec![a];
        let mut chain = vec![];
        while let Some(x) = stack.pop() {
            if derefs(body, x) {
                return Err(Refusal::LiveAddress);
            }
            let (values, base) = address_values(body, x);
            if values.iter().any(|v| !prologue[v.index()]) {
                return Err(Refusal::LiveAddress);
            }
            chain.push(x);
            stack.extend(base);
        }
        for x in chain.into_iter().rev() {
            if !list.contains(&x) {
                list.push(x);
            }
        }
        Ok(())
    };
    let mut cloned = BTreeMap::new();
    for (&k, set) in &live {
        let Inst::Call { call } = body.insts[k] else {
            unreachable!("a site is a call")
        };
        let mut list = vec![];
        for name in (0..nv + body.addresses.len()).filter(|&n| has(set, n)) {
            if name < nv {
                if !prologue[name] {
                    demote[name] = true;
                }
            } else {
                needed(AddressId((name - nv) as u32), &mut list)?;
            }
        }
        // The call's own operands: its values come from the record, and an
        // address it takes that nothing needs after it is built afresh at the
        // resume from operands kept in the record.
        let mut fresh = vec![];
        let site = &body.calls[call.index()];
        if let Callee::Indirect(v) = site.callee
            && !prologue[v.index()]
        {
            demote[v.index()] = true;
        }
        for arg in site.args.get(&body.pools.args).unwrap_or_default() {
            match *arg {
                Arg::Value(v) if !prologue[v.index()] => demote[v.index()] = true,
                Arg::Value(_) => {}
                Arg::Address(a) if list.contains(&a) => {}
                Arg::Address(a) => {
                    let mut stack = vec![a];
                    let mut chain = vec![];
                    while let Some(x) = stack.pop() {
                        if list.contains(&x) {
                            continue;
                        }
                        if derefs(body, x) {
                            return Err(Refusal::LiveAddress);
                        }
                        let (values, base) = address_values(body, x);
                        for v in values {
                            if !prologue[v.index()] {
                                demote[v.index()] = true;
                            }
                        }
                        chain.push(x);
                        stack.extend(base);
                    }
                    for x in chain.into_iter().rev() {
                        if !fresh.contains(&x) {
                            fresh.push(x);
                        }
                    }
                }
            }
        }
        addresses.insert(k, list);
        cloned.insert(k, fresh);
    }
    // An address whose operands are kept in the record is evaluated where it
    // was, from reloaded operands: that needs one evaluation.
    let mut evaluations = vec![0u32; body.addresses.len()];
    for inst in &body.insts {
        if let Inst::Address { address, .. } = *inst {
            evaluations[address.index()] += 1;
        }
    }
    for (a, &count) in evaluations.iter().enumerate() {
        if count > 1 {
            let (values, _) = address_values(body, AddressId(a as u32));
            if values.iter().any(|v| demote[v.index()]) {
                return Err(Refusal::LiveAddress);
            }
        }
    }
    let child = sites
        .values()
        .map(|s| match *s {
            Site::Leaf(_) => 8,
            Site::Callee(g) => area(g),
        })
        .max()
        .unwrap_or(8);
    let mut at = round(CHILD + child, ALIGN);
    let mut slot_at = Vec::with_capacity(body.slots.len());
    for &t in &body.slots {
        let (size, align) = place_layout(module, recipes, t).ok_or(Refusal::SlotLayout)?;
        if align > ALIGN {
            return Err(Refusal::SlotLayout);
        }
        at = round(at, align);
        slot_at.push(at);
        at += size;
    }
    at = round(at, 8);
    let mut value_at = HashMap::new();
    let demoted: Vec<ValueId> = (0..nv)
        .filter(|&v| demote[v])
        .map(|v| ValueId(v as u32))
        .collect();
    for &v in &demoted {
        value_at.insert(v, at);
        at += 8;
    }
    Ok(Shape {
        sites,
        demoted,
        reevaluated: addresses,
        cloned,
        slot_at,
        value_at,
        area: round(at, ALIGN),
    })
}

/// Builds a variant body.
struct Build<'a> {
    old: &'a Body,
    n: Body,
    /// Instructions of the block being built.
    cur: Vec<(Inst, Option<Anchor>)>,
    /// Each closed block's instructions, laid out in block order at the end.
    done: Vec<Vec<(Inst, Option<Anchor>)>>,
    /// Original values readable here without a reload, to what to use.
    local: HashMap<ValueId, ValueId>,
    home: HashMap<ValueId, AddressId>,
}

impl Build<'_> {
    fn value(&mut self, ty: Ty) -> ValueId {
        self.n.values.push(ty);
        ValueId(self.n.values.len() as u32 - 1)
    }
    fn emit(&mut self, inst: Inst) {
        self.cur.push((inst, None));
    }
    fn konst(&mut self, ty: Ty, bits: u64) -> ValueId {
        let result = self.value(ty);
        self.emit(Inst::Const { result, bits });
        result
    }
    fn address(&mut self, base: AddressBase, steps: &[Step], ty: PlaceTy) -> AddressId {
        let steps = PoolRange::push(&mut self.n.pools.steps, steps.iter().copied());
        self.n.addresses.push(AddressInfo { base, steps, ty });
        AddressId(self.n.addresses.len() as u32 - 1)
    }
    /// A pointer `offset` bytes into the record at `rec`, made in the prologue.
    fn at(&mut self, rec: ValueId, offset: u64) -> ValueId {
        let off = self.konst(Ty::Scalar(Scalar::U64), offset);
        let a = self.address(
            AddressBase::Value(rec),
            &[Step::Byte(off)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
        );
        self.emit(Inst::Address {
            address: a,
            fail: None,
        });
        let result = self.value(Ty::Ptr);
        self.emit(Inst::Pointer { result, address: a });
        result
    }
    /// The value of original `v` here: itself, or a reload from its home.
    fn get(&mut self, v: ValueId) -> ValueId {
        if let Some(&x) = self.local.get(&v) {
            return x;
        }
        let Some(&home) = self.home.get(&v) else {
            return v;
        };
        let result = self.value(self.old.values[v.index()]);
        self.emit(Inst::Read {
            result,
            address: home,
            mode: ReadMode::Bits,
        });
        self.local.insert(v, result);
        result
    }
    /// Original `v` was just defined here.
    fn defined(&mut self, v: ValueId) {
        if let Some(&home) = self.home.get(&v) {
            self.emit(Inst::Store {
                address: home,
                value: v,
            });
        }
        self.local.insert(v, v);
    }
    fn block(&mut self) -> BlockId {
        self.n.blocks.push(Block {
            params: PoolRange::EMPTY,
            insts: PoolRange::EMPTY,
            anchor: None,
        });
        self.n.terminators.push(Terminator::Unreachable);
        BlockId(self.n.blocks.len() as u32 - 1)
    }
    /// End the block being built as `id`.
    fn close(&mut self, id: BlockId, terminator: Terminator, anchor: Option<Anchor>) {
        if self.done.len() <= id.index() {
            self.done.resize_with(id.index() + 1, Vec::new);
        }
        self.done[id.index()] = std::mem::take(&mut self.cur);
        self.n.blocks[id.index()].anchor = anchor;
        self.n.terminators[id.index()] = terminator;
    }
    /// The body, its blocks' instructions in block order.
    fn finish(mut self) -> Body {
        for (id, insts) in std::mem::take(&mut self.done).into_iter().enumerate() {
            let start = self.n.insts.len() as u32;
            for (inst, a) in insts {
                self.n.insts.push(inst);
                self.n.inst_anchors.push(a);
            }
            let len = self.n.insts.len() as u32 - start;
            self.n.blocks[id].insts = PoolRange::new(start, len);
        }
        self.n
    }
    fn edge(&mut self, target: BlockId, args: Vec<ValueId>, cold: bool) -> EdgeId {
        let args = PoolRange::push(&mut self.n.pools.values, args);
        self.n.edges.push(Edge { target, args, cold });
        EdgeId(self.n.edges.len() as u32 - 1)
    }
}

/// How a variant gets its record and calls its sites.
struct Calls<'a> {
    /// The variant and its signature for a represented callee.
    variant: &'a dyn Fn(FuncId) -> (FuncId, SigId),
    returnable: &'a dyn Fn(HostOp) -> SigId,
    frame_record: SigId,
}

/// The variant of `old` with shape `s`. A root makes its record with
/// `FrameRecord`; any other variant takes it as a trailing parameter.
fn build(old: &Body, s: &Shape, root: bool, results_ty: &[Ty], calls: &Calls) -> Body {
    let nb = old.blocks.len();
    let mut b = Build {
        old,
        n: Body {
            callable_parameters: old.callable_parameters.clone(),
            values: old.values.clone(),
            slots: vec![],
            addresses: old.addresses.clone(),
            blocks: vec![],
            terminators: vec![],
            insts: vec![],
            inst_anchors: vec![],
            calls: vec![],
            edges: vec![],
            pools: Pools {
                values: vec![],
                steps: old.pools.steps.clone(),
                args: vec![],
                arms: vec![],
            },
            compact_roots: old.compact_roots.clone(),
        },
        cur: vec![],
        done: vec![],
        local: HashMap::new(),
        home: HashMap::new(),
    };
    // Block 0 is the prologue; original block i > 0 keeps its number and the
    // original entry becomes block `nb`.
    for _ in 0..=nb {
        b.block();
    }
    let map = |x: BlockId| if x.0 == 0 { BlockId(nb as u32) } else { x };
    let u64_ = Ty::Scalar(Scalar::U64);
    // Prologue: parameters, the record, constants, homes, then the dispatch.
    let mut entry: Vec<ValueId> = params(old, 0).to_vec();
    for inst in &old.insts {
        if hoisted(inst).is_some() {
            b.emit(inst.clone());
        }
    }
    let rec = if root {
        let size = b.konst(u64_, s.area);
        let rec = b.value(Ty::Ptr);
        let args = PoolRange::push(&mut b.n.pools.args, [Arg::Value(size)]);
        let results = PoolRange::push(&mut b.n.pools.values, [rec]);
        b.n.calls.push(CallSite {
            callee: Callee::Runtime(RtOp::FrameRecord),
            signature: calls.frame_record,
            args,
            results,
            fail: None,
            cold: false,
        });
        b.emit(Inst::Call {
            call: CallId(b.n.calls.len() as u32 - 1),
        });
        rec
    } else {
        let rec = b.value(Ty::Ptr);
        entry.push(rec);
        rec
    };
    b.n.blocks[0].params = PoolRange::push(&mut b.n.pools.values, entry);
    let word = |b: &mut Build, p: ValueId| {
        let a = b.address(AddressBase::Value(p), &[], PlaceTy::Value(u64_));
        b.emit(Inst::Address {
            address: a,
            fail: None,
        });
        a
    };
    let own = word(&mut b, rec);
    let child = b.at(rec, CHILD);
    let child_word = word(&mut b, child);
    let zero = b.konst(u64_, 0);
    let slot_ptr: Vec<ValueId> = s.slot_at.iter().map(|&off| b.at(rec, off)).collect();
    // Slots live in the record.
    for info in &mut b.n.addresses {
        if let AddressBase::Slot(slot) = info.base {
            info.base = AddressBase::Value(slot_ptr[slot.index()]);
        }
    }
    let (_, pointers) = prologue_plan(old);
    for &k in &pointers {
        let Inst::Pointer { address, .. } = old.insts[k] else {
            unreachable!()
        };
        for a in chain(old, address) {
            b.emit(Inst::Address {
                address: a,
                fail: None,
            });
        }
        b.emit(old.insts[k].clone());
    }
    let skip: BTreeSet<usize> = pointers.into_iter().collect();
    for &v in &s.demoted {
        let p = b.at(rec, s.value_at[&v]);
        let a = b.address(
            AddressBase::Value(p),
            &[],
            PlaceTy::Value(old.values[v.index()]),
        );
        b.emit(Inst::Address {
            address: a,
            fail: None,
        });
        b.home.insert(v, a);
    }
    let sites: Vec<(usize, Site)> = s.sites.iter().map(|(&k, &v)| (k, v)).collect();
    let number: HashMap<usize, u64> = sites
        .iter()
        .enumerate()
        .map(|(i, (k, _))| (*k, i as u64 + 1))
        .collect();
    let site_const: Vec<ValueId> = (0..sites.len())
        .map(|i| b.konst(u64_, i as u64 + 1))
        .collect();
    let dummies: Vec<ValueId> = results_ty.iter().map(|&t| b.konst(t, 0)).collect();
    let own_word = b.value(u64_);
    b.emit(Inst::Read {
        result: own_word,
        address: own,
        mode: ReadMode::Bits,
    });
    // Resume blocks, one per site, and the dispatch.
    let resume: Vec<BlockId> = sites.iter().map(|_| b.block()).collect();
    let check: Vec<BlockId> = sites.iter().map(|_| b.block()).collect();
    let park: Vec<BlockId> = sites.iter().map(|_| b.block()).collect();
    let post: Vec<BlockId> = sites.iter().map(|_| b.block()).collect();
    let mut arms = vec![];
    for (i, &r) in resume.iter().enumerate() {
        let edge = b.edge(r, vec![], true);
        arms.push(SwitchArm {
            value: i as u64 + 1,
            edge,
        });
    }
    let otherwise = b.edge(BlockId(nb as u32), vec![], false);
    let arms = PoolRange::push(&mut b.n.pools.arms, arms);
    b.close(
        BlockId(0),
        Terminator::Switch {
            value: own_word,
            arms,
            otherwise,
        },
        None,
    );
    // The call a site makes, with its operands mapped by `get`.
    let site_call = |b: &mut Build,
                     call: &CallSite,
                     site: Site,
                     results: Vec<ValueId>,
                     fresh: &HashMap<AddressId, AddressId>|
     -> CallId {
        let old_args: Vec<Arg> = call.args.get(&old.pools.args).unwrap_or_default().to_vec();
        let mut args: Vec<Arg> = old_args
            .iter()
            .map(|a| match *a {
                Arg::Value(v) => Arg::Value(b.get(v)),
                Arg::Address(a) => Arg::Address(*fresh.get(&a).unwrap_or(&a)),
            })
            .collect();
        let (callee, signature) = match site {
            Site::Leaf(op) => {
                args.insert(0, Arg::Value(child));
                (Callee::Runtime(RtOp::Returnable(op)), (calls.returnable)(op))
            }
            Site::Callee(g) => {
                args.push(Arg::Value(child));
                let (v, sig) = (calls.variant)(g);
                (Callee::Direct(v), sig)
            }
        };
        let args = PoolRange::push(&mut b.n.pools.args, args);
        let results = PoolRange::push(&mut b.n.pools.values, results);
        b.n.calls.push(CallSite {
            callee,
            signature,
            args,
            results,
            fail: call.fail.map(map),
            cold: call.cold,
        });
        CallId(b.n.calls.len() as u32 - 1)
    };
    let fresh_params = |b: &mut Build, bi: usize| -> PoolRange<ValueId> {
        let ps = params(old, bi).to_vec();
        PoolRange::push(&mut b.n.pools.values, ps)
    };
    for bi in 0..nb {
        let id = map(BlockId(bi as u32));
        if bi != 0 {
            b.n.blocks[id.index()].params = fresh_params(&mut b, bi);
        }
        b.local.clear();
        let mut current = id;
        for &p in params(old, bi) {
            b.defined(p);
        }
        if bi == 0 {
            b.local.clear();
        }
        let range = old.blocks[bi].insts;
        let first = range.start as usize;
        for (j, inst) in range.get(&old.insts).unwrap_or_default().iter().enumerate() {
            let k = first + j;
            let anchor = old.inst_anchors.get(k).copied().flatten();
            if hoisted(inst).is_some()
                || skip.contains(&k)
                || matches!(inst, Inst::StorageLive { .. } | Inst::StorageDead { .. })
            {
                continue;
            }
            if let Some(&site) = s.sites.get(&k) {
                let Inst::Call { call } = *inst else {
                    unreachable!()
                };
                let i = sites.iter().position(|(x, _)| *x == k).unwrap();
                let old_call = &old.calls[call.index()];
                let results: Vec<ValueId> = old_call
                    .results
                    .get(&old.pools.values)
                    .unwrap_or_default()
                    .to_vec();
                let c = site_call(&mut b, old_call, site, results.clone(), &HashMap::new());
                b.cur.push((Inst::Call { call: c }, anchor));
                for &r in &results {
                    b.defined(r);
                }
                let e = b.edge(check[i], vec![], false);
                b.close(current, Terminator::Jump(e), None);
                // The resume: clear the word, evaluate the needed addresses
                // again, make the call again.
                b.local.clear();
                b.emit(Inst::Store {
                    address: own,
                    value: zero,
                });
                for &a in &s.reevaluated[&k] {
                    let fail = old.insts.iter().find_map(|x| match *x {
                        Inst::Address { address, fail } if address == a => Some(fail),
                        _ => None,
                    });
                    b.emit(Inst::Address {
                        address: a,
                        fail: fail.flatten().map(map),
                    });
                }
                let mut fresh = HashMap::new();
                for &a in &s.cloned[&k] {
                    let mut info = old.addresses[a.index()].clone();
                    info.base = match info.base {
                        AddressBase::Slot(slot) => AddressBase::Value(slot_ptr[slot.index()]),
                        AddressBase::Value(v) => AddressBase::Value(b.get(v)),
                        AddressBase::Address(x) => AddressBase::Address(*fresh.get(&x).unwrap_or(&x)),
                    };
                    let steps: Vec<Step> = info
                        .steps
                        .get(&old.pools.steps)
                        .unwrap_or_default()
                        .iter()
                        .map(|s| match *s {
                            Step::Index(x) => Step::Index(b.get(x)),
                            Step::Element(x) => Step::Element(b.get(x)),
                            Step::Byte(x) => Step::Byte(b.get(x)),
                            other => other,
                        })
                        .collect();
                    let copy = b.address(info.base, &steps, info.ty);
                    let fail = old.insts.iter().find_map(|x| match *x {
                        Inst::Address { address, fail } if address == a => Some(fail),
                        _ => None,
                    });
                    b.emit(Inst::Address {
                        address: copy,
                        fail: fail.flatten().map(map),
                    });
                    fresh.insert(a, copy);
                }
                let again: Vec<ValueId> = results
                    .iter()
                    .map(|r| b.value(old.values[r.index()]))
                    .collect();
                let c = site_call(&mut b, old_call, site, again.clone(), &fresh);
                b.cur.push((Inst::Call { call: c }, anchor));
                for (r, x) in results.iter().zip(&again) {
                    if let Some(&home) = b.home.get(r) {
                        b.emit(Inst::Store {
                            address: home,
                            value: *x,
                        });
                    }
                }
                let e = b.edge(check[i], vec![], false);
                b.close(resume[i], Terminator::Jump(e), None);
                // The check: a parked callee leaves its word nonzero.
                b.local.clear();
                let w = b.value(u64_);
                b.emit(Inst::Read {
                    result: w,
                    address: child_word,
                    mode: ReadMode::Bits,
                });
                let parked = b.value(Ty::Scalar(Scalar::Bool));
                b.emit(Inst::Compare {
                    result: parked,
                    op: CompareOp::Ne,
                    left: w,
                    right: zero,
                });
                let yes = b.edge(park[i], vec![], true);
                let no = b.edge(post[i], vec![], false);
                b.close(
                    check[i],
                    Terminator::Branch {
                        condition: parked,
                        yes,
                        no,
                    },
                    None,
                );
                b.emit(Inst::Store {
                    address: own,
                    value: site_const[number[&k] as usize - 1],
                });
                let ret = PoolRange::push(&mut b.n.pools.values, dummies.clone());
                b.close(park[i], Terminator::Return(ret), None);
                current = post[i];
                continue;
            }
            // Operands of an address whose values live in the record.
            if let Inst::Address { address, .. } = *inst {
                let (values, _) = address_values(old, address);
                if values.iter().any(|v| b.home.contains_key(v)) {
                    let mut info = old.addresses[address.index()].clone();
                    if let AddressBase::Slot(slot) = info.base {
                        info.base = AddressBase::Value(slot_ptr[slot.index()]);
                    }
                    if let AddressBase::Value(v) = info.base {
                        info.base = AddressBase::Value(b.get(v));
                    }
                    let steps: Vec<Step> = info
                        .steps
                        .get(&old.pools.steps)
                        .unwrap_or_default()
                        .iter()
                        .map(|s| match *s {
                            Step::Index(x) => Step::Index(b.get(x)),
                            Step::Element(x) => Step::Element(b.get(x)),
                            Step::Byte(x) => Step::Byte(b.get(x)),
                            other => other,
                        })
                        .collect();
                    info.steps = PoolRange::push(&mut b.n.pools.steps, steps);
                    b.n.addresses[address.index()] = info;
                }
            }
            let mut new = if let Inst::Call { call } = *inst {
                let old_call = &old.calls[call.index()];
                let callee = match old_call.callee {
                    Callee::Indirect(v) => Callee::Indirect(b.get(v)),
                    c => c,
                };
                let args: Vec<Arg> = old_call
                    .args
                    .get(&old.pools.args)
                    .unwrap_or_default()
                    .iter()
                    .map(|a| match *a {
                        Arg::Value(v) => Arg::Value(b.get(v)),
                        Arg::Address(a) => Arg::Address(a),
                    })
                    .collect();
                let args = PoolRange::push(&mut b.n.pools.args, args);
                let results = PoolRange::push(
                    &mut b.n.pools.values,
                    old_call
                        .results
                        .get(&old.pools.values)
                        .unwrap_or_default()
                        .iter()
                        .copied(),
                );
                b.n.calls.push(CallSite {
                    callee,
                    signature: old_call.signature,
                    args,
                    results,
                    fail: old_call.fail.map(map),
                    cold: old_call.cold,
                });
                Inst::Call {
                    call: CallId(b.n.calls.len() as u32 - 1),
                }
            } else {
                map_uses(inst, &mut |v| b.get(v))
            };
            map_fail(&mut new, map);
            b.cur.push((new, anchor));
            let (_, defs) = operands(old, inst);
            for d in defs {
                if d < old.values.len() {
                    b.defined(ValueId(d as u32));
                }
            }
        }
        // The terminator, its operands mapped.
        let edge = |b: &mut Build, e: EdgeId| -> EdgeId {
            let old_edge = &old.edges[e.index()];
            let args: Vec<ValueId> = old_edge
                .args
                .get(&old.pools.values)
                .unwrap_or_default()
                .iter()
                .map(|&v| b.get(v))
                .collect();
            b.edge(map(old_edge.target), args, old_edge.cold)
        };
        let terminator = match &old.terminators[bi] {
            Terminator::Jump(e) => Terminator::Jump(edge(&mut b, *e)),
            Terminator::Branch { condition, yes, no } => {
                let condition = b.get(*condition);
                let yes = edge(&mut b, *yes);
                let no = edge(&mut b, *no);
                Terminator::Branch { condition, yes, no }
            }
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                let value = b.get(*value);
                let old_arms = arms.get(&old.pools.arms).unwrap_or_default().to_vec();
                let mut new_arms = vec![];
                for arm in old_arms {
                    new_arms.push(SwitchArm {
                        value: arm.value,
                        edge: edge(&mut b, arm.edge),
                    });
                }
                let otherwise = edge(&mut b, *otherwise);
                Terminator::Switch {
                    value,
                    arms: PoolRange::push(&mut b.n.pools.arms, new_arms),
                    otherwise,
                }
            }
            Terminator::Return(r) => {
                let values: Vec<ValueId> = r
                    .get(&old.pools.values)
                    .unwrap_or_default()
                    .to_vec()
                    .into_iter()
                    .map(|v| b.get(v))
                    .collect();
                Terminator::Return(PoolRange::push(&mut b.n.pools.values, values))
            }
            Terminator::Raise { cause, cleanup } => {
                let cause = match *cause {
                    RaiseCause::Capacity {
                        requested,
                        available,
                    } => RaiseCause::Capacity {
                        requested: b.get(requested),
                        available: b.get(available),
                    },
                    other => other,
                };
                Terminator::Raise {
                    cause,
                    cleanup: cleanup.map(map),
                }
            }
            t @ (Terminator::Propagate | Terminator::Unreachable) => t.clone(),
        };
        b.close(current, terminator, old.blocks[bi].anchor);
    }
    b.finish()
}

/// What [`apply`] did, for the driver's report.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    /// Functions with a represented variant, and why the others that reach an
    /// owned wait were refused.
    pub represented: Vec<FuncId>,
    pub refused: Vec<(FuncId, Refusal)>,
    /// Spawns whose task entry was replaced by a root variant.
    pub roots: usize,
    /// Spawns of entries that reach an owned wait but stay stackful.
    pub stackful_roots: usize,
}

/// A spawn of the task entry adapter `entry` whose stored callee is `callee`,
/// found in the body of `function` at call `call`.
struct Spawn {
    function: usize,
    reference: usize,
    entry: FuncId,
    callee: FuncId,
    indirect: CallId,
}

/// The spawns whose entry adapter calls a stored function: the spawning body
/// stores `fnref C` into the input's field 0 and passes `fnref E`, and `E`
/// makes one indirect call of the code it reads from its input's field 0.
fn spawns(module: &Module) -> Vec<Spawn> {
    let mut out = vec![];
    for (f, function) in module.functions.iter().enumerate() {
        let Some(body) = &function.body else { continue };
        let refs: HashMap<ValueId, (usize, FuncId)> = body
            .insts
            .iter()
            .enumerate()
            .filter_map(|(k, i)| match *i {
                Inst::FunctionRef { result, function } => Some((result, (k, function))),
                _ => None,
            })
            .collect();
        let mut uses = HashMap::<ValueId, usize>::new();
        for inst in &body.insts {
            for u in operands(body, inst).0 {
                if u < body.values.len() {
                    *uses.entry(ValueId(u as u32)).or_default() += 1;
                }
            }
        }
        for bi in 0..body.blocks.len() {
            for u in terminator_uses(body, bi) {
                if u < body.values.len() {
                    *uses.entry(ValueId(u as u32)).or_default() += 1;
                }
            }
        }
        for inst in &body.insts {
            let Inst::Call { call } = *inst else { continue };
            let c = &body.calls[call.index()];
            if !matches!(c.callee, Callee::Runtime(RtOp::Spawn { .. } | RtOp::SpawnUnit { .. })) {
                continue;
            }
            let args = c.args.get(&body.pools.args).unwrap_or_default();
            let (Some(Arg::Address(input)), Some(Arg::Value(code))) = (args.get(1), args.get(2))
            else {
                continue;
            };
            let Some(&(reference, entry)) = refs.get(code) else {
                continue;
            };
            if uses.get(code) != Some(&1) {
                continue;
            }
            // The callee stored into field 0 of the input.
            let stored = body.insts.iter().find_map(|i| match *i {
                Inst::Store { address, value } => {
                    let info = &body.addresses[address.index()];
                    let steps = info.steps.get(&body.pools.steps).unwrap_or_default();
                    (info.base == AddressBase::Address(*input) && steps == [Step::Field(0)])
                        .then_some(value)
                }
                _ => None,
            });
            let Some(callee) = stored.and_then(|v| refs.get(&v)).map(|r| r.1) else {
                continue;
            };
            let Some(entry_body) = &module.functions[entry.index()].body else {
                continue;
            };
            let indirect: Vec<CallId> = entry_body
                .insts
                .iter()
                .filter_map(|i| match *i {
                    Inst::Call { call }
                        if matches!(entry_body.calls[call.index()].callee, Callee::Indirect(_)) =>
                    {
                        Some(call)
                    }
                    _ => None,
                })
                .collect();
            let [indirect] = indirect[..] else { continue };
            if entry_body.calls[indirect.index()].signature
                != module.functions[callee.index()].signature
                && module.signatures[entry_body.calls[indirect.index()].signature.index()]
                    != module.signatures[module.functions[callee.index()].signature.index()]
            {
                continue;
            }
            out.push(Spawn {
                function: f,
                reference,
                entry,
                callee,
                indirect,
            });
        }
    }
    out
}

fn sig_id(module: &mut Module, s: Signature) -> SigId {
    if let Some(i) = module.signatures.iter().position(|x| *x == s) {
        return SigId(i as u32);
    }
    module.signatures.push(s);
    SigId(module.signatures.len() as u32 - 1)
}

fn sites_of(body: &Body, ok: &[bool]) -> BTreeMap<usize, Site> {
    let mut sites = BTreeMap::new();
    for (k, inst) in body.insts.iter().enumerate() {
        if let Inst::Call { call } = *inst {
            let c = &body.calls[call.index()];
            if let Some(op) = leaf(c.callee) {
                sites.insert(k, Site::Leaf(op));
            } else if let Callee::Direct(g) = c.callee
                && ok[g.index()]
            {
                sites.insert(k, Site::Callee(g));
            }
        }
    }
    sites
}

/// Give every represented task entry and the functions it reaches a variant
/// that parks at owned waits with its stack given back.
pub fn apply(module: &mut Module) -> Applied {
    let mut applied = Applied::default();
    let Ok(recipes) = hirsel_core_layout::compute(&module.types) else {
        return applied;
    };
    let n = module.functions.len();
    // Functions that reach a wait through direct calls.
    let mut reaches = vec![false; n];
    loop {
        let mut changed = false;
        for f in 0..n {
            if reaches[f] {
                continue;
            }
            let Some(body) = &module.functions[f].body else {
                continue;
            };
            if body.calls.iter().any(|c| {
                leaf(c.callee).is_some()
                    || matches!(c.callee, Callee::Direct(g) if reaches[g.index()])
            }) {
                reaches[f] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut ok = vec![false; n];
    let mut why: BTreeMap<usize, Refusal> = BTreeMap::new();
    for f in 0..n {
        if !reaches[f] {
            continue;
        }
        let function = &module.functions[f];
        let body = function.body.as_ref().expect("reaching bodies are defined");
        match refusal(module, function, body) {
            Some(r) => {
                why.insert(f, r);
            }
            None => ok[f] = true,
        }
    }
    // Shapes, callees first; a refusal changes its callers' sites, so start
    // over until none is refused.
    let mut shapes: BTreeMap<usize, Shape> = BTreeMap::new();
    'outer: loop {
        shapes.clear();
        loop {
            let mut progress = false;
            for f in 0..n {
                if !ok[f] || shapes.contains_key(&f) {
                    continue;
                }
                let body = module.functions[f].body.as_ref().unwrap();
                let sites = sites_of(body, &ok);
                if sites
                    .values()
                    .any(|s| matches!(*s, Site::Callee(g) if !shapes.contains_key(&g.index())))
                {
                    continue;
                }
                let area = |g: FuncId| shapes[&g.index()].area;
                match shape(module, &recipes, body, sites, &area) {
                    Ok(s) => {
                        shapes.insert(f, s);
                        progress = true;
                    }
                    Err(r) => {
                        ok[f] = false;
                        why.insert(f, r);
                        continue 'outer;
                    }
                }
            }
            if !progress {
                break;
            }
        }
        // What remains waits on itself through a cycle.
        let cyclic: Vec<usize> = (0..n).filter(|&f| ok[f] && !shapes.contains_key(&f)).collect();
        if cyclic.is_empty() {
            break;
        }
        for f in cyclic {
            ok[f] = false;
            why.insert(f, Refusal::Recursive);
        }
    }
    // Roots.
    let mut roots = vec![];
    for spawn in spawns(module) {
        let entry = &module.functions[spawn.entry.index()];
        if !ok[spawn.callee.index()]
            || module.step_free.contains_key(&spawn.entry)
            || refusal(module, entry, entry.body.as_ref().unwrap()).is_some()
        {
            if reaches[spawn.callee.index()] {
                applied.stackful_roots += 1;
            }
            continue;
        }
        let mut body = entry.body.clone().unwrap();
        body.calls[spawn.indirect.index()].callee = Callee::Direct(spawn.callee);
        body.calls[spawn.indirect.index()].signature =
            module.functions[spawn.callee.index()].signature;
        let sites = sites_of(&body, &ok);
        let area = |g: FuncId| shapes[&g.index()].area;
        match shape(module, &recipes, &body, sites, &area) {
            Ok(s) => roots.push((spawn, body, s)),
            Err(_) => applied.stackful_roots += 1,
        }
    }
    // Only what a root reaches gets a variant.
    let mut needed = vec![false; n];
    let mut work: Vec<usize> = roots.iter().map(|r| r.0.callee.index()).collect();
    while let Some(f) = work.pop() {
        if std::mem::replace(&mut needed[f], true) {
            continue;
        }
        for site in shapes[&f].sites.values() {
            if let Site::Callee(g) = *site {
                work.push(g.index());
            }
        }
    }
    for (&f, r) in &why {
        applied.refused.push((FuncId(f as u32), *r));
    }
    if roots.is_empty() {
        return applied;
    }
    let frame_record = sig_id(module, RtOp::FrameRecord.signature());
    let returnable: BTreeMap<u8, SigId> = [HostOp::RecvBounded, HostOp::SendPair]
        .into_iter()
        .map(|op| (op.code(), sig_id(module, RtOp::Returnable(op).signature())))
        .collect();
    let mut variant: BTreeMap<usize, (FuncId, SigId)> = BTreeMap::new();
    for f in (0..n).filter(|&f| needed[f]) {
        let canonical = &module.functions[f];
        let mut sig = module.signatures[canonical.signature.index()].clone();
        sig.params.push(Param::Value(Ty::Ptr));
        let instance = canonical.instance;
        let versioned = canonical.versioned;
        let identity = module.code_identity(FuncId(f as u32));
        let sig = sig_id(module, sig);
        let id = FuncId(module.functions.len() as u32);
        module.functions.push(Function {
            instance,
            signature: sig,
            body: None,
            versioned,
        });
        module.code_variants.insert(
            id,
            ContentHash::of_parts("hirsel.returnable.v1", [identity.0.as_slice()]),
        );
        variant.insert(f, (id, sig));
        applied.represented.push(FuncId(f as u32));
    }
    let lookup = |g: FuncId| variant[&g.index()];
    let ret = |op: HostOp| returnable[&op.code()];
    let calls = Calls {
        variant: &lookup,
        returnable: &ret,
        frame_record,
    };
    let mut bodies = vec![];
    for (&f, &(id, _)) in &variant {
        let old = module.functions[f].body.as_ref().unwrap();
        let results = &module.signatures[module.functions[f].signature.index()].results;
        bodies.push((id, build(old, &shapes[&f], false, results, &calls)));
    }
    let mut entries: BTreeMap<(FuncId, FuncId), FuncId> = BTreeMap::new();
    let mut root_bodies = vec![];
    let mut rewrites = vec![];
    for (spawn, body, s) in &roots {
        let key = (spawn.entry, spawn.callee);
        let id = match entries.get(&key) {
            Some(&id) => id,
            None => {
                let canonical = &module.functions[spawn.entry.index()];
                let id = FuncId((module.functions.len() + root_bodies.len()) as u32);
                let identity = module.code_identity(spawn.entry);
                let callee = module.code_identity(spawn.callee);
                root_bodies.push((
                    Function {
                        instance: canonical.instance,
                        signature: canonical.signature,
                        body: Some(build(
                            body,
                            s,
                            true,
                            &module.signatures[canonical.signature.index()].results,
                            &calls,
                        )),
                        versioned: canonical.versioned,
                    },
                    ContentHash::of_parts(
                        "hirsel.returnable.root.v1",
                        [identity.0.as_slice(), callee.0.as_slice()],
                    ),
                ));
                entries.insert(key, id);
                id
            }
        };
        rewrites.push((spawn.function, spawn.reference, id));
    }
    for (id, body) in bodies {
        module.functions[id.index()].body = Some(body);
    }
    for (function, hash) in root_bodies {
        let id = FuncId(module.functions.len() as u32);
        module.functions.push(function);
        module.code_variants.insert(id, hash);
    }
    for (f, k, id) in rewrites {
        let body = module.functions[f].body.as_mut().unwrap();
        if let Inst::FunctionRef { function, .. } = &mut body.insts[k] {
            *function = id;
        }
        applied.roots += 1;
    }
    applied
}

/// A line per function that reaches an owned wait, for `HIRSEL_RETURNABLE_REPORT`.
pub fn report(module: &Module, applied: &Applied) -> String {
    let name = |f: FuncId| {
        module.instances[module.functions[f.index()].instance.index()]
            .definition
            .to_string()
    };
    let mut s = format!(
        "returnable: {} represented, {} refused, {} roots, {} stackful roots\n",
        applied.represented.len(),
        applied.refused.len(),
        applied.roots,
        applied.stackful_roots
    );
    for &f in &applied.represented {
        s.push_str(&format!("  represented f{} {}\n", f.0, name(f)));
    }
    for &(f, r) in &applied.refused {
        s.push_str(&format!("  refused f{} {} {r:?}\n", f.0, name(f)));
    }
    s
}
