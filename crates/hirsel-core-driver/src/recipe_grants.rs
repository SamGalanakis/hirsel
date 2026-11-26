//! The authority a recipe's grants give, as lines two recipes can be diffed by,
//! and the refusals of an entry's grants that do not match its parameters.
use super::selection;
use super::{EntryParameter, NativeOptions, NativeStop};
use hirsel_core_ir::release_recipe::{GrantBinding, GrantKind, Recipe};
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
            GrantKind::Dir => units.extend(
                grant
                    .values
                    .iter()
                    .map(|v| format!("{name}={}", hirsel_core_ids::launch::dir_display(v))),
            ),
            GrantKind::Net | GrantKind::Env | GrantKind::Foreign => {
                units.extend(grant.values.iter().map(|v| format!("{name}={v}")));
            }
            // The program's own arguments, which every release reads.
            GrantKind::Args => {}
            _ => units.extend(grant.values.iter().cloned()),
        }
    }
    units
}

/// R3: the names of the entry's parameters that a recipe's grant bindings leave
/// ungranted, in parameter order. A binding gives a parameter no values exactly
/// when the launch would refuse it (CAP-4); `Args` is always present.
pub(crate) fn ungranted(bindings: &[GrantBinding], parameters: &[EntryParameter]) -> Vec<String> {
    bindings
        .iter()
        .filter(|binding| binding.kind != GrantKind::Args && binding.values.is_empty())
        .map(|binding| parameters[binding.ordinal as usize].name.clone())
        .collect()
}

/// R3: one warning line per `main` parameter the build leaves ungranted, in
/// parameter order. The build is still accepted.
pub(crate) fn warn_ungranted(names: &[String]) {
    for name in names {
        eprintln!(
            "hirsel: warning: grant `{name}` is not in the build's configuration; the executable will not start"
        );
    }
}

/// CAP-4: a grant with no matching parameter of the entry, then FFI-4's
/// ungranted `Foreign` parameters ([`refuse_ungranted_foreign`]).
pub(crate) fn refuse_unmatched_grants(
    entry: &str,
    parameters: &[EntryParameter],
    options: &NativeOptions,
) -> Result<(), NativeStop> {
    let mut counts = [0usize; GRANT_NAMES.len()];
    for tag in parameters
        .iter()
        .filter_map(|p| p.ty.strip_prefix("core@1/core/").and_then(kind))
    {
        counts[usize::from(tag)] += 1;
    }
    let unmatched = GRANT_NAMES
        .iter()
        .enumerate()
        .find(|&(tag, _)| options.grant_mask & (1 << tag) != 0 && counts[tag] == 0)
        .map(|(_, name)| format!("--grant {name}"));
    let unmatched = unmatched
        .or_else(|| {
            options
                .directories
                .get(counts[GrantKind::Dir as usize])
                .map(|dir| format!("--dir {:?}", hirsel_core_ids::launch::dir_display(dir)))
        })
        .or_else(|| {
            options
                .exec_grants
                .get(counts[GrantKind::Exec as usize])
                .map(|grant| format!("--grant Exec={}", grant.request.text()))
        })
        .or_else(|| {
            options
                .foreign_grants
                .iter()
                .find(|interface| {
                    !parameters
                        .iter()
                        .any(|param| param.foreign_interface() == Some(interface.as_str()))
                })
                .map(|interface| format!("--grant Foreign={interface}"))
        })
        .or_else(|| {
            ((!options.net_rules.is_empty() || !options.unix_roots.is_empty())
                && counts[GrantKind::Net as usize] == 0)
                .then(|| "--grant Net".to_owned())
        })
        .or_else(|| {
            (options.env_patterns.is_some() && counts[GrantKind::Env as usize] == 0)
                .then(|| "--grant Env".to_owned())
        })
        .or_else(|| {
            options
                .secret_grants
                .iter()
                .find(|grant| {
                    !parameters
                        .iter()
                        .any(|p| p.name == grant.parameter && p.ty == "core@1/core/Secret")
                })
                .map(|grant| format!("--grant Secret:{}", grant.parameter))
        })
        .or_else(|| {
            options
                .cred_grants
                .iter()
                .find(|grant| {
                    !parameters
                        .iter()
                        .any(|p| p.name == grant.parameter && p.ty == "core@1/core/Cred")
                })
                .map(|grant| format!("--grant Cred:{}", grant.parameter))
        });
    if let Some(grant) = unmatched {
        return Err(NativeStop::Entry(format!(
            "{grant} has no matching parameter of {entry}"
        )));
    }
    refuse_ungranted_foreign(parameters, options).map_err(NativeStop::Entry)
}

/// FFI-4: every `Foreign[I]` parameter whose interface the options do not
/// grant, one `not granted: Foreign[D]` line each, in parameter order; the
/// caller refuses the entry with them.
fn refuse_ungranted_foreign(
    parameters: &[EntryParameter],
    options: &NativeOptions,
) -> Result<(), String> {
    let mut lines: Vec<String> = Vec::new();
    for interface in parameters
        .iter()
        .filter_map(EntryParameter::foreign_interface)
    {
        let line = format!("not granted: Foreign[{interface}]");
        if !options.foreign_grants.contains(interface) && !lines.contains(&line) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        Ok(())
    } else {
        Err(lines.join("\n"))
    }
}

/// A launch grant rule as `hirsel` prints it.
pub(super) fn net_rule(rule: &hirsel_core_net::GrantRule) -> String {
    use hirsel_core_net::{AddressFamily, GrantRule, Reach, RuleRow, TlsTrust};
    match rule {
        GrantRule::Address(rule) => {
            let direction = rule.direction().word();
            let rule = RuleRow::from(*rule);
            let ip = if rule.family == AddressFamily::V4 as u8 {
                std::net::IpAddr::V4(std::net::Ipv4Addr::from(rule.lo as u32))
            } else {
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(
                    (u128::from(rule.hi) << 64) | u128::from(rule.lo),
                ))
            };
            let reach = if rule.reach == Reach::Public as u8 {
                " public"
            } else {
                ""
            };
            format!(
                "{direction} {ip}/{} {}-{}{reach}",
                rule.bits, rule.first, rule.last
            )
        }
        GrantRule::Tls(rule) => {
            let (first, last) = rule.ports();
            let trust = match rule.trust() {
                TlsTrust::System => "System".to_owned(),
                TlsTrust::Roots(hash) => format!(
                    "Roots=sha256:{}",
                    selection(hirsel_core_ids::ContentHash(hash))
                ),
                TlsTrust::Any => "Any".to_owned(),
            };
            format!("Tls {} {first}-{last} {trust}", rule.pattern_text())
        }
        GrantRule::Resolve(rule) => format!("Resolve {}", rule.pattern_text()),
        GrantRule::Unix(rule) => {
            let (root, path) = rule
                .endpoint()
                .expect("launch rule has a concrete endpoint");
            format!("{} {root} {path}", rule.direction.word())
        }
    }
}
