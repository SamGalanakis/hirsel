use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use hirsel_code::{Loader, RawCode};
use hirsel_core_driver::{
    Backend, CheckProduct, CheckRequest, CoreImage, Driver, LibraryInput, Link, Meter,
    NativeOptions, OutputKind, PackageProduct,
};
use hirsel_core_ids::{ContentHash, DefPath, PackageId};
use hirsel_core_resolve::{CorpusManifest, ModuleSource, PackageManifest};
use hirsel_core_rt::{
    Context,
    process::{self, MainResult},
    run_stop::{EndReason, Execution, run_contained_worker},
    tasks::SchedulerProvider,
};
use hirsel_mod::{Mask, Module};

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
        let handle = hirsel_pack::loader_handle(loader).expect("the primary pool's handle");
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
    run_with_grants(root_limit, &[], body)
}

fn run_with_grants<F>(
    root_limit: u64,
    grants: &[hirsel_core_rt::entry::Grant],
    body: F,
) -> Result<u8, EndReason>
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
        capabilities: grants.as_ptr(),
        capabilities_len: grants.len(),
        close_grants: None,
    };
    let execution = Execution::contained();
    // SAFETY: `config` and `cell` outlive the contained worker run.
    unsafe { run_contained_worker(&execution, &config, trampoline::<F>) }
}
