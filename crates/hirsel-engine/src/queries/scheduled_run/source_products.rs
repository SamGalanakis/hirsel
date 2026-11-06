//! Complete precompute body data from settled source producers. These records
//! carry values and source audits; independent Lower validation retains V authority.
use super::*;
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Body {
    pub products: crate::native_audit::SourceAuditProducts,
    pub constant: Option<hirsel_tir::MaterializedRef>,
}
impl Body {
    pub(super) fn weight(&self) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Self>());
        let audit = &self.products.audit;
        weight.path(&audit.definition);
        weight.flat_vec(&audit.cleanup);
        for (_, allocation) in &audit.cleanup {
            if let hirsel_types::DropAllocation::DependsOn(parameters) = allocation {
                weight.add(std::mem::size_of_val(&**parameters) + 2 * std::mem::size_of::<usize>());
            }
        }
        weight.flat_vec(&audit.closures);
        weight.flat_vec(&audit.invocations);
        weight.vec(&self.products.materialized, |weight, (_, value)| {
            weight.materialized(value)
        });
        if let Some(value) = &self.constant {
            weight.arc(value);
            weight.materialized(value);
        }
        weight.bytes()
    }
}
pub(super) enum Source {
    Live(Arc<Body>),
    Spilled {
        record: Reference,
        decode_bytes: u64,
    },
}
const SPILL: Domain = Domain {
    kind: 0x4600_0035,
    schema: 1,
};
impl Source {
    pub(super) fn weight(&self) -> u64 {
        match self {
            Self::Live(body) => body.weight() + (2 * std::mem::size_of::<usize>()) as u64,
            Self::Spilled { .. } => 0,
        }
    }
    pub(super) fn view_bytes(&self) -> u64 {
        match self {
            Self::Live(body) => body.weight(),
            Self::Spilled { decode_bytes, .. } => *decode_bytes,
        }
    }
    pub(super) fn spill(&mut self, queries: &FrontRunQueries) -> Result<(), QueryError> {
        let Self::Live(body) = self else {
            return Ok(());
        };
        let (bytes, codec) = queries
            .records
            .encode_with_decode_bytes(&queries.anchors, &**body)
            .ok_or(QueryError::NotImplemented)?;
        let decode_bytes = body
            .weight()
            .checked_mul(2)
            .and_then(|n| n.checked_add(codec))
            .ok_or(QueryError::NotImplemented)?;
        if decode_bytes > queries.weight.scratch || bytes.len() as u64 > queries.weight.scratch {
            return Err(operational("source spill exceeds scratch admission"));
        }
        let record = queries
            .store
            .put_content(SPILL, &bytes)
            .ok_or(QueryError::NotImplemented)?;
        *self = Self::Spilled {
            record,
            decode_bytes,
        };
        Ok(())
    }
    pub(super) fn export(
        &self,
        records: &crate::NativeRecords,
        context: &crate::NativeRecordContext,
        store: &dyn ResultStore,
        grant: &hirsel_engine_admission::MemoryReservation,
    ) -> Option<Vec<u8>> {
        match self {
            Self::Live(body) => {
                records.encode_with_admission(context, &**body, grant, body.weight())
            }
            Self::Spilled {
                record,
                decode_bytes,
            } => {
                if *decode_bytes > grant.bytes() {
                    return None;
                }
                let bytes = store.get_content(*record)?;
                (SPILL.reference(&bytes) == Some(*record)).then(|| bytes.to_vec())
            }
        }
    }
    fn load(&self, queries: &FrontRunQueries) -> Result<Arc<Body>, QueryError> {
        match self {
            Self::Live(body) => Ok(body.clone()),
            Self::Spilled { record, .. } => {
                let bytes = queries
                    .store
                    .get_content(*record)
                    .ok_or(QueryError::NotImplemented)?;
                if SPILL.reference(&bytes) != Some(*record) {
                    return Err(operational("source spill differs from its descriptor"));
                }
                let body: Body = queries
                    .records
                    .decode(&queries.anchors, &bytes)
                    .ok_or(QueryError::NotImplemented)?;
                if body.weight() > self.view_bytes() {
                    return Err(QueryError::NotImplemented);
                }
                Ok(Arc::new(body))
            }
        }
    }
}
/// One immutable selected source value and its original backing admission.
pub struct FrontSourceProducts {
    body: RetainedView<Body>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
impl FrontSourceProducts {
    pub fn audit(&self) -> &crate::SourceAudit {
        &self.body.products.audit
    }
    pub fn materialized(&self) -> &[(hirsel_syntax::ExprId, hirsel_tir::Materialized)] {
        &self.body.products.materialized
    }
    pub fn constant(&self) -> Option<&hirsel_tir::Materialized> {
        self.body.constant.as_deref()
    }
}
impl FrontRunQueries {
    pub fn source_definitions(&self) -> impl Iterator<Item = &DefPath> {
        self.inputs.iter().filter(|path| {
            self.operation
                .capture()
                .bodies
                .contains_key(&DefId::intern(path))
        })
    }
    pub fn completed_semantic_product(&self, path: &DefPath) -> Option<ProductRef> {
        let query = self.semantic_query(path)?;
        self.semantic_completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&query.address.0)
            .map(|(product, _)| product.clone())
    }
    /// Admission bound of one already completed source view. No provider is read.
    pub fn source_product_bytes(&self, product: ProductRef) -> Result<Option<u64>, QueryError> {
        let entry = self
            .semantic_entry(product)
            .ok_or_else(|| operational("source product is unavailable"))?;
        Ok(entry.source.as_ref().map(Source::view_bytes))
    }
    pub fn source_products_with_allowance(
        self: &Arc<Self>,
        product: ProductRef,
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<Option<FrontSourceProducts>, QueryError> {
        let entry = self
            .semantic_entry(product.clone())
            .ok_or_else(|| operational("source product is unavailable"))?;
        let Some(source) = &entry.source else {
            return Ok(None);
        };
        if source.view_bytes() > allowance.bytes() {
            return Err(operational("source product view exceeds caller admission"));
        }
        let live = product
            .live_value::<semantic::LiveSemantic>()
            .ok_or(QueryError::NotImplemented)?;
        Ok(Some(FrontSourceProducts {
            body: RetainedView::new(source.load(self)?, live.ownership.clone()),
            _allowance: allowance,
        }))
    }
}
