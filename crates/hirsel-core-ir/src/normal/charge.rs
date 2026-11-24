//! What a decoded definition path and type shape hold, charged to a decode budget.
use crate::Shape;
use crate::decode::{DecodeBudget, DecodeResult};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::{Case, Field, FieldTy};

pub(crate) fn charge_identity(
    definition: &DefPath,
    dictionaries: &[String],
    budget: &mut DecodeBudget,
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

pub(crate) fn charge_shape(shape: &Shape, budget: &mut DecodeBudget) -> DecodeResult<()> {
    fn field(ty: &FieldTy, budget: &mut DecodeBudget) -> DecodeResult<()> {
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
    fn fields(fields: &[Field], budget: &mut DecodeBudget) -> DecodeResult<()> {
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
