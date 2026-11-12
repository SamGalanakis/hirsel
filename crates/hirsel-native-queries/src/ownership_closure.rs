//! Executable reachability over already C-validated diagnostic bodies. Like
//! native/link.rs's object closure, direct calls and named function references
//! retain their exact instances. Closure code, codec checks, static callable
//! relocations and runtime finalizers are dependencies too.
use hirsel_lir::{
    ir::Inst,
    stage::{Ready, StageOutput},
};
use hirsel_native::ReadyProgram;
use hirsel_syntax::{ContentHash, DefPath};
use std::collections::{BTreeMap, BTreeSet};

pub fn select(
    program: &ReadyProgram,
    units: &[&StageOutput<Ready>],
    entry: &DefPath,
) -> Result<BTreeSet<ContentHash>, String> {
    let bodies = units
        .iter()
        .map(|unit| {
            let body = unit.unit();
            (
                (body.header.definition.clone(), body.header.instance),
                *unit,
            )
        })
        .collect::<BTreeMap<_, _>>();
    if bodies.len() != units.len() || program.summaries.len() != units.len() {
        return Err("ownership executable scope differs from checked bodies".into());
    }
    let instance = *program
        .functions
        .get(entry)
        .ok_or("ownership executable has no checked entry")?;
    let mut pending = vec![(entry.clone(), instance)];
    for instance in program.finalizers.values() {
        let summary = program
            .summaries
            .iter()
            .find(|summary| summary.instance == *instance)
            .ok_or("ownership finalizer has no checked summary")?;
        pending.push((summary.definition.clone(), *instance));
    }
    let mut selected = BTreeSet::new();
    while let Some((definition, instance)) = pending.pop() {
        let body = bodies
            .get(&(definition.clone(), instance))
            .ok_or_else(|| {
                format!("ownership executable dependency has no C-admitted body: {definition}")
            })?
            .unit();
        if !selected.insert(instance) {
            continue;
        }
        let summary = program
            .summaries
            .iter()
            .find(|summary| summary.definition == definition && summary.instance == instance)
            .ok_or("ownership executable body has no checked summary")?;
        let mut dependency = |definition: &DefPath,
                              instance: Option<ContentHash>|
         -> Result<(), String> {
            let instance = instance.ok_or("ownership executable dependency is unspecialized")?;
            pending.push((definition.clone(), instance));
            Ok(())
        };
        for symbol in summary.calls.iter().chain(&summary.references) {
            dependency(&symbol.definition, symbol.instance)?;
        }
        for (path, instance) in summary.glue.entries.iter().chain(&summary.glue.callbacks) {
            dependency(path, *instance)?;
        }
        for check in summary.glue.codecs.iter().flat_map(|codec| &codec.checks) {
            dependency(&check.definition, check.instance)?;
        }
        for instruction in &body.instructions {
            if let Inst::Closure { closure, .. } = *instruction {
                let code = body.tables.closures[closure.index()]
                    .code
                    .ok_or("ownership executable closure has no code")?;
                let symbol = &body.tables.symbols[code.symbol.index()];
                dependency(&symbol.definition, symbol.instance)?;
            }
        }
    }
    Ok(selected)
}
