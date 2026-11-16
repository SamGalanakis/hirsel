//! Test backend for core/tools/std_native.py. Selection and oracles live in the walker.
use hirsel_core_driver::{CheckRequest, Driver, LibraryInput, Meter};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{Identity, Module, eval};
use hirsel_core_layout::{Layout, case_of, compute};
use hirsel_core_resolve::CorpusManifest;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
hirsel_guard::install!();

// This is an explicit test-run budget, shared by every task of one law. It bounds
// user loops independently of process scheduling or the host's current load.
const INSTRUCTIONS: u64 = 1_000_000_000;

fn evaluate(
    driver: &mut Driver,
    store: &Path,
    manifest: &Path,
    entry: DefPath,
    program_entry: bool,
    instructions: u64,
) -> Result<Value, String> {
    let inputs: CorpusManifest =
        serde_json::from_slice(&std::fs::read(manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let product = driver
        .check(
            CheckRequest {
                manifest: Some(
                    manifest
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .to_owned(),
                ),
                inputs,
                store: store.to_owned(),
                library: LibraryInput::Sources,
                entry: program_entry.then(|| entry.clone()),
            },
            &mut Meter::new(false),
        )
        .map_err(|e| e.to_string())?;
    let root = Identity::plain(entry);
    let units = product
        .units([root.clone()], 65_536)
        .map_err(|e| format!("{e:?}"))?;
    let revision = units.revision(&root).unwrap();
    // PROG-9: the revisions a native link of this walk gives its carriers, so a
    // described item has the same revision here as on every other engine.
    let revisions: eval::Revisions = units
        .revisions()
        .ok_or("a records-free walk retains every reached unit")?
        .into_iter()
        .map(|(identity, revision)| (identity.digest(), revision))
        .collect();
    let joined = hirsel_core_materialize::join(&units.modules().map_err(|e| e.to_string())?, &root);
    let (module, function) = match joined {
        Ok(joined) => joined,
        Err(error) => return Ok(json!({"error":error, "revision":revision})),
    };
    if let Err(error) = hirsel_core_ir::wf::module(&module) {
        return Ok(json!({"error":format!("not well formed: {error}"), "revision":revision}));
    }
    let mut result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(&module, function, instructions, &revisions)
    })) {
        Ok(result) => result?,
        Err(fault) => fault_report(fault),
    };
    result["revision"] = json!(revision);
    Ok(result)
}

fn run(
    module: &Module,
    function: hirsel_core_ir::FuncId,
    instructions: u64,
    revisions: &eval::Revisions,
) -> Result<Value, String> {
    let signature = &module.signatures[module.functions[function.index()].signature.index()];
    let recipes = compute(&module.types).map_err(|e| format!("layouts: {e:?}"))?;
    let result_layout = signature
        .dest
        .map(|ty| match &recipes[ty.index()].layout {
            Layout::Fixed(fixed) => Ok(fixed),
            Layout::Compact(_) => Err("an entry result has a fixed layout".to_string()),
        })
        .transpose()?;
    let mut destination = result_layout.map(|fixed| vec![0; fixed.size as usize]);
    // A capability parameter gets handle 0 of a fresh model world. A Result's
    // leading destination is supplied separately by its caller, never a wrapper.
    let handles = vec![0; signature.params.len()];
    let (report, checked) = eval::run_nans_into(
        module,
        function,
        &handles,
        u64::MAX,
        eval::Nans::Scrambled,
        destination.as_deref_mut(),
        instructions,
        revisions,
    );
    let outcome = match report.outcome {
        eval::Outcome::Returned(values) => {
            let values: Vec<u64> = match (result_layout, destination) {
                (Some(layout), Some(bytes)) => {
                    let tag = layout.tag.ok_or("an entry returns a Result variant")?;
                    let start = tag.offset as usize;
                    let mut word = [0; 8];
                    word[..tag.width as usize]
                        .copy_from_slice(&bytes[start..start + tag.width as usize]);
                    vec![u64::from(case_of(&tag, u64::from_le_bytes(word)) == 0)]
                }
                _ => signature
                    .results
                    .iter()
                    .zip(values)
                    .map(|(&t, x)| eval::observed(t, x))
                    .collect(),
            };
            json!({"kind":"returned", "values":values})
        }
        eval::Outcome::Failed(causes) => json!({"kind":"failed", "causes":causes}),
        eval::Outcome::Stopped(name) => json!({"kind":"stopped", "name":name}),
    };
    Ok(json!({"outcome":outcome, "used":report.used,
        "stdout":report.stdout, "stderr":report.stderr,
        "events":report.events.iter().map(|e| [u64::from(e.kind), e.task, e.object, e.value]).collect::<Vec<_>>(),
        "checked":{"loans":checked.loans, "instructions":checked.instructions}}))
}
fn fault_report(fault: Box<dyn std::any::Any + Send>) -> Value {
    let message = fault
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| fault.downcast_ref::<&str>().copied())
        .unwrap_or("non-string evaluator fault");
    json!({"outcome":{"kind":"fault", "message":message}})
}
fn request(driver: &mut Driver, store: &Path, input: &Value) -> Value {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let manifest = input["manifest"].as_str().ok_or("expected manifest")?;
        let entry = DefPath::from_full(input["entry"].as_str().ok_or("expected entry")?)
            .ok_or("invalid entry")?;
        evaluate(
            driver,
            store,
            Path::new(manifest),
            entry,
            input["program_entry"].as_bool().unwrap_or(false),
            input["instructions"].as_u64().unwrap_or(INSTRUCTIONS),
        )
    }));
    match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => json!({"error":error}),
        Err(fault) => fault_report(fault),
    }
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let store = tempfile::tempdir().expect("evaluator store");
    let mut driver = Driver::default();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 {
        let report = request(
            &mut driver,
            store.path(),
            &json!({"manifest":args[0], "entry":args[1]}),
        );
        println!("{report}");
        if report.get("error").is_some() {
            std::process::exit(1);
        }
    } else if args.is_empty() {
        // One resident process and store per gate, one fresh model world per law.
        for line in std::io::stdin().lock().lines() {
            let result = line
                .map_err(|e| e.to_string())
                .and_then(|line| serde_json::from_str::<Value>(&line).map_err(|e| e.to_string()));
            let report = match result {
                Ok(input) => request(&mut driver, store.path(), &input),
                Err(error) => json!({"error":error}),
            };
            println!("{report}");
            std::io::stdout().flush().expect("evaluator response");
        }
    } else {
        eprintln!("expected inputs.json and entry, or JSON requests on stdin");
        std::process::exit(2);
    }
}
