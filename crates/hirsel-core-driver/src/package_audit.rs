use crate::cli::{AuditOption, Request};
use crate::package_content::{Id, Root};
use crate::package_dir::PackageDir;
use crate::package_lock::make_rows;
use crate::package_resolve::Resolution;
use crate::package_store::PackageStore;
use crate::package_workspace::Workspace;
use crate::reachability::authority::{Card, Cards, carried_names, names};
use crate::{CheckRequest, Driver};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type Failure = Box<dyn std::error::Error>;

#[derive(Default)]
pub(crate) struct Checked {
    pub(crate) cards: Cards,
    pub(crate) authority: Vec<String>,
}

pub(crate) fn check(resolved: &Resolution, selected: Option<&Id>) -> Result<Checked, Failure> {
    let mut checked = Checked::default();
    let mut driver = Driver::default();
    let store = crate::store::default_path(&resolved.workspace.directory);
    for id in resolved
        .workspace
        .members
        .keys()
        .filter(|id| selected.is_none_or(|selected| *id == selected))
    {
        let app = PackageDir::from_resolution(resolved, id, true)?;
        let request = CheckRequest::from_app(app, None, &store)?;
        let product = driver.check_for_authority(request)?;
        if !product.accepted() {
            for refusal in &product.diagnostics {
                eprintln!("{}", product.render_refusal(refusal)?);
            }
            for resource in &product.resources {
                eprintln!(
                    "{}: {}",
                    product.render_site(&resource.primary)?,
                    resource.message
                );
            }
            for gap in &product.incomplete {
                eprintln!("{}: {}", product.render_site(&gap.primary)?, gap.message);
            }
            return Err(format!("{id} does not check").into());
        }
        if let Some(error) = &product.authority_refusal
            && !checked.authority.contains(error)
        {
            checked.authority.push(error.clone());
        }
        for (id, card) in &product.authority_cards {
            checked.cards.entry(id.clone()).or_default().merge(card);
        }
    }
    Ok(checked)
}

#[derive(Serialize)]
struct BindingRow {
    row: String,
    authority: Option<Vec<&'static str>>,
}

fn bindings(resolution: &Resolution) -> BTreeMap<Id, Vec<BindingRow>> {
    let mut rows = BTreeMap::<_, Vec<_>>::new();
    for member in resolution.workspace.members.values() {
        for (alias, id) in &member.aliases {
            rows.entry(id.clone()).or_default().push(BindingRow {
                row: format!("{}: dependencies.{alias}", member.manifest_path.display()),
                authority: member.ceilings.get(alias).map(|mask| names(*mask)),
            });
        }
    }
    for (id, root) in &resolution.published {
        if resolution.workspace.members.contains_key(id) {
            continue;
        }
        for (alias, (child, _)) in &resolution.libraries[root].content.bindings {
            rows.entry(child.clone()).or_default().push(BindingRow {
                row: format!("{id}: dependencies.{alias}"),
                authority: None,
            });
        }
    }
    rows
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct CarriedProjection {
    pub(crate) takes: Vec<&'static str>,
    pub(crate) returns: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ReachedProjection<'a> {
    pub(crate) kinds: Vec<&'static str>,
    pub(crate) provided: Vec<&'static str>,
    pub(crate) contained: &'a std::collections::BTreeSet<String>,
    pub(crate) unresolved: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct CardProjection<'a> {
    pub(crate) carried: BTreeMap<&'a str, CarriedProjection>,
    pub(crate) reached: BTreeMap<&'a str, ReachedProjection<'a>>,
    pub(crate) verdict: crate::reachability::authority::Verdict,
}

/// Reusable §11 CoreIR authority card projection shared by `hirsel audit`,
/// registry `check_library`, and Slice I's audit output.
pub(crate) fn project_card(card: &Card) -> CardProjection<'_> {
    let carried = card
        .exports
        .iter()
        .filter_map(|(export, info)| {
            let carried = info.carried;
            (carried.takes | carried.returns != 0).then(|| {
                (
                    export.as_str(),
                    CarriedProjection {
                        takes: carried_names(carried.takes),
                        returns: carried_names(carried.returns),
                    },
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let reached = card
        .exports
        .iter()
        .map(|(export, info)| {
            (
                export.as_str(),
                ReachedProjection {
                    kinds: names(info.reached.kinds),
                    provided: names(info.reached.provided),
                    contained: &info.reached.contained,
                    unresolved: info.reached.unresolved.as_deref(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    CardProjection {
        carried,
        reached,
        verdict: card.verdict(),
    }
}

pub(crate) fn card_json(card: &Card) -> serde_json::Value {
    serde_json::to_value(project_card(card)).expect("authority card projection serializes")
}

fn print_card(card: &Card) {
    println!("  carried authority");
    for (name, export) in &card.exports {
        let carried = export.carried;
        if carried.takes | carried.returns != 0 {
            println!(
                "    {name}: takes [{}], returns [{}]",
                carried_names(carried.takes).join(", "),
                carried_names(carried.returns).join(", ")
            );
        }
    }
    println!("  reached kinds");
    for (name, export) in &card.exports {
        let reached = &export.reached;
        print!("    {name}: [{}]", names(reached.kinds).join(", "));
        if reached.provided != 0 {
            print!(" (provides {})", names(reached.provided).join(", "));
        }
        for module in &reached.contained {
            print!(" (contained {module})");
        }
        if let Some(error) = &reached.unresolved {
            print!(" ({error})");
        }
        println!();
    }
    let verdict = match card.verdict() {
        crate::reachability::authority::Verdict::Status(status) => status.to_owned(),
        crate::reachability::authority::Verdict::Kinds(kinds) => kinds.join(", "),
    };
    println!("  verdict: {verdict}");
}

pub(crate) fn run(request: Request<AuditOption>) -> Result<i32, Failure> {
    let directory = request
        .positional
        .as_deref()
        .map_or(Path::new("."), Path::new);
    let path = request
        .options
        .iter()
        .filter_map(|option| match option {
            AuditOption::PackageStore(path) => Some(PathBuf::from(path)),
            _ => None,
        })
        .last();
    let json = request.options.contains(&AuditOption::Json);
    let workspace =
        Workspace::discover_for(directory, false)?.ok_or("audit requires a package manifest")?;
    let frozen = request.options.contains(&AuditOption::Frozen);
    let mode = if frozen {
        crate::package_lock::Mode::Frozen
    } else {
        crate::package_lock::Mode::Update
    };
    let store = PackageStore::with_transport(path, mode.transport());
    let mut registry = crate::package_add::registry_select::Registry::new(&store)?;
    let resolution = Resolution::resolve(workspace, store, frozen)?;
    let selected = resolution.workspace.member.as_ref();
    let findings = registry.graph(&resolution, selected)?;
    let affected = findings
        .values()
        .any(|f| f.status != "active" || !f.advisories.is_empty());
    let mut checked = check(&resolution, selected)?;
    let mut bindings = bindings(&resolution);
    let mut packages = Vec::new();
    for (id, row) in make_rows(&resolution) {
        let (Some(version), Some(bindings)) = (row.version, bindings.remove(&id)) else {
            continue;
        };
        let finding = findings.get(&id);
        let card = checked.cards.remove(&id.compiler_id());
        if card.is_none() && finding.is_none() {
            continue;
        }
        if selected == Some(&id) {
            continue;
        }
        let root = row.root.map(|root: Root| root.to_string());
        if json {
            packages.push(serde_json::json!({
                "id": id.to_string(), "version": version, "root": root, "source": row.source,
                "registry": finding, "bindings": bindings, "tested": row.tested, "card": card.as_ref().map(card_json),
            }));
        } else {
            println!(
                "{id} {version} {} ({})",
                root.as_deref().unwrap_or("member"),
                row.source
            );
            for binding in bindings {
                println!(
                    "  {}{}",
                    binding.row,
                    binding.authority.map_or_else(String::new, |kinds| {
                        format!(" authority = [{}]", kinds.join(", "))
                    })
                );
            }
            for tested in row.tested {
                println!("  tested: {tested}");
            }
            if let Some(finding) = finding {
                finding.report(&id);
            }
            if let Some(card) = card {
                print_card(&card);
            }
        }
    }
    if json {
        println!("{}", serde_json::json!({"packages": packages}));
    }
    for error in &checked.authority {
        eprintln!("hirsel: {error}");
    }
    Ok(if !affected && checked.authority.is_empty() {
        0
    } else {
        2
    })
}
