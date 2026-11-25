//! The instance walk's order and its stops (resource limit, PROG-6 `target.unsupported`).
use crate::instances::InstanceStop;
use hirsel_core_ir::Identity;
use std::collections::{BTreeMap, BTreeSet};

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
    while let Some(key) = pending.pop_first() {
        if reached.contains(&key) {
            continue;
        }
        if reached.len() == limit {
            return Err(InstanceStop::Resource { limit, next: key });
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
