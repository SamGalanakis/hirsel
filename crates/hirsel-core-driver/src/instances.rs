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
    IndirectTarget(DefPath),
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
impl CheckProduct {
    pub fn instances(
        &self,
        roots: impl IntoIterator<Item = InstanceKey>,
        limit: usize,
    ) -> Result<Vec<InstanceKey>, InstanceStop> {
        collect_instances(roots, limit, |key| {
            if !key.types.is_empty() || !key.dictionaries.is_empty() {
                return Err(InstanceStop::MaterializationRequired(key.clone()));
            }
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
            let Some(function) = product.check.function else {
                return Err(InstanceStop::Incomplete(key.definition.clone()));
            };
            let body = product.ir.functions[function.index()]
                .body
                .as_ref()
                .expect("successful CoreCheck has its emitted body");
            let mut pending = BTreeSet::new();
            for call in &body.calls {
                match call.callee {
                    Callee::Direct(f) => {
                        pending.insert(
                            product.ir.instances[product.ir.functions[f.index()].instance.index()]
                                .clone(),
                        );
                    }
                    Callee::Runtime(_) => {}
                    Callee::Indirect(_) => {
                        return Err(InstanceStop::IndirectTarget(key.definition.clone()));
                    }
                }
            }
            for inst in &body.insts {
                if let Inst::FunctionRef { function, .. } = inst {
                    pending.insert(
                        product.ir.instances
                            [product.ir.functions[function.index()].instance.index()]
                        .clone(),
                    );
                }
            }
            Ok(pending.into_iter().collect())
        })
    }
}
