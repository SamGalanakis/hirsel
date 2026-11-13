//! Check/lower a program request through the native path, without native emission.
//! Kept separate from profiling examples so the ordinary feature graph is measured.
use hirsel_core_driver::{CheckRequest, Driver, LibraryInput, Meter, PackageProduct};
use hirsel_core_ids::DefPath;
use std::path::{Path, PathBuf};
hirsel_guard::install!();
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if (args.len() == 2 || args.len() == 3) && args[0] == "produce" {
        let mut meter = Meter::new(false);
        PackageProduct::bundled(Path::new(&args[1]), &mut meter)?;
        if args.len() == 3 {
            let mut inputs: hirsel_core_resolve::CorpusManifest =
                serde_json::from_slice(&std::fs::read(&args[2])?)?;
            inputs
                .packages
                .retain(|p| ["core", "std"].contains(&p.package.name.as_str()));
            Driver::default().check_reachable_with_product(
                CheckRequest {
                    inputs,
                    entry: None,
                    store: Path::new(&args[1]).join("prepare"),
                    library: LibraryInput::Sources,
                },
                Path::new(&args[1]),
                &mut meter,
            )?;
        } else {
            PackageProduct::bundled_std(Path::new(&args[1]), &mut meter)?;
        }
        return Ok(());
    }
    if args.len() != 6 && args.len() != 7 {
        return Err(
            "usage: hirsel-core-request-cost INPUTS ENTRY STORE PRODUCT-CACHE core|std METRICS [INSTANCE-DETAILS]"
                .into(),
        );
    }
    let mut meter = Meter::new(true);
    let request = CheckRequest {
        inputs: serde_json::from_slice(&std::fs::read(&args[0])?)?,
        entry: Some(DefPath::from_full(&args[1]).ok_or("invalid entry")?),
        store: PathBuf::from(&args[2]),
        library: LibraryInput::Sources,
    };
    meter.finish("startup/discovery");
    let mut driver = Driver::default();
    let checked = match args[4].as_str() {
        "core" => {
            driver.check_reachable_with_core_product(request, Path::new(&args[3]), &mut meter)?
        }
        "std" => driver.check_reachable_with_product(request, Path::new(&args[3]), &mut meter)?,
        _ => return Err("expected core or std product".into()),
    };
    let accepted = checked.accepted();
    let library_bodies_after_check = checked.image.as_ref().map_or(0, |i| i.decoded_bodies());
    let mut instance_details = Vec::new();
    if args.len() == 7 && accepted {
        use hirsel_core_ir::InstanceKey;
        use hirsel_intern::instructions::InstructionCounter;
        let counter = InstructionCounter::open()?;
        let mut previous = counter.read()?;
        let mut phases = serde_json::Map::new();
        meter.finish("instance/setup");
        checked
            .observe_instances(
                [InstanceKey {
                    definition: DefPath::from_full(&args[1]).unwrap(),
                    types: vec![],
                    dictionaries: vec![],
                }],
                100_000,
                |key, phase| {
                    let now = counter.read().unwrap();
                    if phase == "start" {
                        phases.clear();
                    } else {
                        phases.insert(phase.into(), (now - previous).into());
                    }
                    previous = counter.read().unwrap();
                    if phase == "dependencies" {
                        instance_details.push(serde_json::json!({"key":key,"costs":phases}));
                    }
                },
            )
            .map_err(|e| format!("instance collection: {e:?}"))?;
        meter.finish("instance/collection");
        for row in &mut instance_details {
            // Observation order is collection order, which need not be sorted.
            let observed: InstanceKey = serde_json::from_value(row["key"].clone())?;
            let body = checked.demand_body(&observed.definition)?;
            row["library"] = (!checked.definitions.contains_key(&observed.definition)).into();
            row["generic"] = body.check.template.is_some().into();
            row["ir_bytes"] = serde_json::to_vec(&body.ir)?.len().into();
        }
        std::fs::write(&args[6], serde_json::to_vec_pretty(&instance_details)?)?;
        meter.finish("instance/report");
    }
    println!(
        "{}",
        serde_json::json!({"accepted":accepted,"source_bytes":checked.bytes,
        "files":checked.files.len(),"rechecked":checked.rechecked.len(),
        "library_bodies_after_check":library_bodies_after_check})
    );
    meter.finish("output/render");
    std::fs::write(&args[5], serde_json::to_vec_pretty(&meter.result)?)?;
    // Match hirsel-core's process boundary: outputs remain alive until process exit.
    std::process::exit(if accepted { 0 } else { 2 });
}
