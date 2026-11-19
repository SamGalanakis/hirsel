//! Native-only code variants. The checked instance and function-value identities
//! remain type keyed; a private symbol binds only immutable callable code words.
use crate::instances::InstanceSet;
use crate::{ContentHash, DefPath, InstanceStop, NativeStop};
use hirsel_core_ir::{Callee, FuncId, Function, Identity, Inst, Module, SigId};
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
    instance: Identity,
    // A generated closure belongs to its checked source product, not to an
    // independently demanded definition. Edits of that product change this key.
    owner: ContentHash,
    /// The code identity of the body: a unit's root's is its instance's, and a
    /// body a unit carries has its own.
    code: ContentHash,
}
#[derive(Serialize)]
struct Variant<'a> {
    base: &'a Identity,
    bindings: &'a BTreeMap<u32, Known>,
}
pub(crate) fn prepare<'m>(
    modules: &mut Vec<Cow<'m, Module>>,
    reached: &[Identity],
    materialized: &InstanceSet,
    entry: &DefPath,
    limit: usize,
) -> Result<Vec<ContentHash>, NativeStop> {
    let mut originals = BTreeMap::new();
    let mut bodies = BTreeMap::new();
    let mut eligible = BTreeSet::new();
    let mut owners = Vec::with_capacity(modules.len());
    for (unit, (m, instance)) in modules.iter().zip(reached).enumerate() {
        let body = materialized.get(instance);
        let product = body.product.as_ref();
        owners.push(crate::native::key(
            "hirsel.callable-owner.v1",
            &product.map(|p| &p.key),
        )?);
        if product.is_some_and(|p| p.check.template.is_some()) {
            eligible.insert(instance.clone());
        }
        // A unit's root is its instance; every other body is the unit's own copy,
        // found by its private code identity.
        for (i, f) in m.functions.iter().enumerate() {
            let function = FuncId(i as u32);
            if function == body.function {
                originals.insert(m.identity(f.instance), Target { unit, function });
            }
            if f.body.is_some() {
                bodies.insert(m.code_identity(function), Target { unit, function });
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
        originals,
        bodies,
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
    originals: BTreeMap<Identity, Target>,
    bodies: BTreeMap<ContentHash, Target>,
    owners: Vec<ContentHash>,
    eligible: BTreeSet<Identity>,
    root: Target,
    semantic_count: usize,
    limit: usize,
) -> Result<(), NativeStop> {
    // Kept separately from the borrowed checked products: no source work and no
    // private row is ever published to a library image.
    let mut c = Work {
        modules,
        originals,
        bodies,
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
    originals: BTreeMap<Identity, Target>,
    /// Every body of every unit, by code identity.
    bodies: BTreeMap<ContentHash, Target>,
    owners: Vec<ContentHash>,
    eligible: BTreeSet<Identity>,
    variants: BTreeMap<ContentHash, Target>,
    pending: Vec<(Target, BTreeMap<u32, Known>)>,
    live: BTreeSet<ContentHash>,
    semantic_count: usize,
    limit: usize,
}
impl Work<'_, '_> {
    /// The semantic instance of `f` in `unit`.
    fn identity(&self, unit: usize, f: FuncId) -> Identity {
        let m = &self.modules[unit];
        m.identity(m.functions[f.index()].instance)
    }
    fn instance(&self, t: Target) -> Identity {
        self.identity(t.unit, t.function)
    }
    fn known(&self, unit: usize, f: FuncId) -> Option<Known> {
        let code = self.modules[unit].code_identity(f);
        let t = self.bodies.get(&code)?;
        Some(Known {
            owner: self.owners[t.unit],
            instance: self.identity(unit, f),
            code,
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
                base: &instance,
                bindings: &bindings,
            },
        )?;
        if let Some(t) = self.variants.get(&hash) {
            return Ok(*t);
        }
        if self.semantic_count + self.variants.len() >= self.limit {
            return Err(NativeStop::Instances(InstanceStop::Resource {
                limit: self.limit,
                next: instance,
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
        self.owners.push(self.owners[base.unit]);
        self.variants.insert(hash, target);
        self.pending.push((target, bindings));
        Ok(target)
    }
    fn import(
        &mut self,
        unit: usize,
        target: Target,
        signature: SigId,
    ) -> Result<FuncId, NativeStop> {
        let identity = self.modules[target.unit].code_identity(target.function);
        if let Some(i) = (0..self.modules[unit].functions.len())
            .find(|&i| self.modules[unit].code_identity(FuncId(i as u32)) == identity)
        {
            return Ok(FuncId(i as u32));
        }
        // The instance's type rows are imported from its own unit by closure: a
        // row of another unit is numbered otherwise. Imported signatures use the
        // call site's already-closed type table.
        let source = &self.modules[target.unit];
        let instance = source.identity(source.functions[target.function.index()].instance);
        let key = &source.instances[source.functions[target.function.index()].instance.index()];
        let closures: Vec<_> = key.arguments.iter().map(|&t| source.closure(t)).collect();
        let closures: Vec<_> = closures.iter().collect();
        let m = self.modules[unit].to_mut();
        let row =
            hirsel_core_front::import_instance(m, &instance, &closures).map_err(|message| {
                NativeStop::Emit {
                    definition: instance.definition.clone(),
                    message,
                }
            })?;
        let function = FuncId(m.functions.len() as u32);
        m.functions.push(Function {
            versioned: false,
            instance: row,
            signature,
            body: None,
        });
        if identity != instance.digest() {
            m.code_variants.insert(function, identity);
        }
        Ok(function)
    }
    fn visit(&mut self, target: Target, bindings: &BTreeMap<u32, Known>) -> Result<(), NativeStop> {
        let unit = target.unit;
        let m = &self.modules[unit];
        let body = m.functions[target.function.index()].body.as_ref().unwrap();
        let needed = !bindings.is_empty()
            || body.calls.iter().any(|call| match call.callee {
                Callee::Direct(f) => self
                    .eligible
                    .contains(&m.identity(m.functions[f.index()].instance)),
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
                            edits.push((i, self.bodies[&k.code], None));
                        }
                    }
                    Callee::Direct(f) => {
                        let key = m.identity(m.functions[f.index()].instance);
                        if !self.eligible.contains(&key) {
                            continue;
                        }
                        let base = self.originals[&key];
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
            let f = self.import(unit, to, sig)?;
            self.modules[unit].to_mut().functions[target.function.index()]
                .body
                .as_mut()
                .unwrap()
                .calls[i]
                .callee = Callee::Direct(f);
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
            Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } => {
                Some(*function)
            }
            _ => None,
        }));
        dependencies.extend(
            m.types
                .iter()
                .filter_map(|s| s.finalizer.map(|f| FuncId(f.0))),
        );
        for f in dependencies {
            if let Some(&t) = self.bodies.get(&m.code_identity(f)) {
                self.pending.push((t, BTreeMap::new()));
                continue;
            }
            if m.code_variants.contains_key(&f) {
                continue;
            }
            let t = self.originals[&m.identity(m.functions[f.index()].instance)];
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
type t0 = own "u64" value u64 { 0: u64 }
inst n0 = app@1/main/root [] []
inst n1 = lib@1/main/forward [t0] []
inst n2 = lib@1/main/recurse [t0] []
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
        let target = |i: usize| Target {
            unit: 0,
            function: FuncId(i as u32),
        };
        let originals = m
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| (m.identity(f.instance), target(i)))
            .collect();
        let bodies = (0..m.functions.len())
            .filter(|&i| m.functions[i].body.is_some())
            .map(|i| (m.code_identity(FuncId(i as u32)), target(i)))
            .collect();
        let eligible = [1, 2]
            .map(|i| m.identity(hirsel_core_ir::InstanceId(i)))
            .into_iter()
            .collect();
        collect(
            &mut units,
            originals,
            bodies,
            vec![ContentHash::default()],
            eligible,
            Target {
                unit: 0,
                function: FuncId(0),
            },
            4,
            limit,
        )?;
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
        let forward = m.identity(hirsel_core_ir::InstanceId(1)).digest();
        assert_ne!(units[1].code_identity(FuncId(4)), forward);
        // The original row continues to denote the unspecialised function value.
        assert_eq!(units[1].code_identity(FuncId(1)), forward);
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
            .map(|k| Cow::Borrowed(reached.materialized.get(k).ir().unwrap()))
            .collect();
        prepare(&mut modules, keys, &reached.materialized, &entry, 64).unwrap();
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
            let mut modules: Vec<_> = keys
                .iter()
                .map(|k| Cow::Borrowed(reached.materialized.get(k).ir().unwrap()))
                .collect();
            // Bodies a unit carries already have their own code identity; only the
            // collector's variants are new.
            let carried: BTreeSet<_> = modules
                .iter()
                .flat_map(|m| m.code_variants.values().copied())
                .collect();
            prepare(&mut modules, keys, &reached.materialized, &entry, 64).unwrap();
            assert_eq!(
                modules
                    .iter()
                    .flat_map(|m| m
                        .functions
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| f.body.is_some())
                        .filter_map(|(i, _)| m.code_variants.get(&FuncId(i as u32)))
                        .filter(|code| !carried.contains(code)))
                    .count(),
                expected
            );
            assert_eq!(keys.len(), reached.instances.as_ref().unwrap().len());
        }
    }
}
