//! Reachable native instances are collected separately from one-pass source checking.
use super::*;
use hirsel_core_ir::{Callee, Inst, InstanceKey};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceStop {
    Resource {
        limit: usize,
        next: InstanceKey,
    },
    OutsideReached(DefPath),
    Input {
        definition: DefPath,
        message: String,
    },
    Incomplete(DefPath),
    /// Materialization of type/dictionary arguments belongs to the emitter. The
    /// current source checker emits only closed, non-generic function bodies.
    MaterializationRequired(InstanceKey),
}
/// The emitter supplies mechanical instance dependencies. In particular, a growing
/// f[T] -> f[List[T]] family stops as a compiler resource outcome, not a language rule.
pub fn collect_instances(
    roots: impl IntoIterator<Item = InstanceKey>,
    limit: usize,
    mut dependencies: impl FnMut(&InstanceKey) -> Result<Vec<InstanceKey>, InstanceStop>,
) -> Result<Vec<InstanceKey>, InstanceStop> {
    let mut pending: BTreeSet<_> = roots.into_iter().collect();
    let mut reached = BTreeSet::new();
    while let Some(key) = pending.pop_first() {
        if reached.contains(&key) {
            continue;
        }
        if reached.len() == limit {
            return Err(InstanceStop::Resource { limit, next: key });
        }
        let reads = dependencies(&key)?;
        reached.insert(key);
        pending.extend(reads);
    }
    Ok(reached.into_iter().collect())
}
/// Per-build instances retain their materialized modules for native emission.
/// Nongeneric bodies keep the already-published module by reference.
#[derive(Default)]
pub(crate) struct InstanceSet {
    rows: std::collections::BTreeMap<InstanceKey, usize>,
    bodies: Vec<InstanceBody>,
}
pub(crate) struct InstanceBody {
    pub product: Arc<DefinitionProduct>,
    module: Option<Module>,
}
impl InstanceBody {
    pub fn ir(&self) -> &Module {
        self.module.as_ref().unwrap_or(&self.product.ir)
    }
}
impl InstanceSet {
    pub fn get(&self, key: &InstanceKey) -> &InstanceBody {
        &self.bodies[self.rows[key]]
    }
}
impl CheckProduct {
    pub fn instances(
        &self,
        roots: impl IntoIterator<Item = InstanceKey>,
        limit: usize,
    ) -> Result<Vec<InstanceKey>, InstanceStop> {
        self.instances_into(roots, limit, &mut InstanceSet::default())
    }
    pub(crate) fn instances_into(
        &self,
        roots: impl IntoIterator<Item = InstanceKey>,
        limit: usize,
        instances: &mut InstanceSet,
    ) -> Result<Vec<InstanceKey>, InstanceStop> {
        // A concrete argument names a shape published by the caller that requested
        // it. Numeric indices keep these references independent of mutable storage.
        let mut shapes =
            std::collections::BTreeMap::<String, (usize, hirsel_core_layout::TypeRef)>::new();
        collect_instances(roots, limit, |key| {
            if !self.definitions.contains_key(&key.definition)
                && !self
                    .image
                    .as_ref()
                    .is_some_and(|i| i.directory.definitions.contains_key(&key.definition))
            {
                return Err(InstanceStop::OutsideReached(key.definition.clone()));
            }
            let product = self
                .demand_body(&key.definition)
                .map_err(|e| InstanceStop::Input {
                    definition: key.definition.clone(),
                    message: e.to_string(),
                })?;
            if !product.check.refusals.is_empty() || !product.incomplete.is_empty() {
                return Err(InstanceStop::Incomplete(key.definition.clone()));
            }
            let Some(mut function) = product.check.function else {
                return Err(InstanceStop::Incomplete(key.definition.clone()));
            };
            let module = if let Some(template) = &product.check.template {
                let arguments = key
                    .types
                    .iter()
                    .map(|name| {
                        let &(owner, root) =
                            shapes.get(name).ok_or_else(|| InstanceStop::Input {
                                definition: key.definition.clone(),
                                message: format!("unpublished instance type {name}"),
                            })?;
                        Ok((instances.bodies[owner].ir(), root))
                    })
                    .collect::<Result<Vec<_>, InstanceStop>>()?;
                let (ir, row) = hirsel_core_front::materialize(
                    &product.ir,
                    function,
                    template,
                    key,
                    &arguments,
                )
                .map_err(|message| InstanceStop::Input {
                    definition: key.definition.clone(),
                    message,
                })?;
                function = row;
                Some(ir)
            } else if !key.types.is_empty() || !key.dictionaries.is_empty() {
                return Err(InstanceStop::MaterializationRequired(key.clone()));
            } else {
                None
            };
            let row = instances.bodies.len();
            instances.rows.insert(key.clone(), row);
            instances.bodies.push(InstanceBody { product, module });
            let ir = instances.bodies[row].ir();
            for (i, shape) in ir.types.iter().enumerate() {
                shapes
                    .entry(shape.name.clone())
                    .or_insert((row, hirsel_core_layout::TypeRef(i as u32)));
            }
            let pending = external_functions(ir, function)
                .into_iter()
                .map(|f| ir.instances[ir.functions[f.index()].instance.index()].clone())
                .collect::<BTreeSet<_>>();
            Ok(pending.into_iter().collect())
        })
    }
}

/// Generated closure bodies share their producer's module; only signature-only rows
/// require another source product.
pub(crate) fn external_functions(
    module: &hirsel_core_ir::Module,
    root: hirsel_core_ir::FuncId,
) -> BTreeSet<hirsel_core_ir::FuncId> {
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    let mut external = BTreeSet::new();
    while let Some(function) = pending.pop() {
        if !visited.insert(function) {
            continue;
        }
        let Some(body) = &module.functions[function.index()].body else {
            external.insert(function);
            continue;
        };
        for call in &body.calls {
            if let Callee::Direct(f) = call.callee {
                pending.push(f);
            }
        }
        for inst in &body.insts {
            if let Inst::FunctionRef { function, .. } = inst {
                pending.push(*function);
            }
        }
    }
    external
}
