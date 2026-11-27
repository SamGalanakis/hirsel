//! Select the release std roots and their dependencies.
use super::{Unit, held, library};
use hirsel_core_ir::{Callee, FuncId, Identity, Param};
use std::collections::{BTreeMap, BTreeSet};

/// The plain library roots assigned to the side and the units they reach.
pub(super) fn program_side(
    units: &BTreeMap<Identity, Unit>,
    entry: &Identity,
) -> (Vec<Identity>, BTreeSet<Identity>) {
    let plain = |identity: &Identity| {
        identity != entry
            && library(&identity.definition)
            && identity.closures.is_empty()
            && identity.dictionaries.is_empty()
    };
    let mut edges = BTreeMap::new();
    let mut roots = Vec::new();
    let mut local_roots = Vec::new();
    for (identity, unit) in units {
        let module = unit.unbound();
        if library(&identity.definition) {
            let dependencies = module
                .functions
                .iter()
                .filter(|f| f.body.is_none())
                .map(|f| module.identity(f.instance))
                .filter_map(|callee| units.get_key_value(&callee))
                .map(|(callee, _)| callee)
                .collect::<Vec<_>>();
            edges.insert(identity, dependencies);
        }
        if plain(identity) {
            if !held::root(&module, FuncId(0)) {
                roots.push(identity);
            }
            continue;
        }
        // Specialised library instances already lower in the program unit.
        // Keep their plain callees there too, as for program helpers and
        // closures. The entry can still call the side directly.
        let first_helper = usize::from(identity == entry);
        for function in &module.functions[first_helper..] {
            let Some(body) = &function.body else { continue };
            for call in &body.calls {
                if let Callee::Direct(callee) = call.callee {
                    let function = &module.functions[callee.index()];
                    let signature = &module.signatures[function.signature.index()];
                    // These calls pass only floating-point values: the side
                    // can optimise the routine before the caller inlines it.
                    if signature.dest.is_none()
                        && matches!(signature.results.as_slice(), [ty] if ty.is_float())
                        && signature
                            .params
                            .iter()
                            .all(|param| matches!(param, Param::Value(ty) if ty.is_float()))
                    {
                        continue;
                    }
                    let callee = module.identity(function.instance);
                    if plain(&callee)
                        && let Some((callee, _)) = units.get_key_value(&callee)
                    {
                        local_roots.push(callee);
                    }
                }
            }
        }
    }
    fn reach<'a>(
        edges: &BTreeMap<&'a Identity, Vec<&'a Identity>>,
        mut work: Vec<&'a Identity>,
    ) -> BTreeSet<&'a Identity> {
        let mut seen = BTreeSet::new();
        while let Some(identity) = work.pop() {
            if let Some(dependencies) = edges.get(identity)
                && seen.insert(identity)
            {
                work.extend(dependencies.iter().copied());
            }
        }
        seen
    }
    let held = reach(&edges, local_roots);
    roots.retain(|identity| !held.contains(identity));
    let reached = reach(&edges, roots.clone());
    (
        roots.into_iter().cloned().collect(),
        reached.into_iter().cloned().collect(),
    )
}
