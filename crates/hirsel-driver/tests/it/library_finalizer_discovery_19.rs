//! Library-created message storage retains exact source finalizer instances.
use hirsel_driver::native;
use hirsel_engine::requests::{OutputReadiness, RequestEngine, observe_inputs};
use hirsel_engine_runtime::QueryRuntime;

use hirsel_native::Backend;
use hirsel_requests::CompileOperation;
use hirsel_requests::CompileRequest;
use hirsel_requests::CompileResult;
use hirsel_requests::CompilerRequests;
use hirsel_requests::NativeOutput;
use hirsel_requests::NativeRequest;
use std::sync::Arc;

#[test]
fn library_channel_message_discovers_resource_finalizer_without_a_source_body() {
    compile_objects(
        "resource type Message { n:u64 }\n@noalloc drop(value) {}\npub fn main(rt:Io)->Result[(),GrantError] { let tasks=io.tasks(rt)?; let (sender,receiver):(Sender[Message],Receiver[Message])=channel.open(tasks); Ok(()) }\n",
    );
}

#[test]
fn library_private_map_entry_discovers_its_representation_without_a_public_signature() {
    compile_objects(
        "pub fn main()->u64 {let mut values:Map[u64,u64]=map.empty();map.insert(mut values,1u64,2u64);map.len(values)}",
    );
}

#[test]
fn library_detached_task_discovers_its_callable_result_finalizer() {
    compile_objects(include_str!(
        "../../../../conformance/CONC/CONC-2-detached-result-cleanup-run.hir"
    ));
}

fn compile_objects(source: &str) {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(directory.path().join("main.hir"), source).unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"channel_message","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let compiler = hirsel_front_phases::COMPILER.with_owned_engines();
        let target = hirsel_native::TargetSpec::x86_64_unknown_linux_gnu();
        let tools = Arc::new(
            hirsel_native::Toolchain::in_tree(&directory.path().join("tools"), &[&target]).unwrap(),
        );
        let mut expected = None;
        for backend in [Backend::Cranelift, Backend::Llvm] {
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
            let first = loaded.entry.clone();
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
                    definition: Some(first.clone()),
                    test_entries: vec![],
                    supplemental_sources: vec![],
                    output: NativeOutput::Objects { roots: vec![first] },
                    run: None,

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
                    "object request: {:?}",
                    selected
                        .output
                        .failed(reference)
                        .map(|error| error.to_string())
                )
            });
            let output = selected.output.ready(&complete).unwrap();
            let CompileResult::NativeObjects(objects) = &output.result else {
                panic!("object-only request returned another result");
            };
            if let Some(expected) = expected {
                assert_eq!(objects.instances, expected);
            } else {
                expected = Some(objects.instances);
            }
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
                assert!(
                    hirsel_native::object_queries::linked_product(
                        objects.store.as_ref(),
                        product.clone()
                    )
                    .is_err(),
                    "a Codegen output cannot supply an executable"
                );
            }
        }
    });
}
