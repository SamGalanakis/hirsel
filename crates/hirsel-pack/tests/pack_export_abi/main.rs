//! PACK-14 & PACK-15 compiled ABI + real loader Acceptance Gate (Cranelift + LLVM).
//!
//! Exercises all four Slice 3 laws end to end on BOTH Cranelift and LLVM:
//! 1. Overlapping frozen `view` parameters (observing actual caller arg & result
//!    addresses in the ABI harness) returning a nontrivial aggregate.
//! 2. By-value plain argument mutated before failure, partial result + pending-result
//!    finalizer failure, legal `mut` writes, cleanup trace, cause chain (including
//!    PACK-9.7 provenance across RX reclaim), and step/byte bound/payer parity against
//!    an identically compiled linked call.
//! 3. Same-carrier wrong signature + `waits` mismatch returning `NoEntry` with
//!    installed image count and slab bytes strictly unchanged.
//! 4. `code.slot`, `code.activate` (`WrongPool`, `Stale`, valid swap), `code.dispatch`
//!    (`Empty`, hit, miss), reflection (`code.identity`/`code.revision`/`code.bind`/
//!    `code.share`), and pinning across nested calls, unwind, task suspension /
//!    worker migration, and `task.spawn_unit` end publication.

use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use hirsel_code::{Loader, RawCode};
use hirsel_core_driver::{
    Backend, CheckProduct, CheckRequest, CoreImage, Driver, LibraryInput, Link, Meter,
    NativeOptions, OutputKind, PackageProduct,
};
use hirsel_core_ids::{ContentHash, DefPath, PackageId};
use hirsel_core_resolve::{CorpusManifest, ModuleSource, PackageManifest};
use hirsel_core_rt::{
    Context, abi_sites,
    causes::Causes,
    process::{self, MainResult},
    run_stop::{EndReason, Execution, run_contained_worker},
    tasks::SchedulerProvider,
};
use hirsel_mod::{Mask, Module};

hirsel_guard::install!();

const BACKENDS: [Backend; 2] = [Backend::Cranelift, Backend::Llvm];

fn alone() -> MutexGuard<'static, ()> {
    static ALONE: Mutex<()> = Mutex::new(());
    ALONE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn primary_loader() -> (&'static Loader, u64) {
    static PRIMARY: OnceLock<(&'static Loader, u64)> = OnceLock::new();
    *PRIMARY.get_or_init(|| {
        let loader = hirsel_pack::start(hirsel_pack::config(0, Mask::PURE, 64 << 20, 32 << 20))
            .expect("start primary pack loader");
        let handle = hirsel_pack::loader_handle(loader);
        (loader, handle)
    })
}

fn secondary_loader() -> (&'static Loader, u64) {
    static SECONDARY: OnceLock<(&'static Loader, u64)> = OnceLock::new();
    *SECONDARY.get_or_init(|| {
        let loader: &'static Loader = Box::leak(Box::new(
            Loader::new(hirsel_pack::config(0, Mask::PURE, 16 << 20, 8 << 20))
                .expect("start secondary pack loader"),
        ));
        let handle = hirsel_pack::loader_handle(loader);
        (loader, handle)
    })
}

fn core_image() -> Arc<CoreImage> {
    static IMAGE: OnceLock<(tempfile::TempDir, Arc<CoreImage>)> = OnceLock::new();
    let (_, image) = IMAGE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let req = CheckRequest {
            manifest: None,
            inputs: CorpusManifest {
                core: pkg("core"),
                packages: vec![PackageProduct::bundled_manifest(Path::new("core/core"))],
            },
            store: dir.path().join("store"),
            library: LibraryInput::Sources,
            entry: None,
        };
        let (_, image) = Driver::default()
            .produce_image(req, &mut Meter::new(false))
            .expect("build bundled core image");
        (dir, image.expect("bundled core image"))
    });
    Arc::clone(image)
}

fn pkg(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}

fn check_app(source: &str, dir: &Path, exports: &[DefPath]) -> Arc<CheckProduct> {
    let app_file = dir.join("main.hirsel");
    let req = CheckRequest {
        manifest: None,
        inputs: CorpusManifest {
            core: pkg("core"),
            packages: vec![PackageManifest {
                namespace: None,
                package: pkg("app"),
                dependencies: vec![pkg("core")],
                sources: vec![ModuleSource {
                    foreign: None,
                    captured: Some(hirsel_core_resolve::CapturedSource {
                        bytes: source.as_bytes().to_vec().into(),
                        name: Some("main.hirsel".into()),
                        test: false,
                    }),
                    file: app_file,
                    modules: vec!["main".into()],
                    signatures: false,
                }],
                toolchain: false,
            }],
        },
        store: dir.join("store"),
        library: LibraryInput::Image(core_image()),
        entry: None,
    };
    let product = Driver::default()
        .check_pack_exports(req, exports, &mut Meter::new(false))
        .expect("check app source");
    assert!(
        product.accepted(),
        "Hirsel check refused:\n{:#?}",
        product.diagnostics
    );
    product
}

struct CompiledModule {
    bytes: Vec<u8>,
    key: ContentHash,
    product: Arc<CheckProduct>,
}

impl CompiledModule {
    fn entry_identity(&self, fn_name: &str) -> ContentHash {
        let path = DefPath::from_full(&format!("app@1/main/{fn_name}")).unwrap();
        self.product
            .pack_exports
            .get(&path)
            .unwrap_or_else(|| panic!("missing pack_export {fn_name}"))
            .entry_identity
    }

    fn entry_signature(&self, fn_name: &str) -> (ContentHash, bool) {
        let path = DefPath::from_full(&format!("app@1/main/{fn_name}")).unwrap();
        let decl = self
            .product
            .pack_exports
            .get(&path)
            .unwrap_or_else(|| panic!("missing pack_export {fn_name}"));
        (decl.signature, decl.waits)
    }

    fn load_raw(&self, loader: &Loader, fn_name: &str) -> RawCode {
        let id = self.entry_identity(fn_name);
        let (sig, waits) = self.entry_signature(fn_name);
        loader
            .load_by_key(&self.key, &id, &sig, waits, true)
            .unwrap_or_else(|e| panic!("failed to load {fn_name}: {e:?}"))
    }
}

fn compile_pack_module(source: &str, export_names: &[&str], backend: Backend) -> CompiledModule {
    let dir = tempfile::tempdir().unwrap();
    let exports: Vec<DefPath> = export_names
        .iter()
        .map(|name| DefPath::from_full(&format!("app@1/main/{name}")).unwrap())
        .collect();
    let product = check_app(source, dir.path(), &exports);
    let mut opts = NativeOptions::development(
        hirsel_code::TARGET.triple().to_owned(),
        PathBuf::new(),
        Link::Driver(PathBuf::new()),
        PathBuf::new(),
    );
    opts.backend = backend;
    opts.output_kind = OutputKind::Module;
    opts.cas = dir.path().join("cas");
    let bytes = product
        .build_pack_module(&exports, &opts)
        .unwrap_or_else(|e| panic!("build_pack_module failed on {backend:?}: {e:?}"));
    let parsed = Module::parse(&bytes).expect("parse emitted .hirsel-mod");
    let key = parsed.header.key();
    assert_eq!(parsed.entries().count(), export_names.len());
    CompiledModule {
        bytes,
        key,
        product,
    }
}

unsafe extern "C" fn silent(_: *mut u8, _: *const u8, _: usize) {}

/// Run `body` inside a real contained Hirsel worker with full `Context`, `Tasks`,
/// stack, and payer limits.
fn run_in_worker<F>(root_limit: u64, body: F) -> Result<u8, EndReason>
where
    F: FnOnce(*mut Context) -> u8,
{
    unsafe extern "C" fn trampoline<F: FnOnce(*mut Context) -> u8>(
        ctx: *mut Context,
        config: *const process::Config,
        _: *mut MainResult,
    ) -> u8 {
        // SAFETY: `state` points to `Option<F>` live for the worker call.
        let slot = unsafe { &mut *(*config).state.cast::<Option<F>>() };
        let f = slot.take().expect("trampoline runs once");
        f(ctx)
    }

    let mut cell = Some(body);
    let config = process::Config {
        launch: std::ptr::null(),
        describe_output: None,
        root_limit,
        args: std::ptr::null(),
        args_len: 0,
        grants: std::ptr::null(),
        grants_len: 0,
        provider: std::ptr::null(),
        state: (&raw mut cell).cast(),
        diagnostic: silent,
        describe: None,
        scheduler: SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: std::ptr::null(),
        capabilities_len: 0,
        close_grants: None,
    };
    let execution = Execution::contained();
    // SAFETY: `config` and `cell` outlive the contained worker run.
    unsafe { run_contained_worker(&execution, &config, trampoline::<F>) }
}

/// Static descriptor layout matching `hirsel_core_rt::code_ops` (tagged with bit 0 = 1)
/// so a Rust ABI observer can be passed into compiled Hirsel code as a `Code[F]` handle.
#[repr(C, align(16))]
struct ObserverDescriptor {
    entry: usize,
    identity_len: usize,
    revision: usize,
    identity: [u8; 8],
}

// ============================================================================
// LAW 1: Overlapping frozen `view` parameters + direct result-place construction
// ============================================================================

static OBS_ARG0: AtomicUsize = AtomicUsize::new(0);
static OBS_ARG1: AtomicUsize = AtomicUsize::new(0);
static OBS_RESULT_PLACE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn observe_overlap_abi(
    _ctx: *mut Context,
    result_place: *mut [u64; 4],
    arg0: *const [u64; 4],
    arg1: *const [u64; 4],
) -> u8 {
    OBS_ARG0.store(arg0.addr(), Ordering::SeqCst);
    OBS_ARG1.store(arg1.addr(), Ordering::SeqCst);
    OBS_RESULT_PLACE.store(result_place.addr(), Ordering::SeqCst);
    // SAFETY: caller passes two valid views of `Big` and an uninit `Big` result place.
    let a = unsafe { *arg0 };
    let b = unsafe { *arg1 };
    unsafe {
        result_place.write([a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]]);
    }
    0
}

#[test]
fn law_1_overlapping_views_and_direct_result_place_on_both_backends() {
    let _alone = alone();
    let (loader, _) = primary_loader();

    let src = r#"
pub type Big {
    a: u64,
    b: u64,
    c: u64,
    d: u64
}

pub fn overlap_sum(x: Big, y: Big) -> Big {
    Big(a: x.a + y.a, b: x.b + y.b, c: x.c + y.c, d: x.d + y.d)
}

pub fn probe_overlap(c: Code[fn(Big, Big) -> Big]) -> Big {
    let b = Big(a: 11, b: 22, c: 33, d: 44)
    c(b, b)
}

pub fn call_via_bind() -> Big {
    let c = code.bind(overlap_sum)
    let b = Big(a: 11, b: 22, c: 33, d: 44)
    c(b, b)
}
"#;

    for backend in BACKENDS {
        let _ = loader.reclaim();
        assert_eq!(loader.installed(), 0, "{backend:?}: clean slab start");

        let compiled = compile_pack_module(
            src,
            &["overlap_sum", "probe_overlap", "call_via_bind"],
            backend,
        );
        loader.admit_module(&compiled.bytes).unwrap();
        let sum_code = compiled.load_raw(loader, "overlap_sum");
        let probe_code = compiled.load_raw(loader, "probe_overlap");
        let bind_code = compiled.load_raw(loader, "call_via_bind");

        type OverlapFn = unsafe extern "C" fn(
            *mut Context,
            *mut [u64; 4],
            *const [u64; 4],
            *const [u64; 4],
        ) -> u8;
        type ProbeFn = unsafe extern "C" fn(*mut Context, *mut [u64; 4], *const usize) -> u8;
        type BindFn = unsafe extern "C" fn(*mut Context, *mut [u64; 4]) -> u8;

        let sum_fn: OverlapFn = unsafe { std::mem::transmute(sum_code.entry()) };
        let probe_fn: ProbeFn = unsafe { std::mem::transmute(probe_code.entry()) };
        let bind_fn: BindFn = unsafe { std::mem::transmute(bind_code.entry()) };

        let desc = ObserverDescriptor {
            entry: observe_overlap_abi as *const () as usize,
            identity_len: 3,
            revision: 0,
            identity: *b"obs\0\0\0\0\0",
        };
        // Tagged static descriptor (bit 0 = 1) inside a `Code[F]` place word.
        let obs_code_word: usize = (&raw const desc).addr() | 1;

        // Real dynamic `Code[F]` place word for `overlap_sum` via `code.bind` layout.
        let sum_desc = ObserverDescriptor {
            entry: sum_code.entry(),
            identity_len: 3,
            revision: 0,
            identity: *b"sum\0\0\0\0\0",
        };
        let sum_code_word: usize = (&raw const sum_desc).addr() | 1;

        let status = run_in_worker(1 << 20, |ctx| {
            // 1. Direct ABI harness -> compiled Hirsel `overlap_sum` with identical
            //    overlapping `view` pointers and caller-owned uninit `result_place`.
            let input = [11u64, 22, 33, 44];
            let mut direct_out = MaybeUninit::<[u64; 4]>::new([0xdead; 4]);
            let st1 = unsafe { sum_fn(ctx, direct_out.as_mut_ptr(), &input, &input) };
            assert_eq!(st1, 0, "{backend:?}: direct overlap_sum failed");
            let val1 = unsafe { direct_out.assume_init() };
            assert_eq!(
                val1,
                [22, 44, 66, 88],
                "{backend:?}: direct overlap_sum value"
            );

            // 2. Compiled Hirsel caller `probe_overlap` -> Rust observer:
            //    proves arg0 == arg1 (exact same caller place address, zero copy)
            //    AND result_place == outer caller's result_place (direct construction!).
            OBS_ARG0.store(0, Ordering::SeqCst);
            OBS_ARG1.store(0, Ordering::SeqCst);
            OBS_RESULT_PLACE.store(0, Ordering::SeqCst);
            let mut probe_out = MaybeUninit::<[u64; 4]>::new([0xbeef; 4]);
            let expected_result_addr = probe_out.as_mut_ptr().addr();
            let st2 = unsafe { probe_fn(ctx, probe_out.as_mut_ptr(), &obs_code_word) };
            assert_eq!(st2, 0, "{backend:?}: probe_overlap observer failed");
            let val2 = unsafe { probe_out.assume_init() };
            assert_eq!(val2, [22, 44, 66, 88], "{backend:?}: probe_overlap value");
            let arg0 = OBS_ARG0.load(Ordering::SeqCst);
            let arg1 = OBS_ARG1.load(Ordering::SeqCst);
            let res_addr = OBS_RESULT_PLACE.load(Ordering::SeqCst);
            assert_ne!(arg0, 0);
            assert_eq!(
                arg0, arg1,
                "{backend:?}: overlapping frozen view params must share the caller's exact address"
            );
            assert_eq!(
                res_addr, expected_result_addr,
                "{backend:?}: result must be constructed directly in caller's result_place"
            );

            // 3. Compiled Hirsel caller -> compiled Hirsel loaded callee (`overlap_sum`).
            let mut loaded_out = MaybeUninit::<[u64; 4]>::uninit();
            let st3 = unsafe { probe_fn(ctx, loaded_out.as_mut_ptr(), &sum_code_word) };
            assert_eq!(st3, 0, "{backend:?}: probe_overlap loaded callee failed");
            assert_eq!(unsafe { loaded_out.assume_init() }, [22, 44, 66, 88]);

            // 4. Compiled Hirsel `code.bind(overlap_sum)` invoked in Hirsel.
            let mut bind_out = MaybeUninit::<[u64; 4]>::uninit();
            let st4 = unsafe { bind_fn(ctx, bind_out.as_mut_ptr()) };
            assert_eq!(st4, 0, "{backend:?}: call_via_bind failed");
            assert_eq!(unsafe { bind_out.assume_init() }, [22, 44, 66, 88]);
            0
        });
        assert_eq!(status, Ok(0), "{backend:?}: worker exit");

        drop(sum_code);
        drop(probe_code);
        drop(bind_code);
        loader.reclaim().unwrap();
        assert_eq!(loader.installed(), 0, "{backend:?}: slab reclaimed");
    }
}

mod parity_law;
mod slot_laws;
