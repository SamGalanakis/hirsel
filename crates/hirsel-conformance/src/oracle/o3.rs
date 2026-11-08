//! O3, recompute counts: across a resident edit, the definitions a scenario
//! names `unchanged` recompute nothing, and those it names `recomputed` do (so
//! the scenario exercises what it claims).
//!
//! Counts include physical Lower/body V work.
use super::{Report, Verdict, edits::Scenario};
use hirsel_driver::session::FileChange;
use hirsel_driver::session::Session;
use hirsel_query_products::QueryKind;
use std::sync::Arc;

const KINDS: [QueryKind; 5] = [
    QueryKind::Resolve,
    QueryKind::Signature,
    QueryKind::Check,
    QueryKind::Modes,
    QueryKind::Lower,
];

pub fn run(scenarios: &[Scenario]) -> Report {
    let mut report = Report::new("O3");
    for scenario in scenarios {
        if let Err(why) = hirsel_intern::jobs::with_compiler_stack(|| one(scenario, &mut report)) {
            report.add(format!("{}: setup", scenario.name), Verdict::Fail(why));
        }
    }
    report
}

fn one(scenario: &Scenario, report: &mut Report) -> Result<(), String> {
    let single = |tree: &std::collections::BTreeMap<String, String>| match tree.get(&scenario.entry)
    {
        Some(text) if tree.len() == 1 && !scenario.with_std => Ok(text.clone()),
        _ => Err("O3 scenarios are one standalone file today".to_string()),
    };
    let (before, after) = (single(&scenario.before)?, single(&scenario.after)?);
    if scenario.config_before != scenario.config_after {
        return Err("O3 scenarios edit source only".into());
    }
    let context = hirsel_engine::default_context();
    let base = std::env::temp_dir().join(format!(
        "hirsel-o3-{}-{}",
        std::process::id(),
        scenario.name
    ));
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let entry = base.join(&scenario.entry);
    std::fs::write(&entry, &before).map_err(|e| e.to_string())?;
    let config = || {
        let mut config = scenario.config_before.compile(super::cold_store());
        // A check lowers only compile-time evaluation.
        config.lowering = hirsel_query_products::LoweringPolicy::Comptime;
        config
    };
    let mut session = Session::open_configured(&entry, config());
    session.check().map_err(|e| e.to_string())?;
    let names: Vec<&String> = scenario
        .unchanged
        .iter()
        .chain(&scenario.recomputed)
        .collect();
    let definitions = names
        .iter()
        .map(|name| {
            let mut path = hirsel_engine::entry_path(&context);
            path.0.name = name.as_str().into();
            path
        })
        .collect::<Vec<_>>();
    let inspect =
        |session: &mut Session| -> Result<Arc<hirsel_requests::CompileInspection>, String> {
            let selected = session
                .request_applied(hirsel_requests::CompileOperation::Inspect {
                    definitions: definitions.clone(),
                })
                .map_err(|error| error.to_string())?;
            match &selected.result {
                hirsel_requests::CompileResult::Inspect(inspection) => Ok(inspection.clone()),
                _ => Err("inspection request returned another operation".into()),
            }
        };
    let counts =
        |inspection: &hirsel_requests::CompileInspection| -> Result<Vec<[u64; 5]>, String> {
            definitions
                .iter()
                .map(|definition| {
                    let work = inspection
                        .definitions
                        .iter()
                        .find(|work| &work.definition == definition)
                        .ok_or_else(|| format!("inspection omitted definition {definition}"))?;
                    let mut result = [0; 5];
                    for (slot, kind) in result.iter_mut().zip(KINDS) {
                        *slot = work
                            .recomputations
                            .iter()
                            .find(|(observed, _)| *observed == kind)
                            .ok_or_else(|| format!("inspection omitted {kind:?} for {definition}"))?
                            .1;
                    }
                    Ok(result)
                })
                .collect()
        };
    let validations =
        |inspection: &hirsel_requests::CompileInspection| -> Result<Vec<(u64, u64)>, String> {
            definitions
                .iter()
                .map(|definition| {
                    let work = inspection
                        .definitions
                        .iter()
                        .find(|work| &work.definition == definition)
                        .ok_or_else(|| {
                            format!("inspection omitted physical work for {definition}")
                        })?;
                    Ok((work.body_validations, work.lower_calls))
                })
                .collect()
        };
    let initial = inspect(&mut session)?;
    let first = counts(&initial)?;
    let first_work = validations(&initial)?;
    std::fs::write(&entry, &after).map_err(|e| e.to_string())?;
    session.apply(vec![FileChange {
        path: entry.clone(),
        bytes: Some(after.into_bytes()),
    }]);
    session.check_applied().map_err(|e| e.to_string())?;
    let second_inspection = inspect(&mut session)?;
    let second = counts(&second_inspection)?;
    let second_work = validations(&second_inspection)?;
    for ((((name, first), second), first_work), second_work) in names
        .iter()
        .zip(first)
        .zip(second)
        .zip(first_work)
        .zip(second_work)
    {
        let mut grew: Vec<String> = KINDS
            .iter()
            .zip(first.iter().zip(second))
            .filter(|(_, (a, b))| b > *a)
            .map(|(kind, (a, b))| format!("{} +{}", kind.phase_name(), b - a))
            .collect();
        if second_work.0 > first_work.0 {
            grew.push(format!("body V +{}", second_work.0 - first_work.0));
        }
        if second_work.1 > first_work.1 {
            grew.push(format!("physical lower +{}", second_work.1 - first_work.1));
        }
        let unchanged = scenario.unchanged.contains(name);
        let verdict = match (unchanged, grew.is_empty()) {
            (true, true) | (false, false) => Verdict::Pass,
            (true, false) => Verdict::Fail(format!("recomputed: {}", grew.join(", "))),
            (false, true) => Verdict::Fail("recomputed nothing, so the edit misses it".into()),
        };
        let claim = if unchanged { "unchanged" } else { "recomputed" };
        report.add(format!("{} [{name} {claim}]", scenario.name), verdict);
    }
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}
