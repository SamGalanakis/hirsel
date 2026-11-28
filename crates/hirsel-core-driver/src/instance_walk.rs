//! The instance walk's order and its stops (resource limit, PROG-6 `target.unsupported`).
use crate::instances::InstanceStop;
use hirsel_core_ir::Identity;
use std::collections::{BTreeMap, BTreeSet};

/// One retained dependency path may specialize the same definition with twelve
/// successive changes of argument types. Reordering or repeating its existing
/// arguments spends nothing. Other call paths have independent budgets.
pub(crate) const RECURSIVE_INSTANTIATIONS: usize = 12;

fn check_recursive_chain(
    key: &Identity,
    parents: &BTreeMap<Identity, Identity>,
) -> Result<(), InstanceStop> {
    let mut child = key;
    let mut changes = 0;
    let mut at = key;
    while let Some(parent) = parents.get(at) {
        if parent.definition == key.definition {
            if child
                .closures
                .iter()
                .any(|arg| !parent.closures.contains(arg))
            {
                changes += 1;
                if changes > RECURSIVE_INSTANTIATIONS {
                    return Err(InstanceStop::RecursiveInstantiation { next: key.clone() });
                }
            }
            child = parent;
        }
        at = parent;
    }
    Ok(())
}

/// The emitter supplies mechanical instance dependencies. In particular, a growing
/// f[T] -> f[List[T]] family stops as a compiler resource outcome, not a language rule.
pub fn collect_instances(
    roots: impl IntoIterator<Item = Identity>,
    limit: usize,
    mut dependencies: impl FnMut(&Identity) -> Result<Vec<Identity>, InstanceStop>,
) -> Result<Vec<Identity>, InstanceStop> {
    let mut pending: BTreeSet<_> = roots.into_iter().collect();
    let roots = pending.clone();
    let mut parents = BTreeMap::new();
    let mut reached = BTreeSet::new();
    let mut by_definition = BTreeMap::new();
    while let Some(key) = pending.pop_first() {
        if reached.contains(&key) {
            continue;
        }
        if reached.len() == limit {
            return Err(InstanceStop::Resource { limit, next: key });
        }
        if !key.closures.is_empty() && !hirsel_core_front::is_glue(&key.definition) {
            let count = by_definition
                .entry(key.definition.clone())
                .or_insert(0usize);
            if *count > RECURSIVE_INSTANTIATIONS {
                check_recursive_chain(&key, &parents)?;
            }
            *count += 1;
        }
        hirsel_guard::with_memory_context(
            format!("instantiating function {}", key.definition.0.name),
            || {
                let reads = match dependencies(&key) {
                    Err(InstanceStop::Unsupported(mut stop)) => {
                        stop.chain = crate::unsupported_reach::chain(&parents, &key);
                        return Err(InstanceStop::Unsupported(stop));
                    }
                    reads => reads?,
                };
                for read in &reads {
                    if !roots.contains(read) && !parents.contains_key(read) {
                        parents.insert(read.clone(), key.clone());
                    }
                }
                reached.insert(key);
                pending.extend(reads);
                Ok(())
            },
        )?;
    }
    Ok(reached.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use crate::{Driver, Meter, tests::request};
    use hirsel_core_ids::DefPath;

    #[test]
    fn finite_recursive_argument_permutations_remain_reachable() {
        use hirsel_core_ids::ContentHash;
        use hirsel_core_ir::Identity;

        // Rotating fourteen distinct parameters closes the cycle in fourteen steps.
        let mut root = Identity::plain(DefPath::from_full("app@1/main/rotate").unwrap());
        root.closures = (0..14).map(|i| ContentHash([i; 32])).collect();
        let reached = super::collect_instances([root], 14, |key| {
            let mut next = key.clone();
            next.closures.rotate_left(1);
            Ok(vec![next])
        })
        .unwrap();
        assert_eq!(reached.len(), 14);
    }

    #[test]
    fn growing_recursive_instances_have_a_located_resource_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let source = "\
type Wrap[T] { value: T }
fn grow[T](take x: T, n: u64) -> u64 {
    if n == 0 { n } else { grow(Wrap(value: take x), n - 1) }
}
fn probe(n: u64) -> u64 { grow(n, n) }
";
        let entry = DefPath::from_full("app@1/main/probe").unwrap();
        let product = Driver::default()
            .check(request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        let reached = product
            .reachable(&entry, crate::native::DEFAULT_INSTANCE_LIMIT)
            .unwrap();
        let [refusal] = reached.resources.as_slice() else {
            panic!("expected one resource refusal, got {:?}", reached.instances);
        };
        assert!(
            refusal.message.contains("recursive instantiation"),
            "{refusal:?}"
        );
        let span = product.source_site(&refusal.primary).unwrap().span;
        assert_eq!(&source[span.start as usize..span.end as usize], "grow");
    }
}
