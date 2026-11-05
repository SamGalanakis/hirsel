//! Original private std projection and singleton callable ABI vectors from
//! native/seed_parity_tests.rs, executed through source-owned native tasks.
use hirsel_driver::native;
use hirsel_engine::{
    store::{self, ResultStore, VerifyStore},
    *,
};
use hirsel_front_phases::COMPILER;
use hirsel_intern::jobs::with_compiler_stack;
use std::sync::Arc;

fn manifest() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib/std@1/hirsel.json")
}
fn root(package: hirsel_syntax::PackageId, module: &str, name: &str) -> DefPath {
    DefPath(hirsel_syntax::ItemPath {
        package,
        modules: vec![module.into()],
        name: name.into(),
    })
}
#[test]
fn ordinary_store_delete_has_one_consistent_entry_abi() {
    with_compiler_stack(|| {
        let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
        let store: Arc<dyn ResultStore> = Arc::new(store::RequestStore::new(
            Arc::new(store::MemStore::default()),
            budget.reserve(64 << 20).unwrap(),
        ));
        let mut config = COMPILER.configuration();
        config.store = store.clone();
        let mut package = COMPILER
            .with_config(config)
            .with_owned_engines()
            .load_package_declarations(&manifest())
            .unwrap();
        let selected = root(package.package.clone(), "store", "tx_delete");
        // Resolve only this original library root, as in the old singleton test.
        let _validated = package
            .engine
            .native_input_for(std::slice::from_ref(&selected))
            .unwrap();
        for backend in [
            hirsel_native::Backend::Cranelift,
            hirsel_native::Backend::Llvm,
        ] {
            for opt in [
                hirsel_native::OptLevel::None,
                hirsel_native::OptLevel::Default,
                hirsel_native::OptLevel::Speed,
            ] {
                let options = hirsel_native::Options::new(
                    hirsel_native::TargetSpec::x86_64_unknown_linux_gnu(),
                );
                let (actual, ()) = crate::native_object_products_19::compile_roots(
                    &mut package.engine,
                    store.clone(),
                    std::slice::from_ref(&selected),
                    std::slice::from_ref(&selected),
                    backend,
                    opt,
                    options,
                    false,
                    hirsel_intern::jobs::Jobs::ONE,
                    VerifyStore::Off,
                    |_, _| (),
                );
                assert!(!actual.objects.is_empty(), "{backend:?} {opt:?}");
                assert_eq!(actual.objects.len(), actual.summaries.len());
            }
        }
    });
}
