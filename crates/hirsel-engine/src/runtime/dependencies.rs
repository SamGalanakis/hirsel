//! Immutable completion values retain their exact observed products. They
//! convey query completion, never verifier or native-validator acceptance.
use super::{ArchiveAdmission, ProductArchive, ProductRef};
use crate::store::{Action, Address, Domain, ResultStore};
use std::{
    io,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct LiveDependencyProduct(Arc<Owner>);
struct Owner {
    address: Address,
    action: Action,
    domain: Domain,
    reads: Vec<ProductRef>,
    archive: Mutex<Option<ProductArchive>>,
    _admission: Arc<dyn Send + Sync>,
}
impl LiveDependencyProduct {
    pub fn new(
        store: &dyn ResultStore,
        address: Address,
        action: Action,
        domain: Domain,
        mut reads: Vec<ProductRef>,
    ) -> io::Result<Self> {
        reads.shrink_to_fit();
        let bytes = (size_of::<Owner>() + 2 * size_of::<usize>())
            .checked_add(
                reads
                    .capacity()
                    .checked_mul(size_of::<ProductRef>())
                    .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?,
            )
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let admission = store
            .admit_product_memory(bytes as u64)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self(Arc::new(Owner {
            address,
            action,
            domain,
            reads,
            archive: Mutex::new(None),
            _admission: admission,
        })))
    }
    pub fn address(&self) -> Address {
        self.0.address
    }
    pub fn action(&self) -> Action {
        self.0.action
    }
    pub fn domain(&self) -> Domain {
        self.0.domain
    }
    pub fn reads(&self) -> &[ProductRef] {
        &self.0.reads
    }
    pub fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn equivalent(&self, other: &Self) -> bool {
        self.same_owner(other) || (self.domain() == other.domain() && self.reads() == other.reads())
    }
    pub fn archive_working_bytes(&self) -> io::Result<u64> {
        // Each fixed-width domain/address/reference row needs fewer than 512
        // JSON bytes. Include the row vector, encoder growth and final backing.
        (self.reads().len() as u64)
            .checked_mul(2048)
            .and_then(|n| n.checked_add(4096))
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
    }
    pub fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        let mut archive = self.0.archive.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(archive) = &*archive {
            return Ok(archive.clone());
        }
        let result = super::product::materialize_archive(self.domain(), admission, |grant| {
            if grant.bytes() < self.archive_working_bytes()? {
                return Err(io::Error::from(io::ErrorKind::OutOfMemory));
            }
            let rows = self
                .reads()
                .iter()
                .map(|read| {
                    read.archive(ArchiveAdmission::Retained)
                        .map(|archive| (read.domain().kind, read.address.0, archive.reference()))
                })
                .collect::<io::Result<Vec<_>>>()?;
            serde_json::to_vec(&rows).map_err(io::Error::other)
        })?;
        *archive = Some(result.clone());
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::ProcessBudget,
        store::{Bytes, EntryKey, Reference, RequestStore},
    };
    use hirsel_syntax::ContentHash;

    struct NoTransport;
    impl ResultStore for NoTransport {
        fn get(&self, _: &EntryKey) -> Option<Bytes> {
            panic!("live completion read the store")
        }
        fn put(&self, _: &EntryKey, _: &[u8]) {
            panic!("live completion wrote the store")
        }
        fn name(&self) -> &'static str {
            "live-only-test"
        }
        fn persistence(&self) -> crate::store::Persistence {
            crate::store::Persistence::Disabled
        }
    }
    fn value(store: &dyn ResultStore, reads: Vec<ProductRef>) -> LiveDependencyProduct {
        LiveDependencyProduct::new(
            store,
            Address(ContentHash::of_bytes(b"completion")),
            Action(ContentHash::of_bytes(b"complete action")),
            Domain {
                kind: 0x4e00_0022,
                schema: 2,
            },
            reads,
        )
        .unwrap()
    }

    #[test]
    fn live_edges_need_no_transport_and_release_only_after_the_last_consumer() {
        let budget = ProcessBudget::new(64 << 10);
        let store = Arc::new(RequestStore::new(
            Arc::new(NoTransport),
            budget.reserve(64 << 10).unwrap(),
        ));
        let producer = ProductRef::dependencies(value(store.as_ref(), Vec::new()));
        let child_charge = store.retained_bytes();
        let parent = ProductRef::dependencies(value(store.as_ref(), vec![producer.clone()]));
        assert!(store.retained_bytes() > child_charge);
        let equal = ProductRef::dependencies(value(store.as_ref(), vec![producer.clone()]));
        assert!(
            !parent
                .dependency_value()
                .unwrap()
                .same_owner(equal.dependency_value().unwrap())
        );
        assert_eq!(parent, equal);
        let mut stale = equal.clone();
        stale.action = Action(ContentHash::of_bytes(b"stale completion"));
        assert_ne!(parent, stale);
        drop(stale);
        drop(equal);
        let consumer = parent.dependency_value().unwrap().clone();
        assert_eq!(consumer.reads(), std::slice::from_ref(&producer));
        assert!(consumer.reads()[0].lower_value().is_none());
        assert!(consumer.reads()[0].analyzed_value().is_none());
        drop(producer);
        drop(parent);
        let child_view = consumer.reads()[0].clone();
        drop(consumer);
        assert_eq!(store.retained_bytes(), child_charge);
        let weak = Arc::downgrade(&store);
        drop(store);
        assert!(weak.upgrade().is_none());
        assert_eq!(budget.reserved(), 64 << 10);
        assert!(child_view.dependency_value().unwrap().reads().is_empty());
        drop(child_view);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn live_edges_refuse_exhaustion_and_archive_only_with_explicit_working_admission() {
        let tiny = ProcessBudget::new(1);
        let store = RequestStore::new(Arc::new(NoTransport), tiny.reserve(1).unwrap());
        assert!(
            LiveDependencyProduct::new(
                &store,
                Address(ContentHash::default()),
                Action(ContentHash::default()),
                Domain { kind: 1, schema: 2 },
                Vec::new()
            )
            .is_err()
        );
        assert_eq!(store.retained_bytes(), 0);
        assert!(store.exhausted());
        drop(store);
        assert_eq!(tiny.reserved(), 0);

        let budget = ProcessBudget::new(64 << 10);
        let store = RequestStore::new(Arc::new(NoTransport), budget.reserve(64 << 10).unwrap());
        let child = super::super::test_product(
            Address(ContentHash::of_bytes(b"child")),
            Action(ContentHash::of_bytes(b"child action")),
            Domain { kind: 7, schema: 1 },
            b"checked child archive",
        );
        let child_reference = child
            .archive(ArchiveAdmission::Retained)
            .unwrap()
            .reference();
        let product = ProductRef::dependencies(value(&store, vec![child]));
        let live = product.dependency_value().unwrap();
        assert!(product.archive(ArchiveAdmission::Retained).is_err());
        let archive_budget = ProcessBudget::new(64 << 10);
        let mut scratch = archive_budget.reserve(64 << 10).unwrap();
        assert!(
            product
                .archive(ArchiveAdmission::Materialize {
                    scratch: &mut scratch,
                    working_bytes: 1
                })
                .is_err()
        );
        assert_eq!(scratch.bytes(), 64 << 10);
        assert!(product.archive(ArchiveAdmission::Retained).is_err());
        let archive = product
            .archive(ArchiveAdmission::Materialize {
                scratch: &mut scratch,
                working_bytes: live.archive_working_bytes().unwrap(),
            })
            .unwrap();
        let rows: Vec<(u32, ContentHash, Reference)> =
            serde_json::from_slice(archive.bytes()).unwrap();
        assert_eq!(rows, vec![(7, live.reads()[0].address.0, child_reference)]);
        assert_eq!(
            archive.reference(),
            live.domain().reference(archive.bytes()).unwrap()
        );
        assert_eq!(
            product
                .archive(ArchiveAdmission::Retained)
                .unwrap()
                .reference(),
            archive.reference()
        );
        drop(scratch);
        drop(product);
        drop(store);
        assert_eq!(budget.reserved(), 0);
        assert!(archive_budget.reserved() > 0);
        drop(archive);
        assert_eq!(archive_budget.reserved(), 0);
    }
}
