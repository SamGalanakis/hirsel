use super::*;

/// Prepare assembly from the canonical rows `retain` already decoded.
pub fn prepare_retained_assembly(
    retained: &Module,
    sources: &SourcePositions<'_>,
) -> (Annex, Module) {
    let functions = retained
        .functions
        .iter()
        .map(|function| {
            let body = function.body.as_ref()?;
            Some(annex_in_order(
                retained,
                body,
                0..body.blocks.len(),
                |value| value as u32,
                0..body.calls.len(),
                0..body.edges.len(),
                sources,
            ))
        })
        .collect();
    let mut module = retained.clone();
    module.items.clear();
    for body in module.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        body.inst_anchors.fill(None);
        for block in &mut body.blocks {
            block.anchor = None;
        }
        for call in &mut body.calls {
            call.cold = false;
        }
        for edge in &mut body.edges {
            edge.cold = false;
        }
        body.callable_parameters.clear();
    }
    (Annex(functions, None), module)
}

/// Materialize the retained module for existing native and evaluator consumers.
/// Source anchors and hints keep their original facts in the canonical numbering.
pub fn retain(module: &Module, root: FuncId) -> (EncodedBody, Module) {
    let mut orders = Vec::new();
    let mut code = encode_with_orders(module, root, |order| orders.push(order));
    let mut retained = decode_payload(&code.bytes, &Debit::unlimited())
        .expect("our own normal body decodes")
        .0;
    retained.items.clone_from(&module.items);
    let mut functions = Numbers::default();
    for (&row, (function, order)) in code
        .original_functions
        .iter()
        .zip(retained.functions.iter_mut().zip(orders))
    {
        functions.number(row);
        let (Some(body), Some(order)) = (&mut function.body, order) else {
            continue;
        };
        let original = order.body;
        for (block, &old) in body.blocks.iter_mut().zip(&order.blocks.order) {
            let old = &original.blocks[old];
            block.anchor = old.anchor;
            let source = PoolRange::new(old.insts.start, old.insts.len);
            let target = block.insts.start as usize;
            let source = source.get(&original.inst_anchors).unwrap();
            body.inst_anchors[target..target + source.len()].copy_from_slice(source);
        }
        for (call, &old) in body.calls.iter_mut().zip(&order.calls.numbers.order) {
            call.cold = original.calls[old].cold;
        }
        for (edge, &old) in body.edges.iter_mut().zip(&order.edges.numbers.order) {
            edge.cold = original.edges[old].cold;
        }
        body.callable_parameters = original
            .callable_parameters
            .iter()
            .map(|value| ValueId(order.values.get(value.index())))
            .collect();
        body.callable_parameters.sort_unstable();
        body.callable_parameters.dedup();
    }
    for slot in &mut code.slots {
        if let Slot::External(f) = slot {
            *f = FuncId(functions.get(f.index()));
        }
    }
    (code, retained)
}
