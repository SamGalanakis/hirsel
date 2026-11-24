//! What a decoded definition path and type shape hold, and the meter a
//! canonical plan's traversal pays with, charged to a decode budget.
use crate::Shape;
use crate::decode::{Debit, DecodeError, DecodeResult};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::{Case, Field, FieldTy};
use std::cell::Cell;

pub(crate) fn charge_identity(
    definition: &DefPath,
    dictionaries: &[String],
    budget: &Debit<'_>,
) -> DecodeResult<()> {
    let path = &definition.0;
    budget.allocation::<String>(path.modules.len())?;
    budget.allocation::<String>(dictionaries.len())?;
    for component in [&path.package.name, &path.package.version, &path.name]
        .into_iter()
        .chain(&path.modules)
        .chain(dictionaries)
    {
        budget.charge_bytes(component.len())?;
    }
    Ok(())
}

pub(crate) fn charge_shape(shape: &Shape, budget: &Debit<'_>) -> DecodeResult<()> {
    fn field(ty: &FieldTy, budget: &Debit<'_>) -> DecodeResult<()> {
        match ty {
            FieldTy::Seq(element)
            | FieldTy::Array(element, _)
            | FieldTy::View(element)
            | FieldTy::Locator(element)
            | FieldTy::FixedSpan(element)
            | FieldTy::SeqSpan(element)
            | FieldTy::IndexedSpan(element) => {
                budget.allocation::<FieldTy>(1)?;
                field(element, budget)
            }
            FieldTy::Map(key, value) => {
                budget.allocation::<FieldTy>(2)?;
                field(key, budget)?;
                field(value, budget)
            }
            _ => Ok(()),
        }
    }
    fn fields(fields: &[Field], budget: &Debit<'_>) -> DecodeResult<()> {
        budget.allocation::<Field>(fields.len())?;
        for item in fields {
            budget.charge_bytes(item.name.len())?;
            field(&item.ty, budget)?;
        }
        Ok(())
    }
    budget.charge_bytes(shape.name.len())?;
    match &shape.body {
        hirsel_core_layout::Body::Record(items) => fields(items, budget)?,
        hirsel_core_layout::Body::Variant(cases) => {
            budget.allocation::<Case>(cases.len())?;
            for case in cases {
                budget.charge_bytes(case.name.len())?;
                fields(&case.fields, budget)?;
            }
        }
    }
    if let Some(held) = &shape.held {
        field(held, budget)?;
    }
    Ok(())
}

/// Canonical traversal also serves the producer. In a decoder, keep the
/// first refusal and stop descending before more work or allocation.
pub(super) struct PlanMeter<'b> {
    /// The plan's tables and traversal, given back when the plan is done.
    pub(super) work: Debit<'b>,
    /// The live-function bitmap and builtin slots, which a body decode keeps.
    pub(super) live: Debit<'b>,
    pub(super) slots: Debit<'b>,
    error: Cell<Option<DecodeError>>,
    depth: Cell<usize>,
}

impl<'b> PlanMeter<'b> {
    pub(super) fn new(budget: &Debit<'b>) -> Self {
        Self {
            work: budget.scratch(),
            live: budget.scratch(),
            slots: budget.scratch(),
            error: Cell::new(None),
            depth: Cell::new(0),
        }
    }

    /// The first refusal, or the payers of the live-function bitmap and of
    /// the builtin slots.
    pub(super) fn finish(self) -> DecodeResult<(Debit<'b>, Debit<'b>)> {
        match self.error.get() {
            Some(error) => Err(error),
            None => Ok((self.live, self.slots)),
        }
    }

    /// The first refusal; what the meter held is given back.
    pub(super) fn settle(self) -> DecodeResult<()> {
        self.finish().map(drop)
    }

    pub(super) fn do_with<T>(
        &self,
        action: impl FnOnce(&Debit<'b>) -> DecodeResult<T>,
    ) -> Option<T> {
        self.paid_by(&self.work, action)
    }

    pub(super) fn paid_by<T>(
        &self,
        payer: &Debit<'b>,
        action: impl FnOnce(&Debit<'b>) -> DecodeResult<T>,
    ) -> Option<T> {
        if self.error.get().is_some() {
            return None;
        }
        match action(payer) {
            Ok(value) => Some(value),
            Err(error) => {
                self.error.set(Some(error));
                None
            }
        }
    }

    pub(super) fn descend<T>(&self, action: impl FnOnce() -> T) -> Option<T> {
        let depth = self.depth.get() + 1;
        self.do_with(|budget| budget.check_depth(depth))?;
        self.depth.set(depth);
        let result = action();
        self.depth.set(depth - 1);
        Some(result)
    }
}

pub(super) fn reserve<T>(meter: Option<&PlanMeter<'_>>, vec: &mut Vec<T>, count: usize) -> bool {
    meter.is_none_or(|meter| meter.do_with(|budget| budget.reserve(vec, count)).is_some())
}
