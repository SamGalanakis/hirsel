use hirsel_driver::CompilerRequestsExt;
use hirsel_engine::{CompileConfig, LoweringPolicy, loader_inputs::LoaderInputs, store::*};
use hirsel_front_phases::COMPILER;
use hirsel_requests::*;
use hirsel_syntax::{ContentHash, PackageId};
use std::sync::Arc;

#[test]
fn installed_std_compiles_without_its_source_files_and_never_checks_unused_items() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let source_root = directory.path().join("std");
        std::fs::create_dir(&source_root).unwrap();
        let manifest = source_root.join("hirsel.json");
        std::fs::write(
            source_root.join("math.hir"),
            "pub fn twice(x:u8)->u8{x+x}\npub fn keep[T:copyable](x:T)->T in x{x}\n",
        )
        .unwrap();
        // MODE-2 can check a demanded body's whole sibling group. This module
        // is independent of that group, including under the cache oracle.
        std::fs::write(
            source_root.join("spare.hir"),
            "pub fn unused()->u8 uses {} {7u8}\n",
        )
        .unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"std","version":"1"},"sources":[{"path":"math.hir","module":["math"]},{"path":"spare.hir","module":["spare"]}],"dependencies":[]}"#).unwrap();
        let products = directory.path().join("products");
        COMPILER.precompute_package(&manifest, &products).unwrap();
        std::fs::remove_dir_all(&source_root).unwrap();
        for verify_store in [VerifyStore::Off, VerifyStore::Recompute] {
            let config = CompileConfig {
                verify_store,
                store: Arc::new(
                    DirStore::new(directory.path().join("cache"))
                        .with_read_only_root(products.clone()),
                ),
                ..Default::default()
            };
            let mut requests = CompilerRequests::for_host(COMPILER, config);
            let request = CompileRequest::source(
                "pub fn main()->Result[(),()]{if std@1/math.twice(std@1/math.keep(21u8))==42u8{Ok(())}else{Error(())}}\n",
                &hirsel_engine::default_context(),
                CompileOperation::Check {
                    lowering: LoweringPolicy::Reachable,
                },
            );
            let std_package = PackageId {
                name: "std".into(),
                version: "1".into(),
            };
            let mut loaded = requests
                .compiler()
                .load_check_package_inputs(&request.path, &request.inputs, false)
                .unwrap();
            assert_eq!(
                loaded.package_kinds[&std_package],
                hirsel_engine::DependencyKind::Installed
            );
            assert_eq!(loaded.source_paths(), vec![request.path.clone()]);
            let installed_path = loaded
                .files
                .iter()
                .find(|(_, context)| context.package == std_package)
                .map(|(file, _)| loaded.engine.sources.file(*file).unwrap().path.clone())
                .unwrap();
            assert!(
                !loaded
                    .replace_source(&installed_path, "invalid source".into())
                    .unwrap()
            );
            drop(loaded);
            let output = requests.execute_request(&request).unwrap();
            let CompileResult::Check(checked) = &output.result else {
                panic!("expected Check");
            };
            assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
            assert_eq!(checked.completion, hirsel_engine::CheckCompletion::Complete);
            assert_eq!(output.inputs.read_work(), (0, 0));
            let unused = hirsel_syntax::DefPath(hirsel_syntax::ItemPath {
                package: PackageId {
                    name: "std".into(),
                    version: "1".into(),
                },
                modules: vec!["spare".into()],
                name: "unused".into(),
            });
            if verify_store == VerifyStore::Off {
                assert!(
                    !output.work.touched_memos.iter().any(|key| {
                        key.def.path() == &unused
                            && matches!(
                                key.kind,
                                hirsel_engine::QueryKind::Modes
                                    | hirsel_engine::QueryKind::Check
                                    | hirsel_engine::QueryKind::Resolve
                            )
                    }),
                    "a matching installed product must not become an unrelated source Front root: {:?}",
                    output.work.touched_memos,
                );
                assert_eq!(
                    requests
                        .recompute_counts(&[(unused, hirsel_engine::QueryKind::Check)])
                        .unwrap()[0]
                        .2,
                    0
                );
            }
            assert!(
                request
                    .inputs
                    .read(std::path::Path::new("/unobserved"))
                    .is_err()
            );
            let other = CompileRequest::source(
                "pub fn main()->u8{1u8}\n",
                &hirsel_engine::default_context(),
                request.operation.clone(),
            );
            assert_ne!(request.inputs, other.inputs);
            assert!(!request.path.exists());
            let output = requests
                .execute_request(&CompileRequest {
                    operation: CompileOperation::Tir { definition: None },
                    ..request.clone()
                })
                .unwrap();
            let CompileResult::Tir(input) = &output.result else {
                panic!("expected TIR");
            };
            for checked in [false, true] {
                let execution = hirsel_driver::testing::observe_tir(
                    input,
                    &hirsel_engine::RunConfiguration {
                        entry: input.entry.clone(),
                        hash_seed: 0,
                        inputs: vec![],
                    },
                    hirsel_engine::SimulationConfig::default(),
                    hirsel_interp::Limits::default(),
                    checked,
                )
                .unwrap();
                assert!(matches!(
                    execution.outcome,
                    Ok(hirsel_engine::RunOutcome::Returned(
                        hirsel_run_schema::Returned::Ok
                    ))
                ));
            }
        }
        let provider = || {
            DirStore::new(directory.path().join("damage-reader"))
                .with_read_only_root(products.clone())
        };
        let package = PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let installed = provider()
            .installed_package(&package, COMPILER.identity())
            .unwrap();
        let physical = hirsel_artifact_store::Store::new(products.clone());
        let corrupt = |reference: Reference| {
            let payload = physical.get_owned(reference).unwrap();
            let path = physical.output_path(reference);
            let original = std::fs::read(&path).unwrap();
            let offset = original
                .windows(payload.len())
                .position(|bytes| bytes == &*payload)
                .unwrap();
            let mut damaged = original.clone();
            damaged[offset] ^= 1;
            std::fs::write(&path, damaged).unwrap();
            (path, original)
        };
        let path = |module: &str, name: &str| {
            hirsel_syntax::DefPath(hirsel_syntax::ItemPath {
                package: package.clone(),
                modules: vec![module.into()],
                name: name.into(),
            })
        };
        let compile = || {
            let config = CompileConfig {
                store: Arc::new(provider()),
                ..Default::default()
            };
            CompilerRequests::for_host(COMPILER, config).execute_request(&CompileRequest::source(
                "pub fn main()->Result[(),()]{if std@1/math.twice(21u8)==42u8{Ok(())}else{Error(())}}\n",
                &hirsel_engine::default_context(),
                CompileOperation::Tir { definition: None },
            ))
        };
        let unused = installed
            .definition(&path("spare", "unused"))
            .unwrap()
            .ownership
            .body;
        let (file, original) = corrupt(unused);
        let output = compile().unwrap();
        let CompileResult::Tir(input) = &output.result else {
            panic!("expected TIR")
        };
        for checked in [false, true] {
            let execution = hirsel_driver::testing::observe_tir(
                input,
                &hirsel_engine::RunConfiguration {
                    entry: input.entry.clone(),
                    hash_seed: 0,
                    inputs: vec![],
                },
                hirsel_engine::SimulationConfig::default(),
                hirsel_interp::Limits::default(),
                checked,
            )
            .unwrap();
            assert!(matches!(
                execution.outcome,
                Ok(hirsel_engine::RunOutcome::Returned(
                    hirsel_run_schema::Returned::Ok
                ))
            ));
        }
        assert!(InstalledPackage::verify_store(&products).is_err());
        std::fs::write(file, original).unwrap();
        let consumed = installed
            .definition(&path("math", "twice"))
            .unwrap()
            .ownership
            .body;
        let (_file, _original) = corrupt(consumed);
        assert!(
            compile().is_err(),
            "corrupted demanded body must be rejected before decoding"
        );
    });
}

#[test]
fn missing_std_products_name_the_selected_root_without_source_fallback() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.products");
        let store =
            DirStore::new(directory.path().join("cache")).with_read_only_root(missing.clone());
        let error = store
            .installed_package(
                &PackageId {
                    name: "std".into(),
                    version: "1".into(),
                },
                ContentHash::default(),
            )
            .unwrap_err();
        assert!(error.contains(missing.to_str().unwrap()), "{error}");
        assert!(error.contains("missing"), "{error}");
    });
}
