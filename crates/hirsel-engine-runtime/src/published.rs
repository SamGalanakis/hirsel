use super::{ArchiveAdmission, LiveDependencyProduct, ProductArchive};
use hirsel_artifact_store::{Action, Address, Domain};
use std::{any::Any, fmt, io, sync::Arc};

/// An immutable value retains its complete observations and allocation owners.
/// This conveys no verifier or native-validator authority.
pub trait LiveProductValue: Any + Send + Sync {
    fn domain(&self) -> Domain;
    fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive>;
    fn cache_only(&self) -> bool {
        false
    }
    /// Keep a settled value available after its dependency read is consumed.
    fn retain_after_consumption(&self) -> bool {
        false
    }
    fn prepare_eviction(&self, _scratch: u64) -> bool {
        self.cache_only()
    }
    fn evictable(&self) -> bool {
        false
    }
    fn equivalent(&self, _other: &dyn LiveProductValue) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct ProductRef {
    pub address: Address,
    pub action: Action,
    value: Value,
}
#[derive(Clone)]
enum Value {
    Dependencies(LiveDependencyProduct),
    Imported(ProductArchive),
    Live(LiveValue),
}
#[derive(Clone)]
struct LiveValue {
    value: Arc<dyn LiveProductValue>,
    _admission: Option<Arc<dyn Send + Sync>>,
}
impl ProductRef {
    pub fn live(address: Address, action: Action, value: Arc<dyn LiveProductValue>) -> Self {
        Self {
            address,
            action,
            value: Value::Live(LiveValue {
                value,
                _admission: None,
            }),
        }
    }
    /// Admit the complete erased value allocation before allocating it. Clones
    /// share its lease, which returns only after the final reader drops it.
    pub fn admitted_live<T: LiveProductValue>(
        backing: &(impl super::backing::ProductBacking + ?Sized),
        address: Address,
        action: Action,
        value: T,
    ) -> io::Result<Self> {
        let bytes =
            hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<T>::uninit());
        let admission = backing
            .admit_product_memory(bytes)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            address,
            action,
            value: Value::Live(LiveValue {
                value: Arc::new(value),
                _admission: Some(admission),
            }),
        })
    }
    pub fn live_value<T: LiveProductValue>(&self) -> Option<&T> {
        match &self.value {
            Value::Live(value) => (&*value.value as &dyn Any).downcast_ref(),
            _ => None,
        }
    }
    pub fn live_owners(&self) -> usize {
        match &self.value {
            Value::Live(value) => Arc::strong_count(&value.value),
            _ => 0,
        }
    }
    pub fn cache_only(&self) -> bool {
        matches!(&self.value, Value::Live(value) if Arc::strong_count(&value.value) == 1 && value.value.cache_only())
    }
    pub fn retain_after_consumption(&self) -> bool {
        matches!(&self.value, Value::Live(value) if value.value.retain_after_consumption())
    }
    pub fn prepare_eviction(&self, scratch: u64) -> bool {
        matches!(&self.value, Value::Live(value) if Arc::strong_count(&value.value) == 1 && value.value.prepare_eviction(scratch))
    }
    pub fn evictable(&self) -> bool {
        matches!(&self.value, Value::Live(value) if value.value.evictable())
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
    pub fn import_archive(address: Address, action: Action, archive: ProductArchive) -> Self {
        Self {
            address,
            action,
            value: Value::Imported(archive),
        }
    }
    pub fn domain(&self) -> Domain {
        match &self.value {
            Value::Dependencies(value) => value.domain(),
            Value::Imported(archive) => archive.reference().domain,
            Value::Live(value) => value.value.domain(),
        }
    }
    pub fn archive(&self, admission: ArchiveAdmission<'_>) -> io::Result<ProductArchive> {
        match &self.value {
            Value::Dependencies(value) => value.archive(admission),
            Value::Imported(archive) => Ok(archive.clone()),
            Value::Live(value) => value.value.archive(admission),
        }
    }
}
impl PartialEq for ProductRef {
    fn eq(&self, other: &Self) -> bool {
        if self.address != other.address || self.action != other.action {
            return false;
        }
        match (&self.value, &other.value) {
            (Value::Dependencies(a), Value::Dependencies(b)) => a.equivalent(b),
            (Value::Imported(a), Value::Imported(b)) => a.reference() == b.reference(),
            (Value::Live(a), Value::Live(b)) => {
                Arc::ptr_eq(&a.value, &b.value)
                    || ((&*a.value as &dyn Any).type_id() == (&*b.value as &dyn Any).type_id()
                        && a.value.equivalent(&*b.value))
            }
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
