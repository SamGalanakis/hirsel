//! Evaluated expression values with persistent snapshots of the source arena.
use super::{ExprId, Value};
use hirsel_types::indexed::{IndexMap, IndexSet};
use hirsel_types::rows::Rows;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Temporaries {
    rows: Rows<Option<Value>>,
    active: IndexSet,
}
impl Temporaries {
    pub fn new(expressions: usize) -> Self {
        Self {
            rows: Rows::new(None, expressions),
            active: IndexSet::new(expressions),
        }
    }
    pub fn len(&self) -> usize {
        self.active.len()
    }
    pub fn get(&self, expression: &ExprId) -> Option<&Value> {
        let index = expression.0 as usize;
        (index < self.rows.len())
            .then(|| self.rows[index].as_ref())
            .flatten()
    }
    pub fn get_mut(&mut self, expression: &ExprId) -> Option<&mut Value> {
        self.get(expression)?;
        self.rows[expression.0 as usize].as_mut()
    }
    pub fn insert(&mut self, expression: ExprId, value: Value) {
        if self.get(&expression).is_none() {
            self.active.insert(expression.0 as usize);
        }
        self.rows.set(expression.0 as usize, Some(value));
    }
    pub fn remove(&mut self, expression: &ExprId) -> Option<Value> {
        self.get(expression)?;
        self.active.remove(&(expression.0 as usize));
        self.rows[expression.0 as usize].take()
    }
    pub fn iter(&self) -> impl Iterator<Item = (ExprId, &Value)> {
        self.active.iter().map(|index| {
            let expression = ExprId(index as u32);
            (expression, self.get(&expression).unwrap())
        })
    }
    pub fn keys(&self) -> impl Iterator<Item = ExprId> {
        self.active.iter().map(|index| ExprId(index as u32))
    }
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, value)| value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Facts<T>(IndexMap<T>);
impl<T: Clone + PartialEq> Facts<T> {
    pub fn new(expressions: usize) -> Self {
        Self(IndexMap::new(expressions))
    }
    #[cfg(test)]
    pub fn get(&self, expression: &ExprId) -> Option<&T> {
        self.0.get(expression.0 as usize)
    }
    pub fn insert(&mut self, expression: ExprId, value: T) -> bool {
        self.0.insert(expression.0 as usize, value)
    }
    pub fn remove(&mut self, expression: &ExprId) {
        self.0.remove(expression.0 as usize);
    }
    pub fn iter(&self) -> impl Iterator<Item = (ExprId, &T)> {
        self.0.iter().map(|(i, v)| (ExprId(i as u32), v))
    }
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.0.iter().map(|(_, value)| value)
    }
    pub fn overwrite(&mut self, other: &Self) -> bool {
        self.0.overwrite(&other.0)
    }
    pub fn retain(&mut self, mut keep: impl FnMut(ExprId, &T) -> bool) {
        self.0.retain(|i, v| keep(ExprId(i as u32), v));
    }
}
