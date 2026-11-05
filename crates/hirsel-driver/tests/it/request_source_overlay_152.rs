use hirsel_driver::testing::observe_tir;
use hirsel_engine::loader_inputs::namespace::{
    CapturePlan, Change, DiskCapture, Inspection, Key, LeaseBudget, Limits as NamespaceLimits, Root,
};
use hirsel_engine::{
    CompileConfig, RunConfiguration, RunOutcome, SimulationConfig,
    loader_inputs::{CaptureInputs, InputLimits, namespace::NamespaceLease},
};
use hirsel_front_phases::COMPILER;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_requests::*;
use std::{collections::BTreeSet, sync::Arc};

#[test]
fn frozen_source_requests_ignore_newer_disk_and_retain_detached_execution() {
    with_compiler_stack(|| {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("main.hir");
        let manifest = dir.path().join("hirsel.json");
        let original = "pub fn value()->u64{7u64}\npub fn main()->Result[(),()]{failure.check(value()==7u64,Arithmetic());Ok(())}\n";
        std::fs::write(&source, original).unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"frozen","version":"1"},"entry":{"module":[],"name":"main"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[]}"#).unwrap();
        let capture = CaptureInputs::new(InputLimits {
            bytes: 1 << 20,
            operations: 4096,
        });
        drop(
            COMPILER
                .load_program_inputs(&manifest, &capture, false)
                .unwrap(),
        );
        let inputs = capture.freeze();
        let namespace = NamespaceLease::capture(
            &CapturePlan {
                roots: vec![Root {
                    id: "app".into(),
                    path: dir.path().to_path_buf(),
                    editable: true,
                }],
                excluded: BTreeSet::new(),
                failed_probes: vec![],
                entry: manifest.clone(),
                std_manifest: None,
                environment: b"request-source-overlay".to_vec(),
                limits: NamespaceLimits {
                    bytes: 1 << 20,
                    nodes: 1000,
                    aliases: 100,
                    depth: 32,
                    link_hops: 40,
                    edits: 100,
                    attempts: 2,
                },
            },
            &DiskCapture,
            &LeaseBudget::new(16 << 20),
        )
        .unwrap();
        let replacement = original.replace("7u64", "8u64");
        let frozen_overlay = inputs
            .replace_sources(
                &[(
                    source.canonicalize().unwrap(),
                    Arc::<[u8]>::from(replacement.as_bytes()),
                )]
                .into(),
                1 << 20,
            )
            .unwrap();
        let Inspection::Present {
            present,
            bytes: Some(expected_bytes),
            ..
        } = namespace.inspect("app", "main.hir").unwrap()
        else {
            panic!("expected a captured source file")
        };
        let namespace_overlay = namespace
            .overlay(&[Change::Replace {
                key: Key {
                    root: "app".into(),
                    path: "main.hir".into(),
                },
                present,
                expected_bytes,
                bytes: Arc::<[u8]>::from(replacement.as_bytes()),
            }])
            .unwrap()
            .0;
        std::fs::write(&source, "pub fn main(){missing}\n").unwrap();
        let prepare = |requests: &mut CompilerRequests, inputs: CompileInputs| {
            let output = requests
                .execute_request(&CompileRequest {
                    path: manifest.clone(),
                    inputs,
                    operation: CompileOperation::Tir { definition: None },
                })
                .unwrap();
            let CompileResult::Tir(tir) = &output.result else {
                panic!("expected TIR")
            };
            tir.clone()
        };
        for (first_inputs, second_inputs) in [
            (
                CompileInputs::Frozen(inputs.clone()),
                CompileInputs::Frozen(frozen_overlay),
            ),
            (
                CompileInputs::Namespace(namespace),
                CompileInputs::Namespace(namespace_overlay),
            ),
        ] {
            let mut requests = CompilerRequests::for_host(COMPILER, CompileConfig::default());
            let first = prepare(&mut requests, first_inputs);
            let second = prepare(&mut requests, second_inputs);
            assert_ne!(first.revision, second.revision);
            drop(requests);
            for tir in [first, second] {
                for checked in [false, true] {
                    let execution = observe_tir(
                        &tir,
                        &RunConfiguration {
                            entry: tir.entry.clone(),
                            hash_seed: 0,
                            inputs: vec![],
                        },
                        SimulationConfig::default(),
                        hirsel_interp::Limits {
                            steps: Some(10_000),
                            ..Default::default()
                        },
                        checked,
                    )
                    .unwrap();
                    assert!(
                        matches!(
                            execution.outcome,
                            Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
                        ),
                        "{:?}",
                        execution.outcome
                    );
                    assert!(execution.observation.unwrap().failures.is_empty());
                }
            }
        }
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "pub fn main(){missing}\n"
        );
    });
}
