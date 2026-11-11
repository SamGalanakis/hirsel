//! Charged instances (STORE-11 energy). Code that can run under a budget is
//! compiled as a separate charged instance of its definition; all other code
//! carries no charge. A charged instance's digest is derived from its
//! uncharged instance's digest alone, so either can be named from the other.
use hirsel_syntax::ContentHash;

/// The symbol digest domain of a charged instance.
pub const CHARGED_DOMAIN: &str = "hirsel.charged-instance.v1";

/// The symbol digest of the charged instance of the instance whose digest is
/// `uncharged`.
pub fn charged_digest(uncharged: ContentHash) -> ContentHash {
    ContentHash::of_parts(CHARGED_DOMAIN, [uncharged.0.as_slice()])
}

/// The unit with its `Inst::Charge`s removed: what a charged instance's
/// invocation and ownership checks run on, so a twin passes exactly when its
/// uncharged form would.
pub fn strip<S: crate::stage::Stage>(
    unit: &crate::ir::LirUnit<S>,
) -> Result<crate::ir::LirUnit<S>, String> {
    use crate::ir::{Inst, PoolRange};
    let mut out = unit.clone();
    let mut instructions = Vec::with_capacity(unit.instructions.len());
    for block in &mut out.blocks {
        let kept = block
            .instructions
            .get(&unit.instructions)
            .ok_or("charged unit block instructions outside table")?
            .iter()
            .filter(|i| !matches!(i, Inst::Charge { .. }))
            .cloned();
        let start = instructions.len();
        instructions.extend(kept);
        block.instructions = PoolRange::new(
            u32::try_from(start).map_err(|_| "charged unit too large")?,
            u32::try_from(instructions.len() - start).map_err(|_| "charged unit too large")?,
        );
    }
    out.instructions = instructions;
    Ok(out)
}

/// Whether a charged unit carries every charge the charging rule requires,
/// and only those, and names only charged code (STORE-11 energy).
///
/// - There is at most one `Entry` charge, and one whenever the unit can
///   transfer control to another unit by any call (see `transfers`). It
///   precedes every call: no path from the unit's entry reaches a call or a
///   call admission without passing it.
/// - No cycle avoids every charge: with the blocks holding an `Iteration` or
///   the `Entry` charge removed as cut points, the control-flow graph is
///   acyclic, except for a counted loop's back edge, which the loop's one
///   `Span` charge at entry pays for (`counted_loop`). A charge in a block
///   runs on every pass through it, so a cut block charges every trip around
///   any cycle through it. An `Entry` charge inside a cycle (a self-tail
///   loop, whose header is the body's entry block) so pays one unit per
///   activation, and a loop with some path around it uncharged is refused.
///   Every `Iteration` charge is on a cycle.
/// - Every function and constant it names is charged, unless the target can
///   never have a charged instance: a `core@1` definition (runtime operations
///   and LIR-built library bodies, which carry their own size charges). An
///   uncharged source-defined function or constant, or a foreign function,
///   is refused by the symbol's own flag.
///
/// The finalizer branch of a charged drop is glue, not a unit charge.
pub fn check<S: crate::stage::Stage>(unit: &crate::ir::LirUnit<S>) -> Result<(), String> {
    use crate::ir::{Energy, Inst, SymbolKind, Terminator};
    let fail = |what: String| Err(format!("{what} (STORE-11 energy)"));
    if let Some(symbol) = unit.tables.symbols.iter().find(|s| {
        matches!(s.kind, SymbolKind::Function | SymbolKind::Constant)
            && !s.charged
            && !s.definition.package_is("core", "1")
    }) {
        return fail(format!(
            "a charged unit names uncharged {:?} {:?}",
            symbol.kind, symbol.definition
        ));
    }
    let n = unit.blocks.len();
    let (forward, instructions) = graph(unit)?;

    // Entry: on the straight path, before any transfer or cycle.
    let transfers = transfers(unit)?;
    let entries = instructions
        .iter()
        .flat_map(|i| i.iter())
        .filter(|i| {
            matches!(
                i,
                Inst::Charge {
                    energy: Energy::Entry,
                    ..
                }
            )
        })
        .count();
    if entries > 1 || (transfers && entries == 0) {
        return fail(format!(
            "a charged unit that {} control to another unit carries {entries} entry charges",
            if transfers {
                "transfers"
            } else {
                "transfers no"
            }
        ));
    }
    if entries == 1 {
        // The blocks reachable from the entry without passing the charge.
        let mut seen = vec![false; n];
        let mut pending = vec![unit.entry.index()];
        let mut charged = false;
        while let Some(at) = pending.pop() {
            if at >= n || std::mem::replace(&mut seen[at], true) {
                continue;
            }
            let mut passes = false;
            for inst in instructions[at] {
                // A failure here, the entry charge's own included, leaves
                // before the charge is paid.
                if let Some(failure) = inst.failure() {
                    let edge = unit.tables.failures[failure.index()].cleanup;
                    pending.push(unit.tables.edges[edge.index()].target.index());
                }
                match inst {
                    Inst::Charge {
                        energy: Energy::Entry,
                        ..
                    } => {
                        passes = true;
                        break;
                    }
                    Inst::FrameEnter { .. } => {
                        return fail("a call is admitted before the entry charge".into());
                    }
                    _ => {}
                }
            }
            if passes {
                charged = true;
                continue;
            }
            if matches!(unit.terminators[at], Terminator::Call { .. }) {
                return fail("a call is reachable before the entry charge".into());
            }
            let ran = instructions[at].len();
            pending.extend(
                forward[at]
                    .iter()
                    .filter(|o| o.ran == ran)
                    .map(|o| o.target),
            );
        }
        if !charged {
            return fail("the entry charge is unreachable from the unit's entry".into());
        }
    }

    cycles_paid(unit, true)
}

/// Whether no cycle of `unit` avoids every charge it emits, where its `Entry`
/// charge is emitted only when `entry` is set (STORE-11 energy). A backend
/// that drops an `Entry` charge keeps it whenever this refuses the unit
/// without it: in a self-tail loop the entry charge is the loop's only one.
///
/// Every iteration charge is on a cycle, and every other cycle passes a
/// block holding an emitted `Iteration` or `Entry` charge, except that a
/// counted loop's single charge (`Span`) at its entry pays for its back edge
/// when the loop's shape proves the amount bounds its iterations
/// (`counted_loop`).
///
/// A cycle may run through failure transfers: an instruction's or a
/// terminator's failure enters cleanup, which a `Recover` or `Isolate` can
/// resume into ordinary code. A charge pays only the transfers that leave
/// after it ran, so a failure out of the charge itself pays nothing.
pub fn cycles_paid<S: crate::stage::Stage>(
    unit: &crate::ir::LirUnit<S>,
    entry: bool,
) -> Result<(), String> {
    use crate::ir::{Energy, Inst};
    let fail = |what: String| Err(format!("{what} (STORE-11 energy)"));
    let n = unit.blocks.len();
    let (forward, instructions) = graph(unit)?;
    let charges = |b: usize, f: &dyn Fn(&Energy) -> bool| {
        instructions[b]
            .iter()
            .any(|i| matches!(i, Inst::Charge { energy, .. } if f(energy)))
    };
    let targets = |keep: &dyn Fn(usize, &Out) -> bool| {
        (0..n)
            .map(|b| {
                forward[b]
                    .iter()
                    .filter(|o| keep(b, o))
                    .map(|o| o.target)
                    .collect()
            })
            .collect::<Vec<Vec<usize>>>()
    };
    let cyclic = cycles(&targets(&|_, _| true), &vec![true; n]).1;
    let mut paid = Vec::new();
    for (b, block) in instructions.iter().enumerate() {
        for inst in block.iter() {
            if let Inst::Charge {
                energy:
                    Energy::Span {
                        from,
                        to,
                        inclusive,
                    },
                ..
            } = *inst
            {
                let edge = match counted_loop(unit, b, from, to, inclusive) {
                    Ok(edge) => edge,
                    Err(why) => return fail(format!("the counted charge in block {b}: {why}")),
                };
                if paid.contains(&edge) {
                    return fail(format!("two counted charges pay for edge {edge}"));
                }
                paid.push(edge);
            }
        }
        if charges(b, &|e| matches!(e, Energy::Iteration)) && !cyclic[b] {
            return fail(format!("an iteration charge in block {b} is on no cycle"));
        }
    }
    // A transfer pays its trip when a charge ran before it: a failure out of
    // a charge, or out of anything before it, has paid nothing.
    let cut = |e: &Energy| matches!(e, Energy::Iteration) || (entry && matches!(e, Energy::Entry));
    let unpaid = targets(&|b, o| {
        !paid.contains(&o.edge)
            && !instructions[b][..o.ran]
                .iter()
                .any(|i| matches!(i, Inst::Charge { energy, .. } if cut(energy)))
    });
    if let Some(b) = cycles(&unpaid, &vec![true; n]).1.iter().position(|&c| c) {
        return fail(format!("the loop at block {b} has no charge"));
    }
    Ok(())
}

/// A transfer out of a block: to `target` by table edge `edge`, after the
/// block's first `ran` instructions completed. A normal edge or a
/// terminator's failure runs after all of them; an instruction's failure
/// after the ones before it, not the failing one, a failed charge included.
#[derive(Clone, Copy)]
struct Out {
    target: usize,
    edge: usize,
    ran: usize,
}

/// Each block's transfers: its normal edges, its terminator's failure edges
/// and each instruction's failure edge (a failure enters failure cleanup,
/// which a `Recover` or `Isolate` can resume into ordinary code), and its
/// instructions.
#[allow(clippy::type_complexity)]
fn graph<S: crate::stage::Stage>(
    unit: &crate::ir::LirUnit<S>,
) -> Result<(Vec<Vec<Out>>, Vec<&[crate::ir::Inst]>), String> {
    let n = unit.blocks.len();
    let instructions = unit
        .blocks
        .iter()
        .map(|block| {
            block
                .instructions
                .get(&unit.instructions)
                .ok_or_else(|| "charged unit block instructions outside table".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let to = |edge: crate::ir::EdgeId, ran: usize| -> Result<Out, String> {
        unit.tables
            .edges
            .get(edge.index())
            .map(|e| e.target.index())
            .filter(|&t| t < n)
            .map(|target| Out {
                target,
                edge: edge.index(),
                ran,
            })
            .ok_or_else(|| "charged unit edge outside table".to_owned())
    };
    let cleanup = |failure: crate::ir::FailureId| -> Result<crate::ir::EdgeId, String> {
        unit.tables
            .failures
            .get(failure.index())
            .map(|f| f.cleanup)
            .ok_or_else(|| "charged unit failure outside table".to_owned())
    };
    let mut out = Vec::with_capacity(n);
    for (b, block) in instructions.iter().enumerate() {
        let term = unit
            .terminators
            .get(b)
            .ok_or("charged unit block has no terminator")?;
        let mut outs = Vec::new();
        for (i, inst) in block.iter().enumerate() {
            if let Some(failure) = inst.failure() {
                outs.push(to(cleanup(failure)?, i)?);
            }
        }
        for edge in term.normal_edges(&unit.pools) {
            outs.push(to(edge, block.len())?);
        }
        for failure in term.failure_edges(&unit.tables) {
            outs.push(to(cleanup(failure)?, block.len())?);
        }
        out.push(outs);
    }
    Ok((out, instructions))
}

/// The edge a `Span { from, to, inclusive }` charge in block `entry` pays
/// for: the back edge of a loop whose shape proves it runs at most that many
/// iterations (STORE-11 energy).
///
/// - `entry` ends in a jump to the loop's header `h`, outside the loop (the
///   blocks on a cycle with `h` that avoids `entry`). That jump and one back edge from inside
///   the loop are the only edges into `h`, and no other edge enters the loop.
/// - `h` ends in a branch that stays in the loop on yes and leaves it on no.
///   Its condition is `counter < bound` (`<=` when `inclusive`), where
///   `counter` is a parameter of `h` that the entry jump sets to `from` and
///   `bound` is `to` at every test.
/// - The back edge sets `counter` to `counter + 1` by a checked add.
///
/// So the counter starts at `from`, rises by exactly one per iteration
/// without wrapping, and the body runs only while it is in the charged range.
/// Values are compared through the slot versions that hold them (`Store`,
/// `Replace`, and whole-slot `Read`s of a version) and through loop block
/// parameters that only ever receive one class of value.
fn counted_loop<S: crate::stage::Stage>(
    unit: &crate::ir::LirUnit<S>,
    entry: usize,
    from: crate::ir::ValueId,
    to: crate::ir::ValueId,
    inclusive: bool,
) -> Result<usize, String> {
    use crate::ir::{AddressBase, BinaryOp, ConstantValue, Inst, ReadMode, Terminator, ValueId};
    use std::collections::{HashMap, HashSet};
    let n = unit.blocks.len();
    let edge = |e: usize| {
        unit.tables
            .edges
            .get(e)
            .filter(|edge| edge.target.index() < n)
            .ok_or_else(|| "edge outside table".to_owned())
    };
    let arguments = |e: usize| -> Result<&[ValueId], String> {
        edge(e)?
            .arguments
            .get(&unit.pools.values)
            .ok_or_else(|| "edge arguments outside pool".to_owned())
    };
    let parameters = |b: usize| -> Result<&[ValueId], String> {
        unit.blocks[b]
            .parameters
            .get(&unit.pools.values)
            .ok_or_else(|| "block parameters outside pool".to_owned())
    };
    let Terminator::Jump(into) = unit.terminators[entry] else {
        return Err("it does not end in a jump to a loop header".into());
    };
    let header = edge(into.index())?.target.index();
    let forward = (0..n)
        .map(|b| {
            unit.terminators[b]
                .normal_edges(&unit.pools)
                .into_iter()
                .map(|e| edge(e.index()).map(|edge| edge.target.index()))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    // The loop: the blocks on a cycle with `header` that avoids `entry`, so
    // an enclosing loop that `entry` itself is in stays outside it.
    let alive: Vec<bool> = (0..n).map(|b| b != entry).collect();
    let (component, cyclic) = cycles(&forward, &alive);
    if header == entry || !cyclic[header] {
        return Err("its jump does not enter a loop".into());
    }
    let inside = |b: usize| b != entry && component[b] == component[header];

    // Every edge into the loop: the entry jump, one back edge into the
    // header, and edges between the loop's own blocks. A failure edge never
    // enters it.
    let mut sources: HashMap<usize, Vec<usize>> = HashMap::new();
    for b in 0..n {
        for e in unit.terminators[b].normal_edges(&unit.pools) {
            sources.entry(e.index()).or_default().push(b);
        }
    }
    let failures: HashSet<usize> = unit
        .tables
        .failures
        .iter()
        .map(|f| f.cleanup.index())
        .collect();
    let mut back = None;
    for (e, target) in unit.tables.edges.iter().enumerate() {
        let target = target.target.index();
        if target >= n || !inside(target) {
            continue;
        }
        let from_blocks = sources.get(&e).map_or(&[][..], Vec::as_slice);
        if failures.contains(&e) || from_blocks.is_empty() {
            return Err(format!("edge {e} enters the loop from no ordinary block"));
        }
        if e == into.index() {
            if from_blocks != [entry] {
                return Err("its jump is shared".into());
            }
            continue;
        }
        if let &[s] = from_blocks
            && inside(s)
        {
            if target == header && back.replace(e).is_some() {
                return Err("the loop header has two back edges".into());
            }
            continue;
        }
        return Err(format!("edge {e} enters the loop other than at its header"));
    }
    let back = back.ok_or("the loop header has no back edge")?;

    // Where each value comes from: through slot versions to the value stored.
    let mut defined: HashMap<ValueId, &Inst> = HashMap::new();
    let mut in_header = HashSet::new();
    for (b, block) in unit.blocks.iter().enumerate() {
        for inst in block
            .instructions
            .get(&unit.instructions)
            .ok_or("block instructions outside table")?
        {
            let results: &[ValueId] = match inst {
                Inst::Read { result, .. }
                | Inst::Binary { result, .. }
                | Inst::Const { result, .. } => std::slice::from_ref(result),
                Inst::Store { version, .. } | Inst::Replace { version, .. } => {
                    std::slice::from_ref(version)
                }
                _ => &[],
            };
            for &r in results {
                defined.insert(r, inst);
                if b == header {
                    in_header.insert(r);
                }
            }
        }
    }
    let whole = |address: crate::ir::AddressId| {
        unit.addresses.get(address.index()).is_some_and(|a| {
            matches!(a.base, AddressBase::Slot(_))
                && a.projection
                    .get(&unit.pools.projections)
                    .is_some_and(<[_]>::is_empty)
        })
    };
    let source = |mut v: ValueId| {
        for _ in 0..=unit.values.len() {
            v = match defined.get(&v) {
                Some(Inst::Read {
                    address,
                    reaching,
                    mode: ReadMode::Bits | ReadMode::Take,
                    ..
                }) if whole(*address) => *reaching,
                Some(Inst::Store { address, value, .. })
                | Some(Inst::Replace { address, value, .. })
                    if whole(*address) =>
                {
                    *value
                }
                _ => return v,
            };
        }
        v
    };
    // The loop block parameters that only ever receive a value of `seeds`
    // (the greatest such set): equal, at every use, to the seed's latest value.
    let class = |seeds: &HashSet<ValueId>, header_too: bool| -> Result<HashSet<ValueId>, String> {
        let mut members = HashSet::new();
        let mut incoming: Vec<(ValueId, Vec<ValueId>)> = Vec::new();
        for b in (0..n).filter(|&b| inside(b) && (header_too || b != header)) {
            for (i, &p) in parameters(b)?.iter().enumerate() {
                let mut from = Vec::new();
                for (e, target) in unit.tables.edges.iter().enumerate() {
                    if target.target.index() == b {
                        from.push(
                            *arguments(e)?
                                .get(i)
                                .ok_or("edge passes too few arguments")?,
                        );
                    }
                }
                members.insert(p);
                incoming.push((p, from));
            }
        }
        loop {
            let before = members.len();
            for (p, from) in &incoming {
                if members.contains(p)
                    && !from.iter().all(|&a| {
                        let a = source(a);
                        seeds.contains(&a) || members.contains(&a)
                    })
                {
                    members.remove(p);
                }
            }
            if members.len() == before {
                break;
            }
        }
        Ok(members.into_iter().chain(seeds.iter().copied()).collect())
    };

    let Terminator::Branch { condition, yes, no } = unit.terminators[header] else {
        return Err("the loop header does not end in a branch".into());
    };
    if !inside(edge(yes.index())?.target.index()) || inside(edge(no.index())?.target.index()) {
        return Err("the loop header's branch does not exit on its no edge".into());
    }
    let test = source(condition);
    let Some(&&Inst::Binary {
        op, left, right, ..
    }) = defined.get(&test).filter(|_| in_header.contains(&test))
    else {
        return Err("the loop header's branch tests no comparison made in it".into());
    };
    if op
        != if inclusive {
            BinaryOp::Le
        } else {
            BinaryOp::Lt
        }
    {
        return Err("the loop header's test is not the charged range's bound".into());
    }
    let bound = class(&HashSet::from([source(to)]), true)?;
    if !bound.contains(&source(right)) {
        return Err("the loop header's bound is not the charged amount's".into());
    }
    let start = source(from);
    let entering = arguments(into.index())?;
    let closing = arguments(back)?;
    let one = |v: ValueId| {
        matches!(defined.get(&source(v)), Some(Inst::Const { constant, .. })
            if unit.tables.constants.get(constant.index())
                .is_some_and(|c| c.value == ConstantValue::Integer(1)))
    };
    for (k, &counter) in parameters(header)?.iter().enumerate() {
        if source(*entering.get(k).ok_or("edge passes too few arguments")?) != start {
            continue;
        }
        let current = class(&HashSet::from([counter]), false)?;
        if !current.contains(&source(left)) {
            continue;
        }
        let steps: HashSet<ValueId> = defined
            .iter()
            .filter(|(_, inst)| {
                matches!(**inst, Inst::Binary { op: BinaryOp::Add, left, right, .. }
                    if current.contains(&source(*left)) && one(*right))
            })
            .map(|(&r, _)| r)
            .collect();
        let next = class(&steps, false)?;
        if next.contains(&source(
            *closing.get(k).ok_or("edge passes too few arguments")?,
        )) {
            return Ok(back);
        }
    }
    Err("no header parameter starts at the charged start, is tested, and steps by one".into())
}

/// Whether `unit` can transfer control to another unit: a call that adds an
/// activation (LANG-10), whether direct, through an adapter, of a callable
/// value or iterator step, or of a core operation that runs code it is given.
/// Drops reach finalizers through drop glue, which charges its finalizer
/// branch itself.
fn transfers<S: crate::stage::Stage>(unit: &crate::ir::LirUnit<S>) -> Result<bool, String> {
    use crate::ir::{Callee, Terminator};
    for terminator in &unit.terminators {
        let Terminator::Call { call, .. } = terminator else {
            continue;
        };
        let site = unit
            .tables
            .calls
            .get(call.index())
            .ok_or("charged unit call outside table")?;
        let definition = match site.callee {
            Callee::Indirect(_) | Callee::Bound { .. } => return Ok(true),
            Callee::Direct(symbol) => {
                &unit
                    .tables
                    .symbols
                    .get(symbol.index())
                    .ok_or("charged unit symbol outside table")?
                    .definition
            }
            Callee::Operation(operation) => {
                &unit
                    .tables
                    .operations
                    .get(operation.0 as usize)
                    .ok_or("charged unit operation outside table")?
                    .definition
            }
        };
        // FACT-3's spare-capacity append is its source primitive `list.push`
        // with the capacity check removed (as invocation coverage holds it):
        // the same leaf, which lowering saw and gave no entry charge.
        if hirsel_core_contract::depth::adds_activation(definition)
            && !definition.is_core("$native.list_push_spare")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The strongly connected components of `forward` restricted to the `alive`
/// blocks (Kosaraju), and whether each block lies on a cycle among them: in a
/// component of two or more blocks, or with an edge to itself.
fn cycles(forward: &[Vec<usize>], alive: &[bool]) -> (Vec<usize>, Vec<bool>) {
    let n = forward.len();
    let edges = |b: usize| forward[b].iter().copied().filter(move |&s| alive[s]);
    let mut order = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    for root in (0..n).filter(|&b| alive[b]) {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, 0usize)];
        while let Some((b, i)) = stack.pop() {
            if let Some(s) = edges(b).nth(i) {
                stack.push((b, i + 1));
                if !seen[s] {
                    seen[s] = true;
                    stack.push((s, 0));
                }
            } else {
                order.push(b);
            }
        }
    }
    let mut backward = vec![Vec::new(); n];
    for b in (0..n).filter(|&b| alive[b]) {
        for s in edges(b) {
            backward[s].push(b);
        }
    }
    let mut component = vec![usize::MAX; n];
    let mut sizes = Vec::new();
    for &root in order.iter().rev() {
        if component[root] != usize::MAX {
            continue;
        }
        let id = sizes.len();
        sizes.push(0usize);
        component[root] = id;
        let mut stack = vec![root];
        while let Some(b) = stack.pop() {
            sizes[id] += 1;
            for &p in &backward[b] {
                if component[p] == usize::MAX {
                    component[p] = id;
                    stack.push(p);
                }
            }
        }
    }
    let cyclic = (0..n)
        .map(|b| alive[b] && (sizes[component[b]] > 1 || forward[b].contains(&b)))
        .collect();
    (component, cyclic)
}
