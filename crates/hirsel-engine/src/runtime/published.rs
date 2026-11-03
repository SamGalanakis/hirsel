use super::{
    ArchiveAdmission, LiveAnalyzedProduct, LiveDependencyProduct, LiveGrowthProduct,
    LiveLowerProduct, LiveSourceGrowthProduct, LiveSummaryProduct, ProductArchive,
};
use hirsel_artifact_store::{Action, Address, Domain};
use std::{any::Any, fmt, io, sync::Arc};

/// A compiler-owned immutable value. Implementations retain their complete
/// observations and allocation admission. This conveys no validation authority.
pub trait LiveProductValue: Any + Send + Sync {
    fn domain(&self) -> Domain;
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive>;
}

/// An immutable publication retains its concrete value and admission owners.
/// Portable bytes are obtained only by explicitly materializing its archive.
#[derive(Clone)]
pub struct ProductRef {
    pub address: Address,
    pub action: Action,
    value: Value,
}

#[derive(Clone)]
enum Value {
    Summary(LiveSummaryProduct),
    Growth(LiveGrowthProduct),
    SourceGrowth(LiveSourceGrowthProduct),
    Analyzed(LiveAnalyzedProduct),
    Lower(LiveLowerProduct),
    Dependencies(LiveDependencyProduct),
    Imported(ProductArchive),
    Live(Arc<dyn LiveProductValue>),
}

impl ProductRef {
    pub fn live(address: Address, action: Action, value: Arc<dyn LiveProductValue>) -> Self {
        Self {
            address,
            action,
            value: Value::Live(value),
        }
    }
    pub fn live_value<T: LiveProductValue>(&self) -> Option<&T> {
        match &self.value {
            Value::Live(value) => (&**value as &dyn Any).downcast_ref(),
            _ => None,
        }
    }
    pub fn source_growth(value: LiveSourceGrowthProduct) -> Self {
        Self {
            address: value.address(),
            action: value.action(),
            value: Value::SourceGrowth(value),
        }
    }
    pub fn source_growth_value(&self) -> Option<&LiveSourceGrowthProduct> {
        match &self.value {
            Value::SourceGrowth(value) => Some(value),
            _ => None,
        }
    }
    pub fn growth(value: LiveGrowthProduct) -> Self {
        Self {
            address: value.address(),
            action: value.action(),
            value: Value::Growth(value),
        }
    }
    pub fn growth_value(&self) -> Option<&LiveGrowthProduct> {
        match &self.value {
            Value::Growth(value) => Some(value),
            _ => None,
        }
    }
    pub fn dependencies(value: LiveDependencyProduct) -> Self {
        Self {
            address: value.address(),
            action: value.action(),
            value: Value::Dependencies(value),
        }
    }
    pub fn dependency_value(&self) -> Option<&LiveDependencyProduct> {
        match &self.value {
            Value::Dependencies(value) => Some(value),
            _ => None,
        }
    }
    pub fn summary(value: LiveSummaryProduct) -> Self {
        Self {
            address: value.address(),
            action: value.action(),
            value: Value::Summary(value),
        }
    }
    pub fn analyzed(value: LiveAnalyzedProduct) -> Self {
        Self {
            address: value.address(),
            action: value.action(),
            value: Value::Analyzed(value),
        }
    }
    pub fn lower(value: LiveLowerProduct) -> Self {
        Self {
            address: value.address(),
            action: value.action(),
            value: Value::Lower(value),
        }
    }
    /// Import a fully checked archive whose complete backing is already admitted.
    pub fn import_archive(address: Address, action: Action, archive: ProductArchive) -> Self {
        Self {
            address,
            action,
            value: Value::Imported(archive),
        }
    }
    pub fn domain(&self) -> Domain {
        match &self.value {
            Value::Summary(value) => value.domain(),
            Value::Growth(value) => value.domain(),
            Value::SourceGrowth(value) => value.domain(),
            Value::Analyzed(value) => value.domain(),
            Value::Lower(value) => value.domain(),
            Value::Dependencies(value) => value.domain(),
            Value::Imported(archive) => archive.reference().domain,
            Value::Live(value) => value.domain(),
        }
    }
    pub fn summary_value(&self) -> Option<&LiveSummaryProduct> {
        match &self.value {
            Value::Summary(value) => Some(value),
            _ => None,
        }
    }
    pub fn analyzed_value(&self) -> Option<&LiveAnalyzedProduct> {
        match &self.value {
            Value::Analyzed(value) => Some(value),
            _ => None,
        }
    }
    pub fn lower_value(&self) -> Option<&LiveLowerProduct> {
        match &self.value {
            Value::Lower(value) => Some(value),
            _ => None,
        }
    }
    /// Retained admission consumes only an existing archive. A new encoding
    /// requires the boundary's explicit working partition. Failure is retryable.
    pub fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        match &self.value {
            Value::Summary(value) => value.archive_with(admission),
            Value::Growth(value) => value.archive_with(admission),
            Value::SourceGrowth(value) => value.archive_with(admission),
            Value::Analyzed(value) => value.archive_with(admission),
            Value::Lower(value) => value.archive_with(admission),
            Value::Dependencies(value) => value.archive(admission),
            Value::Imported(archive) => Ok(archive.clone()),
            Value::Live(value) => value.archive(admission),
        }
    }
}

impl PartialEq for ProductRef {
    fn eq(&self, other: &Self) -> bool {
        if self.address != other.address || self.action != other.action {
            return false;
        }
        match (&self.value, &other.value) {
            (Value::Summary(a), Value::Summary(b)) => a.same_owner(b),
            (Value::Growth(a), Value::Growth(b)) => a.same_owner(b),
            (Value::SourceGrowth(a), Value::SourceGrowth(b)) => a.same_owner(b),
            (Value::Analyzed(a), Value::Analyzed(b)) => a.same_owner(b),
            (Value::Lower(a), Value::Lower(b)) => a.same_owner(b),
            (Value::Dependencies(a), Value::Dependencies(b)) => a.equivalent(b),
            (Value::Imported(a), Value::Imported(b)) => a.reference() == b.reference(),
            (Value::Live(a), Value::Live(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
impl Eq for ProductRef {}
impl fmt::Debug for ProductRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProductRef")
            .field("address", &self.address)
            .field("action", &self.action)
            .field("domain", &self.domain())
            .finish()
    }
}
