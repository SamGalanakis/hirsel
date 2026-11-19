//! Exact step batching. A reservation covers the bound and the current slice.
//! Fast copies refund their unexecuted suffix; short windows keep checked work.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
mod regions;
pub(super) use regions::plans as regions;

#[cfg(all(test, not(hirsel_lazy_core)))]
#[test]
fn a_parent_trace_does_not_hide_a_fixed_trip_child() {
    let source = r#"core v1
sig g0 = (u64) -> (u64)
inst n0 = tests@1/nested_steps/run [] []
fn f0 = n0 g0 {
  slot s0 = u64
  slot s1 = u64
  slot s2 = u64
  addr a0 = s0 : u64
  addr a1 = s1 : u64
  addr a2 = s2 : u64
b0(v0: u64):
  live s0
  live s1
  live s2
  address a0
  address a1
  address a2
  v1: u64 = const 0
  v2: u64 = const 1
  v3: u64 = const 8
  store a0, v1
  store a2, v1
  jump b1
b1:
  v4: u64 = read a0
  v5: bool = lt v4, v0
  branch v5, b2, b6
b2:
  step
  store a1, v1
  jump b3
b3:
  v6: u64 = read a1
  v7: bool = lt v6, v3
  branch v7, b4, b5
b4:
  step
  v8: u64 = read a2
  v9: u64 = wrapping_add v8, v2
  store a2, v9
  v10: u64 = wrapping_add v6, v2
  store a1, v10
  jump b3
b5:
  step
  v11: u64 = wrapping_add v4, v2
  store a0, v11
  jump b1
b6:
  v12: u64 = read a2
  dead s2
  dead s1
  dead s0
  return v12
}
"#;
    let module = hirsel_core_ir::text::parse(source).unwrap();
    hirsel_core_ir::wf::module(&module).unwrap();
    let body = module.functions[0].body.as_ref().unwrap();
    let facts = failure::Failures::new(&module);
    let neutral = neutral_functions(&module, None);
    let selected = loops(&module, body, &facts.constants[0], &neutral);
    assert!(
        selected
            .iter()
            .any(|p| p.nodes.contains(&3) && p.nodes.contains(&4)),
        "the eight-trip child must remain selected or be absorbed in full"
    );
}

#[derive(Clone)]
pub(super) struct Loop {
    pub header: usize,
    pub reserve: usize,
    prefix: u64,
    nodes: BTreeSet<usize>,
    exits: BTreeSet<usize>,
    cuts: BTreeMap<(usize, usize), u64>,
    index: ValueId,
    limit: ValueId,
    yes: EdgeId,
    no: EdgeId,
    cost: u64,
    exact: bool,
    constant: Option<(u64, u64)>,
    // Most trips from any unsigned start. A parent may absorb this child at
    // that bound; its fast copy counts the steps taken and refunds the rest.
    bound: Option<u64>,
    stride: Stride,
    inclusive: bool,
    offset: u64,
}
fn next(body: &Body, block: usize) -> Option<Vec<usize>> {
    let edge = |e: EdgeId| body.edges[e.index()].target.index();
    Some(match &body.terminators[block] {
        Terminator::Jump(e) => vec![edge(*e)],
        Terminator::Branch { yes, no, .. } => vec![edge(*yes), edge(*no)],
        _ => return None,
    })
}
#[derive(Clone, Copy)]
enum Stride {
    Constant(u64),
    Value(ValueId),
    Slot(AddressId, Ty),
}
fn root_slot(body: &Body, mut address: AddressId) -> Option<SlotId> {
    loop {
        match body.addresses[address.index()].base {
            AddressBase::Slot(slot) => return Some(slot),
            AddressBase::Address(parent) => address = parent,
            AddressBase::Value(_) => return None,
        }
    }
}
fn writes(body: &Body, inst: &Inst, root: Option<SlotId>) -> bool {
    let address = match *inst {
        Inst::Store { address, .. } | Inst::SetCase { address, .. } => address,
        Inst::Copy { to, .. } => to,
        Inst::Grow { carrier, .. } => carrier,
        _ => return false,
    };
    root_slot(body, address) == root
}
fn escapes(body: &Body, address: AddressId) -> bool {
    let root = root_slot(body, address);
    body.insts.iter().any(|inst| match *inst {
        Inst::Pointer { address, .. } => root_slot(body, address) == root,
        Inst::Call { call } => body.calls[call.index()]
            .args
            .get(&body.pools.args)
            .unwrap()
            .iter()
            .any(|arg| matches!(arg, Arg::Address(a) if root_slot(body, *a) == root)),
        _ => false,
    })
}
fn read_index(
    body: &Body,
    value: ValueId,
    index: ValueId,
    slot: AddressId,
    nodes: &BTreeSet<usize>,
) -> bool {
    value == index
        || nodes.iter().any(|&b| {
            body.blocks[b].insts.get(&body.insts).unwrap().iter().any(|i|
        matches!(*i, Inst::Read { result, address, .. } if result == value && address == slot))
        })
}

fn stride(
    body: &Body,
    value: ValueId,
    constants: &[Option<u64>],
    nodes: &BTreeSet<usize>,
    header: usize,
) -> Option<Stride> {
    if let Some(n) = constants[value.index()] {
        return (n != 0).then_some(Stride::Constant(n));
    }
    for &b in nodes {
        for inst in body.blocks[b].insts.get(&body.insts).unwrap() {
            if let Inst::Read {
                result, address, ..
            } = *inst
                && result == value
            {
                if !local(body, address)
                    || body.addresses[address.index()].steps.len != 0
                    || escapes(body, address)
                {
                    return None;
                }
                let root = root_slot(body, address);
                if nodes.iter().any(|&b| {
                    body.blocks[b]
                        .insts
                        .get(&body.insts)
                        .unwrap()
                        .iter()
                        .any(|i| writes(body, i, root))
                }) {
                    return None;
                }
                return Some(Stride::Slot(address, body.values[value.index()]));
            }
        }
    }
    let flow = flow::Flow::new(body);
    let at = (header, 0);
    let parameter = body.blocks[0]
        .params
        .get(&body.pools.values)
        .unwrap()
        .contains(&value);
    if parameter {
        return Some(Stride::Value(value));
    }
    // For a nonconstant value, use the defining read only. Computed in-trip
    // strides need a separate invariant proof.
    for (b, row) in body.blocks.iter().enumerate() {
        for (n, inst) in row.insts.get(&body.insts).unwrap().iter().enumerate() {
            if matches!(*inst, Inst::Read { result, .. } if result == value)
                && flow.available((b, n), at)
            {
                return Some(Stride::Value(value));
            }
        }
    }
    None
}
fn local(body: &Body, mut address: AddressId) -> bool {
    loop {
        match body.addresses[address.index()].base {
            AddressBase::Slot(_) => return true,
            AddressBase::Address(parent) => address = parent,
            AddressBase::Value(_) => return false,
        }
    }
}
// A borrowed cursor can be induction too. Its pointer must be fixed before the
// trip, and the trip may write only that exact place. This needs no alias claim
// about any other pointer: all other writes and effectful calls stay checked.
fn borrowed_index(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    address: AddressId,
    header: usize,
    nodes: &BTreeSet<usize>,
) -> bool {
    let place = &body.addresses[address.index()];
    let AddressBase::Value(pointer) = place.base else {
        return false;
    };
    if place.steps.len != 0 {
        return false;
    }
    let flow = flow::Flow::new(body);
    let fixed = body.blocks[0].params.get(&body.pools.values).unwrap().contains(&pointer)
        || body.blocks.iter().enumerate().any(|(b, row)| {
            !nodes.contains(&b) && row.insts.get(&body.insts).unwrap().iter().enumerate().any(|(n, i)| {
                matches!(*i, Inst::Read { result, .. } | Inst::Pointer { result, .. } if result == pointer)
                    && flow.available((b, n), (header, 0))
            })
        });
    fixed
        && nodes.iter().all(|&b| {
            body.blocks[b]
                .insts
                .get(&body.insts)
                .unwrap()
                .iter()
                .all(|i| match *i {
                    Inst::Store {
                        address: target, ..
                    } => target == address,
                    Inst::Call { .. } => pure(module, body, constants, i, false),
                    Inst::Const { .. }
                    | Inst::ReadStatic { .. }
                    | Inst::FunctionRef { .. }
                    | Inst::DescriptorRef { .. }
                    | Inst::Unary { .. }
                    | Inst::Binary { .. }
                    | Inst::Compare { .. }
                    | Inst::Convert { .. }
                    | Inst::IntArith { .. }
                    | Inst::MulAdd { .. }
                    | Inst::ScalarUnary { .. }
                    | Inst::Address { .. }
                    | Inst::Read { .. }
                    | Inst::UpdateMask { .. }
                    | Inst::Pointer { .. }
                    | Inst::Case { .. }
                    | Inst::Step { .. } => true,
                    _ => false,
                })
        })
}
pub(super) fn pure(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    inst: &Inst,
    stores: bool,
) -> bool {
    match *inst {
        Inst::Step { .. } => true,
        Inst::Store { address, .. } => stores && local(body, address),
        Inst::Binary {
            op, left, right, ..
        } if failure::constant_safe(op, body.values[left.index()], constants[right.index()]) => {
            true
        }
        Inst::Call { call } => {
            let Callee::Direct(f) = body.calls[call.index()].callee else {
                return false;
            };
            let function = &module.functions[f.index()];
            let signature = &module.signatures[function.signature.index()];
            if signature.dest.is_some()
                || signature
                    .params
                    .iter()
                    .any(|p| !matches!(p, Param::Value(Ty::Scalar(_))))
                || signature
                    .results
                    .iter()
                    .any(|t| !matches!(t, Ty::Scalar(_)))
            {
                return false;
            }
            let Some(callee) = &function.body else {
                return false;
            };
            let values = failure::constants(callee);
            empty::loop_free(callee)
                && callee.terminators.iter().all(|t| {
                    matches!(
                        t,
                        Terminator::Jump(_) | Terminator::Branch { .. } | Terminator::Return(_)
                    )
                })
                && callee.insts.iter().all(|i| {
                    !matches!(i, Inst::Call { .. } | Inst::Step { .. })
                        && pure(module, callee, &values, i, true)
                })
        }
        Inst::Const { .. }
        | Inst::ReadStatic { .. }
        | Inst::FunctionRef { .. }
        | Inst::DescriptorRef { .. }
        | Inst::Unary { .. }
        | Inst::Binary { .. }
        | Inst::Compare { .. }
        | Inst::Convert { .. }
        | Inst::IntArith { .. }
        | Inst::ScalarUnary { .. }
        | Inst::MulAdd { .. }
        | Inst::Address { .. }
        | Inst::Read { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::Pointer { .. }
        | Inst::UpdateMask { .. } => !inst.can_fail(body),
        _ => false,
    }
}
fn inert_field(
    module: &Module,
    field: &hirsel_core_layout::FieldTy,
    seen: &mut BTreeSet<usize>,
) -> bool {
    use hirsel_core_layout::FieldTy::*;
    match field {
        Inline(t) => inert_drop(module, t.index(), seen),
        Seq(e) | Array(e, _) => inert_field(module, e, seen),
        Map(_, _) | Box(_) => false,
        // Counted payloads are shareable; their cleanup runs no user code.
        Rc(_) | Arc(_) => true,
        OwnFn | Handle(_) => false,
        _ => true,
    }
}
fn inert_drop(module: &Module, ty: usize, seen: &mut BTreeSet<usize>) -> bool {
    if !seen.insert(ty) {
        return true;
    }
    let shape = &module.types[ty];
    if shape.finalizer.is_some() {
        return false;
    }
    match &shape.body {
        hirsel_core_layout::Body::Record(fs) => fs.iter().all(|f| inert_field(module, &f.ty, seen)),
        hirsel_core_layout::Body::Variant(cs) => cs
            .iter()
            .flat_map(|c| &c.fields)
            .all(|f| inert_field(module, &f.ty, seen)),
    }
}
fn neutral_runtime(op: RtOp) -> bool {
    matches!(
        op,
        RtOp::DestId
            | RtOp::CompareBytes
            | RtOp::HashBytes
            | RtOp::CauseNext
            | RtOp::CauseValue
            | RtOp::ArgsLen
            | RtOp::ArgsGet
    )
}
pub(super) fn neutral_inst(module: &Module, body: &Body, inst: &Inst, neutral: &[bool]) -> bool {
    match *inst {
        Inst::Step { .. } => false,
        Inst::Drop { ty, .. } => inert_drop(module, ty.index(), &mut BTreeSet::new()),
        Inst::Call { call } => match body.calls[call.index()].callee {
            Callee::Direct(f) => neutral[f.index()],
            Callee::Runtime(op) => neutral_runtime(op),
            Callee::Indirect(_) => false,
        },
        _ => true,
    }
}
pub(super) fn neutral_functions(
    module: &Module,
    program: Option<&BTreeSet<hirsel_core_ids::ContentHash>>,
) -> Vec<bool> {
    let mut neutral = module
        .functions
        .iter()
        .enumerate()
        .map(|(i, _)| {
            program.is_some_and(|known| known.contains(&module.code_identity(FuncId(i as u32))))
        })
        .collect::<Vec<_>>();
    loop {
        let mut changed = false;
        for (i, f) in module.functions.iter().enumerate() {
            if !neutral[i]
                && f.body.as_ref().is_some_and(|b| {
                    b.insts
                        .iter()
                        .all(|inst| neutral_inst(module, b, inst, &neutral))
                })
            {
                neutral[i] = true;
                changed = true;
            }
        }
        if !changed {
            return neutral;
        }
    }
}
#[cfg(hirsel_lazy_core)]
pub(super) fn program_neutral(modules: &[&Module]) -> BTreeSet<hirsel_core_ids::ContentHash> {
    let mut known = BTreeSet::new();
    loop {
        let before = known.len();
        for module in modules {
            let neutral = neutral_functions(module, Some(&known));
            for (i, accepted) in neutral.iter().enumerate() {
                if *accepted {
                    known.insert(module.code_identity(FuncId(i as u32)));
                }
            }
        }
        if known.len() == before {
            return known;
        }
    }
}
fn region_inst(module: &Module, body: &Body, inst: &Inst, neutral: &[bool]) -> bool {
    matches!(inst, Inst::Step { .. }) || neutral_inst(module, body, inst, neutral)
}

#[derive(Clone, Copy)]
pub(super) struct Window {
    pub end: Local,
    start: Local,
    spent: Local,
    charged: Local,
}
#[derive(Clone)]
pub(super) enum Precharge {
    Window {
        spent: Value,
        charged: Value,
    },
    Group {
        remaining: u64,
    },
    Iteration {
        index: ValueId,
        start: Value,
        charged: Value,
        stride: u64,
        cost: u64,
        prefix: u64,
    },
}
impl FunctionEmitter<'_> {
    pub(super) fn step_window(&mut self) -> Window {
        let mut slot = || {
            let p = self.fresh();
            self.sink.alloca(p, &Type::I64, 8);
            p
        };
        Window {
            end: slot(),
            start: slot(),
            spent: slot(),
            charged: slot(),
        }
    }
    fn reserve_progress(&mut self, window: Window, charge: &Value) {
        self.store(&Type::I64, &int(0), &Value::Local(window.spent), 8);
        self.store(&Type::I64, charge, &Value::Local(window.charged), 8);
    }
    pub(super) fn step_refund(&mut self) {
        let refund = match self.precharge.clone() {
            None => return,
            Some(Precharge::Group { remaining: 0 }) => return,
            Some(Precharge::Group { remaining }) => int(remaining),
            Some(Precharge::Iteration {
                index,
                start,
                charged,
                stride,
                cost,
                prefix,
            }) => {
                let index = self.step_word(index);
                let start = self.load(&Type::I64, &start, 8);
                let distance = self.bin(Binary::Sub, &Type::I64, &index, &start);
                let trips = self.bin(Binary::UDiv, &Type::I64, &distance, &int(stride));
                let completed = self.bin(Binary::Mul, &Type::I64, &trips, &int(cost));
                let spent = self.bin(Binary::Add, &Type::I64, &completed, &int(prefix));
                let charged = self.load(&Type::I64, &charged, 8);
                self.bin(Binary::Sub, &Type::I64, &charged, &spent)
            }
            Some(Precharge::Window { spent, charged }) => {
                let spent = self.load(&Type::I64, &spent, 8);
                let charged = self.load(&Type::I64, &charged, 8);
                self.bin(Binary::Sub, &Type::I64, &charged, &spent)
            }
        };
        let counter = self.step_counter();
        let left = self.load_steps(&counter);
        let left = self.bin(Binary::Add, &Type::I64, &left, &refund);
        self.store_steps(&left, &counter);
    }
    pub(super) fn prepaid_step(&mut self) -> bool {
        match self.precharge.clone() {
            None => return false,
            Some(Precharge::Group { remaining }) => {
                self.precharge = Some(Precharge::Group {
                    remaining: remaining - 1,
                });
            }
            Some(Precharge::Iteration {
                index,
                start,
                charged,
                stride,
                cost,
                prefix,
            }) => {
                self.precharge = Some(Precharge::Iteration {
                    index,
                    start,
                    charged,
                    stride,
                    cost,
                    prefix: prefix + 1,
                });
            }
            Some(Precharge::Window { spent, .. }) => {
                let old = self.load(&Type::I64, &spent, 8);
                let next = self.bin(Binary::Add, &Type::I64, &old, &int(1));
                self.store(&Type::I64, &next, &spent, 8);
            }
        }
        true
    }
}
fn path_cost(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    header: usize,
    exit: usize,
    block: usize,
    state: &mut BTreeMap<usize, Option<u64>>,
    known: &BTreeMap<usize, Loop>,
    neutral: &[bool],
) -> Option<u64> {
    if block == header || block == exit {
        return Some(0);
    }
    if let Some(child) = known.get(&block) {
        // A trace's cuts are edge-specific. A parent cannot absorb its nodes
        // as a whole loop without also carrying those cuts and their prefixes.
        if !child.cuts.is_empty() {
            return None;
        }
        let trips = child.bound?;
        for &node in &child.nodes {
            state.insert(node, Some(0));
        }
        let after = body.edges[child.no.index()].target.index();
        let tail = path_cost(
            module, body, constants, header, exit, after, state, known, neutral,
        )?;
        return child.cost.checked_mul(trips)?.checked_add(tail);
    }
    if let Some(cost) = state.get(&block) {
        return *cost;
    }
    let insts = body.blocks[block].insts.get(&body.insts)?;
    if !insts.iter().all(|i| region_inst(module, body, i, neutral)) {
        return None;
    }
    state.insert(block, None);
    let successors = next(body, block).or_else(|| {
        matches!(
            body.terminators[block],
            Terminator::Return(_)
                | Terminator::Raise { .. }
                | Terminator::Propagate
                | Terminator::Unreachable
        )
        .then(Vec::new)
    })?;
    let mut first = 0;
    for successor in successors {
        first = first.max(path_cost(
            module, body, constants, header, exit, successor, state, known, neutral,
        )?);
    }
    let cost = first.checked_add(
        insts
            .iter()
            .filter(|i| matches!(i, Inst::Step { .. }))
            .count() as u64,
    )?;
    state.insert(block, Some(cost));
    Some(cost)
}
// A fixed-cost, non-failing trip needs no progress value or refund. Keeping
// that value out of the loop also preserves LLVM's loop idiom recognition.
fn exact_cost(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    header: usize,
    block: usize,
    state: &mut BTreeMap<usize, Option<u64>>,
    known: &BTreeMap<usize, Loop>,
) -> Option<u64> {
    if block == header {
        return Some(0);
    }
    if let Some(child) = known.get(&block) {
        if !child.exact {
            return None;
        }
        let (_, trips) = child.constant?;
        let after = body.edges[child.no.index()].target.index();
        let tail = exact_cost(module, body, constants, header, after, state, known)?;
        return child.cost.checked_mul(trips)?.checked_add(tail);
    }
    if let Some(cost) = state.get(&block) {
        return *cost;
    }
    state.insert(block, None);
    let insts = body.blocks[block].insts.get(&body.insts)?;
    if !insts.iter().all(|i| pure(module, body, constants, i, true)) {
        return None;
    }
    let mut successors = next(body, block)?.into_iter();
    let first = exact_cost(
        module,
        body,
        constants,
        header,
        successors.next()?,
        state,
        known,
    )?;
    for successor in successors {
        if exact_cost(module, body, constants, header, successor, state, known)? != first {
            return None;
        }
    }
    let cost = first.checked_add(
        insts
            .iter()
            .filter(|i| matches!(i, Inst::Step { .. }))
            .count() as u64,
    )?;
    state.insert(block, Some(cost));
    Some(cost)
}
// Keep uniform cheap trips together. Leave a more expensive edge before its
// next effect and resume the original checked body after refunding the prefix.
fn minimum_cost(
    module: &Module,
    body: &Body,
    header: usize,
    exit: usize,
    block: usize,
    state: &mut BTreeMap<usize, Option<u64>>,
    neutral: &[bool],
    headers: &BTreeSet<usize>,
) -> Option<u64> {
    if block == header {
        return Some(0);
    }
    if block == exit {
        return None;
    }
    // A nested loop's exit is not a zero-trip alternative to its body. Leave
    // its header outside the trace so its own plan remains independently usable.
    if headers.contains(&block) {
        return None;
    }
    if let Some(cost) = state.get(&block) {
        return *cost;
    }
    state.insert(block, None);
    let insts = body.blocks[block].insts.get(&body.insts)?;
    if !insts.iter().all(|i| region_inst(module, body, i, neutral)) {
        return None;
    }
    let tail = next(body, block)?
        .into_iter()
        .filter_map(|b| minimum_cost(module, body, header, exit, b, state, neutral, headers))
        .min()?;
    let cost = tail.checked_add(
        insts
            .iter()
            .filter(|i| matches!(i, Inst::Step { .. }))
            .count() as u64,
    )?;
    state.insert(block, Some(cost));
    Some(cost)
}
fn cheap_trace(
    module: &Module,
    body: &Body,
    header: usize,
    entry: usize,
    exit: usize,
    neutral: &[bool],
    headers: &BTreeSet<usize>,
) -> Option<(u64, BTreeSet<usize>, BTreeMap<(usize, usize), u64>)> {
    let mut costs = BTreeMap::new();
    let cost = minimum_cost(
        module, body, header, exit, entry, &mut costs, neutral, headers,
    )?;
    if cost == 0 {
        return None;
    }
    let mut nodes = BTreeSet::from([header]);
    let mut pending = vec![entry];
    let mut cuts = BTreeMap::new();
    while let Some(block) = pending.pop() {
        if !nodes.insert(block) {
            continue;
        }
        let own = body.blocks[block]
            .insts
            .get(&body.insts)?
            .iter()
            .filter(|i| matches!(i, Inst::Step { .. }))
            .count() as u64;
        let remaining = costs[&block]? - own;
        let prefix = cost - remaining;
        for target in next(body, block)? {
            let tail = if target == header {
                Some(0)
            } else {
                costs.get(&target).copied().flatten()
            };
            if tail == Some(remaining) {
                pending.push(target);
            } else {
                cuts.insert((block, target), prefix);
            }
        }
    }
    // Zero-charge cycles cannot use induction-derived progress. Every kept
    // cycle must be the counted loop's back edge, so topologically remove the
    // other nodes before choosing the trace.
    let mut degrees = nodes
        .iter()
        .filter(|&&b| b != header)
        .map(|&b| (b, 0usize))
        .collect::<BTreeMap<_, _>>();
    for &b in &nodes {
        if b == header {
            continue;
        }
        for target in next(body, b)? {
            if target != header && !cuts.contains_key(&(b, target)) {
                *degrees.get_mut(&target)? += 1;
            }
        }
    }
    let mut ready = degrees
        .iter()
        .filter_map(|(&b, &d)| (d == 0).then_some(b))
        .collect::<Vec<_>>();
    let mut removed = 0;
    while let Some(b) = ready.pop() {
        removed += 1;
        for target in next(body, b)? {
            if target != header && !cuts.contains_key(&(b, target)) {
                let degree = degrees.get_mut(&target)?;
                *degree -= 1;
                if *degree == 0 {
                    ready.push(target);
                }
            }
        }
    }
    (removed == nodes.len() - 1).then_some((cost, nodes, cuts))
}
pub(super) fn loops(
    module: &Module,
    body: &Body,
    constants: &[Option<u64>],
    neutral: &[bool],
) -> Vec<Loop> {
    let mut known = BTreeMap::new();
    let headers = std::cell::OnceCell::new();
    loop {
        let before = known.len();
        for (header, row) in body.blocks.iter().enumerate() {
            if known.contains_key(&header) {
                continue;
            }
            let (index, index_slot, condition, comparison, limit, offset) =
                match row.insts.get(&body.insts).unwrap() {
                    [
                        Inst::Read {
                            result: index,
                            address,
                            ..
                        },
                        Inst::Compare {
                            result,
                            op: comparison @ (CompareOp::Lt | CompareOp::Le),
                            left,
                            right,
                        },
                    ] if left == index => (index, address, result, comparison, right, 0),
                    [
                        Inst::Read {
                            result: index,
                            address,
                            ..
                        },
                        Inst::Const { result: amount, .. },
                        Inst::Binary {
                            result: sum,
                            op: BinaryOp::Add,
                            left,
                            right,
                            ..
                        },
                        Inst::Compare {
                            result,
                            op: comparison @ (CompareOp::Lt | CompareOp::Le),
                            left: tested,
                            right: limit,
                        },
                    ] if left == index && right == amount && tested == sum => {
                        let Some(offset) = constants[amount.index()] else {
                            continue;
                        };
                        (index, address, result, comparison, limit, offset)
                    }
                    [
                        Inst::Read {
                            result: index,
                            address,
                            ..
                        },
                        Inst::Binary {
                            result: gap,
                            op: BinaryOp::Sub,
                            left: limit,
                            right,
                            ..
                        },
                        Inst::Const { result: amount, .. },
                        Inst::Compare {
                            result,
                            op: comparison @ (CompareOp::Gt | CompareOp::Ge),
                            left: tested,
                            right: threshold,
                        },
                    ] if right == index && tested == gap && threshold == amount => {
                        let Some(offset) = constants[amount.index()] else {
                            continue;
                        };
                        let comparison = if *comparison == CompareOp::Ge {
                            &CompareOp::Le
                        } else {
                            &CompareOp::Lt
                        };
                        (index, address, result, comparison, limit, offset)
                    }
                    _ => continue,
                };
            if !matches!(
                body.values[index.index()],
                Ty::Scalar(Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64)
            ) {
                continue;
            }
            let Terminator::Branch {
                condition: tested,
                yes,
                no,
            } = body.terminators[header]
            else {
                continue;
            };
            if tested != *condition || body.edges[no.index()].args.len != 0 {
                continue;
            }
            let entry = body.edges[yes.index()].target.index();
            let mut state = BTreeMap::new();
            let full_cost = path_cost(
                module,
                body,
                constants,
                header,
                body.edges[no.index()].target.index(),
                entry,
                &mut state,
                &known,
                neutral,
            );
            let trace = cheap_trace(
                module,
                body,
                header,
                entry,
                body.edges[no.index()].target.index(),
                neutral,
                headers.get_or_init(|| {
                    let flow = flow::Flow::new(body);
                    let mut headers = BTreeSet::new();
                    for from in 0..body.blocks.len() {
                        for to in next(body, from).unwrap_or_default() {
                            if flow.block_dominates(to, from) {
                                headers.insert(to);
                            }
                        }
                    }
                    headers
                }),
            )
            .filter(|(cost, _, _)| full_cost.is_none_or(|full| *cost < full));
            let (cost, mut nodes, cuts) = if let Some(trace) = trace {
                trace
            } else if let Some(cost) = full_cost {
                (cost, state.keys().copied().collect(), BTreeMap::new())
            } else {
                continue;
            };
            if cost == 0 {
                continue;
            }
            nodes.insert(header);
            if nodes.contains(&body.edges[no.index()].target.index()) {
                continue;
            }
            let exits = if cuts.is_empty() {
                nodes
                    .iter()
                    .copied()
                    .filter(|&b| b != header)
                    .flat_map(|b| next(body, b).unwrap_or_default())
                    .filter(|b| !nodes.contains(b))
                    .collect::<BTreeSet<_>>()
            } else {
                BTreeSet::new()
            };
            // Every back edge advances by the same positive stride. A checked
            // overflow exits through the refund path; zero strides stay checked.
            let inclusive = *comparison == CompareOp::Le;
            if if local(body, *index_slot) {
                escapes(body, *index_slot)
            } else {
                !borrowed_index(module, body, constants, *index_slot, header, &nodes)
            } {
                continue;
            }
            let mut delta = None;
            let mut updates = Vec::new();
            let mut valid = true;
            for &block in &nodes {
                for inst in body.blocks[block].insts.get(&body.insts).unwrap() {
                    if !matches!(inst, Inst::Store { .. })
                        && writes(body, inst, root_slot(body, *index_slot))
                    {
                        valid = false;
                    }
                    if let Inst::Store { address, value } = *inst {
                        if root_slot(body, address) == root_slot(body, *index_slot) {
                            if address != *index_slot {
                                valid = false;
                                continue;
                            }
                            let increment = body.blocks[block]
                                .insts
                                .get(&body.insts)
                                .unwrap()
                                .iter()
                                .find_map(|i| match *i {
                                    Inst::IntArith {
                                        result,
                                        flag: None,
                                        op: ArithOp::Add,
                                        mode: ArithMode::Wrapping,
                                        left,
                                        right,
                                    } if result == value
                                        && read_index(body, left, *index, *index_slot, &nodes)
                                        && constants[right.index()] == Some(1)
                                        && !inclusive =>
                                    {
                                        Some(right)
                                    }
                                    Inst::Binary {
                                        result,
                                        op: BinaryOp::Add,
                                        left,
                                        right,
                                        ..
                                    } if result == value
                                        && read_index(body, left, *index, *index_slot, &nodes) =>
                                    {
                                        Some(right)
                                    }
                                    _ => None,
                                });
                            if let Some(increment) = increment {
                                delta = Some(increment);
                            } else {
                                valid = false;
                            }
                            updates.push(block);
                        }
                    }
                }
                if block != header && cuts.is_empty() {
                    for (other, _) in body.blocks.iter().enumerate() {
                        if !nodes.contains(&other)
                            && next(body, other).is_some_and(|n| n.contains(&block))
                        {
                            valid = false;
                        }
                    }
                }
            }
            if !valid || updates.len() != 1 {
                continue;
            }
            let Some(stride) = delta.and_then(|v| stride(body, v, constants, &nodes, header))
            else {
                continue;
            };
            if !cuts.is_empty() && !matches!(stride, Stride::Constant(_)) {
                continue;
            }
            let update = updates[0];
            // Every back edge passes the induction update, including a source
            // continue. Reject a path that could repeat a trip without advancing.
            if nodes.iter().any(|&b| {
                b != header
                    && next(body, b).is_some_and(|n| n.contains(&header))
                    && !cuts.contains_key(&(b, header))
                    && b != update
            }) {
                continue;
            }
            let mut initial = None;
            let mut fixed = true;
            for (b, row) in body.blocks.iter().enumerate() {
                if nodes.contains(&b) || !next(body, b).is_some_and(|n| n.contains(&header)) {
                    continue;
                }
                let stored = row
                    .insts
                    .get(&body.insts)
                    .unwrap()
                    .iter()
                    .rev()
                    .find_map(|i| {
                        if let Inst::Store { address, value } = *i {
                            if address == *index_slot {
                                return Some(constants[value.index()]);
                            }
                        }
                        None
                    })
                    .flatten();
                match (initial, stored) {
                    (_, None) => fixed = false,
                    (Some(a), Some(b)) if a != b => fixed = false,
                    (_, Some(n)) => initial = Some(n),
                }
            }
            let constant = if fixed {
                initial
                    .zip(constants[limit.index()])
                    .and_then(|(start, end)| {
                        let Stride::Constant(step) = stride else {
                            return None;
                        };
                        let gap = end.checked_sub(start)?.checked_sub(offset)?;
                        let count = if inclusive {
                            (gap / step).checked_add(1)?
                        } else {
                            gap / step + u64::from(gap % step != 0)
                        };
                        Some((start, count))
                    })
                    .filter(|(_, trips)| cost.checked_mul(*trips).is_some())
            } else {
                None
            };
            let bound = constant.map(|(_, trips)| trips).or_else(|| {
                let Stride::Constant(step) = stride else {
                    return None;
                };
                let gap = constants[limit.index()]?.checked_sub(offset)?;
                let trips = if inclusive {
                    (gap / step).checked_add(1)?
                } else {
                    gap / step + u64::from(gap % step != 0)
                };
                cost.checked_mul(trips).map(|_| trips)
            });
            let exact = exits.is_empty()
                && cuts.is_empty()
                && exact_cost(
                    module,
                    body,
                    constants,
                    header,
                    entry,
                    &mut BTreeMap::new(),
                    &known,
                ) == Some(cost);
            let mut plan = Loop {
                header,
                reserve: header,
                prefix: 0,
                nodes,
                exits,
                cuts,
                index: *index,
                limit: *limit,
                yes,
                no,
                cost,
                exact,
                constant,
                bound,
                stride,
                inclusive,
                offset,
            };
            if !plan.cuts.is_empty() && iteration_prefixes(body, &plan).is_none() {
                continue;
            }
            if !local(body, *index_slot) && update != header {
                if !flow::Flow::new(body).block_dominates(header, update) {
                    continue;
                }
                let Some(prefixes) = iteration_prefixes(body, &plan) else {
                    continue;
                };
                plan.reserve = update;
                plan.prefix = prefixes[&update];
            }
            known.insert(header, plan);
        }
        if known.len() == before {
            break;
        }
    }
    let nested = known
        .values()
        .flat_map(|p| p.nodes.iter().copied().filter(|b| *b != p.header))
        .collect::<BTreeSet<_>>();
    known
        .into_values()
        .filter(|p| !nested.contains(&p.header))
        .collect()
}
// A successful trip with one prefix at every join needs no running progress
// counter. Reconstruct its consumed prefix only when leaving the reservation.
// Cycles other than the latch, unequal joins and unequal back edges retain the
// general progress path. Failure edges leave before their following effect.
fn iteration_prefixes(body: &Body, plan: &Loop) -> Option<BTreeMap<usize, u64>> {
    let Stride::Constant(_) = plan.stride else {
        return None;
    };
    let mut prefixes = BTreeMap::from([(plan.header, 0)]);
    let mut pending = vec![(body.edges[plan.yes.index()].target.index(), 0u64)];
    while let Some((block, prefix)) = pending.pop() {
        if block == plan.header {
            if prefix != plan.cost {
                return None;
            }
            continue;
        }
        if let Some(old) = prefixes.insert(block, prefix) {
            if old != prefix {
                return None;
            }
            continue;
        }
        if !plan.nodes.contains(&block) {
            continue;
        }
        let spent = prefix.checked_add(
            body.blocks[block]
                .insts
                .get(&body.insts)?
                .iter()
                .filter(|i| matches!(i, Inst::Step { .. }))
                .count() as u64,
        )?;
        for successor in next(body, block).unwrap_or_default() {
            if !plan.cuts.contains_key(&(block, successor)) {
                pending.push((successor, spent));
            }
        }
    }
    plan.nodes
        .iter()
        .all(|b| prefixes.contains_key(b))
        .then_some(prefixes)
}

impl FunctionEmitter<'_> {
    pub(super) fn step_counter(&mut self) -> Value {
        self.step_counter
            .clone()
            .unwrap_or_else(|| self.offset(&CONTEXT, memory_layout::CONTEXT_STEPS as u64))
    }
    pub(super) fn flush_steps(&mut self) {
        if let Some(counter) = self.step_counter.clone() {
            let value = self.load_steps(&counter);
            let context = self.offset(&CONTEXT, memory_layout::CONTEXT_STEPS as u64);
            self.store_steps(&value, &context);
        }
    }
    pub(super) fn reload_steps(&mut self) {
        if let Some(counter) = self.step_counter.clone() {
            let context = self.offset(&CONTEXT, memory_layout::CONTEXT_STEPS as u64);
            let value = self.load_steps(&context);
            self.store_steps(&value, &counter);
        }
    }
}
impl FunctionEmitter<'_> {
    pub(super) fn step_label(&self, block: usize) -> Label {
        self.step_labels
            .get(&block)
            .copied()
            .unwrap_or_else(|| Label::block(block))
    }
    fn step_word(&mut self, value: ValueId) -> Value {
        let v = self.numeric(value);
        let t = ty(self.body.values[value.index()]);
        if t == Type::I64 {
            v
        } else {
            self.zext(&t, &v, &Type::I64)
        }
    }
    pub(super) fn step_dispatch(&mut self, plan: &Loop, window: Window, number: usize) {
        let n = self.counter();
        let gated = plan.reserve != plan.header;
        if !gated {
            let condition = match self.body.terminators[plan.header] {
                Terminator::Branch { condition, .. } => condition,
                _ => unreachable!(),
            };
            let value = self.numeric(condition);
            let exists = self.cmp(Predicate::Ne, &Type::I8, &value, &int(0));
            let reserve = Label::new("step_reserve", n);
            let out = self.edge_label(plan.no);
            self.cond(&exists, reserve, out);
            self.emit_edge(out, plan.no);
            self.block(reserve);
        }
        let index = self.step_word(plan.index);
        self.store(&Type::I64, &index, &Value::Local(window.start), 8);
        let limit = self.step_word(plan.limit);
        let gap = self.bin(Binary::Sub, &Type::I64, &limit, &index);
        // The original checked header has already proved index+offset meets
        // its bound. Keep that addition in every clone; only the trip formula
        // uses the equivalent nonnegative remaining distance.
        let gap = if plan.offset == 0 {
            gap
        } else {
            self.bin(Binary::Sub, &Type::I64, &gap, &int(plan.offset))
        };
        let stride = match plan.stride {
            Stride::Constant(n) => int(n),
            Stride::Value(v) => self.step_word(v),
            Stride::Slot(a, t) => {
                let pointer = self.address(a);
                let word = self.load(&ty(t), &pointer, 1);
                if ty(t) == Type::I64 {
                    word
                } else {
                    self.zext(&ty(t), &word, &Type::I64)
                }
            }
        };
        let nonzero = self.cmp(Predicate::Ne, &Type::I64, &stride, &int(0));
        let divisor = self.select(&Type::I64, &nonzero, &stride, &int(1));
        let quotient = self.bin(Binary::UDiv, &Type::I64, &gap, &divisor);
        let trips = if plan.inclusive {
            self.bin(Binary::Add, &Type::I64, &quotient, &int(1))
        } else {
            let rem = self.bin(Binary::URem, &Type::I64, &gap, &divisor);
            let extra = self.cmp(Predicate::Ne, &Type::I64, &rem, &int(0));
            let extra = self.zext(&Type::I1, &extra, &Type::I64);
            self.bin(Binary::Add, &Type::I64, &quotient, &extra)
        };
        let count_fits = self.cmp(Predicate::Ne, &Type::I64, &trips, &int(0));
        let eligible = self.bin(Binary::And, &Type::I1, &nonzero, &count_fits);
        let counter = self.step_counter();
        let before = self.load_steps(&counter);
        // Try the whole remaining loop without dividing the lease. Short
        // loops usually fit; the partial path alone needs a trip quota.
        let charge = self.bin(Binary::Mul, &Type::I64, &trips, &int(plan.cost));
        let charge_fits = self.cmp(
            Predicate::Ule,
            &Type::I64,
            &trips,
            &int(i64::MAX as u64 / plan.cost),
        );
        // Equality remains checked so the final Step yields before its effect.
        let remaining = self.bin(Binary::Sub, &Type::I64, &charge, &int(plan.prefix));
        let full = self.cmp(Predicate::Sgt, &Type::I64, &before, &remaining);
        let full = self.bin(Binary::And, &Type::I1, &full, &eligible);
        let full = self.bin(Binary::And, &Type::I1, &full, &charge_fits);
        let whole = Label::new("step_whole", n);
        let partial = Label::new("step_partial", n);
        self.likely(&full, whole, partial);
        self.block(whole);
        let after = self.bin(Binary::Sub, &Type::I64, &before, &remaining);
        self.store_steps(&after, &counter);
        if !plan.exact {
            self.reserve_progress(window, &charge);
            self.store(
                &Type::I64,
                &int(plan.prefix),
                &Value::Local(window.spent),
                8,
            );
        }
        self.br(version_label(
            number,
            self.body.blocks.len(),
            plan.reserve,
            true,
        ));
        self.block(partial);
        // Equality must take checked execution: its last Step yields before
        // the following source effect. A fast region never reaches that boundary.
        let spare = self.cmp(Predicate::Sgt, &Type::I64, &before, &int(1));
        let room = self.bin(Binary::Add, &Type::I64, &before, &int(plan.prefix));
        let room = self.bin(Binary::Sub, &Type::I64, &room, &int(1));
        let room = self.select(&Type::I64, &spare, &room, &int(0));
        let quota = self.bin(Binary::UDiv, &Type::I64, &room, &int(plan.cost));
        let quota = self.select(&Type::I64, &eligible, &quota, &int(0));
        let any = self.cmp(Predicate::Ne, &Type::I64, &quota, &int(0));
        let chunk = Label::new("step_chunk", n);
        let checked = if gated {
            Label::new("step_body", n)
        } else {
            self.edge_label(plan.yes)
        };
        self.cond(&any, chunk, checked);
        if !gated {
            self.emit_edge(checked, plan.yes);
        }
        self.block(chunk);
        // This edge has a nonzero quota below trips. Its stop index fits;
        // a whole remaining loop uses the original checked induction instead.
        let span = self.bin(Binary::Mul, &Type::I64, &quota, &stride);
        let stop = self.bin(Binary::Add, &Type::I64, &index, &span);
        self.store(&Type::I64, &stop, &Value::Local(window.end), 8);
        let charge = self.bin(Binary::Mul, &Type::I64, &quota, &int(plan.cost));
        let remaining = self.bin(Binary::Sub, &Type::I64, &charge, &int(plan.prefix));
        let after = self.bin(Binary::Sub, &Type::I64, &before, &remaining);
        self.store_steps(&after, &counter);
        if !plan.exact {
            self.reserve_progress(window, &charge);
            self.store(
                &Type::I64,
                &int(plan.prefix),
                &Value::Local(window.spent),
                8,
            );
        }
        self.br(version_label(
            number,
            self.body.blocks.len(),
            plan.reserve,
            false,
        ));
        if gated {
            self.block(checked);
        }
    }
    pub(super) fn step_versions(
        &mut self,
        plan: &Loop,
        window: Window,
        number: usize,
        location: impl Fn(Option<Anchor>) -> Option<usize>,
    ) -> Result<()> {
        let prefixes = (!plan.exact)
            .then(|| iteration_prefixes(self.body, plan))
            .flatten();
        for full in [true, false] {
            self.step_labels = plan
                .nodes
                .iter()
                .map(|&b| (b, version_label(number, self.body.blocks.len(), b, full)))
                .chain(
                    plan.exits
                        .iter()
                        .map(|&b| (b, trace_exit_label(number, self.body.blocks.len(), b, full))),
                )
                .collect();
            self.unchecked_steps = true;
            self.precharge = (!plan.exact).then_some(Precharge::Window {
                spent: Value::Local(window.spent),
                charged: Value::Local(window.charged),
            });
            let n = self.counter();
            let done = Label::new("step_chunk_done", n);
            for &block in &plan.nodes {
                self.block(self.step_label(block));
                self.enter_block(block);
                if let Some(prefixes) = &prefixes {
                    let Stride::Constant(stride) = plan.stride else {
                        unreachable!();
                    };
                    self.precharge = Some(Precharge::Iteration {
                        index: plan.index,
                        start: Value::Local(window.start),
                        charged: Value::Local(window.charged),
                        stride,
                        cost: plan.cost,
                        prefix: prefixes[&block],
                    });
                }
                let row = &self.body.blocks[block];
                for (offset, inst) in row.insts.get(&self.body.insts).unwrap().iter().enumerate() {
                    if !matches!(inst, Inst::Const { .. }) {
                        self.debug_location =
                            location(self.body.inst_anchors[row.insts.start as usize + offset]);
                        self.inst(inst, row.insts.start as usize + offset)?;
                    }
                }
                self.anchor = row.anchor;
                self.debug_location = location(row.anchor);
                if block == plan.header {
                    let Terminator::Branch { condition, .. } = self.body.terminators[block] else {
                        unreachable!()
                    };
                    let condition = self.numeric(condition);
                    let in_range = self.cmp(Predicate::Ne, &Type::I8, &condition, &int(0));
                    if full {
                        let yes = self.edge_label(plan.yes);
                        self.cond(&in_range, yes, done);
                        self.emit_edge(yes, plan.yes);
                    } else {
                        let index = self.step_word(plan.index);
                        let limit = self.load(&Type::I64, &Value::Local(window.end), 8);
                        let within_chunk = self.cmp(Predicate::Ult, &Type::I64, &index, &limit);
                        let iteration = Label::new("step_chunk_iteration", n);
                        self.cond(&within_chunk, iteration, done);
                        self.block(iteration);
                        // The partial quota ends before the original loop does.
                        // Preserve the source bound for accesses in its body.
                        self.runtime_void("llvm.assume", &[arg(Type::I1, in_range)]);
                        let to = self.edge(plan.yes);
                        self.br(to);
                    }
                } else {
                    let old = plan
                        .cuts
                        .keys()
                        .filter(|&&(from, _)| from == block)
                        .map(|&(_, to)| {
                            let label =
                                trace_cut_label(number, self.body.blocks.len(), block, to, full);
                            (to, self.step_labels.insert(to, label))
                        })
                        .collect::<Vec<_>>();
                    self.term(&self.body.terminators[block])?;
                    for (to, label) in old {
                        if let Some(label) = label {
                            self.step_labels.insert(to, label);
                        } else {
                            self.step_labels.remove(&to);
                        }
                    }
                }
            }
            self.step_labels.clear();
            self.unchecked_steps = false;
            self.live = None;
            self.block(done);
            if prefixes.is_some() {
                let Stride::Constant(stride) = plan.stride else {
                    unreachable!();
                };
                self.precharge = Some(Precharge::Iteration {
                    index: plan.index,
                    start: Value::Local(window.start),
                    charged: Value::Local(window.charged),
                    stride,
                    cost: plan.cost,
                    prefix: 0,
                });
            }
            self.step_refund();
            self.precharge = None;
            if full {
                let out = self.edge(plan.no);
                self.br(out);
            } else {
                self.br(Label::block(plan.header));
            }
            for &exit in &plan.exits {
                self.block(trace_exit_label(number, self.body.blocks.len(), exit, full));
                self.precharge = if let Some(prefixes) = &prefixes {
                    let Stride::Constant(stride) = plan.stride else {
                        unreachable!();
                    };
                    Some(Precharge::Iteration {
                        index: plan.index,
                        start: Value::Local(window.start),
                        charged: Value::Local(window.charged),
                        stride,
                        cost: plan.cost,
                        prefix: prefixes[&exit],
                    })
                } else {
                    Some(Precharge::Window {
                        spent: Value::Local(window.spent),
                        charged: Value::Local(window.charged),
                    })
                };
                self.step_refund();
                self.br(Label::block(exit));
            }
            for (&(from, to), &prefix) in &plan.cuts {
                self.block(trace_cut_label(
                    number,
                    self.body.blocks.len(),
                    from,
                    to,
                    full,
                ));
                let Stride::Constant(stride) = plan.stride else {
                    unreachable!();
                };
                self.precharge = Some(Precharge::Iteration {
                    index: plan.index,
                    start: Value::Local(window.start),
                    charged: Value::Local(window.charged),
                    stride,
                    cost: plan.cost,
                    prefix,
                });
                self.step_refund();
                self.br(Label::block(to));
            }
            self.precharge = None;
        }
        Ok(())
    }
}
fn trace_cut_label(number: usize, blocks: usize, from: usize, to: usize, full: bool) -> Label {
    Label::new(
        if full {
            "step_full_cut"
        } else {
            "step_fast_cut"
        },
        (number * blocks + from) * blocks + to,
    )
}
fn trace_exit_label(number: usize, blocks: usize, block: usize, full: bool) -> Label {
    Label::new(
        if full {
            "step_full_exit"
        } else {
            "step_fast_exit"
        },
        number * blocks + block,
    )
}

fn version_label(number: usize, blocks: usize, block: usize, full: bool) -> Label {
    Label::new(
        if full { "step_full" } else { "step_fast" },
        number * blocks + block,
    )
}

#[derive(Clone, Copy)]
pub(super) struct Group {
    pub start: usize,
    pub end: usize,
    cost: u64,
}
pub(super) fn groups(
    module: &Module,
    body: &Body,
    neutral: &[bool],
    range: PoolRange<Inst>,
) -> BTreeMap<usize, Group> {
    let insts = range.get(&body.insts).unwrap();
    let mut groups = BTreeMap::new();
    let mut at = 0;
    while at < insts.len() {
        if !matches!(insts[at], Inst::Step { .. }) {
            at += 1;
            continue;
        }
        let start = at;
        let mut end = at + 1;
        let mut cost = 1;
        at += 1;
        while at < insts.len() && region_inst(module, body, &insts[at], neutral) {
            if matches!(insts[at], Inst::Step { .. }) {
                cost += 1;
                end = at + 1;
            }
            at += 1;
        }
        if cost > 1 {
            let start = range.start as usize + start;
            let end = range.start as usize + end;
            groups.insert(start, Group { start, end, cost });
        }
    }
    groups
}
impl FunctionEmitter<'_> {
    pub(super) fn step_effect_group(
        &mut self,
        group: Group,
        location: impl Fn(Option<Anchor>) -> Option<usize>,
    ) -> Result<()> {
        let counter = self.step_counter();
        let before = self.load_steps(&counter);
        let enough = self.cmp(Predicate::Sgt, &Type::I64, &before, &int(group.cost));
        let n = self.counter();
        let fast = Label::new("step_effect_fast", n);
        let checked = Label::new("step_effect_checked", n);
        let ready = Label::new("step_effect_ready", n);
        self.likely(&enough, fast, checked);
        self.block(fast);
        let after = self.bin(Binary::Sub, &Type::I64, &before, &int(group.cost));
        self.store_steps(&after, &counter);
        // Both copies start with the same slots live.
        let live = self.live.clone();
        for prepaid in [true, false] {
            self.live = live.clone();
            self.precharge = prepaid.then_some(Precharge::Group {
                remaining: group.cost,
            });
            for index in group.start..group.end {
                let inst = &self.body.insts[index];
                self.debug_location = location(self.body.inst_anchors[index]);
                if !matches!(inst, Inst::Const { .. }) {
                    self.inst(inst, index)?;
                }
            }
            self.precharge = None;
            self.br(ready);
            if prepaid {
                self.block(checked);
            }
        }
        self.block(ready);
        Ok(())
    }
}
