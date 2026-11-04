//! Keyed writes locate Map fields after an earlier write may detach the parent.
#[path = "../support/history.rs"]
#[allow(dead_code, reason = "shared four-engine test support")]
mod history;
use hirsel_driver::native;
use hirsel_driver::requests::*;
use hirsel_engine::{
    requests::{OutputReadiness, RequestEngine, observe_inputs},
    runtime::QueryRuntime,
    with_compiler_stack,
};
use hirsel_front_phases::COMPILER;
use std::sync::Arc;

const UPSERT: &str = "pub fn main() {\n  let mut counts: unique Map[u32, u64] = map.empty()\n  core@1/map.upsert_with(\n    mut counts, 0u32,\n    fn() -> unique u64 uses {} { 1 },\n    fn(mut count: unique u64) -> () uses {} { count += 1 }\n  )\n}\n";

#[test]
fn native_upsert_initializer_preserves_prepared_map_entry_addresses() {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("main.hir"), UPSERT).unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"keyed_address","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let compiler = COMPILER.with_owned_engines();
        let target = hirsel_native::TargetSpec::x86_64_unknown_linux_gnu();
        let tools = Arc::new(
            hirsel_native::Toolchain::in_tree(&directory.path().join("tools"), &[&target]).unwrap(),
        );
        for backend in [
            hirsel_native::Backend::Cranelift,
            hirsel_native::Backend::Llvm,
        ] {
            let mut selector =
                CompilerRequests::for_host(compiler.clone(), compiler.configuration())
                    .with_toolchain(tools.clone());
            let (loaded, inputs) = observe_inputs(
                selector.input_limits(),
                selector.capture_attempts(),
                |inputs| compiler.load_check_package_inputs(&manifest, inputs, true),
            )
            .unwrap();
            let loaded = loaded.unwrap();
            let entry = loaded.entry.clone();
            drop(loaded);
            let request = CompileRequest {
                path: manifest.clone(),
                inputs: inputs.into(),
                operation: CompileOperation::NativeBuild(NativeRequest {
                    options: {
                        let mut options = hirsel_native::BuildProfile::Default
                            .options(
                                backend,
                                target.clone(),
                                hirsel_native::Validation::EveryStage,
                            )
                            .unwrap();
                        options.opt = hirsel_native::OptLevel::None;
                        options
                    },
                    definition: Some(entry.clone()),
                    test_entries: vec![],
                    supplemental_sources: vec![],
                    output: NativeOutput::Objects { roots: vec![entry] },
                    run: None,
                    fuel: false,
                    evidence: None,
                }),
            };
            let mut selected = selector.select(&request).unwrap();
            drop(selector);
            let complete = {
                let mut runtime = selected.runtime.lock().unwrap();
                let request = runtime.request(&selected.roots);
                runtime.wait(request)
            }
            .unwrap_or_else(|reference| {
                panic!(
                    "{backend:?}: {:?}",
                    selected
                        .output
                        .failed(reference)
                        .map(|error| error.to_string())
                )
            });
            let output = selected.output.ready(&complete).unwrap();
            let CompileResult::NativeObjects(objects) = &output.result else {
                panic!("upsert request did not complete native objects");
            };
            assert!(!objects.products.is_empty());
            for product in &objects.products {
                let fragment = hirsel_native::object_queries::fragment_product(
                    objects.store.as_ref(),
                    product.clone(),
                )
                .unwrap();
                let object = hirsel_native::object_queries::load_object(
                    objects.store.as_ref(),
                    fragment.object,
                )
                .unwrap();
                assert!(object.bytes.starts_with(b"\x7fELF"));
            }
        }
    });
}

#[test]
fn keyed_append_and_existing_value_updates_agree_on_all_four_engines() {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let runner = history::Runner::new(directory.path()).unwrap();
        let source = r#"
pub fn main()->Result[(),()] {
  let mut counts: unique Map[u32,u64] = map.empty()
  for i in 0u64..3u64 {
    core@1/map.upsert_with(mut counts,0u32,
      fn()->unique u64 uses {} {1},
      fn(mut count:unique u64)->() uses {} {count+=1})
  }
  map.insert(mut counts,1u32,9u64)
  if map.get(counts,0u32)!=Some(3u64) {return Error(())}
  if map.get(counts,1u32)!=Some(9u64) {return Error(())}
  if map.len(counts)!=2u64 {return Error(())}
  Ok(())
}
"#;
        let report = runner.run("keyed-parent-detach", source);
        assert_eq!(report.cells.len(), 4, "{report:#?}");
        assert!(report.agrees(), "{report:#?}");
        assert!(report.disagreements(b"", false).is_empty(), "{report:#?}");
    });
}
