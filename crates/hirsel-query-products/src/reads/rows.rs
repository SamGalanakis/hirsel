//! The canonical row array behind exact immutable read observations.
use super::*;

use std::sync::Arc;
type Row = (ReadKey, ContentHash);

#[derive(Clone, Debug, Default)]
pub struct Rows(Arc<[Row]>);
impl From<Vec<Row>> for Rows {
    fn from(rows: Vec<Row>) -> Self {
        Self(rows.into())
    }
}
impl Rows {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn as_ptr(&self) -> *const () {
        self.0.as_ptr().cast()
    }
    pub fn allocation_bytes(&self) -> usize {
        hirsel_engine_admission::retained_arc_bytes(&*self.0) as usize
    }
    pub fn binary_search_by(
        &self,
        compare: impl FnMut(&Row) -> std::cmp::Ordering,
    ) -> Result<usize, usize> {
        self.0.binary_search_by(compare)
    }
}
impl std::ops::Index<usize> for Rows {
    type Output = Row;
    fn index(&self, at: usize) -> &Row {
        &self.0[at]
    }
}
