//! Immutable completion values retain their exact observed products. They
//! convey query completion, never verifier or native-validator acceptance.
use super::{ArchiveAdmission, ProductArchive, ProductRef};
use hirsel_artifact_store::{Action, Address, Domain};
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
        store: &(impl crate::backing::ProductBacking + ?Sized),
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
        let result = ProductArchive::materialize(self.domain(), admission, |grant| {
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
