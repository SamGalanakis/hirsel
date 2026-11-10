//! Engine facts for production entry selection and admission.
use crate::DefinitionQueries;
use hirsel_engine_admission::{callee_roots, identified_entry};
use hirsel_run_schema::{CalleeFacts, CodeReach};
use std::collections::BTreeMap;

/// What a runner call carries for each entry `entry`'s closure names (RUN-3, RUN-8,
/// RUN-11): every function reference to a definition with an entry's identity, with its
/// own program revision, definition hash, signature fingerprint and reach (CAP-6). The
/// hash and fingerprint are also what `runner.entry_id` reads.
pub fn callee_facts(
    engine: &mut crate::QueryEngine,
    entry: &hirsel_syntax::DefPath,
) -> Result<BTreeMap<String, CalleeFacts>, hirsel_types::QueryError> {
    let hashes = engine.definition_hashes([entry.clone()])?;
    let mut facts = BTreeMap::new();
    for site in hashes.call_sites.values().flatten() {
        let path = site.callee.to_string();
        if !matches!(site.kind, hirsel_proof_export::CallSiteKind::Reference(_))
            || facts.contains_key(&path)
        {
            continue;
        }
        let signature = engine.item_signature(&site.callee)?.value;
        if !identified_entry(engine.types(), &signature) {
            continue;
        }
        let program = engine.semantic_program_revision(&site.callee)?.digest();
        let reach = match engine.unsafe_reach(&site.callee)? {
            None => CodeReach::Safe,
            Some(reach) => CodeReach::Escapes(reach.spelled()),
        };
        facts.insert(
            path,
            CalleeFacts {
                program,
                code: site.hash.0,
                fingerprint: signature.fingerprint.0,
                reach,
            },
        );
    }
    Ok(facts)
}

/// The facts a runner host checks an arriving call against (RUN-8) for one definition it
/// holds, `path`, and the call form its signature takes: none when it is not a callee entry.
pub fn entry_facts(
    engine: &mut crate::QueryEngine,
    path: &hirsel_syntax::DefPath,
) -> Result<Option<(CalleeFacts, hirsel_run_schema::CallForm)>, hirsel_types::QueryError> {
    let signature = engine.item_signature(path)?.value;
    let Ok(roots) = callee_roots(engine.types(), &signature) else {
        return Ok(None);
    };
    let hashes = engine.definition_hashes([path.clone()])?;
    let Some(code) = hashes.hashes.get(path) else {
        return Ok(None);
    };
    let program = engine.semantic_program_revision(path)?.digest();
    let reach = match engine.unsafe_reach(path)? {
        None => CodeReach::Safe,
        Some(reach) => CodeReach::Escapes(reach.spelled()),
    };
    let form = if roots.is_empty() {
        hirsel_run_schema::CallForm::Compute
    } else {
        hirsel_run_schema::CallForm::Call
    };
    Ok(Some((
        CalleeFacts {
            program,
            code: code.0,
            fingerprint: signature.fingerprint.0,
            reach,
        },
        form,
    )))
}

/// The callee entries a runner host serves from its program: every callee entry the
/// package of `entry` defines, with the facts an arriving call is checked against.
pub fn served_entries(
    engine: &mut crate::QueryEngine,
    entry: &hirsel_syntax::DefPath,
) -> Result<BTreeMap<String, (CalleeFacts, hirsel_run_schema::CallForm)>, hirsel_types::QueryError>
{
    let mut served = BTreeMap::new();
    for path in engine.item_paths() {
        if path.0.package != entry.0.package {
            continue;
        }
        if let Some(facts) = entry_facts(engine, &path)? {
            served.insert(path.to_string(), facts);
        }
    }
    Ok(served)
}
