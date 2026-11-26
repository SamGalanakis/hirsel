//! Diagnostic object builder for the external M1 hot-loop C/Rust harness.
use hirsel_core_driver::{Backend, CheckRequest, Driver, LibraryInput, Meter, NativeOptions};
use hirsel_core_ids::DefPath;
use hirsel_core_resolve::CorpusManifest;
use std::{
    fs,
    path::{Path, PathBuf},
};
hirsel_guard::install!();
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let manifest: CorpusManifest = serde_json::from_slice(&fs::read(&args[0])?)?;
    let out = Path::new(&args[1]);
    fs::create_dir_all(out)?;
    let request = CheckRequest {
        manifest: None,
        inputs: manifest,
        store: out.join("checks"),
        library: LibraryInput::Sources,
        entry: None,
    };
    let checked = Driver::default().check_with_product(
        request,
        &out.join("core-product"),
        &mut Meter::new(false),
    )?;
    let mut names = serde_json::Map::new();
    let defaults = [
        "utf8",
        "sum",
        "find",
        "split",
        "parse",
        "push",
        "mem",
        "allocate",
        "find_byte",
    ];
    let roots: Vec<_> = args
        .get(4)
        .map_or_else(|| defaults.to_vec(), |names| names.split(',').collect());
    for name in roots {
        let entry = DefPath::from_full(&format!("hot@1/loops/{name}")).unwrap();
        let mut options = NativeOptions::development(
            "x86_64-unknown-linux-gnu".into(),
            PathBuf::from(&args[2]),
            hirsel_core_driver::Link::Driver(PathBuf::from(&args[3])),
            out.join("unused"),
        );
        options.backend = if args.iter().any(|s| s == "cranelift") {
            Backend::Cranelift
        } else {
            Backend::Llvm
        };
        options.optimize = true;
        options.root_limit = 1 << 30;
        options.logical_charge_report = args
            .get(5)
            .filter(|mode| mode.as_str() == "observe-charge")
            .map(|_| out.join("logical-charge.json"));
        let objects = checked.emit_reachable(&entry, &options)?;
        let mut paths = vec![];
        for (i, obj) in objects.iter().enumerate() {
            let path = out.join(format!("{name}-{i}.o"));
            fs::copy(&obj.path, &path)?;
            paths.push(path);
            if obj.instance.definition == entry {
                names.insert(
                    name.into(),
                    serde_json::json!({"symbol":format!("h{}",obj.instance.digest()),"objects":[]}),
                );
            }
        }
        names.get_mut(name).unwrap()["objects"] = serde_json::json!(paths);
        let own = &checked.definitions[&entry];
        fs::write(
            out.join(format!("{name}.core")),
            hirsel_core_ir::text::print(own.ir()?),
        )?;
        println!("{name}: {} objects", objects.len());
    }
    fs::write(out.join("symbols.json"), serde_json::to_vec_pretty(&names)?)?;
    Ok(())
}
