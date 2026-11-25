//! comptime Phase 0 (throwaway, never lands): G1 evaluator cost probes.
//! Run with `COMPTIME_G1=<out.json>`; ignored otherwise.
use super::*;
use hirsel_core_ir::{FuncId, eval};
use hirsel_intern::instructions::InstructionCounter;

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
"#;

/// User-space instructions of this thread and every thread it starts after
/// opening (perf `inherit`). A child's count joins the parent's when the child
/// exits, so read after the evaluator's scoped task threads have ended.
struct Inherited(i32);
impl Inherited {
    fn open() -> Self {
        let mut attr = [0u8; 136];
        attr[4..8].copy_from_slice(&136u32.to_ne_bytes());
        attr[8..16].copy_from_slice(&1u64.to_ne_bytes()); // instructions
        // inherit | exclude_kernel | exclude_hv
        attr[40..48].copy_from_slice(&((1u64 << 1) | (1 << 5) | (1 << 6)).to_ne_bytes());
        // SAFETY: complete perf_event_attr; pid 0, cpu -1: this thread and its children.
        let fd = unsafe {
            libc::syscall(libc::SYS_perf_event_open, attr.as_ptr(), 0, -1, -1, 8u64)
        };
        assert!(fd >= 0, "perf_event_open: {}", std::io::Error::last_os_error());
        Inherited(fd as i32)
    }
    fn read(&self) -> u64 {
        let mut v = 0u64;
        // SAFETY: 8 writable bytes; our fd.
        let n = unsafe { libc::read(self.0, (&raw mut v).cast(), 8) };
        assert_eq!(n, 8);
        v
    }
}

pub(crate) fn phase0_product(dir: &Path, app: &str) -> Arc<CheckProduct> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = CheckRequest::libraries(&root.join("core"), &dir.join("store")).unwrap();
    let file = dir.join("p.hirsel");
    std::fs::write(&file, app).unwrap();
    request.inputs.packages.push(hirsel_core_resolve::PackageManifest {
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
    let Ok(out) = std::env::var("COMPTIME_G1") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let product = phase0_product(dir.path(), APP);
    let counter = InstructionCounter::open().unwrap();
    let all = Inherited::open();
    let mut rows = vec![];
    let only = std::env::var("COMPTIME_G1_ONLY").ok();
    for name in ["empty_probe", "crc_probe", "regex_probe"] {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        let identity =
            hirsel_core_ir::Identity::plain(DefPath::from_full(&format!("p@1/p/{name}")).unwrap());
        // Check-only product: the callees' checked IR is read from the product
        // (definitions, or the image), never lowered again here.
        let units = product.units([identity.clone()], 1 << 20).unwrap();
        let reached = units.modules().unwrap().len();
        let address = units.address(&identity).map(|a| format!("{a:?}"));
        let modules = units.modules().unwrap();
        let (module, entry) = hirsel_core_materialize::join(&modules, &identity).unwrap();
        let mut samples = vec![];
        let mut result = None;
        for _ in 0..5 {
            let (before, before_all) = (counter.read().unwrap(), all.read());
            let (_, checked) =
                eval::run_nans(&module, entry, &[], u64::MAX, eval::Nans::Canonical);
            let caller = counter.read().unwrap() - before;
            let host = all.read() - before_all;
            samples.push((host, caller, checked.instructions, checked.steps));
        }
        // The pure core on one thread with the root's stack, counted on that thread.
        let mut pure = vec![];
        for _ in 0..pure_runs() {
            let (module, entry) = (&module, entry);
            let row = std::thread::scope(|s| {
                std::thread::Builder::new()
                    .stack_size(128 << 20)
                    .spawn_scoped(s, move || {
                        let c = InstructionCounter::open().unwrap();
                        let before = c.read().unwrap();
                        let (report, checked) = eval::run_pure(module, entry, u64::MAX, None);
                        let host = c.read().unwrap() - before;
                        (host, checked.instructions, checked.steps, format!("{:?}", report.outcome))
                    })
                    .unwrap()
                    .join()
                    .unwrap()
            });
            result = Some(row.3.clone());
            pure.push((row.0, row.1, row.2));
        }
        rows.push(serde_json::json!({
            "name": name,
            "reached_units": reached,
            "address": address,
            "outcome": result,
            "samples": samples,
            "pure": pure,
        }));
    }
    std::fs::write(out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}

fn pure_runs() -> usize {
    std::env::var("COMPTIME_G1_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(5)
}

/// G7: COMPTIME-DIRECTION §3.3's three `constant_call_absent` edits on one
/// resident driver. Run with `COMPTIME_G7=<out.json>`.
#[test]
fn comptime_g7_sequence() {
    let Ok(out) = std::env::var("COMPTIME_G7") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let main = "pub fn main() -> Result[(), Exit] { Error(Exit(status: b.read() + 10u8)) }";
    let reader = "pub fn read() -> u8 { a.X }\npub fn other() -> u8 { 7u8 }";
    let steps: [(&str, &str, &str); 4] = [
        ("base", "pub fn f()->u8{1u8}\npub const X:u8=f()", reader),
        ("f returns 2u8", "pub fn f()->u8{2u8}\npub const X:u8=f()", reader),
        ("f returns 0u8+1u8", "pub fn f()->u8{0u8 + 1u8}\npub const X:u8=f()", reader),
        (
            "unrelated body",
            "pub fn f()->u8{0u8 + 1u8}\npub const X:u8=f()",
            "pub fn read() -> u8 { a.X }\npub fn other() -> u8 { 8u8 }",
        ),
    ];
    let mut resident = Driver::default();
    let options = crate::native::tests::native_options(Backend::Cranelift, dir.path());
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut rows = vec![];
    for (name, a, b) in steps {
        for (file, text) in [("a.hirsel", a), ("b.hirsel", b), ("main.hirsel", main)] {
            std::fs::write(dir.path().join(file), text).unwrap();
        }
        let mut request =
            CheckRequest::libraries(&root.join("core"), &dir.path().join("store")).unwrap();
        request.inputs.packages.retain(|p| p.package == crate::package("core"));
        let source = |file: &str, module: &str| hirsel_core_resolve::ModuleSource {
            foreign: false,
            captured: None,
            file: dir.path().join(file),
            modules: vec![module.into()],
            signatures: false,
        };
        request.inputs.packages.push(hirsel_core_resolve::PackageManifest {
            namespace: None,
            package: crate::package("app"),
            sources: vec![
                source("a.hirsel", "a"),
                source("b.hirsel", "b"),
                source("main.hirsel", "main"),
            ],
            dependencies: vec![crate::package("core")],
        });
        request.entry = Some(entry.clone());
        resident.layers.law = true;
        let meter = &mut Meter::new(false);
        let before = crate::comptime::evaluations();
        let product = resident.check(request, meter).unwrap();
        let checked_evals = crate::comptime::evaluations() - before;
        let class = format!("{:?}", meter.result.layer_class);
        let accepted = product.accepted();
        let built = product.build(&entry, &options);
        let evaluations = crate::comptime::evaluations() - before;
        let row = match built {
            Ok(built) => {
                let status = std::process::Command::new(&built.executable)
                    .status()
                    .unwrap()
                    .code();
                serde_json::json!({
                    "step": name, "accepted": accepted, "gate": class,
                    "evaluations_check": checked_evals, "evaluations": evaluations,
                    "compiled": built.compiled, "exit": status,
                    "output": status.map(|s| s - 10),
                })
            }
            Err(stop) => serde_json::json!({
                "step": name, "accepted": accepted, "gate": class,
                "evaluations": evaluations, "stop": format!("{stop:?}"),
                "diagnostics": format!("{:?}", product.diagnostics),
            }),
        };
        eprintln!("{row}");
        rows.push(row);
    }
    std::fs::write(out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}
