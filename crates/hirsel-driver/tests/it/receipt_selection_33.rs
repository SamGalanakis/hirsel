//! Original direct-call discovery through current source/V and CT-5 providers.
use hirsel_engine::*;
use hirsel_engine_store as store;
use hirsel_front_phases::compiler_adapters;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_native::receipt_source::provider::*;
use hirsel_native::receipt_source::selection::*;
use hirsel_rc::instance::{Actuals, key_of};
use hirsel_rc::sealed_source::{SourceError, closed_storage_type};
use hirsel_syntax::DefPath;
use hirsel_types::{GenericArgument, SignatureRef, TypeInterner, TypeMemo};
use std::sync::Arc;

struct Simulated<'a> {
    front: &'a Front<'a>,
    stale: Option<DefPath>,
}
impl Provider for Simulated<'_> {
    fn types(&self) -> &TypeInterner {
        self.front.types()
    }
    fn current(&self, path: &DefPath) -> Result<Current, SourceError> {
        let mut current = self.front.current(path)?;
        if self.stale.as_ref() == Some(path) {
            current.revision = hirsel_syntax::ContentHash::of_bytes(b"stale target revision");
        }
        Ok(current)
    }
    fn signature(&self, path: &DefPath) -> Result<SignatureRef, SourceError> {
        self.front.signature(path)
    }
    fn original(&self, path: &DefPath) -> Result<Arc<NativeOriginalBody>, SourceError> {
        self.front.original(path)
    }
}

fn source(text: &str) -> (QueryEngine, NativeOriginalReader, DefPath) {
    let store: Arc<dyn store::ResultStore> = Arc::new(store::MemStore::default());
    let mut engine = QueryEngine::new(default_environment(), compiler_adapters())
        .with_result_store(store.clone());
    let file = engine
        .sources
        .add("receipt-selection.hir", text.into())
        .unwrap();
    let checked = source_check_request(&mut engine, file, &default_context()).unwrap();
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let path = entry_path(&default_context());
    let facts = crate::native_front_fixture::products(&mut engine, &[path.clone()]).unwrap();
    let reader = facts
        .original_context
        .reader(facts.originals.iter().map(|product| (**product).clone()))
        .unwrap();
    (engine, reader, path)
}
fn base(front: &Front<'_>, path: &DefPath) -> hirsel_layout::instance::InstanceKey {
    let original = front.original(path).unwrap();
    key_of(
        front.types(),
        path,
        original.definition().entry,
        &Actuals::none(),
        None,
        None,
    )
    .unwrap()
}
fn call_block(
    front: &Front<'_>,
    key: &hirsel_layout::instance::InstanceKey,
    actuals: &Actuals,
    name: &str,
) -> u32 {
    let observed = named(front, key, actuals, &TypeMemo::new()).unwrap();
    *observed.demand().projection().calls().iter().find(|(_, call)| {
        matches!(&call.callee, hirsel_tir::Callee::Definition {path, ..} if path.0.name == name)
    }).expect("the original named call exists").0
}

#[test]
fn original_direct_calls_close_caller_binders_and_keep_exact_source_observations() {
    with_compiler_stack(|| {
        let (_engine, reader, path) = source(
            r#"
fn id[T](take x:T)->T { take x }
fn wrap[T](take x:T)->T { id(take x) }
pub fn main()->u64 { wrap(7u64) }
"#,
        );
        let front = Front::new(&reader);
        let root = base(&front, &path);
        let mut simulated = Simulated {
            front: &front,
            stale: None,
        };
        let block = call_block(&front, &root, &Actuals::none(), "wrap");
        let actual = direct(&front, &root, &Actuals::none(), block, &TypeMemo::new()).unwrap();
        let same = direct(&simulated, &root, &Actuals::none(), block, &TypeMemo::new()).unwrap();
        assert_eq!(actual.base, same.base);
        assert_eq!(actual.source.entry(), same.source.entry());
        assert_eq!(
            actual.source.demand().projection(),
            same.source.demand().projection()
        );
        let [GenericArgument::Type(ty)] = actual.actuals.arguments.as_slice() else {
            panic!("one selected original type")
        };
        let original = front.original(actual.base.definition()).unwrap();
        let entry = &original.definition().bodies[original.definition().entry.0 as usize];
        let local = entry.parameters[0];
        let port = hirsel_rc::instance::substitute(
            front.types(),
            &TypeMemo::new(),
            entry.locals[local.0 as usize].ty,
            &actual.actuals,
            true,
        )
        .unwrap();
        assert_eq!(
            actual.source.entry().parameters()[0].ty,
            closed_storage_type(front.types(), port).unwrap()
        );
        // Generic actuals use the language's normalization; original ports
        // additionally retain their own receiving mode defaults.
        assert_eq!(
            actual.source.entry().parameters()[0].ty.ty,
            closed_storage_type(front.types(), *ty).unwrap().ty
        );
        let block = call_block(&front, &actual.base, &actual.actuals, "id");
        let nested = direct(
            &front,
            &actual.base,
            &actual.actuals,
            block,
            &TypeMemo::new(),
        )
        .unwrap();
        assert_eq!(
            nested.source.entry().result(),
            actual.source.entry().result()
        );
        assert_eq!(nested.base.definition().0.name, "id");
        assert!(nested.actuals.outer.is_empty());
        let [GenericArgument::Type(nested_type)] = nested.actuals.arguments.as_slice() else {
            panic!("one closed inner type actual")
        };
        assert_eq!(
            hirsel_types::CanonicalSpelling::closed(front.types(), *nested_type),
            hirsel_types::CanonicalSpelling::closed(front.types(), *ty)
        );
        let nested_original = front.original(nested.base.definition()).unwrap();
        let nested_body =
            &nested_original.definition().bodies[nested_original.definition().entry.0 as usize];
        assert_eq!(nested.actuals.binder, Some(nested_body.parameter_scope));
        assert_ne!(nested.actuals.binder, actual.actuals.binder);
        assert!(matches!(
            direct(
                &front,
                &actual.base,
                &Actuals::none(),
                block,
                &TypeMemo::new()
            ),
            Err(SourceError::Malformed(
                "original named actual arity differs"
            ))
        ));
        simulated.stale = Some(actual.base.definition().clone());
        assert!(matches!(
            direct(
                &simulated,
                &root,
                &Actuals::none(),
                call_block(&front, &root, &Actuals::none(), "wrap"),
                &TypeMemo::new()
            ),
            Err(SourceError::Malformed(
                "receipt source differs from current front selection"
            ))
        ));
        assert!(matches!(
            direct(&front, &root, &Actuals::none(), u32::MAX, &TypeMemo::new()),
            Err(SourceError::Malformed(
                "receipt direct-call source selector"
            ))
        ));
    });
}
