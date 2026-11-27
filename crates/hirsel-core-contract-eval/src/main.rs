//! Test backend for core/tools/std_native.py. Selection and oracles live in the walker.
use hirsel_core_driver::{CheckProduct, CheckRequest, Driver, LibraryInput, Meter};
use hirsel_core_eval as eval;
use hirsel_core_ids::{
    DefPath,
    launch::{DEFAULT_N_PLACE, Metadata},
};
use hirsel_core_ir::{Identity, Module};
use hirsel_core_layout::{Layout, case_of, compute};
use hirsel_core_resolve::CorpusManifest;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
hirsel_guard::install!();

// This is an explicit test-run budget, shared by every task of one law. It bounds
// user loops independently of process scheduling or the host's current load.
const INSTRUCTIONS: u64 = 1_000_000_000;

/// What one law's run is given: its budget, whether it records events and its launch identity.
struct Setting {
    instructions: u64,
    root_limit: u64,
    events: bool,
    launch: Option<Metadata>,
    meter: Option<eval::StepMeter>,
}

fn evaluate(
    driver: &mut Driver,
    store: &Path,
    manifest: &Path,
    entry: DefPath,
    program_entry: bool,
    setting: Setting,
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
        run(&module, function, &revisions, setting, &product)
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
    revisions: &eval::Revisions,
    setting: Setting,
    product: &CheckProduct,
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
    // Sites as a native link binds them to this request's sources.
    let anchored = hirsel_core_ir::source::anchored_items(module);
    let sources = product.native_sources(&anchored).ok();
    let sites = match &sources {
        Some(sources) => sources.item_sites(&module.items)?,
        None => Vec::new(),
    };
    let (report, checked) = eval::run_entry_with_meter(
        module,
        function,
        &handles,
        setting.root_limit,
        eval::Nans::Scrambled,
        destination.as_deref_mut(),
        setting.instructions,
        revisions,
        setting.events,
        &sites,
        setting.launch.as_ref(),
        None,
        setting.meter,
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
    let positions = std::cell::OnceCell::new();
    let position = |site: Option<hirsel_core_ir::Anchor>| {
        let site = site?;
        let sources = sources.as_ref()?;
        let (file, line, column) = positions
            .get_or_init(|| sources.positions())
            .position(&module.items[site.item.index()], site.start)?;
        Some((&file.path, line, column))
    };
    let sites = report
        .sites
        .iter()
        .map(|site| position(*site))
        .collect::<Vec<_>>();
    Ok(
        json!({"outcome":outcome, "used":report.used, "cause_sites":sites,
        "steps":report.steps, "step_floor":report.step_floor, "refusal_site":position(report.step_refusal),
        "stdout":report.stdout, "stderr":report.stderr,
        "events":report.events.iter().map(|e| [u64::from(e.kind), e.task, e.object, e.value]).collect::<Vec<_>>(),
        "event_sources":report.events.iter().map(|e| e.source.map(|s| module.items[s.item.index()].to_string())).collect::<Vec<_>>(),
        "checked":{"loans":checked.loans, "instructions":checked.instructions,
                   "task_stack_peak":checked.task_stack_peak, "task_call_depth":checked.task_call_depth}}),
    )
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
        let launch = input["launch_image"]
            .as_str()
            .map(|image| {
                // The paired native test runner uses the driver's default target.
                let launch = Metadata::artifact(
                    Path::new(image),
                    "x86_64-unknown-linux-gnu",
                    DEFAULT_N_PLACE,
                    false,
                );
                if let Some(reason) = launch.unavailable() {
                    Err(format!("test image identity: {reason}"))
                } else {
                    Ok(launch)
                }
            })
            .transpose()?;
        let run = || {
            evaluate(
                driver,
                store,
                Path::new(manifest),
                entry,
                input["program_entry"].as_bool().unwrap_or(false),
                Setting {
                    instructions: input["instructions"].as_u64().unwrap_or(INSTRUCTIONS),
                    root_limit: input["root_limit"].as_u64().unwrap_or(u64::MAX),
                    launch,
                    // CONC-10 events only for a law that reads them: a flood makes millions.
                    events: input["events"].as_bool().unwrap_or(false),
                    meter: input["meter"]
                        .as_bool()
                        .filter(|&enabled| enabled)
                        .map(|_| eval::StepMeter {
                            allowance: input["meter_limit"].as_u64(),
                        }),
                },
            )
        };
        let report = if let Some(bytes) = input["task_stack_bytes"].as_u64() {
            eval::with_task_stack(usize::try_from(bytes).map_err(|e| e.to_string())?, run)
        } else {
            run()
        }?;
        if let Some(out) = input["sdk_out"].as_str() {
            if report["outcome"] != json!({"kind":"returned","values":[1]}) {
                return Err(format!("Frame schema export failed: {report}"));
            }
            let bytes: Vec<u8> = report["stdout"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_u64().unwrap() as u8)
                .collect();
            if bytes != hirsel_core_driver::sdk_command::FRAME_SCHEMA {
                return Err("evaluator Frame schema differs from frame-schema.bin".into());
            }
            let description = std::fs::read(
                input["sdk_describe"]
                    .as_str()
                    .ok_or("expected sdk_describe")?,
            )
            .map_err(|e| e.to_string())?;
            let description =
                hirsel_core_driver::sdk::Describe::read(&description).map_err(|e| e.to_string())?;
            hirsel_core_driver::sdk::write_with_frame(&description, &bytes, Path::new(out))
                .map_err(|e| e.to_string())?;
        }
        Ok(report)
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
