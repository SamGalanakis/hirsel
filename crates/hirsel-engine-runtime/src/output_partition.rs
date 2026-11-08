use std::sync::Arc;

/// A borrowed value view whose clones retain its charged source/output owner.
/// It cannot detach the backing Arc from that ownership.
pub struct RetainedView<T> {
    value: Arc<T>,
    owner: Arc<dyn Send + Sync>,
}
impl<T> RetainedView<T> {
    pub fn new(value: Arc<T>, owner: Arc<impl Send + Sync + 'static>) -> Self {
        Self { value, owner }
    }
}
impl<T> Clone for RetainedView<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            owner: self.owner.clone(),
        }
    }
}
impl<T> std::ops::Deref for RetainedView<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
