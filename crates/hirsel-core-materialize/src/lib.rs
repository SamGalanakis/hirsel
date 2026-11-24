//! One instance's module from its identity and its arguments' closures.
//!
//! An instance is named by its [`Identity`]: its definition, its dictionaries and the
//! closure hash of each type argument. [`materialize`] builds the instance of a
//! published generic body from that identity and the closures alone, and
//! [`materialize_glue`] builds a glue operation's instance, which has no published
//! definition. No shape is read from the module of the instance's caller, and none
//! is found by its name alone. [`children`] names the instances a module calls, each
//! with its arguments' closures, and a [`Resolver`] holds every closure a walk has
//! seen, so a child's arguments are found by hash.

use hirsel_core_front::GenericTemplate;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::{Closure, FuncId, Identity, Module, TypeId};
use rustc_hash::FxHashMap as HashMap;
use std::collections::BTreeSet;
use std::sync::Arc;

mod join;
pub use join::{Unit, join, join_units, link_symbol};

/// A published definition's body: its module, its function row and, for a generic
/// body, the template its instances substitute.
#[derive(Clone, Copy)]
pub struct Published<'a> {
    pub module: &'a Module,
    pub function: FuncId,
    pub template: Option<&'a GenericTemplate>,
}

/// An instance: its module (None when it is the published module itself), its
/// function row and the instances it calls.
pub struct Materialized {
    pub module: Option<Module>,
    pub function: FuncId,
    pub children: Vec<Child>,
    pub code: hirsel_core_ir::normal::EncodedBody,
}

/// An instance a body calls, with its arguments' closures in parameter order.
#[derive(Clone, Debug)]
pub struct Child {
    /// The calling module's signature-only row for this instance.
    pub function: FuncId,
    pub identity: Identity,
    pub closures: Vec<Arc<Closure>>,
}

/// Every closure a walk has seen, by hash.
#[derive(Default)]
pub struct Resolver {
    closures: HashMap<ContentHash, Arc<Closure>>,
}
impl Resolver {
    pub fn add(&mut self, children: &[Child]) {
        self.add_closures(children.iter().flat_map(|c| &c.closures));
    }
    /// Add closures by their hashes. A closure read from disk is added only
    /// after its hash is checked against its rows ([`Closure::new`]).
    pub fn add_closures<'a>(&mut self, closures: impl IntoIterator<Item = &'a Arc<Closure>>) {
        for closure in closures {
            self.closures
                .entry(closure.hash)
                .or_insert_with(|| Arc::clone(closure));
        }
    }
    /// The closures of `identity`'s arguments, or the first hash no closure has.
    pub fn arguments(&self, identity: &Identity) -> Result<Vec<&Closure>, ContentHash> {
        identity
            .closures
            .iter()
            .map(|h| self.closures.get(h).map(|c| &**c).ok_or(*h))
            .collect()
    }
}

/// The instance `identity` of the published body `own`, whose arguments' closures are
/// `arguments`. A non-generic body is its published module.
///
/// Every body a unit carries is named by its code identity, which decides its code,
/// and every unit that carries it defines it weak, so the linker keeps one and a
/// function value made in one unit is the same word in every other.
pub fn materialize(
    own: &Published,
    identity: &Identity,
    arguments: &[&Closure],
) -> Result<Materialized, String> {
    let Some(template) = own.template else {
        if !identity.closures.is_empty() || !identity.dictionaries.is_empty() {
            return Err("a non-generic body has no type or dictionary arguments".into());
        }
        return Ok(retained(own.module, own.function));
    };
    let (module, function) =
        hirsel_core_front::materialize(own.module, own.function, template, identity, arguments)?;
    Ok(retained(&module, function))
}

/// The instance `identity` of a glue operation ([`hirsel_core_front::is_glue`]).
pub fn materialize_glue(
    identity: &Identity,
    arguments: &[&Closure],
) -> Result<Materialized, String> {
    let (module, function) = hirsel_core_front::materialize_glue(identity, arguments)?;
    Ok(retained(&module, function))
}

fn retained(module: &Module, function: FuncId) -> Materialized {
    let (code, module) = hirsel_core_ir::normal::retain(module, function);
    let children = children_for_slots(&module, &code.slots);
    Materialized {
        children,
        code,
        module: Some(module),
        function: FuncId(0),
    }
}

/// The instances `module`'s function `root` reaches outside the module: the
/// signature-only rows its calls, function references and layouts' finalizers reach.
/// Closures are taken once per row of the module.
pub fn children(module: &Module, root: FuncId) -> Vec<Child> {
    let code = hirsel_core_ir::normal::encode(module, root);
    children_for_slots(module, &code.slots)
}

fn children_for_slots(module: &Module, slots: &[hirsel_core_ir::normal::Slot]) -> Vec<Child> {
    let mut memo: HashMap<TypeId, Arc<Closure>> = HashMap::default();
    slots
        .iter()
        .filter_map(|slot| match *slot {
            hirsel_core_ir::normal::Slot::External(f) => Some(f),
            hirsel_core_ir::normal::Slot::Builtin(_) => None,
        })
        .map(|f| {
            let instance = module.functions[f.index()].instance;
            let key = &module.instances[instance.index()];
            let closures: Vec<_> = key
                .arguments
                .iter()
                .map(|&t| Arc::clone(memo.entry(t).or_insert_with(|| Arc::new(module.closure(t)))))
                .collect();
            Child {
                function: f,
                identity: Identity {
                    definition: key.definition.clone(),
                    dictionaries: key.dictionaries.clone(),
                    closures: closures.iter().map(|c| c.hash).collect(),
                },
                closures,
            }
        })
        .collect()
}

/// Retained external functions, also used to continue reach diagnostics when
/// materialization stopped before the complete instance graph was built.
pub fn external_functions(module: &Module, root: FuncId) -> BTreeSet<FuncId> {
    hirsel_core_ir::normal::encode(module, root)
        .slots
        .into_iter()
        .filter_map(|slot| match slot {
            hirsel_core_ir::normal::Slot::External(f) => Some(f),
            hirsel_core_ir::normal::Slot::Builtin(_) => None,
        })
        .collect()
}
