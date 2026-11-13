//! Test backend for core/tools/std_native.py. Selection and oracles live in the walker.
use hirsel_core_front::{Def, Emitter, ModuleInput, Program};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::eval;
use hirsel_core_resolve::CorpusManifest;
use serde_json::{Value, json};
use std::collections::HashSet;
hirsel_guard::install!();

fn evaluate() -> Result<Value, String> {
    let mut args = std::env::args().skip(1);
    let manifest = args.next().ok_or("expected inputs.json and entry")?;
    let entry = DefPath::from_full(&args.next().ok_or("missing entry")?).ok_or("invalid entry")?;
    if args.next().is_some() {
        return Err("expected inputs.json and entry".into());
    }
    let inputs: CorpusManifest =
        serde_json::from_slice(&std::fs::read(manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let corpus = inputs.load().map_err(|e| e.to_string())?;
    let parsed = corpus.parse();
    for (source, output) in corpus.sources.iter().zip(&parsed.files) {
        if !output.diagnostics.is_empty() || output.resource_refusal.is_some() {
            return Err(format!(
                "{}: {:?} {:?}",
                source.file.display(),
                output.diagnostics,
                output.resource_refusal
            ));
        }
    }
    let index = corpus.index(&parsed);
    let modules = corpus
        .sources
        .iter()
        .zip(parsed.files)
        .map(|(source, output)| ModuleInput {
            id: index.module(&source.module).unwrap(),
            syntax: output.syntax,
        })
        .collect::<Vec<_>>();
    let program = Program::declare(&index, &modules);
    if !program.refusals.is_empty() || !program.gaps.is_empty() {
        return Err(format!("{:?} {:?}", program.refusals, program.gaps));
    }
    let Some(Def::Fn(root)) = program.by_path.get(&entry).copied() else {
        return Err("entry is not a function".into());
    };
    let mut emitter = Emitter::new();
    let mut checked = HashSet::new();
    let mut pending = vec![root];
    let mut function = None;
    // As in the existing front evaluator assembly: check each demanded source body
    // once, then mechanically instantiate templates, including newly demanded bodies.
    loop {
        while let Some(f) = pending.pop() {
            if !checked.insert(f) {
                continue;
            }
            let check = program.check_lower(f as usize, &mut emitter);
            if !check.refusals.is_empty() || !check.gaps.is_empty() {
                return Err(format!("{}: {check:?}", program.fns[f as usize].path));
            }
            if f == root {
                function = check.function;
            }
        }
        emitter.materialize_instances()?;
        for row in &emitter.module.functions {
            let key = &emitter.module.instances[row.instance.index()];
            if let Some(Def::Fn(f)) = program.by_path.get(&key.definition)
                && !checked.contains(f)
                && program.fns[*f as usize].body.is_some()
            {
                pending.push(*f);
            }
        }
        if pending.is_empty() {
            break;
        }
    }
    let function = function.ok_or("entry body is unavailable")?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eval::run(&emitter.module, function, &[], u64::MAX)
    }));
    std::panic::set_hook(hook);
    Ok(match result {
        Ok(report) => json!({"outcome": match report.outcome {
            eval::Outcome::Returned(values) => json!({"kind":"returned", "values":values}),
            eval::Outcome::Failed(causes) => json!({"kind":"failed", "causes":causes}),
        }, "used":report.used, "stdout":report.stdout, "stderr":report.stderr}),
        Err(fault) => {
            let message = fault
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| fault.downcast_ref::<&str>().copied())
                .unwrap_or("non-string evaluator fault");
            json!({"outcome":{"kind":"fault", "message":message}})
        }
    })
}
fn main() {
    match evaluate() {
        Ok(result) => println!("{result}"),
        Err(message) => {
            println!("{}", json!({"error":message}));
            std::process::exit(1);
        }
    }
}
