//! Checked archives retain their full backing and refundable admission.
use hirsel_artifact_store::{Bytes, Domain, Reference};
use hirsel_engine_admission::{RetainedArchiveLease, retained_arc_bytes};
use std::{io, sync::Arc};
pub enum ArchiveAdmission<'a> {
    Retained,
    Materialize {
        scratch: &'a mut hirsel_engine_admission::MemoryReservation,
        working_bytes: u64,
    },
}

fn materialize_archive(
    domain: Domain,
    admission: ArchiveAdmission<'_>,
    publish: impl FnOnce(&hirsel_engine_admission::MemoryReservation) -> io::Result<Vec<u8>>,
) -> io::Result<ProductArchive> {
    let ArchiveAdmission::Materialize {
        scratch,
        working_bytes: required,
    } = admission
    else {
        return Err(io::Error::from(io::ErrorKind::OutOfMemory));
    };
    let mut grant = scratch
        .split(required)
        .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
    // The owning codec declares its complete temporary working bound and
    // runs only under that partition. No encoding occurs during admission.
    let result = publish(&grant).and_then(|bytes| {
        let bytes = bytes.into_boxed_slice();
        let retained = archive_bytes(bytes.len())
            .ok_or_else(|| io::Error::other("live product archive layout overflow"))?;
        if retained > grant.bytes() {
            return Err(io::Error::from(io::ErrorKind::OutOfMemory));
        }
        let reference = domain
            .reference(&bytes)
            .ok_or_else(|| io::Error::other("live product archive exceeds the record limit"))?;
        let reservation = grant
            .split(retained)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(ProductArchive(ArchiveBacking::Materialized(Arc::new(
            ArchiveOwner {
                bytes,
                reference,
                _reservation: reservation,
            },
        ))))
    });
    if scratch.join(grant).is_err() {
        return Err(io::Error::other("live product archive partition changed"));
    }
    result
}

/// Canonical bytes and their reference share their full backing admission.
/// Publication to any store is separate; cache eviction cannot invalidate an
/// archive still held by a product or an external reader.
#[derive(Clone)]
pub struct ProductArchive(ArchiveBacking);
#[derive(Clone)]
enum ArchiveBacking {
    Materialized(Arc<ArchiveOwner>),
    Imported(Arc<ImportedArchive>),
}
struct ImportedArchive {
    reference: Reference,
    bytes: Bytes,
    admission: ImportedAdmission,
}
enum ImportedAdmission {
    Owned(hirsel_engine_admission::MemoryReservation),
    Shared {
        bytes: u64,
        _owner: Arc<dyn Send + Sync>,
    },
}
impl ImportedAdmission {
    fn bytes(&self) -> u64 {
        match self {
            Self::Owned(reservation) => reservation.bytes(),
            Self::Shared { bytes, .. } => *bytes,
        }
    }
}
struct ArchiveOwner {
    bytes: Box<[u8]>,
    reference: Reference,
    _reservation: hirsel_engine_admission::MemoryReservation,
}
impl ProductArchive {
    /// Encode a compiler-owned value only under an explicit boundary grant.
    /// The resulting checked archive retains its complete backing reservation.
    pub fn materialized_required_bytes(capacity: usize) -> Option<u64> {
        archive_bytes(capacity)
    }
    pub fn materialize(
        domain: Domain,
        admission: ArchiveAdmission<'_>,
        encode: impl FnOnce(&hirsel_engine_admission::MemoryReservation) -> io::Result<Vec<u8>>,
    ) -> io::Result<Self> {
        materialize_archive(domain, admission, encode)
    }
    /// Retain a checked archive on the request footprint until its last
    /// reader drops it.
    pub fn open_retained(
        store: &(impl super::backing::ProductBacking + ?Sized),
        reference: Reference,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> io::Result<Self> {
        let bytes = store
            .get_content(reference)
            .ok_or_else(|| io::Error::other("product archive is unavailable"))?;
        Self::from_retained_bytes(reference, bytes, request)
    }
    pub fn retained_required_bytes(reference: Reference, bytes: &Bytes) -> io::Result<u64> {
        Self::checked_import_charge(reference, bytes)?
            .checked_add(retained_arc_bytes(&std::mem::MaybeUninit::<
                RetainedArchiveLease,
            >::uninit()))
            .ok_or_else(|| io::Error::other("product archive backing layout overflow"))
    }
    /// Retain freshly encoded bytes on the request footprint, keeping the
    /// complete concrete backing without a transport reread.
    pub fn from_retained_bytes(
        reference: Reference,
        bytes: Bytes,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> io::Result<Self> {
        let charge = Self::retained_required_bytes(reference, &bytes)?;
        let owner = Arc::new(RetainedArchiveLease(request.reserve(charge)?));
        Self::from_admitted_content(reference, bytes, charge, owner)
    }
    /// Import from an existing reusable producer partition. Detached archives
    /// return their charge only after their final reader drops them.
    pub fn open_in(
        store: &(impl super::backing::ProductBacking + ?Sized),
        reference: Reference,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> io::Result<Self> {
        let bytes = store
            .get_content(reference)
            .ok_or_else(|| io::Error::other("product archive is unavailable"))?;
        let charge = Self::checked_import_charge(reference, &bytes)?;
        let reservation = request
            .reserve(charge)
            .map_err(|_| io::Error::other("product archive backing admission exhausted"))?;
        Ok(Self(ArchiveBacking::Imported(Arc::new(ImportedArchive {
            reference,
            bytes,
            admission: ImportedAdmission::Owned(reservation),
        }))))
    }
    pub fn from_encoded_in(
        reference: Reference,
        bytes: Vec<u8>,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> io::Result<Self> {
        if reference.domain.reference(&bytes) != Some(reference) {
            return Err(io::Error::other(
                "product archive content differs from its reference",
            ));
        }
        let bytes = bytes.into_boxed_slice();
        let charge = archive_bytes(bytes.len())
            .ok_or_else(|| io::Error::other("product archive backing layout overflow"))?;
        let reservation = request
            .reserve(charge)
            .map_err(|_| io::Error::other("product archive backing admission exhausted"))?;
        Ok(Self(ArchiveBacking::Materialized(Arc::new(ArchiveOwner {
            reference,
            bytes,
            _reservation: reservation,
        }))))
    }
    pub fn bytes(&self) -> &[u8] {
        match &self.0 {
            ArchiveBacking::Materialized(owner) => &owner.bytes,
            ArchiveBacking::Imported(owner) => &owner.bytes,
        }
    }
    pub fn reference(&self) -> Reference {
        match &self.0 {
            ArchiveBacking::Materialized(owner) => owner.reference,
            ArchiveBacking::Imported(owner) => owner.reference,
        }
    }
    pub fn retained_bytes(&self) -> u64 {
        match &self.0 {
            ArchiveBacking::Materialized(owner) => owner._reservation.bytes(),
            ArchiveBacking::Imported(owner) => owner.admission.bytes(),
        }
    }
    /// Import checked bytes under a complete backing reservation. A small view
    /// admits the entire region it keeps alive, including across eviction.
    pub fn open(
        store: &(impl super::backing::ProductBacking + ?Sized),
        reference: Reference,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> io::Result<Self> {
        let bytes = store
            .get_content(reference)
            .ok_or_else(|| io::Error::other("product archive is unavailable"))?;
        let charge = Self::checked_import_charge(reference, &bytes)?;
        let reservation = allowance
            .split(charge)
            .ok_or_else(|| io::Error::other("product archive backing admission exhausted"))?;
        Ok(Self(ArchiveBacking::Imported(Arc::new(ImportedArchive {
            reference,
            bytes,
            admission: ImportedAdmission::Owned(reservation),
        }))))
    }
    pub fn checked_import_charge(reference: Reference, bytes: &Bytes) -> io::Result<u64> {
        if !bytes.matches_reference(reference) {
            return Err(io::Error::other(
                "product archive content differs from its reference",
            ));
        }
        let mut retained = Some(retained_arc_bytes(
            &std::mem::MaybeUninit::<ImportedArchive>::uninit(),
        ));
        bytes.visit_retained_allocations(|_, bytes| {
            retained = retained.and_then(|n| n.checked_add(bytes as u64));
        });
        retained.ok_or_else(|| io::Error::other("product archive backing layout overflow"))
    }
    pub fn from_admitted_content(
        reference: Reference,
        bytes: Bytes,
        charge: u64,
        owner: Arc<dyn Send + Sync>,
    ) -> io::Result<Self> {
        let required = Self::checked_import_charge(reference, &bytes)?;
        if charge < required {
            return Err(io::Error::other(
                "product archive backing admission exhausted",
            ));
        }
        Ok(Self(ArchiveBacking::Imported(Arc::new(ImportedArchive {
            reference,
            bytes,
            admission: ImportedAdmission::Shared {
                bytes: charge,
                _owner: owner,
            },
        }))))
    }
}
pub fn archive_bytes(capacity: usize) -> Option<u64> {
    retained_arc_bytes(&std::mem::MaybeUninit::<ArchiveOwner>::uninit())
        .checked_add(capacity.try_into().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_DOMAIN: Domain = Domain {
        kind: 0x5150_0001,
        schema: 1,
    };
    #[test]
    fn encoded_archive_transfers_complete_backing_and_lease_without_rereading_transport() {
        let bytes = Bytes::from(b"complete encoded payload".to_vec());
        let pointer = bytes.as_ptr();
        let reference = Domain {
            kind: 0x5150_0003,
            schema: 1,
        }
        .reference(&bytes)
        .unwrap();
        let required = ProductArchive::checked_import_charge(reference, &bytes).unwrap()
            + retained_arc_bytes(&std::mem::MaybeUninit::<RetainedArchiveLease>::uninit());
        let short = hirsel_engine_admission::ProcessBudget::new(required - 1);
        assert!(ProductArchive::from_retained_bytes(reference, bytes.clone(), &short).is_err());
        assert_eq!(short.reserved(), 0);
        let budget = hirsel_engine_admission::ProcessBudget::new(required);
        let mut foreign = reference;
        foreign.output.0.0[0] ^= 1;
        assert!(ProductArchive::from_retained_bytes(foreign, bytes.clone(), &budget).is_err());
        assert_eq!(budget.reserved(), 0);
        let archive = ProductArchive::from_retained_bytes(reference, bytes, &budget).unwrap();
        assert_eq!(budget.reserved(), required);
        assert_eq!(archive.bytes().as_ptr(), pointer);
        assert_eq!(archive.retained_bytes(), required);
        let detached = archive.clone();
        drop(archive);
        assert_eq!(budget.reserved(), required);
        assert_eq!(detached.bytes(), b"complete encoded payload");
        drop(detached);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn encoded_archive_charge_covers_spare_capacity_and_detached_views() {
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let before = budget.free();
        let (archive, allocation) = crate::test_alloc::measured(|| {
            let mut bytes = Vec::with_capacity(4096);
            bytes.extend_from_slice(b"complete buffer");
            let reference = TEST_DOMAIN.reference(&bytes).unwrap();
            ProductArchive::from_encoded_in(reference, bytes, &budget).unwrap()
        });
        let charged = before - budget.free();
        assert!(allocation.live > 0);
        assert!(charged >= allocation.live as u64);
        assert_eq!(archive.retained_bytes(), charged);
        let detached = archive.clone();
        drop(archive);
        assert!(budget.reserved() >= charged);
        assert_eq!(detached.bytes(), b"complete buffer");
        assert_eq!(
            detached.reference(),
            TEST_DOMAIN.reference(b"complete buffer").unwrap()
        );
        drop(detached);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn imported_archive_admits_complete_range_backing_and_refuses_substitution() {
        struct Store(Bytes);
        impl crate::backing::ProductBacking for Store {
            fn admit_product_memory(&self, _: u64) -> Option<Arc<dyn Send + Sync>> {
                None
            }
            fn get_content(&self, _: Reference) -> Option<Bytes> {
                Some(self.0.clone())
            }
        }
        let mut backing = vec![0; 4096];
        backing[100..108].copy_from_slice(b"complete");
        let bytes = Bytes::from(backing).range(100, 8, 1).unwrap();
        let reference = TEST_DOMAIN.reference(b"complete").unwrap();
        let mut charge = retained_arc_bytes(&std::mem::MaybeUninit::<ImportedArchive>::uninit());
        bytes.visit_retained_allocations(|_, bytes| charge += bytes as u64);
        assert!(charge > 4096);
        let budget = hirsel_engine_admission::ProcessBudget::new(charge);
        let mut short = budget.reserve(charge - 1).unwrap();
        assert!(ProductArchive::open(&Store(bytes.clone()), reference, &mut short).is_err());
        assert_eq!(short.bytes(), charge - 1);
        drop(short);
        let mut exact = budget.reserve(charge).unwrap();
        assert!(
            ProductArchive::open(
                &Store(Bytes::from(b"replaced".as_slice())),
                reference,
                &mut exact
            )
            .is_err()
        );
        assert_eq!(
            exact.bytes(),
            charge,
            "substitution is refused before retaining bytes"
        );
        let store = Store(bytes.clone());
        let archive = ProductArchive::open(&store, reference, &mut exact).unwrap();
        assert_eq!(exact.bytes(), 0);
        assert_eq!(archive.retained_bytes(), charge);
        drop(store);
        drop(exact);
        let reader = archive.clone();
        drop(archive);
        assert_eq!(budget.reserved(), charge);
        assert_eq!(reader.bytes(), b"complete");
        drop(reader);
        assert_eq!(budget.reserved(), 0);

        let charge =
            charge + retained_arc_bytes(&std::mem::MaybeUninit::<RetainedArchiveLease>::uninit());
        let budget = hirsel_engine_admission::ProcessBudget::new(charge);
        let store = Store(bytes);
        let first = ProductArchive::open_retained(&store, reference, &budget).unwrap();
        let reader = first.clone();
        assert_eq!(budget.reserved(), charge);
        assert!(ProductArchive::open_retained(&store, reference, &budget).is_err());
        drop(first);
        assert_eq!(budget.reserved(), charge);
        drop(reader);
        assert_eq!(budget.reserved(), 0);
        let replacement = ProductArchive::open_retained(&store, reference, &budget).unwrap();
        assert_eq!(budget.reserved(), charge);
        drop(replacement);
        assert_eq!(budget.reserved(), 0);
    }
}
