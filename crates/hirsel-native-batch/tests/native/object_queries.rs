use super::*;
use hirsel_engine::store::{DirStore, MAX_RECORD};
use hirsel_native::object_queries::*;
use hirsel_native::query_model::*;
fn output_path(store: &DirStore, reference: Reference) -> std::path::PathBuf {
    hirsel_artifact_store::Store::new(store.root().into()).output_path(reference)
}
use hirsel_lir::layout_types::TargetSpec;
use std::cell::Cell;

fn admitted(backend: Arc<dyn ResultStore>) -> hirsel_engine::store::RequestStore {
    let budget = hirsel_engine::runtime::ProcessBudget::new(64 << 20);
    hirsel_engine::store::RequestStore::new(backend, budget.reserve(64 << 20).unwrap())
}

const INPUT: Domain = Domain {
    kind: 0x4f00ff00,
    schema: 1,
};
fn hash(s: &str) -> ContentHash {
    ContentHash::of_bytes(s.as_bytes())
}
fn input(store: &dyn ResultStore, s: &str) -> Reference {
    store.put_content(INPUT, s.as_bytes()).unwrap()
}
fn product(store: &dyn ResultStore, s: &str) -> ProductRef {
    crate::fixture_product(store, Address(hash(s)), Action(hash(s)), input(store, s))
}
fn ready(store: &dyn ResultStore) -> ReadyProduct {
    ReadyProduct {
        product: product(store, "Ready product"),
        debug: true,
        ready: input(store, "Ready"),
        layout_closure: input(store, "layouts"),
        emission_context: input(store, "emit"),
        unit_summary: input(store, "summary"),
        optimizer_reads: vec![],
    }
}
fn inputs(store: &dyn ResultStore, fragments: Vec<FragmentProduct>) -> LinkInputs {
    LinkInputs {
        execution: vec![],
        fragments,
        static_manifest: input(store, "statics"),
        runtime_archives: vec![input(store, "runtime")],
        foreign_inputs: vec![],
        launch_manifest: input(store, "launch"),
    }
}
// Test-only NativeProducts implementation; never a production fallback.
struct NativeFixture {
    program: Arc<crate::ReadyProgram>,
    address: InstanceAddress,
    descriptor: Arc<ReadyProduct>,
    owner: usize,
    opt: hirsel_lir::emission::OptLevel,
    materializations: Arc<Cell<usize>>,
}
impl NativeProducts for NativeFixture {
    fn instance(
        &mut self,
        _: &InstanceAddress,
    ) -> Result<Arc<hirsel_native::query_model::InstanceProduct>, String> {
        Err("fixture does not produce Value".into())
    }
    fn arena_summary(
        &mut self,
        _: &hirsel_native::query_model::ComponentKey,
    ) -> Result<Arc<hirsel_native::query_model::ArenaSummary>, String> {
        Err("fixture does not produce arena summaries".into())
    }
    fn ready(&mut self, key: &InstanceAddress) -> Result<Arc<ReadyProduct>, String> {
        if *key != self.address {
            return Err("unknown fixture owner".into());
        }
        Ok(self.descriptor.clone())
    }
    fn with_ready<R>(
        &mut self,
        products: &[Arc<ReadyProduct>],
        use_units: impl FnOnce(
            &[&hirsel_lir::stage::ValidatedUnit<hirsel_lir::stage::Ready>],
            &hirsel_lir::emission::EmitInputs<'_>,
        ) -> Result<R, String>,
    ) -> Result<R, String> {
        if products.len() != 1 || products[0].product != self.descriptor.product {
            return Err("foreign fixture descriptor".into());
        }
        self.materializations.set(self.materializations.get() + 1);
        let program = &self.program;
        let units = [&program.units[self.owner]];
        let inputs = hirsel_lir::emission::EmitInputs {
            ahead: None,
            continuation: None,
            alternate: false,
            index: None,
            input: &program.input,
            target: &program.target,
            layouts: &program.table,
            keys: &program.keys,
            manifest: &program.manifest,
            statics: &program.statics,
            inline_instances: &program.inline_instances,
            available_externally: &Default::default(),
            callbacks: !program.callbacks.is_empty(),
            tracking: program.tracking(),
            foreign: &program.foreign_dispatch,
            opt: self.opt,
            verify_backend: true,
            measurement: false,
            fuel: program.fuel,
        };
        use_units(&units, &inputs)
    }
}

#[test]
fn actual_ready_fixture_emits_links_restores_and_exits_on_both_backends() {
    hirsel_engine::with_compiler_stack(|| {
        use hirsel_engine::{QueryEngine, default_context, default_environment, entry_path};
        use hirsel_native as native;
        let mut engine = QueryEngine::new(
            default_environment(),
            hirsel_front_phases::compiler_adapters(),
        );
        let file = engine
            .sources
            .add(
                "object-fixture.hir",
                "pub fn main()->Result[(),String]{Ok(())}".into(),
            )
            .unwrap();
        let checked =
            hirsel_engine::source_check_request(&mut engine, file, &default_context()).unwrap();
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let entry = entry_path(&default_context());
        let reference = hirsel_engine::run_observed_snapshot(
            &mut engine,
            &hirsel_engine::RunConfiguration {
                entry: entry.clone(),
                hash_seed: 0,
                inputs: vec![],
            },
            hirsel_engine::SimulationConfig::default(),
        )
        .unwrap();
        let reference = crate::Observation::of_reference(reference.observation.as_ref().unwrap());
        let input = engine.export_native_input().unwrap();
        let validated = engine.import_native_input(input).unwrap();
        let mut options = crate::Options::new(TargetSpec::x86_64_unknown_linux_gnu());
        options.validation = crate::Validation::EveryStage;
        let program = Arc::new(crate::build(&validated, &options).unwrap());
        let owner = program
            .units
            .iter()
            .position(|u| u.unit().header.definition == entry)
            .unwrap();
        let unit = program.units[owner].unit();
        let instance =
            hirsel_layout::instance::InstanceKey::new(&entry, 0, &[], engine.types(), None, None)
                .unwrap();
        assert_eq!(instance.digest(), unit.header.instance);
        let address = InstanceAddress {
            instance,
            target_abi: program.target.digest(),
            policy: hash("V/import/C/Ready every stage"),
        };
        let root = tempfile::tempdir().unwrap();
        let tools =
            crate::Toolchain::in_tree(&root.path().join("toolchain"), &[&program.target]).unwrap();
        let sites = crate::Sites::new(validated.snapshot());
        let heap = hirsel_native_testing::run_heap(&program, &entry, &sites).unwrap();
        assert_eq!(heap.observation, reference, "checked H");
        for backend in [crate::Backend::Cranelift, crate::Backend::Llvm] {
            for opt in [
                hirsel_lir::emission::OptLevel::None,
                hirsel_lir::emission::OptLevel::Default,
            ] {
                let dir = root.path().join(format!("{}-{opt:?}", backend.name()));
                std::fs::create_dir_all(&dir).unwrap();
                let store: Arc<dyn hirsel_engine::store::ResultStore> = Arc::new(admitted(
                    Arc::new(hirsel_engine::store::MemStore::default()),
                ));
                let mut descriptor = ready(store.as_ref());
                descriptor.ready = store
                    .put_content(INPUT, hirsel_codec::to_text(unit).as_bytes())
                    .unwrap();
                descriptor.debug = true;
                let count = Arc::new(Cell::new(0));
                let fixture = NativeFixture {
                    program: program.clone(),
                    address: address.clone(),
                    descriptor: Arc::new(descriptor),
                    owner,
                    opt,
                    materializations: count.clone(),
                };
                let baseline = crate::emit_backend_objects(
                    &program,
                    backend,
                    opt,
                    crate::Partition::Singleton,
                    false,
                    None,
                )
                .unwrap();
                let owner_symbol =
                    hirsel_lir::emission::symbol_name(&entry, Some(&unit.header.instance));
                let remaining: Vec<_> = baseline
                    .into_iter()
                    .filter(|object| {
                        !object
                            .exports
                            .iter()
                            .any(|export| export.name == owner_symbol)
                    })
                    .collect();
                let supplementary =
                    store_link_input(store.as_ref(), hirsel_codec::to_text(&remaining).as_bytes())
                        .unwrap();
                let links = Cell::new(0);
                let link_store = store.clone();
                let mut queries = ObjectQueries::new(
                    store.clone(),
                    fixture,
                    |_: &LinkKey, _: &LinkInputs, objects: &[ObjectArtifact]| {
                        links.set(links.get() + 1);
                        let mut objects = objects.to_vec();
                        objects.extend(remaining.iter().cloned());
                        let path = crate::link_objects(&program, &entry, &objects, &tools, &dir)
                            .map_err(|error| error.to_string())?;
                        std::fs::read(path).map_err(|error| error.to_string())
                    },
                );
                let key = CodegenKey {
                    owner: address.clone(),
                    backend: backend.name().into(),
                    backend_version: hash(&crate::llvm_version()),
                    target_features: hash("default target features"),
                    profile_options: hash(&format!("{opt:?},debug=true")),
                    runtime_abi: hash("current ABI"),
                };
                let cold = queries.codegen(&key).unwrap();
                let warm = queries.codegen(&key).unwrap();
                assert_eq!(cold.product, warm.product);
                assert_eq!(count.get(), 1, "hit materialized Ready");
                let mut manifest = inputs(link_store.as_ref(), vec![(*cold).clone()]);
                manifest.static_manifest = supplementary;
                let archive = tools
                    .runtime
                    .get(&(
                        program.target.triple.clone(),
                        crate::RuntimeBuild::default(),
                    ))
                    .unwrap();
                manifest.runtime_archives = vec![
                    store_link_input(link_store.as_ref(), &std::fs::read(archive).unwrap())
                        .unwrap(),
                ];
                let key = LinkKey {
                    roots: vec![address.clone()].into(),
                    target: program.target.digest(),
                    linker_options: hash("actual pinned lld+kit fixture"),
                };
                let cold = queries.link(&key, &manifest).unwrap();
                let warm = queries.link(&key, &manifest).unwrap();
                assert_eq!(cold.product, warm.product);
                assert_eq!(links.get(), 1);
                let path = dir.join("restored-program");
                materialize_executable(store.as_ref(), &cold, &path).unwrap();
                let run = crate::launch(&path, &program.target, &tools, &dir, false).unwrap();
                assert_eq!(run.exit, Some(0), "{run:?}");
                let observation = crate::observe(&run, &program, &sites).unwrap();
                assert_eq!(observation, reference, "{backend:?} {opt:?}");
            }
        }
    });
}

use hirsel_engine::runtime::ArchiveAdmission;
use hirsel_engine::store::{Action, Address, Domain, Output, Reference, ResultStore, VerifyStore};
use hirsel_lir::stage::ObjectArtifact;
use std::path::Path;
