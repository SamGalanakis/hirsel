//! Native-only code variants. The checked instance and function-value identities
//! remain type keyed; a private symbol binds only immutable code words: an
//! immutable callable parameter's, or a callable field's of a record the
//! callee views. A call through a bound word is direct, to its code.
use crate::instances::InstanceSet;
use crate::{ContentHash, DefPath, InstanceStop, NativeStop};
use hirsel_core_ir::{
    Callee, FuncId, Function, Identity, Inst, Loan, Module, Param, PlaceTy, SigId,
};
use hirsel_core_layout::{RecipeRow, TypeRef};
use serde::Serialize;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

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
/// Known code by (entry parameter, byte offset of the code word in its place).
type Bindings = BTreeMap<(u32, u64), Known>;
#[derive(Serialize)]
struct Variant<'a> {
    base: &'a Identity,
    code: ContentHash,
    bindings: Vec<(&'a (u32, u64), &'a Known)>,
}
/// A code word of a callee's parameter whose known code the callee can use: it
/// calls through it, or passes, stores or copies the place holding it.
#[derive(Clone, Copy)]
struct Demand {
    param: u32,
    offset: u64,
    /// An immutable callable parameter's own word, not a record field's.
    callable: bool,
    /// A record field's word, or a callable the body calls passing a record
    /// whose code word is known, or forwards to a callee where it carries.
    /// Only a generic callee's variants bind a callable that carries none.
    carries: bool,
}

/// Whether function `f` of `m` takes known code a caller can bind: an immutable
/// callable parameter, or a viewed record holding a code word.
pub(crate) fn takes_code(m: &Module, f: FuncId) -> bool {
    let function = &m.functions[f.index()];
    function
        .body
        .as_ref()
        .is_some_and(|b| !b.callable_parameters.is_empty())
        || m.signatures[function.signature.index()]
            .params
            .iter()
            .any(|p| match p {
                Param::Address {
                    place: PlaceTy::Declared(t),
                    loan: Loan::View,
                } => origins::holds_code(&m.types, TypeRef(t.0)),
                _ => false,
            })
}
pub(crate) fn prepare<'m>(
    modules: &mut Vec<Cow<'m, Module>>,
    reached: &[Identity],
    materialized: &InstanceSet,
    entry: Roots<'_>,
    limit: usize,
) -> Result<Vec<ContentHash>, NativeStop> {
    prepare_inputs(modules, reached, entry, limit, |instance| {
        let body = materialized.get(instance);
        let product = body.product.as_ref();
        let owner = crate::native::key("hirsel.callable-owner.v1", &product.map(|p| &p.key))?;
        Ok((
            body.function,
            owner,
            product.is_some_and(|p| p.check.template.is_some()),
        ))
    })
}

pub(crate) fn prepare_inputs<'m>(
    modules: &mut Vec<Cow<'m, Module>>,
    reached: &[Identity],
    entry: Roots<'_>,
    limit: usize,
    mut input: impl FnMut(&Identity) -> Result<(FuncId, ContentHash, bool), NativeStop>,
) -> Result<Vec<ContentHash>, NativeStop> {
    let mut originals = BTreeMap::new();
    let mut bodies = BTreeMap::new();
    let mut eligible = BTreeSet::new();
    let mut owners = Vec::with_capacity(modules.len());
    for (unit, (m, instance)) in modules.iter().zip(reached).enumerate() {
        let (root, owner, generic) = input(instance)?;
        owners.push(owner);
        if generic {
            eligible.insert(instance.clone());
        }
        // A unit's root is its instance; every other body is the unit's own copy,
        // found by its private code identity.
        for (i, f) in m.functions.iter().enumerate() {
            let function = FuncId(i as u32);
            if function == root {
                originals.insert(m.identity(f.instance), Target { unit, function });
            }
            if f.body.is_some() {
                bodies.insert(m.code_identity(function), Target { unit, function });
            }
        }
    }
    let roots = match entry {
        Roots::Entry(entry) => vec![
            *originals
                .iter()
                .find(|(k, _)| &k.definition == entry)
                .unwrap()
                .1,
        ],
        // Every root is a public entry of the unit set: each walks alone, in
        // identity order, so the variants and the originals kept are a
        // function of the set, never of which root a program called.
        Roots::All(roots) => roots.iter().map(|r| originals[r]).collect(),
    };
    // The collector owns modules only during this call. Cow's source lifetime
    // remains independent of that mutable borrow.
    let body_identities = owners.clone();
    collect(
        modules,
        originals,
        bodies,
        owners,
        eligible,
        roots,
        reached.len(),
        limit,
    )?;
    Ok(body_identities)
}

/// Where the collector starts: a program's entry, or every root of a unit set
/// that has no single entry (the separately lowered std, `std_release`).
#[derive(Clone, Copy)]
pub(crate) enum Roots<'a> {
    Entry(&'a DefPath),
    All(&'a [Identity]),
}

#[allow(clippy::too_many_arguments)]
fn collect<'m>(
    modules: &mut Vec<Cow<'m, Module>>,
    originals: BTreeMap<Identity, Target>,
    bodies: BTreeMap<ContentHash, Target>,
    owners: Vec<ContentHash>,
    eligible: BTreeSet<Identity>,
    roots: Vec<Target>,
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
        optional: 0,
        bound: BTreeMap::new(),
        recipes: RefCell::new(BTreeMap::new()),
        demands: RefCell::new(BTreeMap::new()),
        computing: RefCell::new(BTreeSet::new()),
        pending: roots
            .into_iter()
            .rev()
            .map(|root| (root, Bindings::new()))
            .collect(),
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
    if let Some(path) = std::env::var_os("HIRSEL_STD_INVENTORY") {
        c.inventory(std::path::Path::new(&path))?;
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
    /// How many of `variants` only save an indirect call: those spend a limit
    /// of their own, never a generic callee's.
    optional: usize,
    /// What each variant binds, by its code identity, for the inventory.
    bound: BTreeMap<ContentHash, Vec<Known>>,
    pending: Vec<(Target, Bindings)>,
    live: BTreeSet<ContentHash>,
    semantic_count: usize,
    limit: usize,
    /// Each unit's layout recipes, with the type count they were computed for:
    /// an import appends rows.
    recipes: RefCell<BTreeMap<usize, (usize, Rc<Vec<RecipeRow>>)>>,
    /// What each body can use of known code, by its code identity.
    demands: RefCell<BTreeMap<ContentHash, Rc<Vec<Demand>>>>,
    /// The bodies whose demands are being computed, to end a recursive forward.
    computing: RefCell<BTreeSet<ContentHash>>,
}
impl Work<'_, '_> {
    /// The binding-owner inventory (std-reuse S1): a JSON line per live body
    /// with the packages its definition, its type arguments' rows and hooks,
    /// and each callable it binds come from, appended to `path`.
    fn inventory(&self, path: &std::path::Path) -> Result<(), NativeStop> {
        use std::io::Write as _;
        fn package(definition: &DefPath) -> String {
            definition.0.package.name.clone()
        }
        // The packages a unit's instance reads through its type arguments: each
        // row's shape name (a builtin row names none) and each hook.
        let closures = |unit: usize, f: FuncId| -> BTreeSet<String> {
            let m = &self.modules[unit];
            let key = &m.instances[m.functions[f.index()].instance.index()];
            let mut packages = BTreeSet::new();
            for &t in &key.arguments {
                for row in m.closure(t).rows {
                    if let Some((p, _)) = row.shape.name.split_once('@') {
                        packages.insert(p.to_owned());
                    }
                    for hook in row.hooks.iter().flatten() {
                        packages.insert(package(&hook.definition));
                    }
                }
            }
            packages
        };
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for (unit, m) in self.modules.iter().enumerate() {
            for (i, function) in m.functions.iter().enumerate() {
                let f = FuncId(i as u32);
                let code = m.code_identity(f);
                if function.body.is_none() || !self.live.contains(&code) {
                    continue;
                }
                let identity = m.identity(function.instance);
                let bound = self.bound.get(&code).map(|known| {
                    known
                        .iter()
                        .map(|k| {
                            let t = self.bodies[&k.code];
                            let mut packages = closures(t.unit, t.function);
                            packages.insert(package(&k.instance.definition));
                            serde_json::json!({
                                "definition": k.instance.definition.to_string(),
                                "packages": packages,
                            })
                        })
                        .collect::<Vec<_>>()
                });
                let row = serde_json::json!({
                    "symbol": hirsel_core_ir::symbol(code),
                    "definition": identity.definition.to_string(),
                    "package": package(&identity.definition),
                    "dictionaries": identity.dictionaries.len(),
                    "closures": identity.closures.len(),
                    "closure_packages": closures(unit, f),
                    "variant": m.code_variants.contains_key(&f),
                    "bound": bound,
                });
                writeln!(out, "{row}")?;
            }
        }
        Ok(())
    }
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
    fn recipes(&self, unit: usize) -> Rc<Vec<RecipeRow>> {
        let types = &self.modules[unit].types;
        if let Some((n, r)) = self.recipes.borrow().get(&unit)
            && *n == types.len()
        {
            return r.clone();
        }
        // A table the layout refuses binds nothing: every field offset and
        // layout constant stays unknown.
        let r = Rc::new(hirsel_core_layout::compute(types).unwrap_or_default());
        self.recipes
            .borrow_mut()
            .insert(unit, (types.len(), r.clone()));
        r
    }
    /// The code words body `t` can use of what a caller knows.
    fn demands(&self, t: Target) -> Rc<Vec<Demand>> {
        let m = &self.modules[t.unit];
        let code = m.code_identity(t.function);
        if let Some(d) = self.demands.borrow().get(&code) {
            return d.clone();
        }
        let function = &m.functions[t.function.index()];
        if self.computing.borrow().contains(&code) {
            // A recursive forward carries nothing the outer body does not.
            return Rc::new(Vec::new());
        }
        let mut keys = Vec::new();
        if let Some(body) = function.body.as_ref() {
            let signature = &m.signatures[function.signature.index()];
            let recipes = self.recipes(t.unit);
            let entry = body.blocks[0].params.get(&body.pools.values).unwrap();
            for v in &body.callable_parameters {
                keys.push(Demand {
                    param: entry.iter().position(|x| x == v).unwrap() as u32,
                    offset: 0,
                    callable: true,
                    carries: false,
                });
            }
            for (v, param, place) in origins::frozen_parameters(signature, body) {
                if body.callable_parameters.contains(&v) {
                    continue;
                }
                for offset in place
                    .map(|t| origins::code_words(&m.types, &recipes, TypeRef(t.0)))
                    .unwrap_or_default()
                {
                    keys.push(Demand {
                        param,
                        offset,
                        callable: false,
                        carries: true,
                    });
                }
            }
            if !keys.is_empty() {
                let mut o = origins::Origins::new(m, &recipes, signature, body);
                let called: Vec<_> = body
                    .calls
                    .iter()
                    .filter_map(|c| match c.callee {
                        Callee::Indirect(v) => o.value(v),
                        _ => None,
                    })
                    .collect();
                keys.retain(|d| {
                    o.uses(d.param)
                        || called.contains(&origins::Origin::Parameter(d.param, d.offset))
                });
                if keys.iter().any(|d| d.callable) {
                    self.computing.borrow_mut().insert(code);
                    for d in keys.iter_mut().filter(|d| d.callable) {
                        d.carries = self.carries(t, &recipes, &mut o, d.param);
                    }
                    self.computing.borrow_mut().remove(&code);
                }
            }
        }
        let keys = Rc::new(keys);
        self.demands.borrow_mut().insert(code, keys.clone());
        keys
    }
    /// Whether `t`'s callable parameter `p` carries record-held code: `t` calls
    /// through it passing a viewed record with a code word it knows or is bound,
    /// or passes it on to a callee whose parameter carries.
    fn carries(&self, t: Target, recipes: &[RecipeRow], o: &mut origins::Origins, p: u32) -> bool {
        let m = &self.modules[t.unit];
        let body = m.functions[t.function.index()].body.as_ref().unwrap();
        let this = origins::Origin::Parameter(p, 0);
        body.calls.iter().any(|call| {
            let args = call.args.get(&body.pools.args).unwrap();
            match call.callee {
                Callee::Indirect(v) if o.value(v) == Some(this) => {
                    let signature = &m.signatures[call.signature.index()];
                    let dest = usize::from(signature.dest.is_some());
                    args.iter().enumerate().any(|(i, &arg)| {
                        let place = i.checked_sub(dest).and_then(|i| signature.params.get(i));
                        let Some(Param::Address {
                            place: PlaceTy::Declared(ty),
                            loan: Loan::View,
                        }) = place
                        else {
                            return false;
                        };
                        origins::code_words(&m.types, recipes, TypeRef(ty.0))
                            .into_iter()
                            .any(|k| o.word(arg, k).is_some())
                    })
                }
                Callee::Direct(f) => {
                    let Some(&to) = self
                        .originals
                        .get(&m.identity(m.functions[f.index()].instance))
                    else {
                        return false;
                    };
                    let forwarded: Vec<u32> = (0..args.len() as u32)
                        .filter(|&i| o.word(args[i as usize], 0) == Some(this))
                        .collect();
                    !forwarded.is_empty()
                        && self
                            .demands(to)
                            .iter()
                            .any(|d| d.callable && d.carries && forwarded.contains(&d.param))
                }
                _ => false,
            }
        })
    }
    fn resolve(
        &self,
        unit: usize,
        bindings: &Bindings,
        origin: Option<origins::Origin>,
    ) -> Option<Known> {
        match origin? {
            origins::Origin::Function(f) => self.known(unit, f),
            origins::Origin::Parameter(p, k) => bindings.get(&(p, k)).cloned(),
        }
    }
    /// A private body of `base` binding `bindings`. A generic callee's callable
    /// bindings spend the instance limit as its instances do (`strict`); any
    /// other binding only saves an indirect call, so at its own limit the call
    /// keeps the shared body.
    fn request(
        &mut self,
        base: Target,
        bindings: Bindings,
        strict: bool,
    ) -> Result<Option<Target>, NativeStop> {
        let instance = self.instance(base);
        let hash = crate::native::key(
            "hirsel.callable-code.v2",
            &Variant {
                base: &instance,
                code: self.modules[base.unit].code_identity(base.function),
                bindings: bindings.iter().collect(),
            },
        )?;
        if let Some(t) = self.variants.get(&hash) {
            return Ok(Some(*t));
        }
        let spent = if strict {
            self.variants.len() - self.optional
        } else {
            self.optional
        };
        if self.semantic_count + spent >= self.limit {
            if !strict {
                return Ok(None);
            }
            return Err(NativeStop::Instances(InstanceStop::Resource {
                limit: self.limit,
                next: instance,
            }));
        }
        self.optional += usize::from(!strict);
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
        self.bound
            .insert(hash, bindings.values().cloned().collect());
        self.pending.push((target, bindings));
        Ok(Some(target))
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
    fn visit(&mut self, target: Target, bindings: &Bindings) -> Result<(), NativeStop> {
        let unit = target.unit;
        let mut edits = Vec::new();
        {
            let m = &self.modules[unit];
            let function = &m.functions[target.function.index()];
            let body = function.body.as_ref().unwrap();
            let bases: Vec<Option<Target>> = body
                .calls
                .iter()
                .map(|call| match call.callee {
                    Callee::Direct(f) => self
                        .originals
                        .get(&m.identity(m.functions[f.index()].instance))
                        .copied(),
                    _ => None,
                })
                .collect();
            let needed = !bindings.is_empty()
                || bases.iter().flatten().any(|&b| !self.demands(b).is_empty());
            if needed {
                let recipes = self.recipes(unit);
                let mut origins = origins::Origins::new(
                    m,
                    &recipes,
                    &m.signatures[function.signature.index()],
                    body,
                );
                for (i, call) in body.calls.iter().enumerate() {
                    // A call through known code is direct to that code's body,
                    // which may itself bind what this call passes it.
                    let (to, devirtualized) = match call.callee {
                        Callee::Indirect(v) => {
                            match self.resolve(unit, bindings, origins.value(v)) {
                                Some(k) => (self.bodies[&k.code], true),
                                None => continue,
                            }
                        }
                        Callee::Direct(_) => match bases[i] {
                            Some(b) => (b, false),
                            None => continue,
                        },
                        Callee::Runtime(..) => continue,
                    };
                    let eligible = !devirtualized && self.eligible.contains(&self.instance(to));
                    let args = call.args.get(&body.pools.args).unwrap();
                    let mut passed = Bindings::new();
                    let mut strict = false;
                    for d in self.demands(to).iter() {
                        if !eligible && !d.carries {
                            continue;
                        }
                        let Some(&arg) = args.get(d.param as usize) else {
                            continue;
                        };
                        if let Some(k) = self.resolve(unit, bindings, origins.word(arg, d.offset)) {
                            strict |= eligible && d.callable;
                            passed.insert((d.param, d.offset), k);
                        }
                    }
                    if devirtualized || !passed.is_empty() {
                        edits.push((i, to, (!passed.is_empty()).then_some((passed, strict))));
                    }
                }
            }
        }
        for (i, base, passed) in edits {
            let to = match passed {
                Some((p, strict)) => self.request(base, p, strict)?.unwrap_or(base),
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
                self.pending.push((t, Bindings::new()));
                continue;
            }
            if m.code_variants.contains_key(&f) {
                continue;
            }
            // A callee in a separately lowered std is not a unit here.
            let Some(&t) = self
                .originals
                .get(&m.identity(m.functions[f.index()].instance))
            else {
                continue;
            };
            self.pending.push((t, Bindings::new()));
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
            vec![Target {
                unit: 0,
                function: FuncId(0),
            }],
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
        prepare(
            &mut modules,
            keys,
            &reached.materialized,
            Roots::Entry(&entry),
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
            prepare(
                &mut modules,
                keys,
                &reached.materialized,
                Roots::Entry(&entry),
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
                        .filter_map(|(i, _)| m.code_variants.get(&FuncId(i as u32)))
                        .filter(|code| !carried.contains(code)))
                    .count(),
                expected
            );
            assert_eq!(keys.len(), reached.instances.as_ref().unwrap().len());
        }
    }

    /// The callee of each non-runtime call in each live body of `name` that
    /// `entry` reaches: the definition of a direct callee, None if indirect.
    fn reached_calls(source: &str, entry: &str, name: &str) -> Vec<Vec<Option<String>>> {
        let dir = tempfile::tempdir().unwrap();
        let product = Driver::default()
            .check(
                crate::tests::request(source, dir.path()),
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        let entry = DefPath::from_full(&format!("app@1/main/{entry}")).unwrap();
        let reached = product.reachable(&entry, 64).unwrap();
        let keys = reached.require().unwrap();
        let mut modules: Vec<_> = keys
            .iter()
            .map(|k| Cow::Borrowed(reached.materialized.get(k).ir().unwrap()))
            .collect();
        prepare(
            &mut modules,
            keys,
            &reached.materialized,
            Roots::Entry(&entry),
            64,
        )
        .unwrap();
        let name = format!("app@1/main/{name}");
        let mut bodies = vec![];
        for m in &modules {
            for f in &m.functions {
                let Some(body) = &f.body else { continue };
                if m.identity(f.instance).definition.to_string() != name {
                    continue;
                }
                bodies.push(
                    body.calls
                        .iter()
                        .filter_map(|c| match c.callee {
                            Callee::Direct(g) => Some(Some(
                                m.identity(m.functions[g.index()].instance)
                                    .definition
                                    .to_string(),
                            )),
                            Callee::Indirect(_) => Some(None),
                            Callee::Runtime(..) => None,
                        })
                        .collect(),
                );
            }
        }
        bodies
    }
    #[test]
    fn record_carried_code_is_direct_only_where_every_write_agrees() {
        // `second` is the record's second callable field, at a nonzero
        // offset; `forward` passes the viewed record on. Closures are numbered
        // in each body's lowering order: `first` is $closure0, `second` is
        // $closure1.
        let source = r#"
scoped type Pair {
    first: scoped fn(u64) -> u64,
    second: scoped fn(u64) -> u64,
    flag: bool
}
fn call_second(p: Pair, x: u64) -> u64 { p.second(x) }
fn forward(p: Pair, x: u64) -> u64 { call_second(p, x) }
fn known() -> u64 {
    let a: u64 = 1
    let b: u64 = 2
    let p = Pair(first: scoped fn(x: u64) -> u64 { x + a }, second: scoped fn(x: u64) -> u64 { x * b }, flag: true)
    forward(p, 20u64)
}
fn unknown(p: Pair) -> u64 { forward(p, 20u64) }
fn swap(mut p: Pair) { p.second = p.first }
fn lent() -> u64 {
    let a: u64 = 1
    let b: u64 = 2
    let mut p = Pair(first: scoped fn(x: u64) -> u64 { x + a }, second: scoped fn(x: u64) -> u64 { x * b }, flag: true)
    swap(mut p)
    forward(p, 20u64)
}
fn reassigned() -> u64 {
    let a: u64 = 1
    let b: u64 = 2
    let mut p = Pair(first: scoped fn(x: u64) -> u64 { x + a }, second: scoped fn(x: u64) -> u64 { x * b }, flag: true)
    p.second = fn(x: u64) -> u64 { x + 1 }
    forward(p, 20u64)
}
"#;
        let direct = |to: &str| Some(format!("app@1/main/{to}"));
        let known = reached_calls(source, "known", "call_second");
        assert!(!known.is_empty());
        assert!(
            known
                .iter()
                .all(|calls| calls == &[direct("known$closure1")]),
            "{known:?}"
        );
        assert_eq!(
            reached_calls(source, "unknown", "call_second"),
            [vec![None]],
            "no caller knows the code"
        );
        // After the mutable loan or the field write, `second` holds other
        // code; the constructor's `second` is the wrong target.
        for entry in ["lent", "reassigned"] {
            let calls = reached_calls(source, entry, "call_second");
            assert!(!calls.is_empty());
            assert!(
                calls
                    .iter()
                    .flatten()
                    .all(|c| c != &direct(&format!("{entry}$closure1"))),
                "{entry}: {calls:?}"
            );
        }
    }
}
