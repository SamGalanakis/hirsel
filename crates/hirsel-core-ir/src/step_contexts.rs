//! MEM-30 step contexts. A step taken while no step bound is current is never
//! observed: no bound is charged and no slice ends. Each function body runs in
//! one of two contexts, checked (a bound may be current) or free (none is), and
//! a free context keeps only the steps taken inside the body's own `bound steps`
//! scopes. Direct calls carry their context to the callee; a body that runs in
//! both gets a private free code variant, and the entries that code pointers,
//! finalizers and the launcher reach select their body by the step window.
use crate::*;
use hirsel_core_ids::ContentHash;
use std::collections::BTreeMap;

const CHECKED: usize = 0;
const FREE: usize = 1;

/// What a separately lowered body set (the release std, `std_release` in the
/// driver) publishes about each entry another image calls, computed from that
/// set's bodies alone.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Summary {
    /// The entry has a step-free body under [`free_identity`], beside its
    /// checked one; its own symbol selects either by the step window.
    pub free: bool,
    /// Running it can make a step bound current (a `bound steps` scope or a
    /// flow stage), through its direct calls and the functions it names.
    pub opens_bound: bool,
    /// Running it can issue `LaunchWord`, by the same edges.
    pub launches: bool,
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
                        Callee::Indirect(_) | Callee::Runtime(_) => true,
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
    let mut pending = Vec::new();
    for (f, _) in entered.iter().enumerate().filter(|(_, e)| **e) {
        pending.push((f, FREE));
        if bounded_image && f != entry.index() {
            pending.push((f, CHECKED));
        }
    }
    if bounded_image && entered[entry.index()] && referenced(module, entry) {
        pending.push((entry.index(), CHECKED));
    }
    split(module, &entered, pending, &summaries);
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
    let reach = |is: &dyn Fn(&Body) -> bool| {
        let n = module.functions.len();
        let mut yes: Vec<bool> = module
            .functions
            .iter()
            // A function this module holds no body for may do either: its
            // body is not here to show otherwise.
            .map(|f| f.body.as_ref().is_none_or(is))
            .collect();
        loop {
            let mut changed = false;
            for f in 0..n {
                if yes[f] {
                    continue;
                }
                let Some(body) = &module.functions[f].body else {
                    continue;
                };
                let reaches = body.insts.iter().any(|inst| match *inst {
                    Inst::Call { call } => matches!(
                        body.calls[call.index()].callee,
                        Callee::Direct(g) if yes[g.index()]
                    ),
                    Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } => {
                        yes[function.index()]
                    }
                    _ => false,
                }) || module
                    .step_free
                    .get(&FuncId(f as u32))
                    .is_some_and(|s| yes[s.body.index()]);
                if reaches {
                    yes[f] = true;
                    changed = true;
                }
            }
            if !changed {
                return yes;
            }
        }
    };
    let opens = reach(&|b| opens_in(b));
    let launches = reach(&|b| {
        b.calls
            .iter()
            .any(|c| c.callee == Callee::Runtime(RtOp::LaunchWord))
    });
    // A body may reach a bound or a launch only through a function value it
    // calls; that function's own image sees it, as it sees every body it holds.
    roots
        .iter()
        .map(|&f| Summary {
            free: module.step_free.contains_key(&f),
            opens_bound: opens[f.index()],
            launches: launches[f.index()],
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

/// The entry is also a function value somewhere, so a bound may be current when
/// it is entered by pointer.
fn referenced(module: &Module, entry: FuncId) -> bool {
    module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .flat_map(|b| &b.insts)
        .any(|inst| {
            matches!(*inst, Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } if function == entry)
        })
        || module
            .types
            .iter()
            .any(|shape| shape.finalizer.is_some_and(|f| f.0 == entry.0))
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
