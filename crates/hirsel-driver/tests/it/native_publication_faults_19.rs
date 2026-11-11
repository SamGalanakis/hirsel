//! Original private plan missing/corrupt/publication-failure vectors over Codegen.
use hirsel_driver::native;
use hirsel_engine::*;
use hirsel_engine_runtime::*;
use hirsel_engine_store as store;
use hirsel_engine_store::ResultStore;
use hirsel_intern::jobs::with_compiler_stack;

use hirsel_front_phases::compiler_adapters;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Faults {
    disk: store::DirStore,
    hidden: Mutex<Vec<store::Reference>>,
    flipped: Mutex<Vec<store::Reference>>,
    /// Once set, another build rewrites each arena association with its own
    /// action right after this build first reads it.
    foreign: Mutex<Option<Vec<store::Address>>>,
    arena_writes: AtomicUsize,
    substituted: Mutex<Vec<(store::Address, store::Association)>>,
    drop_writes: AtomicBool,
    fragment_puts: AtomicUsize,
}
impl ResultStore for Faults {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        self.disk.admit_product_memory(bytes)
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.disk.retention_scope()
    }

    fn capability(&self) -> hirsel_engine_store::ProviderCapability {
        self.disk.capability()
    }

    fn get(&self, key: &store::EntryKey) -> Option<store::Bytes> {
        self.disk.get(key)
    }
    fn put(&self, key: &store::EntryKey, bytes: &[u8]) -> hirsel_engine_store::WriteOutcome {
        self.disk.put(key, bytes)
    }
    fn local_product_store(&self) -> Option<hirsel_artifact_store::Store> {
        self.disk.local_product_store()
    }
    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        self.disk.verifier()
    }
    fn get_content(&self, reference: store::Reference) -> Option<store::Bytes> {
        if self.hidden.lock().unwrap().contains(&reference) {
            None
        } else if self.flipped.lock().unwrap().contains(&reference) {
            let mut bytes = self.disk.get_content(reference)?.to_vec();
            *bytes.last_mut()? ^= 1;
            Some(bytes.into())
        } else {
            self.disk.get_content(reference)
        }
    }
    fn put_content(&self, domain: store::Domain, bytes: &[u8]) -> Option<store::Reference> {
        if domain.kind == 0x4f00_0001 {
            self.fragment_puts.fetch_add(1, Ordering::Relaxed);
            if self.drop_writes.load(Ordering::Relaxed) {
                return None;
            }
        }
        let written = self.disk.put_content(domain, bytes);
        if let Some(reference) = written {
            self.hidden
                .lock()
                .unwrap()
                .retain(|hidden| *hidden != reference);
        }
        written
    }
    fn lookup_product(&self, address: store::Address) -> Option<store::Association> {
        self.substituted
            .lock()
            .unwrap()
            .iter()
            .find(|(key, _)| *key == address)
            .map(|(_, row)| *row)
            .or_else(|| {
                let mut row = self.disk.lookup_product(address)?;
                if let Some(read) = self.foreign.lock().unwrap().as_mut()
                    && ARENA_KINDS.contains(&row.reference.domain.kind)
                {
                    if read.contains(&address) {
                        row.action = store::Action(hirsel_syntax::ContentHash::of_parts(
                            "another build's arena action",
                            [row.action.0.0.as_slice()],
                        ));
                    } else {
                        read.push(address);
                    }
                }
                Some(row)
            })
    }
    fn associate_product(
        &self,
        address: store::Address,
        action: store::Action,
        reference: store::Reference,
    ) {
        if !(reference.domain.kind == 0x4f00_0001 && self.drop_writes.load(Ordering::Relaxed)) {
            self.disk.associate_product(address, action, reference);
        }
        if ARENA_KINDS.contains(&reference.domain.kind) {
            self.arena_writes.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn name(&self) -> &'static str {
        "native-publication-fault-controls"
    }
}
const ARENA_KINDS: [u32; 2] = [
    hirsel_native::query_model::ARENA_CANDIDATES_KIND,
    hirsel_native::query_model::ARENA_CHOSEN_KIND,
];
fn compile(faults: Arc<Faults>, verify: Verify) -> crate::native_object_products_19::Objects {
    compile_inspect(faults, verify, PLAN, false, |_, _, _| ())
}
const PLAN: &str = "fn helper(x:u64)->u64{x+1u64}\nfn main()->u64{helper(7u64)}";
fn compile_inspect(
    faults: Arc<Faults>,
    verify: Verify,
    source: &str,
    execution: bool,
    inspect: impl FnOnce(
        &mut hirsel_native_queries::roots::RootProducts,
        &Arc<hirsel_native::query_model::NativeRootProduct>,
        &[Arc<hirsel_native::query_model::ReadyProduct>],
    ),
) -> crate::native_object_products_19::Objects {
    let physical = faults.disk.local_product_store().unwrap();
    let publication = physical.request();
    let budget = hirsel_engine_admission::ProcessBudget::new(24 << 20);
    let store: Arc<dyn ResultStore> = Arc::new(store::RequestStore::new(
        faults,
        budget.reserve(24 << 20).unwrap(),
    ));
    let mut engine = QueryEngine::new(default_environment(), compiler_adapters())
        .with_result_store(store.clone());
    engine.verify = verify;
    let file = engine.sources.add("plan.hir", source.into()).unwrap();
    let checked = source_check_request(&mut engine, file, &default_context()).unwrap();
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let mut options =
        hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
    options.validation = hirsel_native::Validation::Ready;
    let result = crate::native_object_products_19::compile_front(
        crate::native_front_fixture::products(&mut engine, &[]).unwrap(),
        store,
        &[entry_path(&default_context())],
        hirsel_native::Backend::Cranelift,
        hirsel_native::OptLevel::None,
        options,
        false,
        hirsel_intern::jobs::Jobs::ONE,
        store::VerifyStore::Off,
        execution,
        inspect,
        |_, _| (),
    )
    .0;
    drop(engine);
    publication.finish().unwrap();
    result
}
#[test]
fn original_missing_corrupt_metadata_and_failed_publication_recompute_without_losing_output() {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Faults {
            disk: store::DirStore::new(directory.path().join("products")),
            hidden: Mutex::new(vec![]),
            flipped: Mutex::new(vec![]),
            foreign: Mutex::new(None),
            arena_writes: AtomicUsize::new(0),
            substituted: Mutex::new(vec![]),
            drop_writes: AtomicBool::new(false),
            fragment_puts: AtomicUsize::new(0),
        });
        let cold = compile(store.clone(), Verify::On);
        assert!(!cold.objects.is_empty());
        let (key, record) = cold.products.first().unwrap();
        store.hidden.lock().unwrap().push(
            record
                .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                .unwrap()
                .reference(),
        );
        let before = store.fragment_puts.load(Ordering::Relaxed);
        let missing = compile(store.clone(), Verify::On);
        assert!(
            store.fragment_puts.load(Ordering::Relaxed) > before,
            "missing record did not recompute"
        );
        assert_eq!(cold.objects, missing.objects);
        store.hidden.lock().unwrap().clear();
        let truncated = store
            .disk
            .put_content(
                record
                    .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                    .unwrap()
                    .reference()
                    .domain,
                b"truncated",
            )
            .unwrap();
        store.substituted.lock().unwrap().push((
            key.address(),
            store::Association {
                action: record.action,
                reference: truncated,
            },
        ));
        let before = store.fragment_puts.load(Ordering::Relaxed);
        let corrupt = compile(store.clone(), Verify::On);
        assert!(
            store.fragment_puts.load(Ordering::Relaxed) > before,
            "malformed record did not recompute"
        );
        assert_eq!(cold.objects, corrupt.objects);
        store.substituted.lock().unwrap().clear();
        // A fresh durable store refuses every fragment publication. The
        // bounded request transport still owns each actual produced output.
        let denied = Arc::new(Faults {
            disk: store::DirStore::new(directory.path().join("denied")),
            hidden: Mutex::new(vec![]),
            flipped: Mutex::new(vec![]),
            foreign: Mutex::new(None),
            arena_writes: AtomicUsize::new(0),
            substituted: Mutex::new(vec![]),
            drop_writes: AtomicBool::new(true),
            fragment_puts: AtomicUsize::new(0),
        });
        let unpublished = compile(denied.clone(), Verify::On);
        assert!(denied.fragment_puts.load(Ordering::Relaxed) > 0);
        assert_eq!(cold.objects, unpublished.objects);
        for (key, _) in &unpublished.products {
            assert!(
                denied.lookup_product(key.address()).is_none(),
                "failed durable write published an association"
            );
        }
        // Verification off still uses independent native C, never an object
        // byte association as source or native validation authority.
        let unverified = compile(denied.clone(), Verify::Off);
        assert_eq!(cold.objects, unverified.objects);
    });
}

/// Every consumer of a Ready body reads and checks the stored bytes, also when
/// the snapshot already validated that body under the execution plan. The
/// validated units a snapshot keeps never stand in for the body itself.
#[test]
fn a_ready_body_validated_under_the_execution_plan_is_refused_once_its_bytes_change() {
    use hirsel_native::query_model::NativeProducts;
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Faults {
            disk: store::DirStore::new(directory.path().join("products")),
            hidden: Mutex::new(vec![]),
            flipped: Mutex::new(vec![]),
            foreign: Mutex::new(None),
            arena_writes: AtomicUsize::new(0),
            substituted: Mutex::new(vec![]),
            drop_writes: AtomicBool::new(false),
            fragment_puts: AtomicUsize::new(0),
        });
        let cold = compile(store.clone(), Verify::On);
        let flipped = store.clone();
        let warm = compile_inspect(
            store.clone(),
            Verify::On,
            PLAN,
            true,
            move |products, _, readies| {
                assert!(!readies.is_empty());
                products.with_ready(readies, |_, _| Ok(())).unwrap();
                for ready in readies {
                    *flipped.flipped.lock().unwrap() = vec![ready.ready];
                    assert!(
                        products.with_ready(readies, |_, _| Ok(())).is_err(),
                        "a changed Ready body was accepted"
                    );
                }
                flipped.flipped.lock().unwrap().clear();
                products.with_ready(readies, |_, _| Ok(())).unwrap();
            },
        );
        assert_eq!(cold.objects, warm.objects);
    });
}

/// Another build sharing the store may rewrite an arena association after
/// this build's lookup hit it and before its Ready check, or before its next
/// run. The association is never authority: the Ready check uses the summary
/// this build holds, and a foreign action on a later lookup is a miss.
#[test]
fn an_arena_association_another_build_rewrote_is_a_miss_never_an_error() {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Faults {
            disk: store::DirStore::new(directory.path().join("products")),
            hidden: Mutex::new(vec![]),
            flipped: Mutex::new(vec![]),
            foreign: Mutex::new(None),
            arena_writes: AtomicUsize::new(0),
            substituted: Mutex::new(vec![]),
            drop_writes: AtomicBool::new(false),
            fragment_puts: AtomicUsize::new(0),
        });
        let cold = compile_inspect(store.clone(), Verify::On, ARENA, false, |_, _, _| ());
        let settled = store.arena_writes.swap(0, Ordering::Relaxed);
        assert!(settled > 0, "the program settled no arena component");
        *store.foreign.lock().unwrap() = Some(vec![]);
        // Every arena lookup hits once, then reads another build's action.
        let rewritten = compile_inspect(store.clone(), Verify::On, ARENA, false, |_, _, _| ());
        assert_eq!(store.arena_writes.swap(0, Ordering::Relaxed), 0);
        // Every arena association now carries a foreign action: all miss.
        let foreign = compile_inspect(store.clone(), Verify::On, ARENA, false, |_, _, _| ());
        assert_eq!(
            store.arena_writes.load(Ordering::Relaxed),
            settled,
            "a foreign arena action was taken as a hit"
        );
        assert_eq!(cold.objects, rewritten.objects);
        assert_eq!(cold.objects, foreign.objects);
    });
}
const ARENA: &str = "type Item { n:u64 }\nfn first(a:Arena,n:u64)->(box Item) in a {if n==0u64 {box Item(n:6u64)} else {second(a,n-1u64)}}\nfn second(a:Arena,n:u64)->(box Item) in a {first(a,n)}\nfn wrapper(a:Arena)->(box Item) in a {second(a,2u64)}\nfn main(){arena.with(fn(a:Arena){let x=wrapper(a);});}";

/// C's verdict on a Ready unit reads the bound layout table besides the body.
/// A unit validated under one table is validated again when asked for under
/// another, and the held unit is handed out only under the same table. (The
/// adapter half of the scope is tested on `ValidatedUnits` itself: no unit of
/// this program adds an adapter beyond another's.)
#[test]
fn a_validated_ready_unit_is_reused_only_under_the_table_it_was_validated_under() {
    use hirsel_native::query_model::{NativeProducts, ReadyProduct};
    use hirsel_native_queries::roots::RootProducts;
    use std::collections::BTreeMap;
    // `other` and `third` bind disjoint layouts.
    const SOURCE: &str = "type P { n:u64 }\ntype Q { a:u64, b:u64 }\nfn other(q:Q)->u64 { q.a + q.b }\nfn third(p:P)->u64 { p.n + 1u64 }\nfn main()->u64 { other(Q(a:1u64,b:2u64)) + third(P(n:3u64)) }";
    /// Validations the call ran, and each returned unit by name and address.
    fn validated(
        products: &mut RootProducts,
        selection: &[Arc<ReadyProduct>],
    ) -> (u64, BTreeMap<String, usize>) {
        let before = products.ready_materializations();
        let units = products
            .with_ready(selection, |units, _| {
                Ok(units
                    .iter()
                    .map(|unit| {
                        (
                            unit.unit().header.definition.0.name.clone(),
                            std::ptr::from_ref(*unit) as usize,
                        )
                    })
                    .collect())
            })
            .unwrap();
        (products.ready_materializations() - before, units)
    }
    fn pick(
        products: &mut RootProducts,
        readies: &[Arc<ReadyProduct>],
        names: &[&str],
    ) -> Vec<Arc<ReadyProduct>> {
        readies
            .iter()
            .filter(|ready| {
                let (_, units) = validated(products, std::slice::from_ref(ready));
                units.keys().any(|name| names.contains(&name.as_str()))
            })
            .cloned()
            .collect()
    }
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Faults {
            disk: store::DirStore::new(directory.path().join("products")),
            hidden: Mutex::new(vec![]),
            flipped: Mutex::new(vec![]),
            foreign: Mutex::new(None),
            arena_writes: AtomicUsize::new(0),
            substituted: Mutex::new(vec![]),
            drop_writes: AtomicBool::new(false),
            fragment_puts: AtomicUsize::new(0),
        });
        compile_inspect(store.clone(), Verify::On, SOURCE, false, |_, _, _| ());
        // Without an execution plan each call binds only its selection, so
        // adding `third` changes the table.
        compile_inspect(
            store.clone(),
            Verify::On,
            SOURCE,
            false,
            |products, _, readies| {
                let other = pick(products, readies, &["other"]);
                let wider = pick(products, readies, &["other", "third"]);
                assert_eq!((other.len(), wider.len()), (1, 2));
                let (_, first) = validated(products, &other);
                let (again, same) = validated(products, &other);
                assert_eq!(again, 0, "same table and adapters validated again");
                assert_eq!(same["other"], first["other"]);
                let (ran, wide) = validated(products, &wider);
                assert_eq!(ran, 2, "a unit validated under one table served another");
                assert_ne!(wide["other"], first["other"]);
            },
        );
    });
}
