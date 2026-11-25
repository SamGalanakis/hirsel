use super::*;

/// The canonical encoding and its root's source annex, in canonical order.
#[cfg(any(test, feature = "snapshot"))]
pub(crate) fn encode_with_root_annex(
    module: &Module,
    root: FuncId,
    sources: &SourcePositions<'_>,
) -> (EncodedBody, FunctionAnnex) {
    let mut annex = None;
    let body = encode_with_orders(module, root, |order| {
        if annex.is_none() {
            annex = order.map(|order| function_annex(module, &order, sources));
        }
    });
    (body, annex.expect("the root is a definition"))
}

/// Prepare assembly from the canonical rows `retain` already decoded.
pub fn prepare_retained_assembly(
    retained: &Module,
    sources: &SourcePositions<'_>,
) -> (Annex, Module) {
    let annex = retained_annex(retained, sources);
    let mut module = retained.clone();
    module.items.clear();
    for body in module.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        body.inst_anchors.fill(None);
        for block in &mut body.blocks {
            block.anchor = None;
            block.decision = false;
        }
        for call in &mut body.calls {
            call.cold = false;
        }
        for edge in &mut body.edges {
            edge.cold = false;
        }
        body.callable_parameters.clear();
    }
    (annex, module)
}

/// Read a retained module's annex in its canonical row order.
pub fn retained_annex(retained: &Module, sources: &SourcePositions<'_>) -> Annex {
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
    Annex(functions, None, None)
}

/// Source anchors and optimization hints left out of a canonical body.
pub struct RetainedSites {
    items: Vec<DefPath>,
    functions: Vec<Option<SourceRows>>,
}

struct SourceRows {
    instructions: Vec<Option<Anchor>>,
    terminators: Vec<Option<Anchor>>,
    decisions: Vec<bool>,
    cold_calls: Vec<bool>,
    cold_edges: Vec<bool>,
    callable_parameters: Vec<ValueId>,
}

impl RetainedSites {
    /// Take the non-encoded fields of a canonical module.
    pub fn take(mut module: Module) -> Self {
        let functions = module
            .functions
            .iter_mut()
            .map(|function| {
                let body = function.body.take()?;
                Some(SourceRows {
                    instructions: body.inst_anchors,
                    terminators: body.blocks.iter().map(|block| block.anchor).collect(),
                    decisions: body.blocks.iter().map(|block| block.decision).collect(),
                    cold_calls: body.calls.iter().map(|call| call.cold).collect(),
                    cold_edges: body.edges.iter().map(|edge| edge.cold).collect(),
                    callable_parameters: body.callable_parameters,
                })
            })
            .collect();
        Self {
            items: module.items,
            functions,
        }
    }

    pub fn items(&self) -> &[DefPath] {
        &self.items
    }

    pub fn anchored_items(&self) -> Vec<DefPath> {
        let anchors = self
            .functions
            .iter()
            .flatten()
            .flat_map(|rows| rows.instructions.iter().chain(&rows.terminators).flatten());
        crate::source::named_items(&self.items, anchors)
    }

    pub fn annex(&self, sources: &SourcePositions<'_>) -> Annex {
        let site = |&anchor| resolve_site(&self.items, sources, anchor);
        let functions = self
            .functions
            .iter()
            .map(|rows| {
                let rows = rows.as_ref()?;
                Some(decided(FunctionAnnex {
                    instructions: rows.instructions.iter().map(site).collect(),
                    terminators: rows.terminators.iter().map(site).collect(),
                    decisions: rows.decisions.clone(),
                    cold_calls: rows.cold_calls.clone(),
                    cold_edges: rows.cold_edges.clone(),
                    callable_parameters: rows
                        .callable_parameters
                        .iter()
                        .map(|parameter| parameter.0)
                        .collect(),
                }))
            })
            .collect();
        Annex(functions, None, None)
    }

    /// Recover the retained module for consumers that need more than its sites.
    pub fn restore(&self, bytes: &[u8]) -> Module {
        let mut module = assembly_module(bytes);
        module.items.clone_from(&self.items);
        for (function, rows) in module.functions.iter_mut().zip(&self.functions) {
            let (Some(body), Some(rows)) = (&mut function.body, rows) else {
                continue;
            };
            body.inst_anchors.clone_from(&rows.instructions);
            for ((block, &anchor), &decision) in body
                .blocks
                .iter_mut()
                .zip(&rows.terminators)
                .zip(&rows.decisions)
            {
                block.anchor = anchor;
                block.decision = decision;
            }
            for (call, &cold) in body.calls.iter_mut().zip(&rows.cold_calls) {
                call.cold = cold;
            }
            for (edge, &cold) in body.edges.iter_mut().zip(&rows.cold_edges) {
                edge.cold = cold;
            }
            body.callable_parameters
                .clone_from(&rows.callable_parameters);
        }
        module
    }
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
            block.decision = old.decision;
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

/// Decode our own normal body bytes for assembly.
pub fn assembly_module(bytes: &[u8]) -> Module {
    decode_payload(bytes, &Debit::unlimited())
        .expect("the producer's normal form is readable")
        .0
}

pub(super) fn resolve_site(
    items: &[DefPath],
    sources: &SourcePositions<'_>,
    anchor: Option<Anchor>,
) -> Option<Site> {
    let anchor = anchor?;
    let (file, line, column) = sources.annex_position(&items[anchor.item.index()], anchor.start)?;
    Some(Site { file, line, column })
}

/// `annex` with a decision only where its terminator's site resolved. A
/// decision is a coverage site and a site needs a position, so one whose
/// anchor's item is not in the build's sources is none: the front drops a
/// decision without an anchor, and `Annex::decode` refuses one without a site.
pub(super) fn decided(mut annex: FunctionAnnex) -> FunctionAnnex {
    for (decision, site) in annex.decisions.iter_mut().zip(&annex.terminators) {
        *decision &= site.is_some();
    }
    annex
}
