//! Reachable native instances are collected separately from one-pass source checking.
//! Each is named by its [`Identity`] and built by `hirsel_core_materialize` from its
//! arguments' closures, which the walk's [`Resolver`] finds by hash.
use super::*;
use hirsel_core_ir::Identity;
use hirsel_core_materialize::{Child, Published, Resolver};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceStop {
    Resource {
        limit: usize,
        next: Identity,
    },
    OutsideReached(DefPath),
    Input {
        definition: DefPath,
        message: String,
    },
    Incomplete(DefPath),
}
/// The emitter supplies mechanical instance dependencies. In particular, a growing
/// f[T] -> f[List[T]] family stops as a compiler resource outcome, not a language rule.
pub fn collect_instances(
    roots: impl IntoIterator<Item = Identity>,
    limit: usize,
    mut dependencies: impl FnMut(&Identity) -> Result<Vec<Identity>, InstanceStop>,
) -> Result<Vec<Identity>, InstanceStop> {
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
    rows: std::collections::BTreeMap<Identity, usize>,
    bodies: Vec<InstanceBody>,
}
pub(crate) struct InstanceBody {
    /// The published definition; None for a glue instance.
    pub product: Option<Arc<DefinitionProduct>>,
    module: Option<Module>,
    pub function: hirsel_core_ir::FuncId,
    /// The module's signature-only rows the walk reached, by ascending row, with
    /// the instance each names.
    externals: Vec<(hirsel_core_ir::FuncId, Identity)>,
}
impl InstanceBody {
    pub fn ir(&self) -> &Module {
        match (&self.module, &self.product) {
            (Some(module), _) => module,
            (None, Some(product)) => product.ir(),
            (None, None) => unreachable!("a glue instance is materialised"),
        }
    }
    /// A published body's module is hashed in its record; a materialized
    /// one is hashed here, by the same rule, through the caller's `scratch`.
    pub fn ir_hash(&self, scratch: &mut Vec<u8>) -> io::Result<ContentHash> {
        match (&self.module, &self.product) {
            (Some(module), _) => crate::body::ir_hash(module, scratch),
            (None, Some(product)) => Ok(product.ir_hash()),
            (None, None) => unreachable!("a glue instance is materialised"),
        }
    }
}
impl InstanceSet {
    pub fn get(&self, key: &Identity) -> &InstanceBody {
        &self.bodies[self.rows[key]]
    }
    /// `key`'s module's [`Module::link_symbols`]. Its root and every external row
    /// the walk reached name an instance of this set, whose symbol `names` holds
    /// once per set row, so no row's closure is taken again; a callable code
    /// variant names its variant, and any other row is named by its module.
    pub fn link_symbols(&self, key: &Identity, names: &mut Vec<Option<String>>) -> Vec<String> {
        names.resize(self.bodies.len(), None);
        let mut name = |identity: &Identity| -> String {
            match self.rows.get(identity) {
                Some(&row) => names[row].get_or_insert_with(|| identity.symbol()).clone(),
                None => identity.symbol(),
            }
        };
        let body = self.get(key);
        let module = body.ir();
        (0..module.functions.len() as u32)
            .map(hirsel_core_ir::FuncId)
            .map(|f| {
                if let Some(&code) = module.code_variants.get(&f) {
                    hirsel_core_ir::symbol(code)
                } else if f == body.function {
                    name(key)
                } else if let Ok(i) = body.externals.binary_search_by_key(&f, |(g, _)| *g) {
                    name(&body.externals[i].1)
                } else {
                    hirsel_core_ir::symbol(module.code_identity(f))
                }
            })
            .collect()
    }
    /// The materialised instance of `definition` whose type arguments' row names are
    /// `arguments`, for tests that name an instance by its source text.
    #[cfg(test)]
    pub fn named(&self, definition: &DefPath, arguments: &[&str]) -> Option<Identity> {
        self.rows.iter().find_map(|(identity, &row)| {
            let body = &self.bodies[row];
            let module = body.ir();
            let key = &module.instances[module.functions[body.function.index()].instance.index()];
            (&identity.definition == definition && module.argument_names(key) == arguments)
                .then(|| identity.clone())
        })
    }
}
impl CheckProduct {
    pub fn instances(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
    ) -> Result<Vec<Identity>, InstanceStop> {
        self.instances_into(roots, limit, &mut InstanceSet::default())
    }
    pub(crate) fn instances_into(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
        instances: &mut InstanceSet,
    ) -> Result<Vec<Identity>, InstanceStop> {
        self.instances_observed(roots, limit, instances, |_, _| {})
    }
    /// Measure library loading and mechanical specialization without changing the
    /// native collection path. The ordinary path erases the no-op observer.
    pub fn observe_instances(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
        observe: impl FnMut(&Identity, &'static str),
    ) -> Result<Vec<Identity>, InstanceStop> {
        self.instances_observed(roots, limit, &mut InstanceSet::default(), observe)
    }
    fn instances_observed(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
        instances: &mut InstanceSet,
        mut observe: impl FnMut(&Identity, &'static str),
    ) -> Result<Vec<Identity>, InstanceStop> {
        let mut resolver = Resolver::default();
        collect_instances(roots, limit, |key| {
            observe(key, "start");
            let input = |message: String| InstanceStop::Input {
                definition: key.definition.clone(),
                message,
            };
            let arguments = resolver.arguments(key).map_err(|hash| {
                input(format!(
                    "no closure {} reached this instance",
                    hirsel_core_ir::symbol(hash)
                ))
            })?;
            let (product, built) = if hirsel_core_front::is_glue(&key.definition) {
                observe(key, "load");
                let built =
                    hirsel_core_materialize::materialize_glue(key, &arguments).map_err(input)?;
                (None, built)
            } else {
                let installed = self
                    .image
                    .as_ref()
                    .map(|i| i.has_body(&key.definition))
                    .transpose()
                    .map_err(|e| input(e.to_string()))?
                    .unwrap_or(false);
                if !self.definitions.contains_key(&key.definition) && !installed {
                    return Err(InstanceStop::OutsideReached(key.definition.clone()));
                }
                let product = self
                    .demand_body(&key.definition)
                    .map_err(|e| input(e.to_string()))?;
                if !product.check.refusals.is_empty() || !product.incomplete.is_empty() {
                    return Err(InstanceStop::Incomplete(key.definition.clone()));
                }
                let Some(function) = product.check.function else {
                    return Err(InstanceStop::Incomplete(key.definition.clone()));
                };
                observe(key, "load");
                let own = Published {
                    module: product.ir(),
                    function,
                    template: product.check.template.as_ref(),
                };
                let built =
                    hirsel_core_materialize::materialize(&own, key, &arguments).map_err(input)?;
                (Some(product), built)
            };
            observe(key, "materialize");
            resolver.add(&built.children);
            let externals: Vec<_> = built
                .children
                .into_iter()
                .map(
                    |Child {
                         function, identity, ..
                     }| (function, identity),
                )
                .collect();
            let pending = externals
                .iter()
                .map(|(_, identity)| identity.clone())
                .collect::<BTreeSet<_>>();
            let row = instances.bodies.len();
            instances.rows.insert(key.clone(), row);
            instances.bodies.push(InstanceBody {
                product,
                module: built.module,
                function: built.function,
                externals,
            });
            observe(key, "dependencies");
            Ok(pending.into_iter().collect())
        })
    }
}
