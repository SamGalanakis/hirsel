use super::*;

impl<'m, 'a> FnEmitter<'m, 'a> {
    /// Every ordinary call retires its cleanup mode before returning. Only explicit
    /// FinalizeBegin/End can change it across instructions in this frame.
    pub(super) fn depth_mode(&mut self, cleanup_at: Value<'m>) -> (Value<'m>, Value<'m>) {
        if let Some(mode) = self.depth_mode {
            return mode;
        }
        use hirsel_lir::runtime_abi::CTX_CLEANUP_MODE_OFFSET;
        let b = &self.prologue;
        let i32_ty = self.module.int_type(32);
        let cleanup = b.load_access(self.module.int_type(16), cleanup_at, 2, Access::Depth);
        let mode = b.load_access(
            i32_ty,
            b.offset(self.context, self.const_u64(CTX_CLEANUP_MODE_OFFSET)),
            4,
            Access::Depth,
        );
        let either = b.binary(BinOp::Or, mode, b.cast(Cast::ZExt, cleanup, i32_ty));
        let cleaning = b.icmp(IntPredicate::Ne, either, self.module.const_zero(i32_ty));
        if self.foreground_entry.is_some() {
            self.depth_mode = Some((cleanup, cleaning));
        }
        (cleanup, cleaning)
    }

    /// LLVM FunctionSpecialization.cpp:861-867 clones a body for a known input
    /// and rewrites only matching direct calls. Here the input is ambient mode:
    /// the original entry dispatches, and same-mode direct calls stay private.
    /// Continuations and bodies that hold cleanup mode keep their ordinary entry.
    pub(super) fn dispatch_foreground(&mut self) {
        let Some(function) = self.foreground_entry else {
            return;
        };
        let cleanup_at = self.prologue.offset(
            self.context,
            self.const_u64(hirsel_lir::runtime_abi::CTX_CLEANUP_COUNT_OFFSET),
        );
        let (_, cleaning) = self.depth_mode(cleanup_at);
        let slow = self.module.append_block(self.function);
        let fast = self.module.append_block(self.function);
        self.prologue.cold_cond_br(cleaning, slow, fast);
        self.builder.position(fast);
        let args: Vec<_> = (0..self.signature.types.len())
            .map(|i| self.module.parameter(self.function, i as u32))
            .collect();
        let result = self.builder.call(function, &args);
        self.builder.ret(result);
        self.prologue_block = slow;
        self.prologue.position(slow);
    }
}

/// The complete direct call-graph SCCs, including bound closure calls.
pub(super) fn recursive<'a>(
    units: impl Iterator<Item = &'a LirUnit<Ready>>,
) -> BTreeSet<ContentHash> {
    let units: Vec<_> = units.collect();
    let indices: BTreeMap<_, _> = units
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.header.instance, index))
        .collect();
    let calls: Vec<Vec<usize>> = units
        .iter()
        .map(|unit| {
            unit.tables
                .calls
                .iter()
                .filter_map(|call| {
                    let symbol = match call.callee {
                        Callee::Direct(symbol) | Callee::Bound { code: symbol, .. } => symbol,
                        _ => return None,
                    };
                    indices
                        .get(&unit.tables.symbols[symbol.index()].instance?)
                        .copied()
                })
                .collect()
        })
        .collect();
    hirsel_graph::components(&calls)
        .into_iter()
        .filter(|component| component.len() > 1 || calls[component[0]].contains(&component[0]))
        .flatten()
        .map(|index| units[index].header.instance)
        .collect()
}

/// Cloning an acyclic body or a body with growing/repeated compiled calls can
/// amortize the dispatch. Primitive-only loops already amortize their mode load.
fn profitable(
    unit: &LirUnit<Ready>,
    depth_leaf: &BTreeSet<hirsel_lir::invocations::room::UnitKey>,
    source_units: &BTreeSet<hirsel_lir::invocations::room::UnitKey>,
    recursive: bool,
) -> bool {
    let successors: Vec<_> = unit
        .terminators
        .iter()
        .map(|terminator| {
            terminator
                .normal_edges(&unit.pools)
                .iter()
                .map(|edge| unit.tables.edges[edge.index()].target.index())
                .collect()
        })
        .collect();
    let cycles: Vec<_> = hirsel_graph::components(&successors)
        .into_iter()
        .filter(|component| component.len() > 1 || successors[component[0]].contains(&component[0]))
        .collect();
    let repeated = cycles.iter().flatten().any(|&block| {
        let Terminator::Call { call, .. } = unit.terminators[block] else {
            return false;
        };
        let Callee::Direct(symbol) = unit.tables.calls[call.index()].callee else {
            return false;
        };
        let symbol = &unit.tables.symbols[symbol.index()];
        let key = (symbol.definition.clone(), symbol.instance);
        source_units.contains(&key) && !depth_leaf.contains(&key)
    });
    cycles.is_empty() || repeated || recursive
}

/// A wrapper cannot amortize mode specialization through a primitive-only loop.
/// Propagate that boundary to its callers; recursive SCCs still grow depth on
/// their own and retain their specialized entry.
pub(super) fn profitable_units<'a>(
    units: impl Iterator<Item = &'a LirUnit<Ready>>,
    depth_leaf: &BTreeSet<hirsel_lir::invocations::room::UnitKey>,
    recursive: &BTreeSet<ContentHash>,
) -> BTreeSet<ContentHash> {
    let units: Vec<_> = units
        .filter(|unit| unit.header.origin == UnitOrigin::Source)
        .collect();
    let sources = units
        .iter()
        .map(|unit| (unit.header.definition.clone(), Some(unit.header.instance)))
        .collect();
    let mut selected: BTreeSet<_> = units
        .iter()
        .filter(|unit| {
            profitable(
                unit,
                depth_leaf,
                &sources,
                recursive.contains(&unit.header.instance),
            )
        })
        .map(|unit| unit.header.instance)
        .collect();
    let mut callers: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for unit in &units {
        for call in &unit.tables.calls {
            let symbol = match call.callee {
                Callee::Direct(symbol) | Callee::Bound { code: symbol, .. } => symbol,
                _ => continue,
            };
            let symbol = &unit.tables.symbols[symbol.index()];
            if sources.contains(&(symbol.definition.clone(), symbol.instance)) {
                callers
                    .entry(symbol.instance.unwrap())
                    .or_default()
                    .push(unit.header.instance);
            }
        }
    }
    let mut pending: Vec<_> = units
        .iter()
        .filter(|unit| !selected.contains(&unit.header.instance))
        .map(|unit| unit.header.instance)
        .collect();
    while let Some(callee) = pending.pop() {
        for caller in callers.get(&callee).into_iter().flatten() {
            if !recursive.contains(caller) && selected.remove(caller) {
                pending.push(*caller);
            }
        }
    }
    selected
}
