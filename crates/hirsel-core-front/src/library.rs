//! Checked library rows are immutable. A request appends its own rows and types.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::{Index, IndexMut};
use std::sync::Arc;

/// Shared library prefix with a request-owned suffix. Numeric row identities remain
/// unchanged when a request adds declarations; no library graph is cloned per body.
#[derive(Clone, Debug)]
pub struct Rows<T> {
    base: Arc<[T]>,
    own: Vec<T>,
}
impl<T> Default for Rows<T> {
    fn default() -> Self {
        Self {
            base: Arc::from([]),
            own: vec![],
        }
    }
}
impl<T> Rows<T> {
    pub fn overlay(base: Arc<[T]>) -> Self {
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
    pub fn push(&mut self, row: T) {
        self.own.push(row);
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.base.iter().chain(&self.own)
    }
}
impl<T: Clone> Rows<T> {
    fn freeze(self) -> Arc<[T]> {
        self.base
            .iter()
            .cloned()
            .chain(self.own)
            .collect::<Vec<_>>()
            .into()
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
    pub decls: Arc<[TypeDecl]>,
    pub fns: Arc<[FnDecl]>,
    pub paths: BTreeMap<hirsel_core_ids::DefPath, Def>,
    pub modules: usize,
    pub definitions: Arc<[Option<Def>]>,
}
impl Program<'_> {
    pub fn into_library(self) -> Library {
        Library {
            types: Arc::new(self.types),
            decls: self.decls.freeze(),
            fns: self.fns.freeze(),
            paths: self.by_path.into_iter().collect(),
            modules: self.modules.len(),
            definitions: self.by_definition.into(),
        }
    }
}
