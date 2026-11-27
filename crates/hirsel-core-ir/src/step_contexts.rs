//! MEM-30 step contexts. A step taken while no step bound is current is never
//! observed: no bound is charged and no slice ends. Each function body runs in
//! one of two contexts, checked (a bound may be current) or free (none is), and
//! a free context keeps only the steps taken inside the body's own `bound steps`
//! scopes. Direct calls carry their context to the callee; a body that runs in
//! both gets a private free code variant, and the entries that code pointers,
//! finalizers and the launcher reach select their body by the step window.
//!
//! In an executable, a function entered by pointer runs checked only when
//! something can enter it while a bound is current: an indirect call made in a
//! checked context whose calling convention it has, a task spawned in a checked
//! context or a flow stage (whose code runs under the stage's bound), or a
//! separately lowered body that a checked context calls, or that opens a bound
//! itself, and that may call a function value of its convention. Finalizers run
//! from drop glue in any context, so they run checked whenever the image opens
//! a bound.
use crate::*;
use hirsel_core_ids::ContentHash;
use std::collections::{BTreeMap, BTreeSet};

const CHECKED: usize = 0;
const FREE: usize = 1;

/// What a separately lowered body set (the release std, `std_release` in the
/// driver) publishes about each entry another image calls, computed from that
/// set's bodies alone.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub struct Summary {
    /// The entry has a step-free body under [`free_identity`], beside its
    /// checked one; its own symbol selects either by the step window.
    pub free: bool,
    /// Running it can make a step bound current (a `bound steps` scope or a
    /// flow stage), through its direct calls and the functions it names.
    pub opens_bound: bool,
    /// Running it can issue `LaunchWord`, by the same edges.
    pub launches: bool,
    /// The calling conventions, as [`convention_bit`]s, of the function values
    /// running it can call, by the same edges: every bit when it reaches a
    /// function whose body is not in its set.
    pub calls_values: u64,
}

/// A function value's calling convention: its signature, with each declared
/// type named by its closure so that the key does not depend on row numbering.
/// The front lowers a function value's signature and the signature of every
/// indirect call of it from the same source callable type, so an indirect call
/// reaches only functions of its own convention.
fn convention(
    module: &Module,
    signature: SigId,
    closures: &mut BTreeMap<TypeId, ContentHash>,
) -> ContentHash {
    let s = &module.signatures[signature.index()];
    let mut name = |t: TypeId| *closures.entry(t).or_insert_with(|| module.closure(t).hash);
    let mut text = String::new();
    if let Some(d) = s.dest {
        text += &format!("d{:?};", name(d));
    }
    for p in &s.params {
        match *p {
            Param::Value(t) => text += &format!("v{t:?};"),
            Param::Address { place, loan } => {
                let place = match place {
                    PlaceTy::Value(t) => format!("{t:?}"),
                    PlaceTy::Declared(t) => format!("{:?}", name(t)),
                };
                text += &format!("a{loan:?}{place};");
            }
        }
    }
    text += &format!("r{:?}", s.results);
    ContentHash::of_parts("hirsel.step-convention.v1", [text.as_bytes()])
}

/// The bit a [`Summary::calls_values`] sets for a convention.
fn convention_bit(convention: ContentHash) -> u64 {
    1 << (convention.0[0] & 63)
}

/// The code identity of the step-free body of the function whose code identity
/// is `identity`.
pub fn free_identity(identity: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.step-free.v1", [identity.0.as_slice()])
}

/// Every function a call, function value or finalizer names has a body here, so
/// no code outside the module enters it or is entered by it except by pointer.
pub fn self_contained(module: &Module) -> bool {
    self_contained_over(module, &BTreeMap::new())
}

/// [`self_contained`], where a function without a body here is also known when
/// `external` summarizes its code identity: a separately lowered body that
/// enters this module only by pointer.
pub fn self_contained_over(module: &Module, external: &BTreeMap<ContentHash, Summary>) -> bool {
    let defined = |f: FuncId| {
        module.functions[f.index()].body.is_some()
            || external.contains_key(&module.code_identity(f))
    };
    module
        .types
        .iter()
        .filter_map(|shape| shape.finalizer)
        .all(|f| defined(FuncId(f.0)))
        && module
            .functions
            .iter()
            .filter_map(|f| f.body.as_ref())
            .all(|body| {
                body.insts.iter().all(|inst| match *inst {
                    Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } => {
                        defined(function)
                    }
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Direct(function) => defined(function),
                        Callee::Indirect(_) | Callee::PackExport(_) | Callee::Runtime(_) => true,
                    },
                    _ => true,
                })
            })
}

/// Specialize a closed, [`self_contained`] executable whose launcher enters
/// `entry` with no step bound current. Fills [`Module::step_free`].
pub fn specialize(module: &mut Module, entry: FuncId) {
    specialize_over(module, entry, &BTreeMap::new());
}

/// [`specialize`] for a module [`self_contained_over`] `external`: a free call
/// of a summarized function whose [`Summary::free`] holds reaches its step-free
/// symbol, and a summarized function that opens a bound makes the image bounded.
pub fn specialize_over(
    module: &mut Module,
    entry: FuncId,
    external: &BTreeMap<ContentHash, Summary>,
) {
    let summaries: Vec<Option<Summary>> = (0..module.functions.len())
        .map(|f| {
            let function = &module.functions[f];
            function
                .body
                .is_none()
                .then(|| {
                    external
                        .get(&module.code_identity(FuncId(f as u32)))
                        .copied()
                })
                .flatten()
        })
        .collect();
    let bounded_image = opens_bound(module) || summaries.iter().flatten().any(|s| s.opens_bound);
    let entered = entered(module, &[entry]);
    let pending = if bounded_image {
        contexts(module, &entered, &summaries)
    } else {
        entered
            .iter()
            .enumerate()
            .filter(|(_, e)| **e)
            .map(|(f, _)| (f, FREE))
            .collect()
    };
    split(module, &entered, pending, &summaries);
}

/// The contexts each body of a bounded image may run in, from the launcher's
/// free entry and every entry by pointer (see the module docs).
fn contexts(
    module: &Module,
    entered: &[bool],
    summaries: &[Option<Summary>],
) -> Vec<(usize, usize)> {
    let n = module.functions.len();
    let scopes: Vec<Option<Vec<bool>>> = module
        .functions
        .iter()
        .map(|f| f.body.as_ref().and_then(bounded_points))
        .collect();
    let mut conventions: BTreeMap<SigId, ContentHash> = BTreeMap::new();
    let mut closures = BTreeMap::new();
    let mut convention_of = |s: SigId| {
        *conventions
            .entry(s)
            .or_insert_with(|| convention(module, s, &mut closures))
    };
    // A task entry adapter named only by spawns calls just the functions
    // those spawns store for it (`returnable::spawns`).
    let mut named = vec![0usize; n];
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            if let Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } = *inst
            {
                named[function.index()] += 1;
            }
        }
    }
    for f in module.types.iter().filter_map(|shape| shape.finalizer) {
        named[f.0 as usize] += 1;
    }
    for (i, func) in module.functions.iter().enumerate() {
        if func.pack_export {
            named[i] += 1;
        }
    }
    let spawns = crate::returnable::spawns(module);
    let mut spawned: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for spawn in &spawns {
        spawned
            .entry(spawn.entry.index())
            .or_default()
            .push(spawn.callee.index());
    }
    spawned.retain(|&adapter, callees| named[adapter] == callees.len());
    // A function whose every naming is a spawn's sole store of it into the
    // input of an adapter above is entered only through that adapter.
    let mut stored = vec![0usize; n];
    for spawn in &spawns {
        if spawn.sole && spawned.contains_key(&spawn.entry.index()) {
            stored[spawn.callee.index()] += 1;
        }
    }
    // The functions entered by pointer, by convention.
    let mut by_convention: BTreeMap<ContentHash, Vec<usize>> = BTreeMap::new();
    for f in (0..n).filter(|&f| entered[f] && stored[f] != named[f] && !spawned.contains_key(&f)) {
        by_convention
            .entry(convention_of(module.functions[f].signature))
            .or_default()
            .push(f);
    }
    let mut pending: Vec<(usize, usize)> = (0..n)
        .filter(|&f| entered[f])
        .map(|f| (f, FREE))
        .chain(
            module
                .types
                .iter()
                .filter_map(|shape| shape.finalizer)
                .map(|f| (f.0 as usize, CHECKED)),
        )
        .collect();
    let mut modes = vec![[false; 2]; n];
    let mut hot: BTreeSet<ContentHash> = BTreeSet::new();
    let mut bits = 0u64;
    let mut every = false;
    while let Some((f, mode)) = pending.pop() {
        if std::mem::replace(&mut modes[f][mode], true) {
            continue;
        }
        let Some(body) = &module.functions[f].body else {
            continue;
        };
        let refs: BTreeMap<ValueId, FuncId> = body
            .insts
            .iter()
            .filter_map(|inst| match *inst {
                Inst::FunctionRef { result, function }
                | Inst::DescriptorRef {
                    result, function, ..
                } => Some((result, function)),
                _ => None,
            })
            .collect();
        let mut new_keys = vec![];
        let mut new_bits = 0u64;
        let mut stage = false;
        for (k, inst) in body.insts.iter().enumerate() {
            let Inst::Call { call } = *inst else {
                continue;
            };
            let site = &body.calls[call.index()];
            let context = if mode == FREE && scopes[f].as_ref().is_some_and(|b| !b[k]) {
                FREE
            } else {
                CHECKED
            };
            match site.callee {
                Callee::Direct(g) => {
                    pending.push((g.index(), context));
                    if let Some(s) = summaries[g.index()]
                        && (context == CHECKED || s.opens_bound)
                    {
                        new_bits |= s.calls_values;
                    }
                }
                Callee::Indirect(_) | Callee::PackExport(_) => match spawned.get(&f) {
                    Some(callees) => pending.extend(callees.iter().map(|&g| (g, context))),
                    None if context == CHECKED => {
                        let key = convention_of(site.signature);
                        if hot.insert(key) {
                            new_keys.push(key);
                        }
                    }
                    None => {}
                },
                // The task inherits the current bound.
                Callee::Runtime(RtOp::Spawn { .. } | RtOp::SpawnUnit { .. })
                    if context == CHECKED =>
                {
                    let args = site.args.get(&body.pools.args).unwrap_or_default();
                    let entries: Vec<FuncId> = args
                        .iter()
                        .filter_map(|a| match a {
                            Arg::Value(v) => refs.get(v).copied(),
                            Arg::Address(_) => None,
                        })
                        .collect();
                    if entries.is_empty() {
                        every = true;
                    }
                    pending.extend(entries.into_iter().map(|g| (g.index(), CHECKED)));
                }
                Callee::Runtime(RtOp::Flow(hirsel_core_layout::flow::Op::Stage)) => stage = true,
                _ => {}
            }
        }
        // A stage's adapter and code run under the stage's own bound.
        if stage {
            if refs.is_empty() {
                every = true;
            }
            pending.extend(refs.values().map(|g| (g.index(), CHECKED)));
        }
        if new_bits & !bits != 0 {
            bits |= new_bits;
            for (key, fs) in &by_convention {
                if convention_bit(*key) & new_bits != 0 {
                    pending.extend(fs.iter().map(|&g| (g, CHECKED)));
                }
            }
        }
        for key in new_keys {
            if let Some(fs) = by_convention.get(&key) {
                pending.extend(fs.iter().map(|&g| (g, CHECKED)));
            }
        }
        if std::mem::take(&mut every) {
            pending.extend((0..n).filter(|&g| entered[g]).map(|g| (g, CHECKED)));
        }
    }
    let mut out = vec![];
    for (f, m) in modes.iter().enumerate() {
        out.extend(
            [CHECKED, FREE]
                .into_iter()
                .filter(|&c| m[c])
                .map(|c| (f, c)),
        );
    }
    out
}

/// Specialize a body set that has no launcher of its own (the release std):
/// every one of `roots` may be entered by another image under a bound or
/// without one, so each root whose free body differs gets both bodies and
/// selects by the step window. A body's context is then a function of the set
/// alone. The roots' [`Summary`]s, in `roots` order.
pub fn specialize_library(module: &mut Module, roots: &[FuncId]) -> Vec<Summary> {
    let none = vec![None; module.functions.len()];
    let entered = entered(module, roots);
    let pending = entered
        .iter()
        .enumerate()
        .filter(|(_, e)| **e)
        .flat_map(|(f, _)| [(f, FREE), (f, CHECKED)])
        .collect();
    split(module, &entered, pending, &none);
    // What a body reaches, as bits ORed over its direct calls, the functions
    // it names and its free body.
    let reach = |own: &dyn Fn(&Body) -> u64| {
        let n = module.functions.len();
        let mut bits: Vec<u64> = module
            .functions
            .iter()
            // A function this module holds no body for may do anything: its
            // body is not here to show otherwise.
            .map(|f| f.body.as_ref().map_or(u64::MAX, own))
            .collect();
        loop {
            let mut changed = false;
            for f in 0..n {
                let Some(body) = &module.functions[f].body else {
                    continue;
                };
                let mut reached = bits[f];
                for inst in &body.insts {
                    match *inst {
                        Inst::Call { call } => {
                            if let Callee::Direct(g) = body.calls[call.index()].callee {
                                reached |= bits[g.index()];
                            }
                        }
                        Inst::FunctionRef { function, .. }
                        | Inst::DescriptorRef { function, .. } => reached |= bits[function.index()],
                        _ => {}
                    }
                }
                if let Some(s) = module.step_free.get(&FuncId(f as u32)) {
                    reached |= bits[s.body.index()];
                }
                if reached != bits[f] {
                    bits[f] = reached;
                    changed = true;
                }
            }
            if !changed {
                return bits;
            }
        }
    };
    let opens = reach(&|b| u64::from(opens_in(b)));
    let launches = reach(&|b| {
        u64::from(
            b.calls
                .iter()
                .any(|c| c.callee == Callee::Runtime(RtOp::LaunchWord)),
        )
    });
    let closures = std::cell::RefCell::new(BTreeMap::new());
    let values = reach(&|b| {
        b.calls
            .iter()
            .filter(|c| matches!(c.callee, Callee::Indirect(_)))
            .fold(0, |bits, c| {
                bits | convention_bit(convention(module, c.signature, &mut closures.borrow_mut()))
            })
    });
    // A body may reach a bound or a launch only through a function value it
    // calls; that function's own image sees it, as it sees every body it holds.
    roots
        .iter()
        .map(|&f| Summary {
            free: module.step_free.contains_key(&f),
            opens_bound: opens[f.index()] != 0,
            launches: launches[f.index()] != 0,
            calls_values: values[f.index()],
        })
        .collect()
}

fn opens_in(b: &Body) -> bool {
    b.calls.iter().any(|c| {
        matches!(
            c.callee,
            Callee::Runtime(RtOp::StepEnter | RtOp::Flow(hirsel_core_layout::flow::Op::Stage))
        )
    })
}

/// Only a `bound steps` scope or a flow stage makes a bound current.
fn opens_bound(module: &Module) -> bool {
    module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .any(opens_in)
}

/// The functions entered other than by a direct call: `roots`, every
/// finalizer and every function a body names.
fn entered(module: &Module, roots: &[FuncId]) -> Vec<bool> {
    let mut entered = vec![false; module.functions.len()];
    for root in roots {
        entered[root.index()] = true;
    }
    for f in module.types.iter().filter_map(|shape| shape.finalizer) {
        entered[f.0 as usize] = true;
    }
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            if let Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } = *inst
            {
                entered[function.index()] = true;
            }
        }
    }
    entered
}

/// Propagate `pending` contexts over direct calls and give each body that runs
/// in both a private free variant. `summaries` holds, for a function without a
/// body here, what its separately lowered image published.
fn split(
    module: &mut Module,
    entered: &[bool],
    mut pending: Vec<(usize, usize)>,
    summaries: &[Option<Summary>],
) {
    let n = module.functions.len();
    let scopes: Vec<Option<Vec<bool>>> = module
        .functions
        .iter()
        .map(|f| f.body.as_ref().and_then(bounded_points))
        .collect();
    let mut modes = vec![[false; 2]; n];
    while let Some((f, mode)) = pending.pop() {
        if std::mem::replace(&mut modes[f][mode], true) {
            continue;
        }
        let Some(body) = &module.functions[f].body else {
            continue;
        };
        for (k, inst) in body.insts.iter().enumerate() {
            if let Inst::Call { call } = *inst
                && let Callee::Direct(g) = body.calls[call.index()].callee
            {
                let free = mode == FREE && scopes[f].as_ref().is_some_and(|b| !b[k]);
                pending.push((g.index(), if free { FREE } else { CHECKED }));
            }
        }
    }
    // A free body differs from its checked one when it drops a step or calls a
    // callee whose free body differs. A summarized callee's differs when its
    // image published a free body.
    let mut differs: Vec<bool> = (0..n)
        .map(|f| modes[f][FREE] && summaries[f].is_some_and(|s| s.free))
        .collect();
    loop {
        let mut changed = false;
        for f in 0..n {
            if differs[f] || !modes[f][FREE] {
                continue;
            }
            let (Some(body), Some(bounded)) = (&module.functions[f].body, &scopes[f]) else {
                continue;
            };
            let free = body.insts.iter().enumerate().any(|(k, inst)| {
                !bounded[k]
                    && match *inst {
                        Inst::Step { .. } => true,
                        Inst::Call { call } => matches!(
                            body.calls[call.index()].callee,
                            Callee::Direct(g) if differs[g.index()]
                        ),
                        _ => false,
                    }
            });
            if free {
                differs[f] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut version: Vec<FuncId> = (0..n as u32).map(FuncId).collect();
    for f in 0..n {
        let declared = module.functions[f].body.is_none();
        if !(differs[f] && (declared || modes[f][CHECKED])) {
            continue;
        }
        let id = FuncId(module.functions.len() as u32);
        let canonical = &module.functions[f];
        let function = Function {
            instance: canonical.instance,
            signature: canonical.signature,
            body: None,
            versioned: canonical.versioned,
            cold: canonical.cold,
            pack_export: canonical.pack_export,
        };
        let identity = module.code_identity(FuncId(f as u32));
        module.functions.push(function);
        module.code_variants.insert(id, free_identity(identity));
        // A summarized function's free body is its image's: this module only
        // calls it by that body's symbol.
        if !declared {
            module.step_free.insert(
                FuncId(f as u32),
                StepFree {
                    body: id,
                    dispatch: entered[f] && !module.functions[f].versioned,
                },
            );
        }
        version[f] = id;
    }
    for f in 0..n {
        if !differs[f] || module.functions[f].body.is_none() {
            continue;
        }
        let body = module.functions[f].body.as_ref().expect("a defined body");
        let bounded = scopes[f].as_ref().expect("an analysed body");
        let free = rewrite(body, bounded, &version);
        module.functions[version[f].index()].body = Some(free);
    }
}

/// For each instruction, whether one of the body's own step scopes is open at it.
/// None when a join disagrees or a scope leaves unopened: the whole body is then
/// treated as bounded.
fn bounded_points(body: &Body) -> Option<Vec<bool>> {
    let mut bounded = vec![true; body.insts.len()];
    let mut depth: Vec<Option<u32>> = vec![None; body.blocks.len()];
    let mut pending = vec![0usize];
    depth[0] = Some(0);
    let mut next = Vec::new();
    while let Some(b) = pending.pop() {
        let mut d = depth[b].expect("a visited block");
        let range = body.blocks[b].insts;
        let start = range.start as usize;
        let insts = range.get(&body.insts)?;
        let mut entered = None;
        next.clear();
        for (offset, inst) in insts.iter().enumerate() {
            bounded[start + offset] = d > 0;
            if let Some(fail) = inst.fail(body) {
                next.push((fail, d));
            }
            if let Inst::Call { call } = *inst {
                let site = &body.calls[call.index()];
                match site.callee {
                    Callee::Runtime(RtOp::StepEnter) => {
                        if offset + 1 != insts.len() {
                            return None;
                        }
                        entered = Some(*site.results.get(&body.pools.values)?.first()?);
                        d += 1;
                    }
                    Callee::Runtime(RtOp::StepLeave | RtOp::StepCatch) => d = d.checked_sub(1)?,
                    _ => {}
                }
            }
        }
        let target = |e: EdgeId| body.edges[e.index()].target;
        match body.terminators[b] {
            // A refused bound is never entered.
            Terminator::Branch { condition, yes, no } => {
                let refused = match entered {
                    Some(admitted) if admitted == condition => d - 1,
                    Some(_) => return None,
                    None => d,
                };
                next.push((target(yes), d));
                next.push((target(no), refused));
            }
            _ if entered.is_some() => return None,
            Terminator::Jump(e) => next.push((target(e), d)),
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                for arm in arms.get(&body.pools.arms)? {
                    next.push((target(arm.edge), d));
                }
                next.push((target(otherwise), d));
            }
            Terminator::Raise { cleanup, .. } => next.extend(cleanup.map(|c| (c, d))),
            Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => {
                if d != 0 {
                    return None;
                }
            }
        }
        for &(successor, d) in &next {
            match depth[successor.index()] {
                Some(old) if old != d => return None,
                Some(_) => {}
                None => {
                    depth[successor.index()] = Some(d);
                    pending.push(successor.index());
                }
            }
        }
    }
    Some(bounded)
}

/// The free body: the steps outside the body's own scopes are gone, and direct
/// calls there reach their callees' free bodies.
fn rewrite(body: &Body, bounded: &[bool], version: &[FuncId]) -> Body {
    let mut free = body.clone();
    free.insts.clear();
    free.inst_anchors.clear();
    for (b, block) in body.blocks.iter().enumerate() {
        let start = free.insts.len() as u32;
        let (first, len) = (block.insts.start as usize, block.insts.len as usize);
        for (k, inst) in body.insts.iter().enumerate().skip(first).take(len) {
            if !bounded[k] {
                match *inst {
                    Inst::Step { .. } => continue,
                    Inst::Call { call } => {
                        if let Callee::Direct(g) = body.calls[call.index()].callee {
                            free.calls[call.index()].callee = Callee::Direct(version[g.index()]);
                        }
                    }
                    _ => {}
                }
            }
            free.insts.push(inst.clone());
            free.inst_anchors.push(body.inst_anchors[k]);
        }
        free.blocks[b].insts = PoolRange::new(start, free.insts.len() as u32 - start);
    }
    free
}

/// The canonical function whose free body each private step-free variant is.
pub fn variants(module: &Module) -> BTreeMap<FuncId, FuncId> {
    module
        .step_free
        .iter()
        .map(|(&canonical, free)| (free.body, canonical))
        .collect()
}
