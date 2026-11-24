//! The authority a recipe's grants give, as lines two recipes can be diffed by.
use super::selection;
use hirsel_core_ir::release_recipe::{GrantKind, Recipe};
use std::collections::BTreeSet;

pub(super) const GRANT_NAMES: [&str; 15] = [
    "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process", "Tasks",
    "Args", "Secret", "Foreign", "Exec", "Cred",
];

pub(super) fn kind(name: &str) -> Option<u8> {
    GRANT_NAMES
        .iter()
        .position(|kind| *kind == name)
        .map(|i| i as u8)
}

pub(super) fn grant_name(kind: GrantKind) -> &'static str {
    GRANT_NAMES[kind as usize]
}

/// One line per authority a recipe's grants give (launch.md "Publish and assemble"): each
/// value of a Dir, Net, Env or Foreign grant, a whole Exec, Secret or Cred binding with
/// its policy, and each other capability by name. Two recipes give the same authority
/// exactly when their sets are equal.
pub(crate) fn grant_units(recipe: &Recipe) -> BTreeSet<String> {
    let mut units = BTreeSet::new();
    for grant in &recipe.launch.grants {
        let name = grant_name(grant.kind);
        match grant.kind {
            GrantKind::Exec | GrantKind::Secret | GrantKind::Cred => {
                if grant.values.is_empty() {
                    continue;
                }
                let mut line = format!("{name}={}", grant.values.join(" "));
                if let Some(policy) = grant.policy {
                    line.push_str(&format!(" policy=sha256:{}", selection(policy)));
                }
                units.insert(line);
            }
            GrantKind::Dir | GrantKind::Net | GrantKind::Env | GrantKind::Foreign => {
                units.extend(grant.values.iter().map(|v| format!("{name}={v}")));
            }
            // The program's own arguments, which every release reads.
            GrantKind::Args => {}
            _ => units.extend(grant.values.iter().cloned()),
        }
    }
    units
}
