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
/// - An `Entry` charge sits on the straight jump path from the unit's entry,
///   off every cycle, with no call admission, call or cycle before it. There
///   is at most one, and one whenever the unit can transfer control to
///   another unit by any call (see `transfers`).
/// - Every cycle passes through an `Iteration` charge, and every `Iteration`
///   charge is on a cycle, except a counted loop's back edge, which the
///   loop's one `Span` charge at entry pays for (`counted_loop`).
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
    let successors = |b: usize| -> Result<Vec<usize>, String> {
        unit.terminators
            .get(b)
            .ok_or("charged unit block has no terminator")?
            .normal_edges(&unit.pools)
            .into_iter()
            .map(|e| {
                unit.tables
                    .edges
                    .get(e.index())
                    .map(|edge| edge.target.index())
                    .filter(|&t| t < n)
                    .ok_or_else(|| "charged unit edge outside table".to_owned())
            })
            .collect()
    };
    let forward = (0..n).map(successors).collect::<Result<Vec<_>, _>>()?;
    let instructions = unit
        .blocks
        .iter()
        .map(|block| {
            block
                .instructions
                .get(&unit.instructions)
                .ok_or("charged unit block instructions outside table")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let holds = |b: usize, f: &dyn Fn(&Energy) -> bool| {
        instructions[b]
            .iter()
            .any(|i| matches!(i, Inst::Charge { energy, .. } if f(energy)))
    };
    let all = vec![true; n];
    let cyclic = cycles(&forward, &all).1;

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
        let mut at = unit.entry.index();
        let mut seen = vec![false; n];
        'path: loop {
            if at >= n || seen[at] || cyclic[at] {
                return fail("no entry charge precedes the unit's first call or loop".into());
            }
            seen[at] = true;
            for inst in instructions[at] {
                match inst {
                    Inst::Charge {
                        energy: Energy::Entry,
                        ..
                    } => break 'path,
                    Inst::FrameEnter { .. } => {
                        return fail("a call is admitted before the entry charge".into());
                    }
                    _ => {}
                }
            }
            match unit.terminators[at] {
                Terminator::Jump(_) => at = forward[at][0],
                _ => return fail("the entry charge is off the unit's straight entry path".into()),
            }
        }
    }

    // Every cycle passes through an iteration charge, and every iteration
    // charge is on a cycle, except that a counted loop's single charge
    // (`Span`) at its entry pays for its back edge when the loop's shape
    // proves the amount bounds its iterations (`counted_loop`).
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
        if holds(b, &|e| matches!(e, Energy::Iteration)) && !cyclic[b] {
            return fail(format!("an iteration charge in block {b} is on no cycle"));
        }
    }
    let unpaid = (0..n)
        .map(|b| {
            unit.terminators[b]
                .normal_edges(&unit.pools)
                .into_iter()
                .filter(|e| !paid.contains(&e.index()))
                .map(|e| unit.tables.edges[e.index()].target.index())
                .collect()
        })
        .collect::<Vec<Vec<usize>>>();
    let alive: Vec<bool> = (0..n)
        .map(|b| !holds(b, &|e| matches!(e, Energy::Iteration)))
        .collect();
    if let Some(b) = cycles(&unpaid, &alive).1.iter().position(|&c| c) {
        return fail(format!("the loop at block {b} has no charge"));
    }
    Ok(())
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
        if hirsel_core_contract::depth::adds_activation(definition) {
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
