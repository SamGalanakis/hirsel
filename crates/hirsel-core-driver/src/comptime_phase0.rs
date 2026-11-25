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
    for name in ["empty_probe", "crc_probe", "regex_probe"] {
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
            let (report, checked) =
                eval::run_nans(&module, entry, &[], u64::MAX, eval::Nans::Canonical);
            let caller = counter.read().unwrap() - before;
            let host = all.read() - before_all;
            samples.push((host, caller, checked.instructions, checked.steps));
            result = Some(format!("{:?}", report.outcome));
        }
        rows.push(serde_json::json!({
            "name": name,
            "reached_units": reached,
            "address": address,
            "outcome": result,
            "samples": samples,
        }));
    }
    std::fs::write(out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}
