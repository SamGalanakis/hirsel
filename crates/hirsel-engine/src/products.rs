//! Admitted typed values. Producers retain their existing snapshot reservation;
//! this owner additionally admits every value allocation it keeps alive.

use crate::{
    NativeRecordContext, NativeRecordSource, NativeRecords, PublishedSummary, SummaryView,
};
use hirsel_artifact_store::{Action, Address, Domain, Reference};
use hirsel_engine_admission::{RetainedArchiveLease, retained_arc_bytes};
use hirsel_engine_runtime::{ArchiveAdmission, ProductArchive};
use hirsel_query_products::depth::DepthEvidence;
use hirsel_query_products::reads::Reads;
use hirsel_query_products::retained::Weight;
use hirsel_span::Span;
use hirsel_tir::DefinitionRef;
use hirsel_types::{QueryOutput, TypeInterner};
use std::{
    io,
    sync::{Arc, Mutex, Weak},
};

pub const LIVE_SUMMARY: Domain = Domain {
    kind: 0x5150_0001,
    schema: 1,
};
pub const LIVE_LOWER: Domain = Domain {
    kind: 0x5150_0002,
    schema: 2,
};

#[derive(Clone)]
pub(crate) struct SharedProductSource(Arc<ProductContext>);
struct ProductContext {
    context: NativeRecordContext,
    records: NativeRecords,
    _reservation: ValueAdmission,
}
#[derive(Clone)]
pub(crate) enum ProductSourceInput {
    Owned(NativeRecordContext, NativeRecords, Arc<dyn Send + Sync>),
    Shared(SharedProductSource, Arc<dyn Send + Sync>),
}
pub(crate) struct ProductSource {
    shared: SharedProductSource,
    _backing: Arc<dyn Send + Sync>,
}
impl ProductSource {
    pub(crate) fn prepare(
        context: NativeRecordContext,
        records: NativeRecords,
        backing: Arc<dyn Send + Sync>,
    ) -> Option<ProductSourceInput> {
        records
            .uses_types(context.record_types())
            .then_some(ProductSourceInput::Owned(context, records, backing))
    }
    pub(crate) fn metadata_bytes() -> u64 {
        retained_arc_bytes(&std::mem::MaybeUninit::<ProductContext>::uninit())
    }
    pub(crate) fn admit_shared(
        context: NativeRecordContext,
        records: NativeRecords,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<SharedProductSource> {
        if !records.uses_types(context.record_types()) {
            return None;
        }
        let reservation = allowance.split(Self::metadata_bytes())?;
        Some(SharedProductSource(Arc::new(ProductContext {
            context,
            records,
            _reservation: ValueAdmission::Owned(reservation),
        })))
    }
    pub(crate) fn backed(
        shared: SharedProductSource,
        backing: Arc<dyn Send + Sync>,
    ) -> ProductSourceInput {
        ProductSourceInput::Shared(shared, backing)
    }
    fn context(&self) -> &NativeRecordContext {
        &self.shared.0.context
    }
    fn records(&self) -> &NativeRecords {
        &self.shared.0.records
    }
    fn retained_bytes(&self) -> u64 {
        self.shared.0._reservation.bytes()
    }
}
impl ProductSourceInput {
    fn metadata_bytes(&self) -> u64 {
        match self {
            Self::Owned(..) => ProductSource::metadata_bytes(),
            Self::Shared(..) => 0,
        }
    }
    fn types(&self) -> &Arc<TypeInterner> {
        match self {
            Self::Owned(context, ..) => context.record_types(),
            Self::Shared(shared, _) => shared.0.context.record_types(),
        }
    }
    fn admit(
        self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<ProductSource> {
        let (shared, backing) = match self {
            Self::Owned(context, records, backing) => (
                ProductSource::admit_shared(context, records, allowance)?,
                backing,
            ),
            Self::Shared(shared, backing) => (shared, backing),
        };
        Some(ProductSource {
            shared,
            _backing: backing,
        })
    }
}

struct Owner<T> {
    address: Address,
    action: Action,
    domain: Domain,
    value: T,
    source: ProductSource,
    archive: Mutex<Option<ProductArchive>>,
    _reservation: ValueAdmission,
}
enum ValueAdmission {
    Owned(hirsel_engine_admission::MemoryReservation),
    Retained {
        bytes: u64,
        _owner: Arc<RetainedArchiveLease>,
    },
}
impl ValueAdmission {
    fn bytes(&self) -> u64 {
        match self {
            Self::Owned(value) => value.bytes(),
            Self::Retained { bytes, .. } => *bytes,
        }
    }
}
impl<T> Owner<T> {
    #[cfg(test)]
    fn archive(
        &self,
        scratch: &mut hirsel_engine_admission::MemoryReservation,
        required: u64,
        publish: impl FnOnce(
            &T,
            &ProductSource,
            &hirsel_engine_admission::MemoryReservation,
        ) -> io::Result<Vec<u8>>,
    ) -> io::Result<ProductArchive> {
        self.archive_with(
            ArchiveAdmission::Materialize {
                scratch,
                working_bytes: required,
            },
            publish,
        )
    }
    fn archive_with(
        &self,
        admission: ArchiveAdmission<'_>,
        publish: impl FnOnce(
            &T,
            &ProductSource,
            &hirsel_engine_admission::MemoryReservation,
        ) -> io::Result<Vec<u8>>,
    ) -> io::Result<ProductArchive> {
        let mut archive = self.archive.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(archive) = &*archive {
            return Ok(archive.clone());
        }
        let materialized = ProductArchive::materialize(self.domain, admission, |grant| {
            publish(&self.value, &self.source, grant)
        })?;
        *archive = Some(materialized.clone());
        Ok(materialized)
    }
}

/// A Summary view retains the complete immutable producer, type/source owners
/// and its own value admission. There are no reusable numeric session slots.
#[derive(Clone)]
pub struct LiveSummaryProduct {
    current: Arc<Owner<Arc<PublishedSummary>>>,
    facet: SummaryFacet,
}

/// Only the settled semantic projection. Positions, reads and actions are
/// consumed through the current product and cannot be observed through this
/// facet. Keeping it retains its complete original admitted backing.
#[derive(Clone)]
pub struct SummaryFacet(SummaryFacetOwner);
#[derive(Clone)]
enum SummaryFacetOwner {
    Definition(Arc<Owner<Arc<PublishedSummary>>>),
    Component(Arc<crate::SourceGrowthComponent>),
}
impl SummaryFacet {
    fn component(value: Arc<crate::SourceGrowthComponent>) -> Self {
        Self(SummaryFacetOwner::Component(value))
    }
    pub fn same_owner(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (SummaryFacetOwner::Definition(a), SummaryFacetOwner::Definition(b)) => {
                Arc::ptr_eq(a, b)
            }
            (SummaryFacetOwner::Component(a), SummaryFacetOwner::Component(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
    pub fn equivalent(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (SummaryFacetOwner::Definition(a), SummaryFacetOwner::Definition(b)) => {
                Arc::ptr_eq(a, b)
                    || SummaryView::from(&*a.value).equivalent(
                        a.source.context().record_types(),
                        SummaryView::from(&*b.value),
                        b.source.context().record_types(),
                    )
            }
            (SummaryFacetOwner::Component(a), SummaryFacetOwner::Component(b)) => {
                crate::summary::SettledSummary::component(a)
                    .equivalent(&crate::summary::SettledSummary::component(b))
            }
            _ => false,
        }
    }
}
impl LiveSummaryProduct {
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.current, &other.current)
    }
    pub(crate) fn value_bytes(value: &PublishedSummary) -> Option<u64> {
        Self::shared_value_bytes(value)?.checked_add(ProductSource::metadata_bytes())
    }
    pub(crate) fn shared_value_bytes(value: &PublishedSummary) -> Option<u64> {
        Self::shared_payload_bytes(value)?.checked_add(Self::shared_owner_bytes())
    }
    pub(crate) fn shared_payload_bytes(value: &PublishedSummary) -> Option<u64> {
        // Storage equations have a separate admitted owner. Ordinary Summary
        // publication does not silently accept an unknown backing layout.
        if value.storage.is_some() {
            return None;
        }
        let mut weight = Weight::default();
        weight.add(retained_arc_bytes(value) as usize);
        weight.definition_summary(&value.summary);
        weight.parameter_growth(&value.growth);
        if let Some(checked) = &value.checked {
            weight.arc(checked, Weight::checked_summary);
        }
        weight.reads(&value.reads);
        Some(weight.bytes)
    }
    pub(crate) fn new(
        identity: (Address, Action),
        value: Arc<PublishedSummary>,
        source: ProductSourceInput,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<Self> {
        let required = Self::shared_value_bytes(&value)?.checked_add(source.metadata_bytes())?;
        let mut reservation = allowance.split(required)?;
        let source = source.admit(&mut reservation)?;
        let current = Arc::new(Owner {
            address: identity.0,
            action: identity.1,
            domain: LIVE_SUMMARY,
            value,
            source,
            archive: Mutex::new(None),
            _reservation: ValueAdmission::Owned(reservation),
        });
        Some(Self {
            facet: SummaryFacet(SummaryFacetOwner::Definition(current.clone())),
            current,
        })
    }
    pub(crate) fn shared_owner_bytes() -> u64 {
        owner_bytes::<Arc<PublishedSummary>>()
    }
    /// The producer's complete fact owner admits every aliased value allocation.
    pub(crate) fn new_backed(
        identity: (Address, Action),
        value: Arc<PublishedSummary>,
        source: ProductSourceInput,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<Self> {
        let mut reservation =
            allowance.split(Self::shared_owner_bytes().checked_add(source.metadata_bytes())?)?;
        let source = source.admit(&mut reservation)?;
        let current = Arc::new(Owner {
            address: identity.0,
            action: identity.1,
            domain: LIVE_SUMMARY,
            value,
            source,
            archive: Mutex::new(None),
            _reservation: ValueAdmission::Owned(reservation),
        });
        Some(Self {
            facet: SummaryFacet(SummaryFacetOwner::Definition(current.clone())),
            current,
        })
    }
    pub(crate) fn cache_owners(&self, owners: usize) -> bool {
        matches!(&self.facet.0, SummaryFacetOwner::Definition(facet) if Arc::ptr_eq(&self.current, facet))
            && Arc::strong_count(&self.current) == owners * 2
    }
    pub fn value(&self) -> &PublishedSummary {
        &self.current.value
    }
    pub(crate) fn value_arc(&self) -> Arc<PublishedSummary> {
        self.current.value.clone()
    }
    pub fn reads(&self) -> &Reads {
        &self.current.value.reads
    }
    pub fn address(&self) -> Address {
        self.current.address
    }
    pub fn action(&self) -> Action {
        self.current.action
    }
    pub fn domain(&self) -> Domain {
        self.current.domain
    }
    pub fn types(&self) -> &Arc<TypeInterner> {
        self.current.source.context().record_types()
    }
    pub fn retained_bytes(&self) -> u64 {
        let SummaryFacetOwner::Definition(facet) = &self.facet.0 else {
            unreachable!()
        };
        self.current._reservation.bytes()
            + self.current.source.retained_bytes()
            + if Arc::ptr_eq(&self.current, facet) {
                0
            } else {
                facet._reservation.bytes()
                    + if Arc::ptr_eq(&self.current.source.shared.0, &facet.source.shared.0) {
                        0
                    } else {
                        facet.source.retained_bytes()
                    }
            }
    }
    pub fn facet(&self) -> &SummaryFacet {
        &self.facet
    }
    pub fn equivalent(&self, other: &Self) -> bool {
        self.facet.equivalent(&other.facet)
    }
    /// Reuse the old settled value while retaining this computation's current
    /// source product, complete reads and action. Intermediate products are
    /// not chained: only the original equal facet survives.
    pub fn backdated(&self, previous: &Self) -> Option<Self> {
        self.equivalent(previous).then(|| Self {
            current: self.current.clone(),
            facet: previous.facet.clone(),
        })
    }
    /// Materialize at most once successfully. Admission or encoding failure
    /// leaves the immutable live value retryable. Stores publish these bytes
    /// separately and bind their complete computation associations as before.
    pub fn archive(
        &self,
        scratch: &mut hirsel_engine_admission::MemoryReservation,
        required: u64,
    ) -> io::Result<ProductArchive> {
        self.archive_with(ArchiveAdmission::Materialize {
            scratch,
            working_bytes: required,
        })
    }
    pub(crate) fn archive_with(
        &self,
        admission: ArchiveAdmission<'_>,
    ) -> io::Result<ProductArchive> {
        self.current
            .archive_with(admission, |value, source, grant| {
                value
                    .reads
                    .encoding_working_bytes()
                    .and_then(|working| {
                        source.records().encode_with_admission(
                            source.context(),
                            &**value,
                            grant,
                            working,
                        )
                    })
                    .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
            })
    }
}

/// The source-growth verdict retains its admitted facets, complete reads and
/// source owner. Settlement never creates a portable record.
#[derive(Clone)]
pub struct LiveGrowthProduct(Arc<Owner<Arc<crate::source_components::SourceGrowthData>>>);
pub const LIVE_GROWTH: Domain = Domain {
    kind: 0x4600_0009,
    schema: 5,
};
impl LiveGrowthProduct {
    pub(crate) fn value_bytes(value: &crate::source_components::SourceGrowthData) -> Option<u64> {
        let mut weight = Weight::default();
        weight.add(retained_arc_bytes(value) as usize);
        weight.arc(&value.diagnostics, |w, diagnostics| {
            w.vec(diagnostics, |w, diagnostic| w.diagnostic(diagnostic));
        });
        weight
            .bytes
            .checked_add(owner_bytes::<Arc<crate::source_components::SourceGrowthData>>())
            .and_then(|n| n.checked_add(ProductSource::metadata_bytes()))
    }
    pub(crate) fn new(
        identity: (Address, Action),
        value: Arc<crate::source_components::SourceGrowthData>,
        source: ProductSourceInput,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<Self> {
        let mut reservation = allowance.split(Self::value_bytes(&value)?)?;
        let source = source.admit(&mut reservation)?;
        Some(Self(Arc::new(Owner {
            address: identity.0,
            action: identity.1,
            domain: LIVE_GROWTH,
            value,
            source,
            archive: Mutex::new(None),
            _reservation: ValueAdmission::Owned(reservation),
        })))
    }
    pub(crate) fn value(&self) -> &Arc<crate::source_components::SourceGrowthData> {
        &self.0.value
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
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn archive_with(
        &self,
        admission: ArchiveAdmission<'_>,
    ) -> io::Result<ProductArchive> {
        self.0.archive_with(admission, |value, source, grant| {
            value
                .reads
                .encoding_working_bytes()
                .and_then(|working| {
                    source.records().encode_with_admission(
                        source.context(),
                        &**value,
                        grant,
                        working,
                    )
                })
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
        })
    }
}

/// Membership and one SCC are ordinary independently addressed live products.
/// Their complete current reads remain separate from the exact settled facet.
#[derive(serde::Serialize)]
pub(crate) enum SourceGrowthQueryData {
    Membership {
        value: Arc<crate::source_components::SourceMembership>,
        reads: Arc<Reads>,
    },
    Component {
        value: Arc<crate::SourceGrowthComponent>,
        reads: Arc<Reads>,
    },
}
pub(crate) struct GrowthPublicationSource {
    source: ProductSource,
    pub(crate) store: Arc<dyn hirsel_engine_store::ResultStore>,
    pub(crate) scratch: u64,
    _reservation: hirsel_engine_admission::MemoryReservation,
}
pub(crate) struct SourceGrowthPublication {
    pub(crate) reads: Arc<crate::queries::ArchivedReader>,
}
#[derive(Clone)]
enum SourceGrowthValue {
    Membership(Arc<crate::source_components::SourceMembership>),
    Component(Arc<crate::SourceGrowthComponent>),
}
#[derive(Clone)]
pub struct LiveSourceGrowthProduct(SourceGrowthValue, Option<SummaryFacet>);
pub const LIVE_SOURCE_MEMBERSHIP: Domain = Domain {
    kind: 0x4600_0024,
    schema: 2,
};
pub const LIVE_SOURCE_COMPONENT: Domain = Domain {
    kind: 0x4600_0025,
    schema: 2,
};
impl LiveSourceGrowthProduct {
    pub(crate) fn source_bytes() -> u64 {
        retained_arc_bytes(&std::mem::MaybeUninit::<GrowthPublicationSource>::uninit())
            + ProductSource::metadata_bytes()
    }
    pub(crate) fn admit_source(
        source: ProductSourceInput,
        store: Arc<dyn hirsel_engine_store::ResultStore>,
        scratch: u64,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<Arc<GrowthPublicationSource>> {
        let mut reservation = allowance.split(Self::source_bytes())?;
        let source = source.admit(&mut reservation)?;
        Some(Arc::new(GrowthPublicationSource {
            source,
            store,
            scratch,
            _reservation: reservation,
        }))
    }
    /// Values own their allocation grant. Reader references retain this source
    /// and use Front's existing per-root metadata reservation.
    pub(crate) fn new(
        identity: (Address, Action),
        value: SourceGrowthQueryData,
        source: Arc<GrowthPublicationSource>,
    ) -> Option<Self> {
        let (value, facet) = match value {
            SourceGrowthQueryData::Membership { mut value, reads } => {
                if (value.product.address, value.product.action) != identity {
                    return None;
                }
                Arc::get_mut(&mut value)?.publication = Some(SourceGrowthPublication {
                    reads: crate::queries::ArchivedReader::capture(source, &reads)?,
                });
                (SourceGrowthValue::Membership(value), None)
            }
            SourceGrowthQueryData::Component { mut value, reads } => {
                if (value.product.address, value.product.action) != identity {
                    return None;
                }
                Arc::get_mut(&mut value)?.publication = Some(SourceGrowthPublication {
                    reads: crate::queries::ArchivedReader::capture(source, &reads)?,
                });
                let facet = SummaryFacet::component(value.clone());
                (SourceGrowthValue::Component(value), Some(facet))
            }
        };
        Some(Self(value, facet))
    }
    pub(crate) fn reopen_component(value: Arc<crate::SourceGrowthComponent>) -> Option<Self> {
        value.publication.as_ref()?;
        let facet = SummaryFacet::component(value.clone());
        Some(Self(SourceGrowthValue::Component(value), Some(facet)))
    }
    fn publication(&self) -> &SourceGrowthPublication {
        match &self.0 {
            SourceGrowthValue::Membership(v) => v.publication.as_ref().unwrap(),
            SourceGrowthValue::Component(v) => v.publication.as_ref().unwrap(),
        }
    }
    pub(crate) fn reader(&self) -> Arc<crate::queries::ArchivedReader> {
        self.publication().reads.clone()
    }
    pub(crate) fn membership(&self) -> Option<Arc<crate::source_components::SourceMembership>> {
        match &self.0 {
            SourceGrowthValue::Membership(value) => Some(value.clone()),
            _ => None,
        }
    }
    pub(crate) fn component(&self) -> Option<Arc<crate::SourceGrowthComponent>> {
        match &self.0 {
            SourceGrowthValue::Component(value) => Some(value.clone()),
            _ => None,
        }
    }
    pub(crate) fn value(&self) -> Option<SourceGrowthQueryData> {
        let reads = self.publication().reads.load()?;
        Some(match &self.0 {
            SourceGrowthValue::Membership(value) => SourceGrowthQueryData::Membership {
                value: value.clone(),
                reads,
            },
            SourceGrowthValue::Component(value) => SourceGrowthQueryData::Component {
                value: value.clone(),
                reads,
            },
        })
    }
    fn identity(&self) -> crate::SourceGrowthIdentity {
        match &self.0 {
            SourceGrowthValue::Membership(v) => v.product,
            SourceGrowthValue::Component(v) => v.product,
        }
    }
    pub fn address(&self) -> Address {
        self.identity().address
    }
    pub fn action(&self) -> Action {
        self.identity().action
    }
    pub fn domain(&self) -> Domain {
        match &self.0 {
            SourceGrowthValue::Membership(_) => LIVE_SOURCE_MEMBERSHIP,
            SourceGrowthValue::Component(_) => LIVE_SOURCE_COMPONENT,
        }
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (SourceGrowthValue::Membership(a), SourceGrowthValue::Membership(b)) => {
                Arc::ptr_eq(a, b)
            }
            (SourceGrowthValue::Component(a), SourceGrowthValue::Component(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
    pub fn facet(&self) -> Option<&SummaryFacet> {
        self.1.as_ref()
    }
    pub fn equivalent(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (SourceGrowthValue::Membership(a), SourceGrowthValue::Membership(b)) => {
                a.components.len() == b.components.len()
                    && a.components
                        .iter()
                        .zip(&b.components)
                        .all(|(a, b)| a.members == b.members && a.inputs == b.inputs)
            }
            (SourceGrowthValue::Component(_), SourceGrowthValue::Component(_)) => self
                .1
                .as_ref()
                .zip(other.1.as_ref())
                .is_some_and(|(a, b)| a.equivalent(b)),
            _ => false,
        }
    }
    pub fn backdated(&self, previous: &Self) -> Option<Self> {
        self.equivalent(previous)
            .then(|| Self(self.0.clone(), previous.1.clone()))
    }
    pub(crate) fn archive_with(
        &self,
        admission: ArchiveAdmission<'_>,
    ) -> io::Result<ProductArchive> {
        let source = &self.publication().reads.source().source;
        ProductArchive::materialize(self.domain(), admission, |grant| {
            self.publication()
                .reads
                .encoding_working_bytes()
                .and_then(|working| {
                    source.records().encode_with_admission(
                        source.context(),
                        &self.value()?,
                        grant,
                        working,
                    )
                })
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
        })
    }
}

/// The ordinary Lower value and complete source observations. Acceptance
/// remains in its existing verifier-owned product, outside facet equality.
#[derive(Clone)]
pub struct LiveLowerProduct(Arc<Owner<LowerValue>>);
/// The retained-partition admission of one live Lower product.
pub(crate) enum LiveAdmission {
    Admitted(LiveLowerProduct),
    /// The retained counter was short; the output is handed back so the
    /// producer can wait for progress and retry.
    Blocked(Box<crate::FrontLowerOutput>),
    /// Type coverage or layout overflow; waiting cannot admit the body.
    Refused,
}
#[derive(Clone)]
pub(crate) struct WeakLowerProduct(Weak<Owner<LowerValue>>);
impl WeakLowerProduct {
    pub(crate) fn metadata_bytes() -> usize {
        size_of::<Owner<LowerValue>>() + 2 * size_of::<usize>()
    }
    pub(crate) fn upgrade(&self) -> Option<LiveLowerProduct> {
        self.0.upgrade().map(LiveLowerProduct)
    }
}
struct LowerValue {
    output: crate::FrontLowerOutput,
}
impl LiveLowerProduct {
    pub(super) fn prepare_eviction(&self, scratch: u64) -> bool {
        self.observations().spill(scratch).is_some()
    }
    pub(crate) fn downgrade(&self) -> WeakLowerProduct {
        WeakLowerProduct(Arc::downgrade(&self.0))
    }
    pub(crate) fn cache_only(&self) -> bool {
        Arc::strong_count(&self.0) == 1 && Arc::strong_count(&self.output().value) == 1
    }
    pub(crate) fn retained_required_bytes(value: &crate::FrontLowerOutput) -> Option<u64> {
        Self::required_bytes(value)?.checked_add(retained_arc_bytes(&std::mem::MaybeUninit::<
            RetainedArchiveLease,
        >::uninit()))
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn required_bytes(value: &crate::FrontLowerOutput) -> Option<u64> {
        Self::shared_required_bytes(value)?.checked_add(ProductSource::metadata_bytes())
    }
    fn shared_required_bytes(value: &crate::FrontLowerOutput) -> Option<u64> {
        let mut weight = Weight::default();
        for (_, bytes) in value.reads.allocations() {
            weight.add(bytes);
        }
        weight.add(size_of_val(&*value.depth.first));
        weight.add(2 * (size_of::<std::sync::OnceLock<Reference>>() + 2 * size_of::<usize>()));
        let mut materialized = hirsel_query_products::retained::MetadataWeight::default();
        materialized.add(
            value
                .materialized
                .capacity()
                .checked_mul(size_of::<(hirsel_syntax::ExprId, hirsel_tir::Materialized)>())?,
        );
        materialized.vec(&value.selections, |w, (_, arguments)| w.flat_vec(arguments));
        for (_, value) in &value.materialized {
            materialized.materialized(value);
        }
        owner_bytes::<LowerValue>()
            .checked_add(hirsel_query_products::retained::retained_tir_bytes(
                &value.output.value,
            ))?
            .checked_add(
                hirsel_query_products::retained::retained_lowering_output_bytes(
                    &value.output.diagnostics,
                    &value.output.dependencies,
                ),
            )?
            .checked_add(weight.bytes)?
            .checked_add(materialized.bytes())?
            .checked_add(
                value
                    .execution
                    .as_ref()
                    .map_or(Some(0), |r| r.retained_bytes())?,
            )?
            .checked_add(
                value
                    .verified
                    .as_ref()
                    .map_or(Some(0), |v| v.retained_bytes())?,
            )
    }
    pub(crate) fn new(
        identity: (Address, Action),
        output: crate::FrontLowerOutput,
        source: ProductSourceInput,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<Self> {
        let required =
            Self::shared_required_bytes(&output)?.checked_add(source.metadata_bytes())?;
        let mut reservation = allowance.split(required)?;
        let source = source.admit(&mut reservation)?;
        Some(Self(Arc::new(Owner {
            address: identity.0,
            action: identity.1,
            domain: LIVE_LOWER,
            value: LowerValue { output },
            source,
            archive: Mutex::new(None),
            _reservation: ValueAdmission::Owned(reservation),
        })))
    }
    /// Transfer a settled producer's existing partition into its live value.
    /// Clones share this one lease; the last owner returns the exact charge.
    /// A short partition hands the output back so admission can wait and retry;
    /// coverage or layout refusal keeps the producer's existing error.
    pub(crate) fn from_retained(
        identity: (Address, Action),
        output: crate::FrontLowerOutput,
        context: NativeRecordContext,
        records: NativeRecords,
        backing: Arc<dyn Send + Sync>,
        allowance: Arc<hirsel_engine_admission::MemoryReservation>,
        remaining: Arc<Mutex<u64>>,
    ) -> LiveAdmission {
        if !records.uses_types(context.record_types()) {
            return LiveAdmission::Refused;
        }
        let Some(bytes) = Self::retained_required_bytes(&output) else {
            return LiveAdmission::Refused;
        };
        let mut available = remaining.lock().unwrap_or_else(|e| e.into_inner());
        let Some(next) = available.checked_sub(bytes) else {
            return LiveAdmission::Blocked(Box::new(output));
        };
        *available = next;
        drop(available);
        let owner = Arc::new(RetainedArchiveLease {
            _allowance: allowance.clone(),
            remaining,
            bytes,
        });
        let source = ProductSource {
            shared: SharedProductSource(Arc::new(ProductContext {
                context,
                records,
                _reservation: ValueAdmission::Retained {
                    bytes: 0,
                    _owner: owner.clone(),
                },
            })),
            _backing: backing,
        };
        LiveAdmission::Admitted(Self(Arc::new(Owner {
            address: identity.0,
            action: identity.1,
            domain: LIVE_LOWER,
            value: LowerValue { output },
            source,
            archive: Mutex::new(None),
            _reservation: ValueAdmission::Retained {
                bytes,
                _owner: owner,
            },
        })))
    }
    pub fn output(&self) -> &QueryOutput<DefinitionRef> {
        &self.0.value.output.output
    }
    pub fn reads(&self) -> &Reads {
        &self.0.value.output.reads
    }
    pub fn depth(&self) -> &DepthEvidence<Span> {
        &self.0.value.output.depth
    }
    /// Complete observations and local verifier-owned evidence from the
    /// original Lower operation. Archive bytes do not grant this authority.
    pub(crate) fn bindings_hold(&self, context: &NativeRecordContext) -> bool {
        self.0.source.context().bindings_hold(context)
    }
    pub fn observations(&self) -> &crate::FrontLowerOutput {
        &self.0.value.output
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
    pub fn types(&self) -> &Arc<TypeInterner> {
        self.0.source.context().record_types()
    }
    pub fn retained_bytes(&self) -> u64 {
        self.0._reservation.bytes() + self.0.source.retained_bytes()
    }
    pub fn archive(
        &self,
        scratch: &mut hirsel_engine_admission::MemoryReservation,
        required: u64,
    ) -> io::Result<ProductArchive> {
        self.archive_with(ArchiveAdmission::Materialize {
            scratch,
            working_bytes: required,
        })
    }
    pub(crate) fn archive_with(
        &self,
        admission: ArchiveAdmission<'_>,
    ) -> io::Result<ProductArchive> {
        self.0.archive_with(admission, |value, source, grant| {
            value
                .output
                .reads
                .encoding_working_bytes()
                .and_then(|working| {
                    source.records().encode_with_admission(
                        source.context(),
                        &(
                            &value.output.output,
                            &value.output.reads,
                            &value.output.depth,
                            &value.output.materialized,
                            value.output.implementation_revision,
                            value.output.signature,
                            value.output.executed,
                            &value.output.selections,
                        ),
                        grant,
                        working,
                    )
                })
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
        })
    }
}

pub const LIVE_ANALYZED: Domain = Domain {
    kind: 0x5150_0003,
    schema: 1,
};

/// A demanded post-Modes body retains its complete typed allocation and source
/// backing. It carries no verifier acceptance for a subsequently lowered body.
#[derive(Clone)]
pub struct LiveAnalyzedProduct(Arc<Owner<AnalyzedValue>>);
pub(crate) struct WeakAnalyzedProduct(Weak<Owner<AnalyzedValue>>);
struct AnalyzedValue {
    output: Arc<QueryOutput<crate::AnalyzedBody>>,
    summary: LiveSummaryProduct,
    phase: Option<hirsel_query_products::QueryKind>,
    diagnostic_phases: Vec<(hirsel_diag::Diagnostic, String)>,
}
impl LiveAnalyzedProduct {
    pub(crate) fn summary(&self) -> &LiveSummaryProduct {
        &self.0.value.summary
    }
    pub(crate) fn cache_only(&self) -> bool {
        Arc::strong_count(&self.0) == 1 && Arc::strong_count(&self.0.value.output) == 1
    }
    pub(crate) fn downgrade(&self) -> WeakAnalyzedProduct {
        WeakAnalyzedProduct(Arc::downgrade(&self.0))
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn required_bytes(outcome: &crate::ModeResult) -> Option<u64> {
        Self::shared_required_bytes(outcome)?.checked_add(ProductSource::metadata_bytes())
    }
    pub(crate) fn shared_required_bytes(outcome: &crate::ModeResult) -> Option<u64> {
        let output = outcome.result.as_ref().ok()?;
        let mut weight = Weight::default();
        weight.vec(&outcome.diagnostic_phases, |w, (diagnostic, phase)| {
            w.diagnostic(diagnostic);
            w.add(phase.capacity());
        });
        owner_bytes::<AnalyzedValue>()
            .checked_add(hirsel_query_products::retained::retained_analyzed_bytes(
                output,
            ))?
            .checked_add(weight.bytes)
    }
    pub(crate) fn new(
        identity: (Address, Action),
        outcome: crate::ModeResult,
        summary: LiveSummaryProduct,
        source: ProductSourceInput,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Option<Self> {
        if summary.value().definition.path() != &outcome.definition
            || summary.reads() != outcome.reads.as_ref()
            || !Arc::ptr_eq(summary.types(), source.types())
        {
            return None;
        }
        let required =
            Self::shared_required_bytes(&outcome)?.checked_add(source.metadata_bytes())?;
        let mut reservation = allowance.split(required)?;
        let source = source.admit(&mut reservation)?;
        let output = outcome.result.ok()?;
        Some(Self(Arc::new(Owner {
            address: identity.0,
            action: identity.1,
            domain: LIVE_ANALYZED,
            value: AnalyzedValue {
                output,
                summary,
                phase: outcome.phase,
                diagnostic_phases: outcome.diagnostic_phases,
            },
            source,
            archive: Mutex::new(None),
            _reservation: ValueAdmission::Owned(reservation),
        })))
    }
    pub fn output(&self) -> &Arc<QueryOutput<crate::AnalyzedBody>> {
        &self.0.value.output
    }
    pub fn reads(&self) -> &Arc<Reads> {
        &self.0.value.summary.current.value.reads
    }
    pub fn phase(&self) -> Option<hirsel_query_products::QueryKind> {
        self.0.value.phase
    }
    pub fn diagnostic_phases(&self) -> &[(hirsel_diag::Diagnostic, String)] {
        &self.0.value.diagnostic_phases
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
    pub fn retained_bytes(&self) -> u64 {
        self.0._reservation.bytes() + self.0.source.retained_bytes()
    }
    pub fn types(&self) -> &Arc<TypeInterner> {
        self.0.source.context().record_types()
    }
    pub fn archive(
        &self,
        scratch: &mut hirsel_engine_admission::MemoryReservation,
        required: u64,
    ) -> io::Result<ProductArchive> {
        self.archive_with(ArchiveAdmission::Materialize {
            scratch,
            working_bytes: required,
        })
    }
    pub(crate) fn archive_with(
        &self,
        admission: ArchiveAdmission<'_>,
    ) -> io::Result<ProductArchive> {
        self.0.archive_with(admission, |value, source, grant| {
            crate::queries::encode_live_analyzed(
                source.records(),
                source.context(),
                &value.output,
                &value.summary.current.value.reads,
                value.phase,
                &value.diagnostic_phases,
                grant,
            )
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
        })
    }
}
impl WeakAnalyzedProduct {
    pub(crate) fn upgrade(&self) -> Option<LiveAnalyzedProduct> {
        self.0.upgrade().map(LiveAnalyzedProduct)
    }
}

fn owner_bytes<T>() -> u64 {
    retained_arc_bytes(&std::mem::MaybeUninit::<Owner<T>>::uninit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckedSummary, DefinitionSummary, ParameterGrowth, PhaseAdapters, QueryEngine};

    use hirsel_syntax::{ContentHash, DefId, DefPath};

    fn fixture() -> (Arc<PublishedSummary>, NativeRecordContext) {
        let engine = QueryEngine::new(crate::default_environment(), PhaseAdapters::default());
        let owner = DefId::intern(&DefPath::core("live-summary"));
        let checked = Arc::new(CheckedSummary {
            summary: DefinitionSummary {
                owner,
                frames: [].into(),
                calls: [].into(),
                staged: [].into(),
            },
            growth: ParameterGrowth::default(),
        });
        let value = Arc::new(PublishedSummary {
            definition: owner,
            summary: checked.summary.clone(),
            growth: checked.growth.clone(),
            storage: None,
            checked: Some(checked),
            reads: Arc::new(Reads::empty()),
            text: ContentHash::default(),
        });
        (value, engine.source_records())
    }
    fn descriptor() -> (Address, Action) {
        (
            Address(ContentHash::default()),
            Action(ContentHash::default()),
        )
    }
    fn product_source(
        context: NativeRecordContext,
        backing: Arc<dyn Send + Sync>,
    ) -> ProductSourceInput {
        let records = NativeRecords::new(
            context.record_types().clone(),
            Arc::new(hirsel_engine_store::MemStore::default()),
        );
        ProductSource::prepare(context, records, backing).unwrap()
    }
    fn archived_growth_fixture_reader(
        context: &NativeRecordContext,
    ) -> Arc<crate::queries::ArchivedReader> {
        let store = Arc::new(hirsel_engine_store::MemStore::default());
        let records = NativeRecords::new(context.record_types().clone(), store.clone());
        let source = ProductSource::prepare(context.clone(), records, Arc::new(())).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let pool =
            hirsel_engine_admission::MemoryReservationPool::new(budget.reserve(1 << 20).unwrap())
                .ok()
                .unwrap();
        let mut allowance = pool.take(LiveSourceGrowthProduct::source_bytes()).unwrap();
        let source =
            LiveSourceGrowthProduct::admit_source(source, store, 64 << 10, &mut allowance).unwrap();
        crate::queries::ArchivedReader::capture_owned(source, &Reads::empty(), &pool).unwrap()
    }
    fn growth_fixture() -> (
        Arc<crate::source_components::SourceGrowthData>,
        NativeRecordContext,
    ) {
        let (_, source) = fixture();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let pool = hirsel_engine_admission::MemoryReservationPool::new(
            budget.reserve(budget.limit()).unwrap(),
        )
        .ok()
        .unwrap();
        let facets = crate::SourceGrowthProducer {
            source: &source,
            output_pool: &pool,
            namespace: descriptor().0,
            scratch: 1 << 20,
        }
        .accept(
            crate::SourceGrowthIdentity {
                address: descriptor().0,
                action: descriptor().1,
                output: hirsel_syntax::ContentHash::default(),
            },
            crate::source_components::SourceGrowthComponents::default(),
        )
        .unwrap();
        (
            Arc::new(crate::source_components::SourceGrowthData {
                facets: Arc::new(facets),
                diagnostics: Arc::new(Vec::new()),
                reads: archived_growth_fixture_reader(&source),
            }),
            source,
        )
    }
    #[test]
    fn live_summary_exact_admission_keeps_the_producer_until_eviction_and_last_view_drop() {
        let (value, source) = fixture();
        let bytes = LiveSummaryProduct::value_bytes(&value).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(bytes * 2 + 17);
        let backing = Arc::new(budget.reserve(17).unwrap());
        let producer = Arc::downgrade(&backing);
        let mut short = budget.reserve(bytes - 1).unwrap();
        assert!(
            LiveSummaryProduct::new(
                descriptor(),
                value.clone(),
                product_source(source.clone(), backing.clone()),
                &mut short
            )
            .is_none()
        );
        assert_eq!(short.bytes(), bytes - 1);
        drop(short);
        let mut exact = budget.reserve(bytes).unwrap();
        let product = LiveSummaryProduct::new(
            descriptor(),
            value,
            product_source(source, backing.clone()),
            &mut exact,
        )
        .unwrap();
        assert_eq!(exact.bytes(), 0);
        assert_eq!(product.retained_bytes(), bytes);
        let view = product.clone();
        drop(backing);
        let mut retained = crate::memo::Holds::new(bytes);
        let key = crate::memo::MemoKey::new(
            hirsel_query_products::QueryKind::Summary,
            product.value().definition,
        );
        retained.hold(key, product, bytes);
        assert_eq!(retained.set_budget(0), vec![key]);
        assert!(producer.upgrade().is_some());
        assert_eq!(budget.reserved(), bytes + 17);
        assert_eq!(view.value().definition, key.def);
        drop(view);
        assert!(producer.upgrade().is_none());
        assert_eq!(budget.reserved(), 0);

        let (value, source) = growth_fixture();
        let bytes = LiveGrowthProduct::value_bytes(&value).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(bytes * 2 + 17);
        let backing = Arc::new(budget.reserve(17).unwrap());
        let producer = Arc::downgrade(&backing);
        let mut short = budget.reserve(bytes - 1).unwrap();
        assert!(
            LiveGrowthProduct::new(
                descriptor(),
                value.clone(),
                product_source(source.clone(), backing.clone()),
                &mut short
            )
            .is_none()
        );
        assert_eq!(short.bytes(), bytes - 1);
        drop(short);
        let mut exact = budget.reserve(bytes).unwrap();
        let product = LiveGrowthProduct::new(
            descriptor(),
            value,
            product_source(source, backing.clone()),
            &mut exact,
        )
        .unwrap();
        assert_eq!(exact.bytes(), 0);
        let view = product.clone();
        drop(backing);
        drop(product);
        assert!(producer.upgrade().is_some());
        assert_eq!(budget.reserved(), bytes + 17);
        assert!(view.value().facets.components.is_empty());
        drop(view);
        assert!(producer.upgrade().is_none());
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn live_archive_refusal_and_materialization_failure_do_not_cache_success() {
        let (value, source) = fixture();
        let budget = hirsel_engine_admission::ProcessBudget::new(32 << 20);
        let backing = Arc::new(budget.reserve(1).unwrap());
        let mut allowance = budget
            .reserve(LiveSummaryProduct::value_bytes(&value).unwrap())
            .unwrap();
        let product = LiveSummaryProduct::new(
            descriptor(),
            value,
            product_source(source, backing),
            &mut allowance,
        )
        .unwrap();
        let required = ProductArchive::materialized_required_bytes(8).unwrap();
        let mut short = budget.reserve(required - 1).unwrap();
        assert!(
            product
                .current
                .archive(&mut short, required, |_, _, _| panic!(
                    "short grant ran archive"
                ))
                .is_err()
        );
        assert_eq!(short.bytes(), required - 1);
        drop(short);
        let mut scratch = budget.reserve(required).unwrap();
        assert!(
            product
                .current
                .archive(&mut scratch, required, |_, _, _| Err(io::Error::other(
                    "codec refused"
                )))
                .is_err()
        );
        assert_eq!(
            scratch.bytes(),
            required,
            "failed archive returns its working partition"
        );
        assert!(
            product
                .current
                .archive(&mut scratch, required, |_, _, _| Ok(vec![0; 9]))
                .is_err()
        );
        assert_eq!(scratch.bytes(), required);
        let oversized = hirsel_artifact_store::MAX_RECORD + 1;
        let oversized_charge = ProductArchive::materialized_required_bytes(oversized).unwrap();
        let mut oversized_grant = budget.reserve(oversized_charge).unwrap();
        assert!(
            product
                .current
                .archive(&mut oversized_grant, oversized_charge, |_, _, _| Ok(
                    vec![0; oversized]
                ))
                .is_err()
        );
        assert_eq!(
            oversized_grant.bytes(),
            oversized_charge,
            "record-limit refusal returns the entire partition"
        );
        drop(oversized_grant);
        let archive = product
            .current
            .archive(&mut scratch, required, |_, _, grant| {
                assert_eq!(grant.bytes(), required);
                Ok(b"complete".to_vec())
            })
            .unwrap();
        assert_eq!(scratch.bytes(), 0);
        assert_eq!(
            archive.reference(),
            LIVE_SUMMARY.reference(b"complete").unwrap()
        );
        drop(scratch);
        let mut empty = budget.reserve(0).unwrap();
        assert_eq!(
            product
                .current
                .archive(&mut empty, u64::MAX, |_, _, _| panic!(
                    "archive encoded twice"
                ))
                .unwrap()
                .reference(),
            archive.reference()
        );
        assert_eq!(product.value().reads.len(), 0);
        drop(product);
        assert_eq!(budget.reserved(), archive.retained_bytes());
        assert_eq!(archive.bytes(), b"complete");
        drop(archive);
        assert_eq!(budget.reserved(), 0);

        let (value, source) = growth_fixture();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let backing = Arc::new(budget.reserve(1).unwrap());
        let mut allowance = budget
            .reserve(LiveGrowthProduct::value_bytes(&value).unwrap())
            .unwrap();
        let product = LiveGrowthProduct::new(
            descriptor(),
            value,
            product_source(source.clone(), backing),
            &mut allowance,
        )
        .unwrap();
        assert!(product.archive_with(ArchiveAdmission::Retained).is_err());
        let required = ProductArchive::materialized_required_bytes(8).unwrap();
        let mut short = budget.reserve(required - 1).unwrap();
        assert!(
            product
                .0
                .archive(&mut short, required, |_, _, _| panic!(
                    "short growth grant encoded"
                ))
                .is_err()
        );
        assert_eq!(short.bytes(), required - 1);
        drop(short);
        let mut scratch = budget.reserve(required).unwrap();
        assert!(
            product
                .0
                .archive(&mut scratch, required, |_, _, _| Err(io::Error::other(
                    "growth codec refused"
                )))
                .is_err()
        );
        assert_eq!(scratch.bytes(), required);
        assert!(product.archive_with(ArchiveAdmission::Retained).is_err());
        let mut working = budget.reserve(64 << 10).unwrap();
        let archive = product
            .archive_with(ArchiveAdmission::Materialize {
                scratch: &mut working,
                working_bytes: 64 << 10,
            })
            .unwrap();
        let records = NativeRecords::new(
            source.record_types().clone(),
            Arc::new(hirsel_engine_store::MemStore::default()),
        );
        let decoded: (
            Vec<(Vec<hirsel_syntax::DefId>, Vec<Span>)>,
            Vec<hirsel_diag::Diagnostic>,
            Reads,
        ) = records.decode(&source, archive.bytes()).unwrap();
        assert!(decoded.0.is_empty());
        assert!(decoded.1.is_empty());
        assert!(decoded.2.is_empty());
        drop(working);
        drop(scratch);
        assert_eq!(
            product
                .archive_with(ArchiveAdmission::Retained)
                .unwrap()
                .reference(),
            archive.reference()
        );
        drop(product);
        assert_eq!(budget.reserved(), archive.retained_bytes());
        drop(archive);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn summary_archive_pins_its_codec_and_refuses_foreign_source_binding() {
        let (value, source) = fixture();
        let (_, foreign) = fixture();
        let budget = hirsel_engine_admission::ProcessBudget::new(2 << 20);
        let backing = Arc::new(budget.reserve(17).unwrap());
        let mut allowance = budget
            .reserve(LiveSummaryProduct::value_bytes(&value).unwrap())
            .unwrap();
        let records = NativeRecords::new(
            source.record_types().clone(),
            Arc::new(hirsel_engine_store::MemStore::default()),
        );
        let foreign = NativeRecords::new(
            foreign.record_types().clone(),
            Arc::new(hirsel_engine_store::MemStore::default()),
        );
        assert!(ProductSource::prepare(source.clone(), foreign, backing.clone()).is_none());
        assert_eq!(
            allowance.bytes(),
            LiveSummaryProduct::value_bytes(&value).unwrap()
        );
        let product = LiveSummaryProduct::new(
            descriptor(),
            value.clone(),
            ProductSource::prepare(source.clone(), records.clone(), backing).unwrap(),
            &mut allowance,
        )
        .unwrap();
        let mut short = budget.reserve(1).unwrap();
        let (refused, allocation) = crate::test_alloc::measured(|| product.archive(&mut short, 1));
        assert!(refused.is_err());
        assert!(
            allocation.peak <= short.bytes() as isize,
            "archive allocated {} bytes under a {} byte working partition",
            allocation.peak,
            short.bytes(),
        );
        assert_eq!(short.bytes(), 1);
        drop(refused);
        drop(short);
        let mut scratch = budget.reserve(1 << 20).unwrap();
        assert_eq!(scratch.bytes(), 1 << 20);
        let archive = product.archive(&mut scratch, 1 << 20).unwrap();
        let decoded: PublishedSummary = records.decode(&source, archive.bytes()).unwrap();
        assert_eq!(decoded.definition, value.definition);
        assert_eq!(decoded.summary, value.summary);
        assert_eq!(decoded.growth, value.growth);
        assert_eq!(
            decoded.checked.as_ref().unwrap().summary,
            value.checked.as_ref().unwrap().summary
        );
        assert_eq!(
            decoded.checked.as_ref().unwrap().growth,
            value.checked.as_ref().unwrap().growth
        );
        assert_eq!(decoded.reads, value.reads);
        assert_eq!(decoded.text, value.text);
        let mut empty = budget.reserve(0).unwrap();
        assert_eq!(
            product.archive(&mut empty, u64::MAX).unwrap().reference(),
            archive.reference(),
            "cached materialization does not require another working grant",
        );
        drop(product);
        drop(scratch);
        assert_eq!(budget.reserved(), archive.retained_bytes());
        drop(archive);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn summary_archive_bounded_vectors_pin_codec_and_refuse_foreign_source_binding() {
        for (name, read_count, type_edges) in [
            ("archive-admission-".repeat(1024), 0usize, 0usize),
            ("archive-reads".into(), 1024, 0),
            ("archive-types".into(), 0, 1024),
        ] {
            let (mut value, source) = fixture();
            let owner = DefId::intern(&DefPath::core(&name));
            let changed = Arc::get_mut(&mut value).unwrap();
            changed.definition = owner;
            changed.summary.owner = owner;
            changed.reads = Arc::new(
                Reads::from_vec(
                    (0..read_count)
                        .map(|index| {
                            let owner =
                                DefId::intern(&DefPath::core(&format!("archive-read-{index}")));
                            let mut output = [0; 32];
                            output[..8].copy_from_slice(&(index as u64).to_le_bytes());
                            (
                                hirsel_query_products::reads::ReadKey::Signature(owner),
                                ContentHash(output),
                            )
                        })
                        .collect(),
                )
                .unwrap(),
            );
            if type_edges != 0 {
                let types = source.record_types();
                let unit = types.intern(hirsel_types::Type::unit());
                let tuple = types.intern(hirsel_types::Type::with_kind(
                    hirsel_types::TypeKind::Tuple(vec![unit; type_edges].into()),
                ));
                changed.summary.frames = [hirsel_query_products::Frame {
                    site: hirsel_types::NameSite::Item,
                    parent: None,
                    kind: crate::FrameKind::Item,
                    generic: false,
                    size_bindings: Box::new([]),
                }]
                .into();
                changed.summary.calls = [crate::SummaryCall {
                    callee: owner,
                    binder: hirsel_syntax::LexicalOwner::first(owner.path())
                        .site(hirsel_types::NameSite::Item),
                    arguments: Box::new([hirsel_types::GenericArgument::Type(tuple)]),
                    owning_frame: 0,
                    target_frame: None,
                    class: crate::CallClass::StaticSelection,
                    site: Span::default(),
                }]
                .into();
            }
            Arc::get_mut(changed.checked.as_mut().unwrap())
                .unwrap()
                .summary = changed.summary.clone();
            let (_, foreign) = fixture();
            let budget = hirsel_engine_admission::ProcessBudget::new(2 << 20);
            let backing = Arc::new(budget.reserve(17).unwrap());
            let mut allowance = budget
                .reserve(LiveSummaryProduct::value_bytes(&value).unwrap())
                .unwrap();
            let records = NativeRecords::new(
                source.record_types().clone(),
                Arc::new(hirsel_engine_store::MemStore::default()),
            );
            let foreign = NativeRecords::new(
                foreign.record_types().clone(),
                Arc::new(hirsel_engine_store::MemStore::default()),
            );
            assert!(ProductSource::prepare(source.clone(), foreign, backing.clone()).is_none());
            assert_eq!(
                allowance.bytes(),
                LiveSummaryProduct::value_bytes(&value).unwrap()
            );
            let product = LiveSummaryProduct::new(
                descriptor(),
                value.clone(),
                ProductSource::prepare(source.clone(), records.clone(), backing).unwrap(),
                &mut allowance,
            )
            .unwrap();
            let working = 4096;
            let mut short = budget.reserve(working).unwrap();
            let (refused, allocated) =
                crate::test_alloc::measured(|| product.archive(&mut short, working));
            assert!(refused.is_err());
            assert!(
                allocated.peak <= working as isize,
                "archive refusal allocated {} bytes under a {working}-byte working partition",
                allocated.peak
            );
            assert_eq!(short.bytes(), working);
            drop(short);
            let expected = records.encode(&source, &*value).unwrap();
            let mut scratch = budget.reserve(1 << 20).unwrap();
            assert_eq!(scratch.bytes(), 1 << 20);
            let archive = product.archive(&mut scratch, 1 << 20).unwrap();
            assert_eq!(archive.bytes(), expected);
            let decoded: PublishedSummary = records.decode(&source, archive.bytes()).unwrap();
            assert_eq!(decoded.definition, value.definition);
            assert_eq!(decoded.summary, value.summary);
            assert_eq!(decoded.growth, value.growth);
            assert_eq!(
                decoded.checked.as_ref().unwrap().summary,
                value.checked.as_ref().unwrap().summary
            );
            assert_eq!(
                decoded.checked.as_ref().unwrap().growth,
                value.checked.as_ref().unwrap().growth
            );
            assert_eq!(decoded.reads, value.reads);
            assert_eq!(decoded.text, value.text);
            let mut empty = budget.reserve(0).unwrap();
            assert_eq!(
                product.archive(&mut empty, u64::MAX).unwrap().reference(),
                archive.reference(),
                "cached materialization does not require another working grant",
            );
            drop(product);
            drop(scratch);
            assert_eq!(budget.reserved(), archive.retained_bytes());
            drop(archive);
            assert_eq!(budget.reserved(), 0);
        }
    }

    #[test]
    fn equal_live_summary_keeps_the_old_facet_and_new_action_and_observations() {
        let (value, source) = fixture();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let old_backing = Arc::new(budget.reserve(17).unwrap());
        let old_producer = Arc::downgrade(&old_backing);
        let new_backing = Arc::new(budget.reserve(19).unwrap());
        let new_producer = Arc::downgrade(&new_backing);
        let mut allowance = budget.reserve(16 << 10).unwrap();
        let old = LiveSummaryProduct::new(
            descriptor(),
            value.clone(),
            product_source(source.clone(), old_backing),
            &mut allowance,
        )
        .unwrap();
        let mut value = (*value).clone();
        let dependency = DefId::intern(&DefPath::core("new-read"));
        let mut reads = hirsel_query_products::reads::ReadsBuilder::new();
        reads.record(
            hirsel_query_products::reads::ReadKey::Signature(dependency),
            ContentHash::of_parts("test", [b"new-read".as_slice()]),
        );
        value.reads = Arc::new(reads.finish().unwrap());
        value.text = ContentHash::of_parts("test", [b"new-source".as_slice()]);
        let action = Action(ContentHash::of_parts("test", [b"new-action".as_slice()]));
        let fresh = LiveSummaryProduct::new(
            (old.address(), action),
            Arc::new(value),
            product_source(source, new_backing),
            &mut allowance,
        )
        .unwrap();
        assert!(!fresh.facet().same_owner(old.facet()));
        let reused = fresh.backdated(&old).unwrap();
        assert!(reused.facet().same_owner(old.facet()));
        assert_eq!(reused.action(), action);
        assert_eq!(reused.reads().len(), 1);
        assert_eq!(old.reads().len(), 0);
        assert_eq!(
            reused.value().text,
            ContentHash::of_parts("test", [b"new-source".as_slice()])
        );
        assert_eq!(
            reused.retained_bytes(),
            fresh.retained_bytes() + old.retained_bytes()
        );
        drop(old);
        drop(fresh);
        drop(allowance);
        assert!(old_producer.upgrade().is_some());
        assert!(new_producer.upgrade().is_some());
        drop(reused);
        assert!(old_producer.upgrade().is_none());
        assert!(new_producer.upgrade().is_none());
        assert_eq!(budget.reserved(), 0);
    }
}

#[cfg(test)]
mod source_component_tests {
    use super::*;
    use crate::{PhaseAdapters, QueryEngine, SourceGrowthProducer};

    use hirsel_syntax::{ContentHash, DefId, DefPath};

    #[test]
    fn equal_component_facet_retains_the_old_owner_and_current_observations() {
        let engine = QueryEngine::new(crate::default_environment(), PhaseAdapters::default());
        let context = engine.source_records();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let pool =
            hirsel_engine_admission::MemoryReservationPool::new(budget.reserve(512 << 10).unwrap())
                .ok()
                .unwrap();
        let producer = SourceGrowthProducer {
            source: &context,
            output_pool: &pool,
            namespace: Address(ContentHash::default()),
            scratch: 64 << 10,
        };
        let records = NativeRecords::new(
            context.record_types().clone(),
            Arc::new(hirsel_engine_store::MemStore::default()),
        );
        let input = ProductSource::prepare(context.clone(), records, Arc::new(())).unwrap();
        let mut allowance = budget
            .reserve(LiveSourceGrowthProduct::source_bytes())
            .unwrap();
        let source = LiveSourceGrowthProduct::admit_source(
            input,
            Arc::new(hirsel_engine_store::MemStore::default()),
            64 << 10,
            &mut allowance,
        )
        .unwrap();
        let owner = DefId::intern(&DefPath::core("component-facet"));
        let make = |label: &[u8], reads: Arc<Reads>| {
            let value = producer
                .component_value(
                    vec![owner].into_boxed_slice(),
                    Box::new([]),
                    Action(ContentHash::of_bytes(label)),
                )
                .unwrap();
            let identity = value.product;
            let product = LiveSourceGrowthProduct::new(
                (identity.address, identity.action),
                SourceGrowthQueryData::Component { value, reads },
                source.clone(),
            )
            .unwrap();
            let SourceGrowthValue::Component(value) = &product.0 else {
                unreachable!()
            };
            let weak = Arc::downgrade(value);
            (product, weak)
        };
        let (old, old_owner) = make(b"old", Arc::new(Reads::empty()));
        let mut reads = hirsel_query_products::reads::ReadsBuilder::new();
        reads.record(
            hirsel_query_products::reads::ReadKey::Environment,
            ContentHash::of_bytes(b"current"),
        );
        let reads = Arc::new(reads.finish().unwrap());
        let (fresh, _) = make(b"new", reads.clone());
        assert!(!fresh.facet().unwrap().same_owner(old.facet().unwrap()));
        let reused = fresh.backdated(&old).unwrap();
        assert!(reused.facet().unwrap().same_owner(old.facet().unwrap()));
        assert_eq!(reused.action(), fresh.action());
        let SourceGrowthQueryData::Component { reads: current, .. } = reused.value().unwrap()
        else {
            unreachable!()
        };
        assert_eq!(current.as_ref(), reads.as_ref());
        drop(old);
        assert!(old_owner.upgrade().is_some());
        drop(reused);
        assert!(old_owner.upgrade().is_none());
        drop(fresh);
        drop(source);
        drop(pool);
        drop(allowance);
        assert_eq!(budget.reserved(), 0);
    }
}

#[cfg(test)]
mod dependency_tests {
    use hirsel_artifact_store::{Action, Address, Domain};
    use hirsel_engine_runtime::{ArchiveAdmission, LiveDependencyProduct, ProductRef};
    use hirsel_engine_store::Bytes;
    use hirsel_engine_store::EntryKey;
    use hirsel_engine_store::Reference;
    use hirsel_engine_store::RequestStore;
    use hirsel_engine_store::ResultStore;
    use std::sync::Arc;

    use hirsel_syntax::ContentHash;

    struct NoTransport;
    impl ResultStore for NoTransport {
        fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
            None
        }

        fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
            None
        }

        fn capability(&self) -> hirsel_engine_store::ProviderCapability {
            hirsel_engine_store::ProviderCapability::Absent
        }

        fn get(&self, _: &EntryKey) -> Option<Bytes> {
            panic!("live completion read the store")
        }
        fn put(&self, _: &EntryKey, _: &[u8]) -> hirsel_engine_store::WriteOutcome {
            panic!("live completion wrote the store")
        }
        fn name(&self) -> &'static str {
            "live-only-test"
        }
    }
    fn value(
        store: &(impl hirsel_engine_runtime::backing::ProductBacking + ?Sized),
        reads: Vec<ProductRef>,
    ) -> LiveDependencyProduct {
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
        let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
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
        assert!(
            consumer.reads()[0]
                .live_value::<crate::products::LiveLowerProduct>()
                .is_none()
        );
        assert!(
            consumer.reads()[0]
                .live_value::<crate::products::LiveAnalyzedProduct>()
                .is_none()
        );
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
        let tiny = hirsel_engine_admission::ProcessBudget::new(1);
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

        let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
        let store = RequestStore::new(Arc::new(NoTransport), budget.reserve(64 << 10).unwrap());
        let child = hirsel_engine_runtime::test_product(
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
        let archive_budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
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

impl hirsel_engine_runtime::LiveProductValue for LiveSummaryProduct {
    fn domain(&self) -> Domain {
        self.domain()
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        self.archive_with(admission)
    }
    fn equivalent(&self, other: &dyn hirsel_engine_runtime::LiveProductValue) -> bool {
        (other as &dyn std::any::Any)
            .downcast_ref::<Self>()
            .is_some_and(|other| self.same_owner(other))
    }
    fn evictable(&self) -> bool {
        true
    }
    fn retain_after_consumption(&self) -> bool {
        // Front still caches this publication. Keep its bounded runtime spill
        // entry so pressure can archive the owner after Check consumes it.
        true
    }
}
impl LiveSummaryProduct {
    pub(crate) fn publish(
        self,
        backing: &(impl hirsel_engine_runtime::backing::ProductBacking + ?Sized),
    ) -> io::Result<hirsel_engine_runtime::ProductRef> {
        hirsel_engine_runtime::ProductRef::admitted_live(
            backing,
            self.address(),
            self.action(),
            self,
        )
    }
}

impl hirsel_engine_runtime::LiveProductValue for LiveGrowthProduct {
    fn domain(&self) -> Domain {
        self.domain()
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        self.archive_with(admission)
    }
    fn equivalent(&self, other: &dyn hirsel_engine_runtime::LiveProductValue) -> bool {
        (other as &dyn std::any::Any)
            .downcast_ref::<Self>()
            .is_some_and(|other| self.same_owner(other))
    }
}
impl LiveGrowthProduct {
    pub(crate) fn publish(
        self,
        backing: &(impl hirsel_engine_runtime::backing::ProductBacking + ?Sized),
    ) -> io::Result<hirsel_engine_runtime::ProductRef> {
        hirsel_engine_runtime::ProductRef::admitted_live(
            backing,
            self.address(),
            self.action(),
            self,
        )
    }
}

impl hirsel_engine_runtime::LiveProductValue for LiveSourceGrowthProduct {
    fn domain(&self) -> Domain {
        self.domain()
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        self.archive_with(admission)
    }
    fn equivalent(&self, other: &dyn hirsel_engine_runtime::LiveProductValue) -> bool {
        (other as &dyn std::any::Any)
            .downcast_ref::<Self>()
            .is_some_and(|other| self.same_owner(other))
    }
}
impl LiveSourceGrowthProduct {
    pub(crate) fn publish(
        self,
        backing: &(impl hirsel_engine_runtime::backing::ProductBacking + ?Sized),
    ) -> io::Result<hirsel_engine_runtime::ProductRef> {
        hirsel_engine_runtime::ProductRef::admitted_live(
            backing,
            self.address(),
            self.action(),
            self,
        )
    }
}

impl hirsel_engine_runtime::LiveProductValue for LiveLowerProduct {
    fn domain(&self) -> Domain {
        self.domain()
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        self.archive_with(admission)
    }
    fn equivalent(&self, other: &dyn hirsel_engine_runtime::LiveProductValue) -> bool {
        (other as &dyn std::any::Any)
            .downcast_ref::<Self>()
            .is_some_and(|other| self.same_owner(other))
    }
    fn retain_after_consumption(&self) -> bool {
        true
    }
    fn prepare_eviction(&self, scratch: u64) -> bool {
        self.cache_only() && LiveLowerProduct::prepare_eviction(self, scratch)
    }
    fn cache_only(&self) -> bool {
        self.cache_only()
    }
}
impl LiveLowerProduct {
    pub(crate) fn publish(
        self,
        backing: &(impl hirsel_engine_runtime::backing::ProductBacking + ?Sized),
    ) -> io::Result<hirsel_engine_runtime::ProductRef> {
        hirsel_engine_runtime::ProductRef::admitted_live(
            backing,
            self.address(),
            self.action(),
            self,
        )
    }
}

impl hirsel_engine_runtime::LiveProductValue for LiveAnalyzedProduct {
    fn domain(&self) -> Domain {
        self.domain()
    }
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        self.archive_with(admission)
    }
    fn equivalent(&self, other: &dyn hirsel_engine_runtime::LiveProductValue) -> bool {
        (other as &dyn std::any::Any)
            .downcast_ref::<Self>()
            .is_some_and(|other| self.same_owner(other))
    }
    fn cache_only(&self) -> bool {
        self.cache_only()
    }
}
impl LiveAnalyzedProduct {
    pub(crate) fn publish(
        self,
        backing: &(impl hirsel_engine_runtime::backing::ProductBacking + ?Sized),
    ) -> io::Result<hirsel_engine_runtime::ProductRef> {
        hirsel_engine_runtime::ProductRef::admitted_live(
            backing,
            self.address(),
            self.action(),
            self,
        )
    }
}
