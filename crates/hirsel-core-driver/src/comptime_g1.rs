//! The comptime G1 probe (COMPTIME-DIRECTION 3.2): host instructions retired on
//! the pure thread for crc, regex and three small loops, on each engine, with
//! the module prepared once. It is a measurement, not a test, and builds only
//! with the `comptime-g1` feature:
//!
//! ```text
//! taskset -c 8 cargo test -p hirsel-core-driver --release --features comptime-g1 \
//!     --lib comptime_g1 -- --nocapture
//! ```
//!
//! `COMPTIME_G1_RUNS` (default 3) sets the runs per probe, `COMPTIME_G1_ENGINE`
//! (`Flat` or `Tree`) keeps one engine, `COMPTIME_G1_PROBE` (`regex_probe`, ...)
//! one probe, `COMPTIME_G1_PREPARE_EACH` moves the
//! module's preparation into the measured window, and `COMPTIME_G1=<file>`
//! writes the rows as JSON. Native regex is 453,268 host instructions, so G1's
//! 150x is 68.0M; crc's limit is 120 host instructions per executed IR.
use crate::*;

const APP: &str = r#"
pub fn table() -> Inline[u32, 2048] {
    let mut t: Inline[u32, 2048] =[0; 2048]
    let mut i: u64 = 0
    let mut n: u32 = 0
    while i < 256 {
        let mut c = n
        let mut bit: u64 = 0
        while bit < 8 {
            if c & 1 == 1 { c = 0xedb88320 ^ (c >> 1) } else { c = c >> 1 }
            bit += 1
        }
        t[i] = c
        i += 1
        n += 1
    }
    while i < 2048 {
        let prev = t[i - 256]
        let low: u64 = int.widen(prev & 255)
        t[i] = t[low] ^ (prev >> 8)
        i += 1
    }
    t
}
pub fn crc_probe() -> u32 {
    let t = table()
    t[257] ^ t[2047]
}
pub fn valid(p: String) {
    let created = std.regex.new(p)
    match take created {
        Ok(_) => {},
        Error(_) => core.fail("malformed regex"),
    }
}
pub fn regex_probe() -> u64 {
    valid("^(\\d{4})-(\\d{2})-(\\d{2})$")
    1
}
pub fn empty_probe() -> u64 { 1 }
fn inc(x: u64) -> u64 { x + 1 }
pub fn calls_probe() -> u64 {
    let mut i: u64 = 0
    let mut s: u64 = 0
    while i < 1000 {
        s = inc(s)
        i += 1
    }
    s
}
pub fn loop_probe() -> u64 {
    let mut i: u64 = 0
    let mut s: u64 = 0
    while i < 1000 {
        s = s + 1
        i += 1
    }
    s
}
"#;

fn product(dir: &Path) -> Arc<CheckProduct> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = CheckRequest::libraries(&root.join("core"), &dir.join("store")).unwrap();
    let file = dir.join("p.hirsel");
    std::fs::write(&file, APP).unwrap();
    request
        .inputs
        .packages
        .push(hirsel_core_resolve::PackageManifest {
            namespace: None,
            package: crate::package("p"),
            sources: vec![hirsel_core_resolve::ModuleSource {
                foreign: false,
                captured: None,
                file,
                modules: vec!["p".into()],
                signatures: false,
            }],
            dependencies: vec![crate::package("core"), crate::package("std")],
            toolchain: false,
        });
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    product
}

#[test]
fn comptime_g1_probe() {
    let runs: usize = std::env::var("COMPTIME_G1_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let only = std::env::var("COMPTIME_G1_ENGINE").ok();
    let probe = std::env::var("COMPTIME_G1_PROBE").ok();
    let prepare = std::env::var_os("COMPTIME_G1_PREPARE_EACH").is_some();
    let dir = tempfile::tempdir().unwrap();
    let product = product(dir.path());
    let mut rows = vec![];
    for name in [
        "empty_probe",
        "loop_probe",
        "calls_probe",
        "crc_probe",
        "regex_probe",
    ] {
        if probe.as_ref().is_some_and(|p| p != name) {
            continue;
        }
        let identity =
            hirsel_core_ir::Identity::plain(DefPath::from_full(&format!("p@1/p/{name}")).unwrap());
        let units = product.units([identity.clone()], 1 << 20).unwrap();
        let modules = units.modules().unwrap();
        let (module, entry) = hirsel_core_materialize::join(&modules, &identity).unwrap();
        let prepared = hirsel_core_eval::PureModule::new(&module);
        for engine in [
            hirsel_core_eval::Engine::Tree,
            hirsel_core_eval::Engine::Flat,
        ] {
            if only.as_ref().is_some_and(|v| *v != format!("{engine:?}")) {
                continue;
            }
            let mut pure = vec![];
            let mut outcome = String::new();
            for _ in 0..runs {
                let input = hirsel_core_eval::PureInput {
                    engine,
                    ..Default::default()
                };
                let (host, run) = if prepare {
                    let counter = hirsel_intern::instructions::InstructionCounter::open().unwrap();
                    let before = counter.read().unwrap();
                    let each = hirsel_core_eval::PureModule::new(&module);
                    let setup = counter.read().unwrap() - before;
                    let run = hirsel_core_eval::run_pure(&each, entry, &[], input);
                    (run.host + setup, run)
                } else {
                    let run = hirsel_core_eval::run_pure(&prepared, entry, &[], input);
                    (run.host, run)
                };
                outcome = format!("{:?}", run.outcome);
                pure.push((host, run.instructions, run.steps));
            }
            let per_ir = pure[0].0 as f64 / pure[0].1.max(1) as f64;
            eprintln!("G1 {name} {engine:?}: {outcome} {pure:?} {per_ir:.1}/IR");
            rows.push(serde_json::json!({
                "name": name,
                "engine": format!("{engine:?}"),
                "outcome": outcome,
                "pure": pure,
            }));
        }
    }
    if let Ok(out) = std::env::var("COMPTIME_G1") {
        std::fs::write(out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    }
}
