//! Local plan ownership and the explicit portable identity boundary. Imported
//! descriptors describe cache identity only; local C still admits every body.
use hirsel_engine::store::ResultStore;
use hirsel_engine::store::{Domain, Reference};
use hirsel_engine_runtime::*;
use hirsel_syntax::ContentHash;
use std::{collections::BTreeMap, sync::Arc};
use std::{io, sync::Mutex};

pub struct ExecutionPlan {
    pub environments: BTreeMap<ContentHash, Option<hirsel_lir::LayoutId>>,
    pub binding: hirsel_lir::products::Binding,
    root: ProductRef,
    reads: Vec<ProductRef>,
    pub ahead: hirsel_lir::ahead::AheadPlan,
    pub continuation: hirsel_lir::continuation::ContinuationPlans,
    context: Reference,
    identity: ContentHash,
    archive: Mutex<Option<ProductArchive>>,
    _admission: Arc<dyn Send + Sync>,
}
impl ExecutionPlan {
    pub fn root(&self) -> &ProductRef {
        &self.root
    }
    pub fn reads(&self) -> &[ProductRef] {
        &self.reads
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: &dyn ResultStore,
        environments: BTreeMap<ContentHash, Option<hirsel_lir::LayoutId>>,
        binding: hirsel_lir::products::Binding,
        root: ProductRef,
        mut reads: Vec<ProductRef>,
        ahead: hirsel_lir::ahead::AheadPlan,
        continuation: hirsel_lir::continuation::ContinuationPlans,
        context: Reference,
    ) -> Result<Arc<Self>, String> {
        reads.shrink_to_fit();
        let mut weight = hirsel_engine::NativeMetadataWeight::default();
        weight.add(size_of::<Self>() + size_of::<ExecutionProduct>() + 4 * size_of::<usize>());
        weight.flat_vec(&reads);
        weight.map(&environments, |_, _, _| ());
        retained::binding(&mut weight, &binding);
        retained::ahead(&mut weight, &ahead);
        retained::continuations(&mut weight, &continuation);
        let admission = store
            .admit_product_memory(weight.bytes())
            .ok_or("execution plan exceeds retained admission")?;
        let identity = descriptor_identity(ahead.identity, continuation.identity, context);
        Ok(Arc::new(Self {
            environments,
            binding,
            root,
            reads,
            ahead,
            continuation,
            context,
            identity,
            archive: Mutex::new(None),
            _admission: admission,
        }))
    }
    fn descriptor(&self) -> (ContentHash, ContentHash, Reference) {
        (
            self.ahead.identity,
            self.continuation.identity,
            self.context,
        )
    }
}
impl LiveProductValue for ExecutionPlan {
    fn domain(&self) -> Domain {
        PLAN
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        let mut cached = self.archive.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(archive) = &*cached {
            return Ok(archive.clone());
        }
        let archive = ProductArchive::materialize(PLAN, admission, |grant| {
            // Three fixed-width identity fields, encoder growth and backing.
            if grant.bytes() < 4096 {
                return Err(io::ErrorKind::OutOfMemory.into());
            }
            serde_json::to_vec(&self.descriptor()).map_err(io::Error::other)
        })?;
        *cached = Some(archive.clone());
        Ok(archive)
    }
}

pub fn archive_plan(
    store: &dyn ResultStore,
    product: &ProductRef,
    admission: ArchiveAdmission<'_>,
    verify: hirsel_engine::store::VerifyStore,
) -> Result<ProductArchive, String> {
    let archive = product.archive(admission).map_err(|e| e.to_string())?;
    let reference = archive.reference();
    if verify == hirsel_engine::store::VerifyStore::Recompute
        && let Some(previous) = store
            .lookup_product(product.address)
            .filter(|previous| previous.action == product.action)
        && previous.reference != reference
    {
        return Err("recomputed execution product differs from cached output".into());
    }
    if store.put_content(PLAN, archive.bytes()) != Some(reference) {
        return Err("execution archive publication identity differs".into());
    }
    store.associate_product(product.address, product.action, reference);
    Ok(archive)
}

/// Equal settled plans retain semantic cutoff independently of query actions.
/// Hash only the already checked planner identities and exact context reference.
/// No plan encoding or allocation is needed for an in-process action key.
pub fn identity(product: &ProductRef) -> Result<ContentHash, String> {
    if product.domain() != PLAN {
        return Err("wrong execution plan domain/schema".into());
    }
    if let Some(plan) = product.live_value::<ExecutionPlan>() {
        return Ok(plan.identity);
    }
    let (ahead, continuation, context) = {
        let archive = product
            .archive(ArchiveAdmission::Retained)
            .map_err(|e| e.to_string())?;
        let descriptor: (ContentHash, ContentHash, Reference) =
            serde_json::from_slice(archive.bytes()).map_err(|e| e.to_string())?;
        if serde_json::to_vec(&descriptor).map_err(|e| e.to_string())? != archive.bytes() {
            return Err("noncanonical execution plan archive".into());
        }
        descriptor
    };
    Ok(descriptor_identity(ahead, continuation, context))
}
fn descriptor_identity(
    ahead: ContentHash,
    continuation: ContentHash,
    context: Reference,
) -> ContentHash {
    ContentHash::of_parts(
        "hirsel.native.execution-identity.v2",
        [
            ahead.0.as_slice(),
            continuation.0.as_slice(),
            context.domain.kind.to_le_bytes().as_slice(),
            context.domain.schema.to_le_bytes().as_slice(),
            context.output.0.0.as_slice(),
            context.len.to_le_bytes().as_slice(),
        ],
    )
}

mod retained;

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_engine::store::{self, Bytes, EntryKey};
    struct NoTransport;
    impl ResultStore for NoTransport {
        fn capability(&self) -> hirsel_engine::store::ProviderCapability {
            hirsel_engine::store::ProviderCapability::Writable
        }

        fn get(&self, _: &EntryKey) -> Option<Bytes> {
            panic!("local plan read bytes")
        }
        fn put(&self, _: &EntryKey, _: &[u8]) {
            panic!("local plan wrote bytes")
        }
        fn name(&self) -> &'static str {
            "live-execution-plan"
        }
    }
    fn hash(bytes: &[u8]) -> ContentHash {
        ContentHash::of_bytes(bytes)
    }
    pub fn observed() -> ProductRef {
        let store = store::MemStore::default();
        let domain = Domain {
            kind: 0x1234,
            schema: 1,
        };
        let reference = store.put_content(domain, b"observed root").unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(4096);
        let mut scratch = budget.reserve(4096).unwrap();
        ProductRef::import_archive(
            Address(hash(b"root")),
            Action(hash(b"root action")),
            ProductArchive::open(&store, reference, &mut scratch).unwrap(),
        )
    }
    pub fn plan(store: &dyn ResultStore, context: Reference) -> Arc<ExecutionPlan> {
        plan_with_identities(store, context, hash(b"ahead"), hash(b"continuation"))
    }
    fn plan_with_identities(
        store: &dyn ResultStore,
        context: Reference,
        ahead: ContentHash,
        continuation: ContentHash,
    ) -> Arc<ExecutionPlan> {
        let target = hirsel_lir::TargetSpec::x86_64_unknown_linux_gnu();
        let root = observed();
        ExecutionPlan::new(
            store,
            BTreeMap::new(),
            hirsel_lir::products::Binding::new(&target, &[]).unwrap(),
            root.clone(),
            vec![root],
            hirsel_lir::ahead::AheadPlan {
                identity: ahead,
                ..Default::default()
            },
            hirsel_lir::continuation::ContinuationPlans {
                identity: continuation,
                ..Default::default()
            },
            context,
        )
        .unwrap()
    }
    fn product(plan: Arc<ExecutionPlan>) -> ProductRef {
        ProductRef::live(Address(hash(b"plan")), Action(hash(b"plan action")), plan)
    }
    pub fn context() -> Reference {
        Domain { kind: 8, schema: 3 }
            .reference(b"emission context")
            .unwrap()
    }
    #[test]
    fn local_plan_retains_observations_and_admission_without_transport() {
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let store =
            store::RequestStore::new(Arc::new(NoTransport), budget.reserve(1 << 20).unwrap());
        let owner = plan(&store, context());
        let charge = store.retained_bytes();
        assert!(charge > size_of::<ExecutionPlan>() as u64);
        let product = product(owner.clone());
        let held = product.clone();
        assert_eq!(product, held);
        assert!(product.archive(ArchiveAdmission::Retained).is_err());
        assert_eq!(owner.reads, vec![owner.root.clone()]);
        assert_eq!(identity(&product).unwrap(), identity(&held).unwrap());
        let mut stale = held.clone();
        stale.action = Action(hash(b"another admission"));
        assert_ne!(stale, product);
        // Action changes do not change a settled plan's cache identity.
        assert_eq!(identity(&stale).unwrap(), identity(&product).unwrap());
        drop(stale);
        drop(owner);
        drop(product);
        assert_eq!(store.retained_bytes(), charge);
        drop(store);
        assert_eq!(budget.reserved(), 1 << 20);
        assert!(held.live_value::<ExecutionPlan>().is_some());
        drop(held);
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn explicit_plan_archive_round_trip_preserves_identity_without_native_authority() {
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let store =
            store::RequestStore::new(Arc::new(NoTransport), budget.reserve(1 << 20).unwrap());
        let live = product(plan(&store, context()));
        let archive_budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
        let mut scratch = archive_budget.reserve(64 << 10).unwrap();
        assert!(
            live.archive(ArchiveAdmission::Materialize {
                scratch: &mut scratch,
                working_bytes: 1
            })
            .is_err()
        );
        assert!(live.archive(ArchiveAdmission::Retained).is_err());
        let transport = store::MemStore::default();
        let archive = archive_plan(
            &transport,
            &live,
            ArchiveAdmission::Materialize {
                scratch: &mut scratch,
                working_bytes: 4096,
            },
            hirsel_engine::store::VerifyStore::Off,
        )
        .unwrap();
        let reference = archive.reference();
        assert_eq!(
            transport.lookup_product(live.address).unwrap().reference,
            reference
        );
        assert_eq!(reference, archive.reference());
        let reopened = ProductRef::import_archive(
            live.address,
            live.action,
            ProductArchive::open(&transport, reference, &mut scratch).unwrap(),
        );
        assert_eq!(identity(&live).unwrap(), identity(&reopened).unwrap());
        assert_ne!(
            live, reopened,
            "an imported descriptor is not the locally admitted plan"
        );
        assert!(reopened.live_value::<ExecutionPlan>().is_none());
        assert_eq!(
            live.archive(ArchiveAdmission::Retained)
                .unwrap()
                .reference(),
            reference
        );
        for changed in [
            Reference {
                len: context().len + 1,
                ..context()
            },
            Reference {
                domain: Domain { kind: 8, schema: 4 },
                ..context()
            },
            Reference {
                output: store::Output(hash(b"other context")),
                ..context()
            },
        ] {
            assert_ne!(
                identity(&live).unwrap(),
                identity(&product(plan(&store, changed))).unwrap()
            );
        }
        for (ahead, continuation) in [
            (hash(b"changed"), hash(b"continuation")),
            (hash(b"ahead"), hash(b"changed")),
        ] {
            let changed = plan_with_identities(&store, context(), ahead, continuation);
            assert_ne!(
                identity(&live).unwrap(),
                identity(&product(changed)).unwrap()
            );
        }
    }
    #[test]
    fn plan_import_refuses_corruption_noncanonical_descriptors_and_wrong_schema() {
        struct Corrupt;
        impl ResultStore for Corrupt {
            fn capability(&self) -> hirsel_engine::store::ProviderCapability {
                hirsel_engine::store::ProviderCapability::ReadOnly
            }

            fn get(&self, _: &EntryKey) -> Option<Bytes> {
                None
            }
            fn put(&self, _: &EntryKey, _: &[u8]) {}
            fn get_content(&self, _: Reference) -> Option<Bytes> {
                Some(Bytes::from(b"damaged".as_slice()))
            }
            fn name(&self) -> &'static str {
                "corrupted-plan-import"
            }
        }
        let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
        let mut scratch = budget.reserve(64 << 10).unwrap();
        let expected = PLAN.reference(b"original descriptor").unwrap();
        assert!(
            ProductArchive::open(&Corrupt as &dyn ResultStore, expected, &mut scratch).is_err()
        );
        let store = store::MemStore::default();
        let canonical =
            serde_json::to_vec(&(hash(b"ahead"), hash(b"continuation"), context())).unwrap();
        let mut noncanonical = vec![b' '];
        noncanonical.extend_from_slice(&canonical);
        for (domain, bytes) in [
            (PLAN, b"truncated".to_vec()),
            (PLAN, noncanonical),
            (
                Domain {
                    schema: PLAN.schema + 1,
                    ..PLAN
                },
                canonical,
            ),
        ] {
            let reference = store.put_content(domain, &bytes).unwrap();
            let imported = ProductRef::import_archive(
                Address(hash(b"plan")),
                Action(hash(b"action")),
                ProductArchive::open(&store, reference, &mut scratch).unwrap(),
            );
            assert!(identity(&imported).is_err());
        }
    }
}

#[cfg(test)]
#[test]
fn plan_admission_refuses_retained_capacity_before_publication() {
    let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
    let store = hirsel_engine::store::RequestStore::new(
        Arc::new(hirsel_engine::store::MemStore::default()),
        budget.reserve(64 << 10).unwrap(),
    );
    let ordinary = tests::plan(&store, tests::context());
    assert!(store.retained_bytes() > 0);
    drop(ordinary);
    assert_eq!(store.retained_bytes(), 0);
    let target = hirsel_lir::TargetSpec::x86_64_unknown_linux_gnu();
    let root = tests::observed();
    let mut ahead = hirsel_lir::ahead::AheadPlan::default();
    ahead
        .refusals
        .insert(ContentHash::default(), String::with_capacity(128 << 10));
    let result = ExecutionPlan::new(
        &store,
        BTreeMap::new(),
        hirsel_lir::products::Binding::new(&target, &[]).unwrap(),
        root,
        Vec::new(),
        ahead,
        Default::default(),
        tests::context(),
    );
    assert!(
        result.is_err(),
        "spare plan capacity escaped retained admission"
    );
    assert_eq!(store.retained_bytes(), 0);
    drop(store);
    assert_eq!(budget.reserved(), 0);
}

const PLAN: Domain = Domain {
    kind: 0x4e00_0031,
    schema: 1,
};
pub struct ExecutionProduct {
    pub product: ProductRef,
    pub plan: Arc<ExecutionPlan>,
}

#[cfg(test)]
use hirsel_engine::store::{Action, Address};
