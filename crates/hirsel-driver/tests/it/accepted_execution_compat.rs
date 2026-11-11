use hirsel_engine::{CompileConfig, Compiler, execution_options, requests::observe_inputs};
use hirsel_front_phases::FrontPhases;
use hirsel_requests::{
    CompileInputs, CompileOperation, CompileRequest, CompileResult, CompilerRequests,
};
use hirsel_syntax::ContentHash;

#[test]
fn incompatible_execution_records_recompute_in_each_compilers_own_store() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(
            directory.path().join("main.hir"),
            "pub fn main()->Result[(),()]{if 6u64*7u64==42u64 {Ok(())} else {Error(())}}",
        )
        .unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"execution_compat","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let base = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
            .join("hirsel/accepted-execution");
        let mut cases = Vec::new();
        for label in [b"first".as_slice(), b"second".as_slice()] {
            let identity = ContentHash::of_parts(
                "hirsel.execution-compat.test.v1",
                [directory.path().as_os_str().as_encoded_bytes(), label],
            );
            let config = CompileConfig::for_memory_cap(None);
            let compiler =
                Compiler::new(FrontPhases::shared, identity.0).with_config(config.clone());
            let requests = CompilerRequests::for_host(compiler.clone(), config.clone());
            let (loaded, inputs) = observe_inputs(
                requests.input_limits(),
                requests.capture_attempts(),
                |inputs| compiler.load_check_package_inputs(&manifest, inputs, true),
            )
            .unwrap();
            let mut program = loaded.unwrap();
            let entry = program.entry.clone();
            let (imports, policy) = program.engine.execution_import_context();
            let options = execution_options(&config, program.engine.comptime_limits);
            let observations = ContentHash::of_parts(
                "hirsel.execution.frozen-inputs.v1",
                inputs.identity_parts().iter().map(Vec::as_slice),
            );
            let address = hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.accepted-execution.inputs.v2",
                [
                    entry.to_string().as_bytes(),
                    &observations.0,
                    &imports.0,
                    &identity.0,
                    &policy.0,
                    &options.0,
                ],
            ));
            let version: String = identity
                .0
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let own = hirsel_artifact_store::Store::new(base.join(version));
            let legacy = hirsel_artifact_store::Store::new(base.join("v1"));
            let domain = hirsel_artifact_store::Domain {
                kind: 0x4603_0006,
                schema: 1,
            };
            let old_encoding = bincode::serialize(&(label, 1_u32)).unwrap();
            let foreign_encoding = bincode::serialize(&(false, label, 2_u64)).unwrap();
            for (store, bytes) in [
                (&legacy, old_encoding.as_slice()),
                (&own, foreign_encoding.as_slice()),
            ] {
                let foreign = store.put(domain, bytes).unwrap();
                store
                    .associate(address, hirsel_artifact_store::Action(address.0), foreign)
                    .unwrap();
            }
            cases.push((
                compiler,
                own,
                address,
                domain.reference(&foreign_encoding).unwrap(),
                CompileInputs::Frozen(inputs),
            ));
            drop(program);
        }
        for fresh in [true, false] {
            for (compiler, own, address, foreign, inputs) in &cases {
                let mut requests =
                    CompilerRequests::for_host(compiler.clone(), compiler.configuration());
                let output = requests
                    .execute_request(&CompileRequest {
                        path: manifest.clone(),
                        operation: CompileOperation::Tir { definition: None },
                        inputs: inputs.clone(),
                    })
                    .unwrap();
                let CompileResult::Tir(input) = &output.result else {
                    panic!("expected executable TIR")
                };
                let launched = hirsel_interp::library::launch_with_runtime(
                    &mut input.provider(),
                    &input.sources,
                    &input.signature,
                    &hirsel_tir::RunConfiguration {
                        entry: input.entry.clone(),
                        hash_seed: 0,
                        inputs: vec![],
                    },
                    hirsel_interp::library::SimulationConfig::default(),
                    hirsel_interp::Limits::default(),
                    &input.runtime,
                )
                .unwrap();
                assert!(
                    matches!(
                        launched.execution.outcome,
                        hirsel_interp::Outcome::Returned { exit_code: 0, .. }
                    ),
                    "{:?}",
                    launched.execution.outcome
                );
                assert_ne!(
                    own.lookup(*address).unwrap().reference,
                    *foreign,
                    "a foreign encoding must be replaced in this compiler's own store"
                );
                assert_eq!(
                    output.work.recomputations == 0,
                    !fresh,
                    "foreign records recompute; the resulting records are reusable"
                );
            }
        }
    });
}
