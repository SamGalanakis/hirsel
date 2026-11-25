//! A walk that reaches `target.unsupported(why)` in retained code refuses the build for
//! its target (PROG-6), naming the reason, the target and the reach chain from the entry.
use hirsel_core_ids::DefPath;
use hirsel_core_ir::Identity;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unsupported {
    pub why: String,
    pub target: hirsel_core_layout::Target,
    /// The entry first, then each function the walk first reached the next one through,
    /// ending at the function whose retained code calls `target.unsupported`.
    pub chain: Vec<DefPath>,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = |d: &DefPath| {
            let mut parts: Vec<&str> = d.0.modules.iter().map(String::as_str).collect();
            parts.push(&d.0.name);
            parts.join(".")
        };
        let (entry, through) = self.chain.split_first().expect("a chain names its entry");
        write!(
            f,
            "PROG-6: {} reaches target.unsupported(\"{}\") for target {}",
            shown(entry),
            self.why,
            self.target.triple()
        )?;
        if !through.is_empty() {
            let through: Vec<_> = through.iter().map(shown).collect();
            write!(f, ", through {}", through.join(" → "))?;
        }
        Ok(())
    }
}

/// `at`'s chain from its root, by each instance's first-discovered parent.
pub(crate) fn chain<'a>(
    parents: &'a BTreeMap<Identity, Identity>,
    mut at: &'a Identity,
) -> Vec<DefPath> {
    let mut chain = vec![at.definition.clone()];
    while let Some(parent) = parents.get(at) {
        chain.push(parent.definition.clone());
        at = parent;
    }
    chain.reverse();
    chain
}
