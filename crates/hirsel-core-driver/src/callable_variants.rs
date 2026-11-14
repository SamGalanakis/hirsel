//! Native-only code variants. The checked instance and function-value identities
//! remain type keyed; a private symbol binds only immutable callable code words.
use crate::instances::InstanceSet;
use crate::{ContentHash, DefPath, InstanceStop, NativeStop};
use hirsel_core_ir::{Callee, FuncId, Function, Inst, InstanceKey, Module, SigId};
use serde::Serialize;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

mod origins;

#[derive(Clone, Copy, Debug)]
struct Target {
    unit: usize,
    function: FuncId,
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
struct Known {
    instance: InstanceKey,
    // A generated closure belongs to its checked source product, not to an
    // independently demanded definition. Edits of that product change this key.
    owner: ContentHash,
}
#[derive(Serialize)]
struct Variant<'a> {
    base: &'a InstanceKey,
    bindings: &'a BTreeMap<u32, Known>,
}
pub(crate) fn prepare<'m>(
    modules: &mut Vec<Cow<'m, Module>>,
    imported: &mut Vec<bool>,
    reached: &[InstanceKey],
    materialized: &InstanceSet,
    entry: &DefPath,
    limit: usize,
) -> Result<Vec<ContentHash>, NativeStop> {
    let mut originals = BTreeMap::new();
    let mut eligible = BTreeSet::new();
    let mut owners = Vec::with_capacity(modules.len());
    for (unit, (m, instance)) in modules.iter().zip(reached).enumerate() {
        let product = &materialized.get(instance).product;
        owners.push(crate::native::key(
            "hirsel.callable-owner.v1",
            &product.key,
        )?);
        if product.check.template.is_some() {
            eligible.insert(instance.clone());
        }
        for (i, f) in m.functions.iter().enumerate() {
            if f.body.is_some() {
                originals.insert(
                    m.instances[f.instance.index()].clone(),
                    Target {
                        unit,
                        function: FuncId(i as u32),
                    },
                );
            }
        }
    }
    let root = *originals
        .iter()
        .find(|(k, _)| &k.definition == entry)
        .unwrap()
        .1;
    // The collector owns modules only during this call. Cow's source lifetime
    // remains independent of that mutable borrow.
    let body_identities = owners.clone();
    collect(
        modules,
        imported,
        originals,
        owners,
        eligible,
        root,
        reached.len(),
        limit,
    )?;
    Ok(body_identities)
}

#[allow(clippy::too_many_arguments)]
fn collect<'m>(
    modules: &mut Vec<Cow<'m, Module>>,
    imported: &mut Vec<bool>,
    originals: BTreeMap<InstanceKey, Target>,
    owners: Vec<ContentHash>,
    eligible: BTreeSet<InstanceKey>,
    root: Target,
    semantic_count: usize,
    limit: usize,
) -> Result<(), NativeStop> {
    // Kept separately from the borrowed checked products: no source work and no
    // private row is ever published to a library image.
    let mut c = Work {
        modules,
        imported,
        originals,
        owners,
        eligible,
        variants: BTreeMap::new(),
        pending: vec![(root, BTreeMap::new())],
        live: BTreeSet::new(),
        semantic_count,
        limit,
    };
    while let Some((target, bindings)) = c.pending.pop() {
        let m = &c.modules[target.unit];
        let identity = m.code_identity(target.function);
        if !c.live.insert(identity) {
            continue;
        }
        c.visit(target, &bindings)?;
    }
    // An original is retained exactly when an unknown call or a function value
    // still reaches it. In particular, known-only sort graphs don't emit both
    // their shared and private copies.
    for m in c.modules.iter_mut() {
        let dead = m
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                (f.body.is_some() && !c.live.contains(&m.code_identity(FuncId(i as u32))))
                    .then_some(i)
            })
            .collect::<Vec<_>>();
        if !dead.is_empty() {
            for i in dead {
                m.to_mut().functions[i].body = None;
            }
        }
    }
    Ok(())
}
// Separate lifetimes allow the original Cow modules to outlive collection.
struct Work<'a, 'm> {
    modules: &'a mut Vec<Cow<'m, Module>>,
    imported: &'a mut Vec<bool>,
    originals: BTreeMap<InstanceKey, Target>,
    owners: Vec<ContentHash>,
    eligible: BTreeSet<InstanceKey>,
    variants: BTreeMap<ContentHash, Target>,
    pending: Vec<(Target, BTreeMap<u32, Known>)>,
    live: BTreeSet<ContentHash>,
    semantic_count: usize,
    limit: usize,
}
impl Work<'_, '_> {
    fn instance(&self, t: Target) -> &InstanceKey {
        let m = &self.modules[t.unit];
        &m.instances[m.functions[t.function.index()].instance.index()]
    }
    fn known(&self, unit: usize, f: FuncId) -> Option<Known> {
        let m = &self.modules[unit];
        let instance = &m.instances[m.functions[f.index()].instance.index()];
        let t = self.originals.get(instance)?;
        Some(Known {
            instance: instance.clone(),
            owner: self.owners[t.unit],
        })
    }
    fn request(
        &mut self,
        base: Target,
        bindings: BTreeMap<u32, Known>,
    ) -> Result<Target, NativeStop> {
        let instance = self.instance(base);
        let hash = crate::native::key(
            "hirsel.callable-code.v1",
            &Variant {
                base: instance,
                bindings: &bindings,
            },
        )?;
        if let Some(t) = self.variants.get(&hash) {
            return Ok(*t);
        }
        if self.semantic_count + self.variants.len() >= self.limit {
            return Err(NativeStop::Instances(InstanceStop::Resource {
                limit: self.limit,
                next: instance.clone(),
            }));
        }
        let mut module = self.modules[base.unit].clone().into_owned();
        let root = module.functions[base.function.index()].clone();
        for f in &mut module.functions {
            f.body = None;
        }
        // The original row still denotes the semantic function value, including
        // references to self. Only direct calls use the additional code row.
        let function = FuncId(module.functions.len() as u32);
        module.functions.push(root);
        module.code_variants.insert(function, hash);
        let target = Target {
            unit: self.modules.len(),
            function,
        };
        self.modules.push(Cow::Owned(module));
        self.imported.push(false);
        self.owners.push(self.owners[base.unit]);
        self.variants.insert(hash, target);
        self.pending.push((target, bindings));
        Ok(target)
    }
    fn import(&mut self, unit: usize, target: Target, signature: SigId) -> FuncId {
        let identity = self.modules[target.unit].code_identity(target.function);
        let instance = self.instance(target).clone();
        if let Some(i) = self.modules[unit]
            .functions
            .iter()
            .enumerate()
            .find_map(|(i, _)| {
                (self.modules[unit].code_identity(FuncId(i as u32)) == identity).then_some(i)
            })
        {
            return FuncId(i as u32);
        }
        let m = self.modules[unit].to_mut();
        let row = m
            .instances
            .iter()
            .position(|k| *k == instance)
            .unwrap_or_else(|| {
                m.instances.push(instance);
                m.instances.len() - 1
            });
        let function = FuncId(m.functions.len() as u32);
        m.functions.push(Function {
            instance: hirsel_core_ir::InstanceId(row as u32),
            signature,
            body: None,
        });
        // Imported signatures use the call site's already-closed type table.
        // TypeIds from another unit must never be copied into this one.
        if identity != m.instances[row].digest() {
            m.code_variants.insert(function, identity);
        }
        function
    }
    fn visit(&mut self, target: Target, bindings: &BTreeMap<u32, Known>) -> Result<(), NativeStop> {
        let unit = target.unit;
        let m = &self.modules[unit];
        let body = m.functions[target.function.index()].body.as_ref().unwrap();
        let needed = !bindings.is_empty()
            || body.calls.iter().any(|call| match call.callee {
                Callee::Direct(f) => self
                    .eligible
                    .contains(&m.instances[m.functions[f.index()].instance.index()]),
                _ => false,
            });
        let mut edits = Vec::new();
        if needed {
            let mut origins = origins::Origins::new(m, body);
            for (i, call) in body.calls.iter().enumerate() {
                match call.callee {
                    Callee::Indirect(v) => {
                        let known = match origins.value(v) {
                            Some(origins::Origin::Parameter(p)) => bindings.get(&p).cloned(),
                            Some(origins::Origin::Function(f)) => self.known(unit, f),
                            None => None,
                        };
                        if let Some(k) = known {
                            edits.push((i, self.originals[&k.instance], None));
                        }
                    }
                    Callee::Direct(f) => {
                        let key = &m.instances[m.functions[f.index()].instance.index()];
                        if !self.eligible.contains(key) {
                            continue;
                        }
                        let base = self.originals[key];
                        let b = self.modules[base.unit].functions[base.function.index()]
                            .body
                            .as_ref()
                            .unwrap();
                        let params = b.blocks[0].params.get(&b.pools.values).unwrap();
                        let mut passed = BTreeMap::new();
                        for v in &b.callable_parameters {
                            let p = params.iter().position(|x| x == v).unwrap() as u32;
                            let arg = call.args.get(&body.pools.args).unwrap()[p as usize];
                            let known = match origins.argument(arg) {
                                Some(origins::Origin::Function(f)) => self.known(unit, f),
                                Some(origins::Origin::Parameter(p)) => bindings.get(&p).cloned(),
                                None => None,
                            };
                            if let Some(k) = known {
                                passed.insert(p, k);
                            }
                        }
                        if !passed.is_empty() {
                            edits.push((i, base, Some(passed)));
                        }
                    }
                    _ => {}
                }
            }
        }
        for (i, base, passed) in edits {
            let to = match passed {
                Some(p) => self.request(base, p)?,
                None => base,
            };
            let sig = self.modules[unit].functions[target.function.index()]
                .body
                .as_ref()
                .unwrap()
                .calls[i]
                .signature;
            let f = self.import(unit, to, sig);
            self.modules[unit].to_mut().functions[target.function.index()]
                .body
                .as_mut()
                .unwrap()
                .calls[i]
                .callee = Callee::Direct(f);
            // A shared library function rewritten to call a private body is also
            // request owned, never a published object with private references.
            self.imported[unit] = false;
        }
        let m = &self.modules[unit];
        let body = m.functions[target.function.index()].body.as_ref().unwrap();
        let mut dependencies = body
            .calls
            .iter()
            .filter_map(|c| match c.callee {
                Callee::Direct(f) => Some(f),
                _ => None,
            })
            .collect::<Vec<_>>();
        dependencies.extend(body.insts.iter().filter_map(|i| match i {
            Inst::FunctionRef { function, .. } => Some(*function),
            _ => None,
        }));
        dependencies.extend(
            m.types
                .iter()
                .filter_map(|s| s.finalizer.map(|f| FuncId(f.0))),
        );
        for f in dependencies {
            if m.code_variants.contains_key(&f) {
                continue;
            }
            let t = self.originals[&m.instances[m.functions[f.index()].instance.index()]];
            self.pending.push((t, BTreeMap::new()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::text;

    fn graph() -> Module {
        text::parse(
            r#"core v1
sig g0 = () -> (u64)
sig g1 = (ptr, u64) -> (u64)
sig g2 = (ptr, u64) -> (u64)
inst n0 = app@1/main/root [] []
inst n1 = lib@1/main/forward ["u64"] []
inst n2 = lib@1/main/recurse ["u64"] []
inst n3 = app@1/main/callback [] []
fn f0 = n0 g0 {
b0:
 v0: ptr = fnref f3
 v1: u64 = const 3
 v2: u64 = call f1 g1 (v0, v1)
 return v2
}
fn f1 = n1 g1 {
 callable v0
b0(v0: ptr, v1: u64):
 v2: u64 = call f2 g1 (v0, v1)
 return v2
}
fn f2 = n2 g1 {
 callable v0
b0(v0: ptr, v1: u64):
 v2: u64 = const 0
 v3: bool = eq v1, v2
 branch v3, b1, b2
b1:
 v4: ptr = const 0x0
 v5: u64 = const 41
 v6: u64 = call v0 g2 (v4, v5)
 return v6
b2:
 v7: u64 = const 1
 v8: u64 = sub v1, v7
 v9: u64 = call f1 g1 (v0, v8)
 return v9
}
fn f3 = n3 g2 {
b0(v0: ptr, v1: u64):
 v2: u64 = const 1
 v3: u64 = add v1, v2
 return v3
}
"#,
        )
        .unwrap()
    }
    fn run(m: &Module, limit: usize) -> Result<Vec<Cow<'_, Module>>, NativeStop> {
        let mut units = vec![Cow::Borrowed(m)];
        let mut imported = vec![true];
        let originals = m
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    m.instances[f.instance.index()].clone(),
                    Target {
                        unit: 0,
                        function: FuncId(i as u32),
                    },
                )
            })
            .collect();
        let eligible = [m.instances[1].clone(), m.instances[2].clone()]
            .into_iter()
            .collect();
        collect(
            &mut units,
            &mut imported,
            originals,
            vec![ContentHash::default()],
            eligible,
            Target {
                unit: 0,
                function: FuncId(0),
            },
            4,
            limit,
        )?;
        assert!(
            imported.iter().all(|i| !i),
            "rewritten library and private units belong to caller"
        );
        Ok(units)
    }
    #[test]
    fn private_forwarding_reuses_recursive_variants_without_changing_semantic_keys() {
        let m = graph();
        assert_eq!(text::parse(&text::print(&m)).unwrap(), m);
        let units = run(&m, 6).unwrap();
        assert_eq!(
            units.len(),
            3,
            "two private recursive bodies, not an unbounded family"
        );
        assert!(units[0].functions[1].body.is_none());
        assert!(units[0].functions[2].body.is_none());
        for u in &units {
            assert_eq!(u.instances, m.instances);
            assert_eq!(text::parse(&text::print(u)).unwrap(), **u);
        }
        let private_calls = units
            .iter()
            .skip(1)
            .flat_map(|m| m.functions.iter().filter_map(|f| f.body.as_ref()))
            .flat_map(|b| &b.calls)
            .collect::<Vec<_>>();
        assert!(
            private_calls
                .iter()
                .all(|c| matches!(c.callee, Callee::Direct(_)))
        );
        assert_ne!(units[1].code_identity(FuncId(4)), m.instances[1].digest());
        // The original row continues to denote the unspecialised function value.
        assert_eq!(units[1].code_identity(FuncId(1)), m.instances[1].digest());
    }
    #[test]
    fn private_growth_spends_the_existing_instance_limit() {
        let m = graph();
        assert!(matches!(
            run(&m, 5),
            Err(NativeStop::Instances(InstanceStop::Resource {
                limit: 5,
                ..
            }))
        ));
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use crate::{Driver, Meter};
    #[test]
    fn scoped_source_forwarding_binds_code_without_binding_capture_values() {
        let dir = tempfile::tempdir().unwrap();
        let product = Driver::default()
            .check(
                crate::tests::request(
                    r#"
fn apply[T: plain](x: T, f: scoped fn(T) -> T) -> T { f(x) }
fn forward[T: plain](x: T, f: scoped fn(T) -> T) -> T { apply(x, f) }
fn root() -> u64 { let delta:u64=1; forward(41u64, fn(x:u64)->u64 {x+delta}) }
"#,
                    dir.path(),
                ),
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        let entry = DefPath::from_full("app@1/main/root").unwrap();
        let reached = product.reachable(&entry, 64).unwrap();
        let keys = reached.require().unwrap();
        let mut modules = keys
            .iter()
            .map(|k| Cow::Borrowed(reached.materialized.get(k).ir()))
            .collect();
        let mut imported = vec![false; keys.len()];
        prepare(
            &mut modules,
            &mut imported,
            keys,
            &reached.materialized,
            &entry,
            64,
        )
        .unwrap();
        assert_eq!(
            modules.len(),
            keys.len() + 2,
            "{}",
            modules
                .iter()
                .map(|m| hirsel_core_ir::text::print(m))
                .collect::<String>()
        );
    }
    #[test]
    fn known_source_callables_get_private_bodies_unknown_parameters_keep_the_shared_body() {
        let dir = tempfile::tempdir().unwrap();
        let product = Driver::default()
            .check(
                crate::tests::request(
                    r#"
fn apply[T: plain](x: T, f: fn(T) -> T) -> T { f(x) }
fn forward[T: plain](x: T, f: fn(T) -> T) -> T { apply(x, f) }
fn inc(x: u64) -> u64 { x + 1 }
fn root() -> u64 { forward(41u64, inc) }
fn unknown(f: fn(u64) -> u64) -> u64 { forward(41u64, f) }
"#,
                    dir.path(),
                ),
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        for (entry, expected) in [("root", 2), ("unknown", 0)] {
            let entry = DefPath::from_full(&format!("app@1/main/{entry}")).unwrap();
            let reached = product.reachable(&entry, 64).unwrap();
            let keys = reached.require().unwrap();
            let mut modules = keys
                .iter()
                .map(|k| Cow::Borrowed(reached.materialized.get(k).ir()))
                .collect();
            let mut imported = vec![false; keys.len()];
            prepare(
                &mut modules,
                &mut imported,
                keys,
                &reached.materialized,
                &entry,
                64,
            )
            .unwrap();
            assert_eq!(
                modules
                    .iter()
                    .flat_map(|m| m
                        .functions
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| f.body.is_some())
                        .filter_map(|(i, _)| m.code_variants.get(&FuncId(i as u32))))
                    .count(),
                expected
            );
            assert_eq!(keys.len(), reached.instances.as_ref().unwrap().len());
        }
    }
}
