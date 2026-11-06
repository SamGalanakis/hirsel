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
        Ok(entry.source.as_ref().map(|source| source.weight()))
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
        if source.weight() > allowance.bytes() {
            return Err(operational("source product view exceeds caller admission"));
        }
        let live = product
            .live_value::<semantic::LiveSemantic>()
            .ok_or(QueryError::NotImplemented)?;
        Ok(Some(FrontSourceProducts {
            body: RetainedView::new(source.clone(), live.ownership.clone()),
            _allowance: allowance,
        }))
    }
}
