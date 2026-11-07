//! Evaluated expression values with persistent snapshots of the source arena.
use super::{ExprId, Value};
use hirsel_types::rows::Rows;
use std::{collections::BTreeSet, rc::Rc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Temporaries {
    rows: Rows<Option<Value>>,
    active: Rc<BTreeSet<ExprId>>,
}
impl Temporaries {
    pub fn new(expressions: usize) -> Self {
        Self {
            rows: Rows::new(None, expressions),
            active: Rc::new(BTreeSet::new()),
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
            Rc::make_mut(&mut self.active).insert(expression);
        }
        self.rows.set(expression.0 as usize, Some(value));
    }
    pub fn remove(&mut self, expression: &ExprId) -> Option<Value> {
        self.get(expression)?;
        Rc::make_mut(&mut self.active).remove(expression);
        self.rows[expression.0 as usize].take()
    }
    pub fn iter(&self) -> impl Iterator<Item = (ExprId, &Value)> {
        self.active
            .iter()
            .map(|expression| (*expression, self.get(expression).unwrap()))
    }
    pub fn keys(&self) -> impl Iterator<Item = ExprId> {
        self.active.iter().copied()
    }
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, value)| value)
    }
}
