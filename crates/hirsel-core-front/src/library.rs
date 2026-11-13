//! Checked library rows are immutable. A request appends its own rows and types.
use crate::*;
use hirsel_core_ids::published::{Map, Publisher, Table};
use serde::{Deserialize, Serialize};
use std::ops::{Index, IndexMut};
use std::sync::Arc;

/// Shared library prefix with a request-owned suffix. Numeric row identities remain
/// unchanged when a request adds declarations; no library graph is cloned per body.
#[derive(Clone, Debug)]
pub struct Rows<T> {
    base: Arc<Table<T>>,
    own: Vec<T>,
}
impl<T> Default for Rows<T> {
    fn default() -> Self {
        Self {
            base: Arc::new(Table::default()),
            own: vec![],
        }
    }
}
impl<T> Rows<T> {
    pub fn overlay(base: Arc<Table<T>>) -> Self {
        Self { base, own: vec![] }
    }
    pub fn base_len(&self) -> usize {
        self.base.len()
    }
    pub fn len(&self) -> usize {
        self.base.len() + self.own.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn own_iter(&self) -> impl Iterator<Item = &T> {
        self.own.iter()
    }
    pub fn push(&mut self, row: T) {
        self.own.push(row);
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.base.iter().chain(&self.own)
    }
}
impl<T: Clone> Rows<T> {
    pub fn resize(&mut self, len: usize, value: T) {
        self.own.resize(len - self.base.len(), value);
    }

    fn freeze(self) -> Arc<Table<T>> {
        if self.base.is_empty() {
            return Arc::new(self.own.into());
        }
        Arc::new(
            self.base
                .iter()
                .cloned()
                .chain(self.own)
                .collect::<Vec<_>>()
                .into(),
        )
    }
}
impl<T> Index<usize> for Rows<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        if i < self.base.len() {
            &self.base[i]
        } else {
            &self.own[i - self.base.len()]
        }
    }
}
impl<T> IndexMut<usize> for Rows<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        &mut self.own[i - self.base.len()]
    }
}

/// One source-checked library's declarations, consumed without semantic revalidation.
/// The driver owns versioned external decoding and the immutable image sections.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Library {
    pub types: Arc<Types>,
    pub decls: Arc<Table<TypeDecl>>,
    pub fns: Arc<Table<FnDecl>>,
    pub constants: Arc<Table<Constant>>,
    pub constant_parts: Arc<Table<ConstValue>>,
    pub constant_ids: Arc<Table<Option<u32>>>,
    pub paths: Map<hirsel_core_ids::DefPath, Def>,
    pub modules: usize,
    pub(crate) core_operations: Arc<Table<Option<u16>>>,
    pub definitions: Arc<Table<Option<Def>>>,
}
impl Library {
    pub fn publish(&self, writer: &mut impl Publisher) -> std::io::Result<Self> {
        Ok(Self {
            types: Arc::new(self.types.publish(writer)?),
            decls: Arc::new(writer.table(&self.decls)?),
            fns: Arc::new(writer.table(&self.fns)?),
            constants: Arc::new(writer.table(&self.constants)?),
            constant_parts: Arc::new(writer.table(&self.constant_parts)?),
            constant_ids: Arc::new(writer.table(&self.constant_ids)?),
            paths: writer.map(&self.paths)?,
            modules: self.modules,
            definitions: Arc::new(writer.table(&self.definitions)?),
            core_operations: Arc::new(writer.table(&self.core_operations)?),
        })
    }
}
impl Program<'_> {
    pub fn into_library(self) -> Library {
        Library {
            types: Arc::new(self.types),
            decls: self.decls.freeze(),
            fns: self.fns.freeze(),
            constants: self.constants.freeze(),
            constant_parts: self.constant_parts.freeze(),
            constant_ids: self.constant_ids.freeze(),
            paths: {
                let mut paths = self.library_paths.as_deref().cloned().unwrap_or_default();
                for (p, d) in self.by_path {
                    paths.insert(p, d);
                }
                paths
            },
            modules: self.modules.len(),
            definitions: self.by_definition.freeze(),
            core_operations: self.core_operations.freeze(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn freezing_owned_rows_moves_the_arena() {
        let mut rows = Rows::default();
        rows.push(String::from("first"));
        rows.push(String::from("second"));
        let allocation = rows.own.as_ptr();
        let frozen = rows.freeze();
        assert_eq!(allocation, frozen.as_ptr());
        assert_eq!(frozen.as_slice(), ["first", "second"]);
    }
}
