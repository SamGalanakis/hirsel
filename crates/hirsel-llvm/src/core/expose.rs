//! Expose bounded helper bodies before exact Step regions are selected.
//! Clone effects and failure continuations in place; charging remains in steps.
use super::*;

fn successors(body: &Body, block: usize) -> Vec<usize> {
    let edge = |e: EdgeId| body.edges[e.index()].target.index();
    match body.terminators[block] {
        Terminator::Jump(e) => vec![edge(e)],
        Terminator::Branch { yes, no, .. } => vec![edge(yes), edge(no)],
        Terminator::Switch {
            arms, otherwise, ..
        } => arms
            .get(&body.pools.arms)
            .unwrap()
            .iter()
            .map(|a| edge(a.edge))
            .chain([edge(otherwise)])
            .collect(),
        _ => Vec::new(),
    }
}
fn supported(i: &Inst) -> bool {
    matches!(
        i,
        Inst::Const { .. }
            | Inst::ReadStatic { .. }
            | Inst::FunctionRef { .. }
            | Inst::DescriptorRef { .. }
            | Inst::Unary { .. }
            | Inst::CompactWidth { .. }
            | Inst::Binary { .. }
            | Inst::Compare { .. }
            | Inst::Convert { .. }
            | Inst::IntArith { .. }
            | Inst::MulAdd { .. }
            | Inst::ScalarUnary { .. }
            | Inst::Address { .. }
            | Inst::Read { .. }
            | Inst::Store { .. }
            | Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::UpdateMask { .. }
            | Inst::Call { .. }
            | Inst::Pointer { .. }
            | Inst::Copy { .. }
            | Inst::Move { .. }
            | Inst::Prefetch { .. }
            | Inst::Case { .. }
            | Inst::SetCase { .. }
            | Inst::Step { .. }
            | Inst::Simd { .. }
            | Inst::PutLanes { .. }
    )
}
fn edge(body: &mut Body, target: BlockId, args: PoolRange<ValueId>) -> EdgeId {
    let id = EdgeId(body.edges.len() as u32);
    body.edges.push(Edge {
        target,
        args,
        cold: false,
    });
    id
}

struct Rename {
    values: Vec<ValueId>,
    slot: u32,
    address: u32,
    block: u32,
    call: u32,
    edge: u32,
    value_pool: u32,
    fail: Option<BlockId>,
}
impl Rename {
    fn v(&self, v: ValueId) -> ValueId {
        self.values[v.index()]
    }
    fn a(&self, a: AddressId) -> AddressId {
        AddressId(a.0 + self.address)
    }
    fn b(&self, b: BlockId) -> BlockId {
        BlockId(b.0 + self.block)
    }
    fn e(&self, e: EdgeId) -> EdgeId {
        EdgeId(e.0 + self.edge)
    }
    fn f(&self, f: Option<BlockId>) -> Option<BlockId> {
        f.map(|b| self.b(b)).or(self.fail)
    }
    fn range(&self, r: PoolRange<ValueId>) -> PoolRange<ValueId> {
        PoolRange::new(r.start + self.value_pool, r.len)
    }
    fn inst(&self, inst: &mut Inst) {
        let v = |v: &mut ValueId| *v = self.v(*v);
        match inst {
            Inst::Const { result, .. }
            | Inst::ReadStatic { result, .. }
            | Inst::FunctionRef { result, .. }
            | Inst::DescriptorRef { result, .. } => v(result),
            Inst::Unary {
                result,
                operand,
                fail,
                ..
            } => {
                v(result);
                v(operand);
                *fail = self.f(*fail);
            }
            Inst::CompactWidth { result, word, .. } => {
                v(result);
                v(word);
            }
            Inst::Binary {
                result,
                left,
                right,
                fail,
                ..
            } => {
                v(result);
                v(left);
                v(right);
                *fail = self.f(*fail);
            }
            Inst::Compare {
                result,
                left,
                right,
                ..
            } => {
                v(result);
                v(left);
                v(right);
            }
            Inst::Convert {
                result,
                operand,
                flag,
                ..
            } => {
                v(result);
                v(operand);
                if let Some(flag) = flag {
                    v(flag);
                }
            }
            Inst::IntArith {
                result,
                left,
                right,
                flag,
                ..
            } => {
                v(result);
                v(left);
                v(right);
                if let Some(flag) = flag {
                    v(flag);
                }
            }
            Inst::MulAdd {
                result,
                left,
                right,
                addend,
            } => {
                v(result);
                v(left);
                v(right);
                v(addend);
            }
            Inst::ScalarUnary {
                result, operand, ..
            } => {
                v(result);
                v(operand);
            }
            Inst::Address { address, fail } => {
                *address = self.a(*address);
                *fail = self.f(*fail);
            }
            Inst::Read {
                result, address, ..
            }
            | Inst::Pointer { result, address }
            | Inst::Case { result, address } => {
                v(result);
                *address = self.a(*address);
            }
            Inst::Store { address, value } => {
                v(value);
                *address = self.a(*address);
            }
            Inst::StorageLive { slot } | Inst::StorageDead { slot } => slot.0 += self.slot,
            Inst::UpdateMask { result, mask, .. } => {
                v(result);
                v(mask);
            }
            Inst::Call { call } => call.0 += self.call,
            Inst::Copy { to, from, bytes } | Inst::Move { to, from, bytes } => {
                *to = self.a(*to);
                *from = self.a(*from);
                v(bytes);
            }
            Inst::SetCase { address, .. } => *address = self.a(*address),
            Inst::Prefetch { data, offset } => {
                v(data);
                v(offset);
            }
            Inst::Simd {
                result,
                op,
                first,
                second,
                fail,
            } => {
                v(result);
                v(first);
                if let Some(second) = second {
                    v(second);
                }
                if matches!(op, SimdOp::Shl | SimdOp::Shr) {
                    *fail = self.f(*fail);
                }
            }
            Inst::PutLanes { to, vector, count } => {
                *to = self.a(*to);
                v(vector);
                v(count);
            }
            Inst::Step { fail } => *fail = self.f(*fail),
            _ => unreachable!("selected helper instruction"),
        }
    }
}

fn expand(
    body: &mut Body,
    block: usize,
    offset: usize,
    callee: &Body,
    module: &Module,
    inherits: bool,
) -> Vec<usize> {
    let range = body.blocks[block].insts;
    let at = range.start as usize + offset;
    let Inst::Call { call } = body.insts[at] else {
        unreachable!()
    };
    let call = body.calls[call.index()].clone();
    let caller_anchor = body.inst_anchors[at];
    let anchor = |a: Option<Anchor>| {
        if inherits && a.is_none_or(|a| module.items[a.item.index()].package_is("core", "1")) {
            caller_anchor
        } else {
            a
        }
    };
    let args = call.args.get(&body.pools.args).unwrap().to_vec();
    let mut values = callee
        .values
        .iter()
        .map(|&ty| {
            let v = ValueId(body.values.len() as u32);
            body.values.push(ty);
            v
        })
        .collect::<Vec<_>>();
    let mut pointers = Vec::new();
    for (&param, arg) in callee.blocks[0]
        .params
        .get(&callee.pools.values)
        .unwrap()
        .iter()
        .zip(args)
    {
        match arg {
            Arg::Value(v) => values[param.index()] = v,
            Arg::Address(address) => pointers.push(Inst::Pointer {
                result: values[param.index()],
                address,
            }),
        }
    }
    let continuation = BlockId(body.blocks.len() as u32);
    body.blocks.push(Block {
        params: call.results,
        insts: PoolRange::new(at as u32 + 1, range.len - offset as u32 - 1),
        anchor: body.blocks[block].anchor,
        decision: body.blocks[block].decision,
    });
    body.terminators.push(body.terminators[block].clone());
    if body.parked_returns.remove(&BlockId(block as u32)) {
        body.parked_returns.insert(continuation);
    }
    let rename = Rename {
        values,
        slot: body.slots.len() as u32,
        address: body.addresses.len() as u32,
        block: body.blocks.len() as u32,
        call: body.calls.len() as u32,
        edge: body.edges.len() as u32,
        value_pool: body.pools.values.len() as u32,
        fail: call.fail,
    };
    body.slots.extend_from_slice(&callee.slots);
    body.pools
        .values
        .extend(callee.pools.values.iter().map(|&v| rename.v(v)));
    for a in &callee.addresses {
        let mut a = a.clone();
        a.base = match a.base {
            AddressBase::Slot(s) => AddressBase::Slot(SlotId(s.0 + rename.slot)),
            AddressBase::Value(v) => AddressBase::Value(rename.v(v)),
            AddressBase::Address(a) => AddressBase::Address(rename.a(a)),
        };
        a.steps = PoolRange::push(
            &mut body.pools.steps,
            a.steps
                .get(&callee.pools.steps)
                .unwrap()
                .iter()
                .map(|&s| match s {
                    Step::Index(v) => Step::Index(rename.v(v)),
                    Step::Element(v) => Step::Element(rename.v(v)),
                    Step::Byte(v) => Step::Byte(rename.v(v)),
                    other => other,
                }),
        );
        body.addresses.push(a);
    }
    for c in &callee.calls {
        let mut c = c.clone();
        if let Callee::Indirect(v) = c.callee {
            c.callee = Callee::Indirect(rename.v(v));
        }
        c.args = PoolRange::push(
            &mut body.pools.args,
            c.args
                .get(&callee.pools.args)
                .unwrap()
                .iter()
                .map(|&a| match a {
                    Arg::Value(v) => Arg::Value(rename.v(v)),
                    Arg::Address(a) => Arg::Address(rename.a(a)),
                }),
        );
        c.results = rename.range(c.results);
        c.fail = rename.f(c.fail);
        body.calls.push(c);
    }
    for e in &callee.edges {
        body.edges.push(Edge {
            target: rename.b(e.target),
            args: rename.range(e.args),
            cold: e.cold,
        });
    }
    for (b, row) in callee.blocks.iter().enumerate() {
        let start = body.insts.len() as u32;
        for i in row.insts.start as usize..(row.insts.start + row.insts.len) as usize {
            let mut inst = callee.insts[i].clone();
            rename.inst(&mut inst);
            body.insts.push(inst);
            body.inst_anchors.push(anchor(callee.inst_anchors[i]));
        }
        body.blocks.push(Block {
            params: if b == 0 {
                PoolRange::EMPTY
            } else {
                rename.range(row.params)
            },
            insts: PoolRange::new(start, body.insts.len() as u32 - start),
            anchor: anchor(row.anchor),
            decision: row.decision,
        });
        let term = match callee.terminators[b] {
            Terminator::Jump(e) => Terminator::Jump(rename.e(e)),
            Terminator::Branch { condition, yes, no } => Terminator::Branch {
                condition: rename.v(condition),
                yes: rename.e(yes),
                no: rename.e(no),
            },
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => Terminator::Switch {
                value: rename.v(value),
                otherwise: rename.e(otherwise),
                arms: PoolRange::push(
                    &mut body.pools.arms,
                    arms.get(&callee.pools.arms)
                        .unwrap()
                        .iter()
                        .map(|a| SwitchArm {
                            value: a.value,
                            edge: rename.e(a.edge),
                        }),
                ),
            },
            Terminator::Return(args) => {
                Terminator::Jump(edge(body, continuation, rename.range(args)))
            }
            Terminator::Propagate => match call.fail {
                Some(fail) => Terminator::Jump(edge(body, fail, PoolRange::EMPTY)),
                None => Terminator::Propagate,
            },
            Terminator::Raise { cause, cleanup } => Terminator::Raise {
                cause: match cause {
                    RaiseCause::Fail(a) => RaiseCause::Fail(rename.a(a)),
                    RaiseCause::Capacity {
                        requested,
                        available,
                    } => RaiseCause::Capacity {
                        requested: rename.v(requested),
                        available: rename.v(available),
                    },
                    plain @ (RaiseCause::Unwrap | RaiseCause::Bounds) => plain,
                },
                cleanup: rename.f(cleanup),
            },
            Terminator::Unreachable => Terminator::Unreachable,
        };
        body.terminators.push(term);
    }
    let start = body.insts.len() as u32;
    body.insts.extend_from_within(range.start as usize..at);
    body.inst_anchors
        .extend_from_within(range.start as usize..at);
    for pointer in pointers {
        body.insts.push(pointer);
        body.inst_anchors.push(None);
    }
    body.blocks[block].insts = PoolRange::new(start, body.insts.len() as u32 - start);
    body.terminators[block] = Terminator::Jump(edge(body, BlockId(rename.block), PoolRange::EMPTY));
    body.blocks[block].decision = false;
    (continuation.index()..body.blocks.len()).collect()
}

// Inline entry/return jumps are not source charge boundaries. Collapse their
// single-predecessor, argument-free edges before selecting straight-line groups.
// Named failure entries remain distinct whenever another edge can reach them.
fn merge_blocks(body: &mut Body) {
    let mut incoming = vec![0usize; body.blocks.len()];
    for (b, row) in body.blocks.iter().enumerate() {
        for successor in successors(body, b) {
            incoming[successor] += 1;
        }
        for inst in row.insts.get(&body.insts).unwrap() {
            if let Some(fail) = inst.fail(body) {
                incoming[fail.index()] += 1;
            }
        }
        if let Terminator::Raise {
            cleanup: Some(fail),
            ..
        } = body.terminators[b]
        {
            incoming[fail.index()] += 1;
        }
    }
    incoming[0] += 1;
    for block in 0..body.blocks.len() {
        while let Terminator::Jump(edge) = body.terminators[block] {
            let target = body.edges[edge.index()].target.index();
            if target == block || incoming[target] != 1 || body.blocks[target].params.len != 0 {
                break;
            }
            let start = body.insts.len() as u32;
            for b in [block, target] {
                let range = body.blocks[b].insts;
                body.insts
                    .extend_from_within(range.start as usize..(range.start + range.len) as usize);
                body.inst_anchors
                    .extend_from_within(range.start as usize..(range.start + range.len) as usize);
            }
            body.blocks[block].insts = PoolRange::new(start, body.insts.len() as u32 - start);
            body.blocks[block].anchor = body.blocks[target].anchor;
            body.blocks[block].decision = body.blocks[target].decision;
            body.terminators[block] = body.terminators[target].clone();
            if body.parked_returns.remove(&BlockId(target as u32)) {
                body.parked_returns.insert(BlockId(block as u32));
            }
            body.blocks[target].insts = PoolRange::EMPTY;
            body.blocks[target].decision = false;
            body.terminators[target] = Terminator::Unreachable;
            incoming[target] = 0;
        }
    }
}

fn isolated_step_bodies(module: &Module) -> Vec<bool> {
    let mut isolated = vec![false; module.functions.len()];
    let mut callers = vec![Vec::new(); module.functions.len()];
    let mut excluded = Vec::new();
    for (i, f) in module.functions.iter().enumerate() {
        if let Some(body) = &f.body {
            isolated[i] = body.insts.iter().all(supported)
                && body
                    .pools
                    .steps
                    .iter()
                    .all(|step| !matches!(step, Step::Deref | Step::Seq(_)));
            for call in &body.calls {
                match call.callee {
                    Callee::Direct(callee) => callers[callee.index()].push(i),
                    Callee::Indirect(_) | Callee::Runtime(_) => isolated[i] = false,
                }
            }
        }
        if !isolated[i] {
            excluded.push(i);
        }
    }
    while let Some(callee) = excluded.pop() {
        for &caller in &callers[callee] {
            if isolated[caller] {
                isolated[caller] = false;
                excluded.push(caller);
            }
        }
    }
    isolated
}

fn literal_recursive_entries(module: &Module) -> Vec<bool> {
    let isolated = isolated_step_bodies(module);
    let recursive = module
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            isolated[i]
                && f.body.as_ref().is_some_and(|body| {
                    body.calls
                        .iter()
                        .any(|c| c.callee == Callee::Direct(FuncId(i as u32)))
                })
        })
        .collect::<Vec<_>>();
    module
        .functions
        .iter()
        .map(|f| {
            f.body.as_ref().is_some_and(|body| {
                let literals = body
                    .insts
                    .iter()
                    .filter_map(|inst| match *inst {
                        Inst::Const { result, bits } if bits != 0 => Some(result),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>();
                body.calls.iter().any(|call| {
                    matches!(call.callee, Callee::Direct(f) if recursive[f.index()])
                        && call
                            .args
                            .get(&body.pools.args)
                            .unwrap()
                            .iter()
                            .any(|arg| matches!(arg, Arg::Value(v) if literals.contains(v)))
                })
            })
        })
        .collect()
}

/// A body's size for exposure: its instructions, less its `StorageDead`s. A
/// slot's storage is priced once, at its `StorageLive`; ending it emits no code.
fn price(b: &Body) -> usize {
    b.insts
        .iter()
        .filter(|i| !matches!(i, Inst::StorageDead { .. }))
        .count()
}

/// The callee price one caller may expose in total. LangArena's cold release
/// compile spent 1.1G instructions more at 2048 with every Guard B row within
/// +3% cycles at 128; Richards and Permute run 1-3% fewer instructions at 128.
const EXPOSE_BUDGET: usize = 128;

/// Spend exposure first where a literal can specialize a short helper.
fn literal_calls(body: &Body) -> Vec<bool> {
    let mut literal = vec![false; body.values.len()];
    for inst in &body.insts {
        if let Inst::Const { result, .. } = *inst {
            literal[result.index()] = true;
        }
    }
    body.calls
        .iter()
        .map(|call| {
            call.args
                .get(&body.pools.args)
                .unwrap()
                .iter()
                .any(|arg| matches!(arg, Arg::Value(value) if literal[value.index()]))
        })
        .collect()
}

/// Expose bounded helper bodies into their callers. An owned module is
/// changed in place; a borrowed one is copied at its first change. Every
/// exposure reads its callers' and callees' bodies as they were before any
/// exposure, as each caller is exposed from the module it was given.
pub(super) fn module(module: Cow<'_, Module>, preserve_step_trees: bool) -> Cow<'_, Module> {
    // Marks have no runtime meaning. Remove them before eligibility, budgets
    // or block merging can let one change the exposed program.
    let mut module = hirsel_core_ir::without_marks(module);
    // Keep a literal entry to isolated recursion visible to the LLVM Step
    // proof, including the entry's private storage initialization. LLVM can
    // still inline the call after the proof has run.
    let step_entries = preserve_step_trees.then(|| literal_recursive_entries(&module));
    let step_entry = |i: usize| step_entries.as_ref().is_some_and(|entries| entries[i]);
    // A coverage body keeps its own blocks, so its flags are the sites
    // `coverage::sites` reads from it: none is copied into a caller, and
    // none takes a callee's.
    let covered = (0..module.functions.len())
        .map(|i| hirsel_core_ir::coverage::in_scope(&module, FuncId(i as u32)))
        .collect::<Vec<_>>();
    let eligible = module
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            f.body.as_ref().is_some_and(|b| {
                !step_entry(i)
                    && !covered[i]
                    && !(module.law_observation
                        && hirsel_core_ir::law::observes(
                            &module.instances[f.instance.index()].definition,
                        ))
                    && price(b) <= 192
                    && b.blocks.len() <= 24
                    && b.insts.iter().all(supported)
            })
        })
        .collect::<Vec<_>>();
    // A caller that keeps an unmetered terminal call's forwarding ABI reads
    // the failure facts of the unexposed module.
    let forwarding_facts = module
        .functions
        .iter()
        .enumerate()
        .any(|(i, f)| {
            f.body.as_ref().is_some_and(|b| {
                !b.calls.is_empty()
                    && !step_entry(i)
                    && !covered[i]
                    && !b.insts.iter().any(|i| matches!(i, Inst::Step { .. }))
            })
        })
        .then(|| {
            (
                hirsel_core_layout::compute(&module.types).expect("producer layouts"),
                failure::Failures::new(&module),
            )
        });
    // The unexposed body of each eligible callee whose own body has been
    // exposed already.
    let mut unexposed: Vec<Option<Body>> = vec![None; module.functions.len()];
    for i in 0..module.functions.len() {
        let Some(body) = expose_caller(
            &module,
            i,
            &eligible,
            &unexposed,
            forwarding_facts.as_ref(),
            step_entry(i) || covered[i],
        ) else {
            continue;
        };
        let exposed = module.to_mut().functions[i].body.replace(body);
        if eligible[i] {
            unexposed[i] = exposed;
        }
    }
    module
}

/// Function `i`'s body with its bounded helpers exposed, or `None` when it
/// exposes none. `unexposed` holds the original of each eligible callee
/// already exposed in `module`.
fn expose_caller(
    module: &Module,
    i: usize,
    eligible: &[bool],
    unexposed: &[Option<Body>],
    forwarding_facts: Option<&(Vec<hirsel_core_layout::RecipeRow>, failure::Failures)>,
    kept: bool,
) -> Option<Body> {
    let f = &module.functions[i];
    let original = f.body.as_ref()?;
    if original.calls.is_empty() || kept {
        return None;
    }
    let callee_body = |f: usize| {
        eligible[f]
            .then(|| unexposed[f].as_ref().or(module.functions[f].body.as_ref()))
            .flatten()
    };
    // An unmetered terminal call already has FORM's forwarding ABI. Keep
    // that call boundary; exposing it cannot join any Step reservations.
    let mut forwarded = BTreeSet::new();
    if !original
        .insts
        .iter()
        .any(|i| matches!(i, Inst::Step { .. }))
    {
        let (recipes, failures) =
            forwarding_facts.expect("forwarding facts for an unmetered caller");
        let mut plan = forward::Forwarding::new(original);
        for block in 0..original.blocks.len() {
            if let Some(ending) = plan.ending(
                module,
                recipes,
                &module.signatures[f.signature.index()],
                &failures.constants[i],
                block,
            ) {
                forwarded.insert(ending.call);
            }
        }
    }
    // A small reader can be hot inside a recursive parser without living
    // in a local loop. Use the same bounded exposure at those direct calls.
    let caller_versioned = f.versioned;
    let mut hot = (0..original.blocks.len()).collect::<BTreeSet<_>>();
    let mut body = original.clone();
    let mut budget = EXPOSE_BUDGET;
    let literal_calls = literal_calls(original);
    // A recursive callee unrolls at most 8 copies deep along one chain of
    // expansions (GCC's max-inline-recursive-depth-auto), as LLVM's loop
    // helpers do after O3: unbounded, Havlak's loop nesting pass spent the
    // budget on 15 nested copies, and SLP searched each loop's 15-wide
    // PHI lists. Each expanded callee with the copy it was expanded into
    // (None for the caller's own body), and each block's copy.
    let mut copies: Vec<(usize, Option<usize>)> = Vec::new();
    let mut chain: Vec<Option<usize>> = vec![None; body.blocks.len()];
    // Copies of f on the chain that ends in this copy.
    let depth = |copies: &[(usize, Option<usize>)], mut copy: Option<usize>, f: usize| {
        let mut n = 0;
        while let Some(c) = copy {
            n += usize::from(copies[c].0 == f);
            copy = copies[c].1;
        }
        n
    };
    // Depth alone does not bound a callee that recurses more than once:
    // its copies grow with the fan-out to the depth, until the budget
    // runs out. The copies a callee makes inside its own copies also
    // share one size limit per caller (GCC's
    // max-inline-insns-recursive-auto). Unbounded, the median-of-nine
    // pivot sample, with three recursive calls, spent the budget on 55
    // copies of itself, each with its comparator calls.
    let mut recursive_growth = std::collections::BTreeMap::<usize, usize>::new();
    loop {
        let mut fallback = None;
        let selected = hot.iter().find_map(|&b| {
            body.blocks[b]
                .insts
                .get(&body.insts)
                .unwrap()
                .iter()
                .enumerate()
                .find_map(|(offset, inst)| {
                    let Inst::Call { call } = *inst else {
                        return None;
                    };
                    if forwarded.contains(&call) {
                        return None;
                    }
                    let literal = literal_calls.get(call.index()).copied().unwrap_or(false);
                    let call = &body.calls[call.index()];
                    let Callee::Direct(f) = call.callee else {
                        return None;
                    };
                    let callee = callee_body(f.index())?;
                    if module.functions[f.index()].versioned && !caller_versioned {
                        return None;
                    }
                    let size = price(callee).max(1);
                    let within = depth(&copies, chain[b], f.index());
                    let site = (!call.cold
                        && f.index() != i
                        && size <= budget
                        && within < 8
                        && (within == 0
                            || recursive_growth.get(&f.index()).copied().unwrap_or(0) + size
                                <= 450))
                        .then_some((b, offset, callee))?;
                    if literal && size <= 64 {
                        Some(site)
                    } else {
                        fallback.get_or_insert(site);
                        None
                    }
                })
        });
        let Some((b, offset, callee)) = selected.or(fallback) else {
            break;
        };
        budget -= price(callee).max(1);
        let Inst::Call { call } = body.insts[body.blocks[b].insts.start as usize + offset] else {
            unreachable!()
        };
        let Callee::Direct(f) = body.calls[call.index()].callee else {
            unreachable!()
        };
        if depth(&copies, chain[b], f.index()) != 0 {
            *recursive_growth.entry(f.index()).or_default() += price(callee).max(1);
        }
        let inherits = hirsel_core_ir::source::inherits_site(
            &module.instances[module.functions[f.index()].instance.index()],
        );
        let added = expand(&mut body, b, offset, callee, module, inherits);
        // The continuation stays in the caller's copy; the callee's blocks
        // form the new one.
        copies.push((f.index(), chain[b]));
        chain.push(chain[b]);
        chain.resize(body.blocks.len(), Some(copies.len() - 1));
        hot.extend(added);
    }
    if budget != EXPOSE_BUDGET {
        merge_blocks(&mut body);
        let len = body
            .blocks
            .iter()
            .map(|block| block.insts.len as usize)
            .sum();
        let old = std::mem::replace(&mut body.insts, Vec::with_capacity(len));
        let anchors = std::mem::replace(&mut body.inst_anchors, Vec::with_capacity(len));
        for block in &mut body.blocks {
            let range = block.insts;
            let start = body.insts.len() as u32;
            body.insts.extend_from_slice(range.get(&old).unwrap());
            body.inst_anchors.extend_from_slice(
                &anchors[range.start as usize..(range.start + range.len) as usize],
            );
            block.insts = PoolRange::new(start, range.len);
        }
        Some(body)
    } else {
        None
    }
}
