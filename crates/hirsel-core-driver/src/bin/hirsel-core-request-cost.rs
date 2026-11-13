//! Check/lower a program request through the native path, without native emission.
//! Kept separate from profiling examples so the ordinary feature graph is measured.
use hirsel_core_driver::{CheckRequest, Driver, LibraryInput, Meter, PackageProduct};
use hirsel_core_ids::DefPath;
use std::path::{Path, PathBuf};
hirsel_guard::install!();
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "produce" {
        let mut meter = Meter::new(false);
        PackageProduct::bundled(Path::new(&args[1]), &mut meter)?;
        PackageProduct::bundled_std(Path::new(&args[1]), &mut meter)?;
        return Ok(());
    }
    if args.len() != 6 {
        return Err("usage: hirsel-core-request-cost INPUTS ENTRY STORE PRODUCT-CACHE core|std METRICS".into());
    }
    let mut meter = Meter::new(true);
    let request = CheckRequest {
        inputs: serde_json::from_slice(&std::fs::read(&args[0])?)?,
        entry: Some(DefPath::from_full(&args[1]).ok_or("invalid entry")?),
        store: PathBuf::from(&args[2]), library: LibraryInput::Sources,
    };
    meter.finish("startup/discovery");
    let mut driver = Driver::default();
    let checked = match args[4].as_str() {
        "core" => driver.check_reachable_with_core_product(request, Path::new(&args[3]), &mut meter)?,
        "std" => driver.check_reachable_with_product(request, Path::new(&args[3]), &mut meter)?,
        _ => return Err("expected core or std product".into()),
    };
    let accepted = checked.accepted();
    println!("{}", serde_json::json!({"accepted":accepted,"source_bytes":checked.bytes,
        "files":checked.files.len(),"rechecked":checked.rechecked.len()}));
    meter.finish("output/render");
    std::fs::write(&args[5], serde_json::to_vec_pretty(&meter.result)?)?;
    // Match hirsel-core's process boundary: outputs remain alive until process exit.
    std::process::exit(if accepted { 0 } else { 2 });
}
