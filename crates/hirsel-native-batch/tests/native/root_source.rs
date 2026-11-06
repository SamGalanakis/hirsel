use super::*;
use hirsel_engine::{DefinitionQueries, QueryEngine, Verify, default_context, default_environment};
use hirsel_engine_store::{Bytes, EntryKey, MemStore, ResultStore};
use hirsel_native::root_source::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct CountedStore {
    inner: MemStore,
    reads: AtomicU64,
}
impl ResultStore for CountedStore {
    fn capability(&self) -> hirsel_engine_store::ProviderCapability {
        self.inner.capability()
    }

    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.get(key)
    }
    fn put(&self, key: &EntryKey, bytes: &[u8]) {
        self.inner.put(key, bytes);
    }
    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        self.inner.verifier()
    }
    fn name(&self) -> &'static str {
        "root-source-test"
    }
}
struct Fixture {
    store: Arc<CountedStore>,
    program: hirsel_rc::instance::Program,
    operations: Arc<hirsel_layout::operation::OperationRegistry>,
    roots: NativeSourceRoots,
    declarations: BTreeMap<DefPath, Arc<NativeDefinitionProduct>>,
    entries: BTreeMap<DefPath, u32>,
    anchors: BTreeMap<DefPath, Vec<Anchor>>,
    signatures: Arc<SignatureSnapshot>,
    semantic: Arc<NativeSemanticProgram>,
    selected: Vec<DefPath>,
    lower: NativeOriginalReader,
    empty: NativeOriginalReader,
    validated: ValidatedNativeInput,
}
impl Fixture {
    fn new(verify: Verify) -> Self {
        Self::with_source(
            verify,
            "fn helper()->u64{7u64}\npub fn main()->Result[(),u64]{Error(helper())}",
        )
    }
    fn with_source(verify: Verify, source: &str) -> Self {
        let store = Arc::new(CountedStore::default());
        let mut engine = QueryEngine::new(
            default_environment(),
            hirsel_front_phases::compiler_adapters(),
        )
        .with_result_store(store.clone());
        engine.verify = verify;
        let file = engine
            .sources
            .add("immutable-launch.hir", source.into())
            .unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        let selected = vec![hirsel_engine::entry_path(&default_context())];
        let roots = engine.native_source_roots(&selected).unwrap();
        let preliminary = engine
            .native_semantic_program(roots.definitions.clone())
            .unwrap();
        let mut paths: BTreeSet<_> = roots.definitions.iter().cloned().collect();
        paths.extend(
            preliminary
                .source_bindings()
                .map(|(path, _, _)| path.clone()),
        );
        let paths: Vec<_> = paths.into_iter().collect();
        let raw = engine.export_native_input().unwrap();
        let validated = engine.import_native_input(raw).unwrap();
        let mut program = hirsel_native::adapt::program(&validated);
        let operations = Arc::new(program.operation_registry());
        let entries = program.definitions.iter().filter(|(path,_)| {
            matches!(&program.signatures[*path].kind,SignatureKind::Function(f) if f.generics.is_empty())
        }).map(|(path,definition)|(path.clone(),definition.entry.0)).collect();
        let mut declarations: BTreeMap<_, _> = paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    Arc::new(engine.native_definition_product(path).unwrap()),
                )
            })
            .collect();
        let lower = engine.native_original_reader(&paths).unwrap();
        for (path, source) in &mut declarations {
            Arc::make_mut(source).lower = lower
                .input(path)
                .and_then(|(output, _, _, _)| output.cloned());
        }
        // The semantic closure also binds CT-3 generator declarations that
        // have no runtime body in NativeInput.
        program.signatures.extend(
            declarations
                .iter()
                .map(|(path, product)| (path.clone(), product.signature.clone())),
        );
        let anchors = declarations
            .iter()
            .map(|(path, product)| {
                (
                    path.clone(),
                    validated.snapshot().definitions.get(path).map_or_else(
                        || vec![product.source.syntax.anchor],
                        |body| hirsel_tir::source_anchors(body),
                    ),
                )
            })
            .collect();
        let semantic = Arc::new(
            engine
                .native_semantic_program(roots.definitions.clone())
                .unwrap(),
        );
        let signatures = engine.signature_snapshot();
        let empty = engine.native_original_reader(&[]).unwrap();
        program.definitions.clear();
        // The constructor cannot fall back to source/controller queries.
        drop(engine);
        store.reads.store(0, Ordering::Relaxed);
        Self {
            store,
            program,
            operations,
            roots,
            declarations,
            entries,
            anchors,
            signatures,
            semantic,
            selected,
            lower,
            empty,
            validated,
        }
    }
    fn facts(&self) -> NativeLinkFacts<'_> {
        NativeLinkFacts {
            program: &self.program,
            operations: self.operations.clone(),
            roots: &self.roots,
            declarations: &self.declarations,
            entries: &self.entries,
            anchors: &self.anchors,
            signatures: self.signatures.clone(),
            semantic: self.semantic.clone(),
            selected: &self.selected,
            callees: None,
        }
    }
}

#[test]
fn settled_source_assembles_without_engine_or_body_reads_and_binds_actual_ready() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        for verify in [Verify::Off, Verify::On] {
            let fixture = Fixture::new(verify);
            let source = NativeLinkSource::from_settled(fixture.facts(), &fixture.lower).unwrap();
            assert_eq!(fixture.store.reads.load(Ordering::Relaxed), 0);
            assert_eq!(source.functions(), &fixture.entries);
            let options =
                hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
            let mut ready = crate::build(&fixture.validated, &options).unwrap();
            let entry = &fixture.selected[0];
            let revision = ready.revision(entry).unwrap().clone();
            source.bind(&mut ready).unwrap();
            assert_eq!(ready.revision(entry), Some(&revision));
            assert!(
                hirsel_native_testing::run_heap(&ready, entry, &source.sites())
                    .unwrap()
                    .observation
                    .failures
                    .is_empty()
            );
        }
    });
}

use hirsel_engine::{
    LinkInput, NativeDefinitionProduct, NativeOriginalReader, NativeRecordSource,
    NativeSemanticProgram, NativeSourceRoots, RepresentationRecord, SignatureSnapshot,
    ValidatedNativeInput,
};
use hirsel_lir::stage::InputIdentity;
use hirsel_span::{Anchor, SourceMap};
use hirsel_syntax::{DefId, DefPath};
use hirsel_types::{ForeignThreads, SignatureKind, SignatureRef, TypeInterner};
use std::collections::{BTreeMap, BTreeSet};
