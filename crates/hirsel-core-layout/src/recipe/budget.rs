use super::*;
use std::mem::size_of;

/// Failure from [`compute_budgeted`]: either a shape layout error or a caller budget limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComputeError<E> {
    Layout(LayoutError),
    Budget(E),
}

/// Compute layout recipes while charging scratch and output allocations to `charge`.
pub fn compute_budgeted<E>(
    shapes: &[Row],
    mut charge: impl FnMut(usize) -> Result<(), E>,
) -> Result<Vec<RecipeRow>, ComputeError<E>> {
    let per_shape = size_of::<State>()
        + 2 * size_of::<TypeRef>()
        + size_of::<Option<RecipeRow>>()
        + size_of::<RecipeRow>()
        + 16;
    let base = shapes.len().checked_mul(per_shape).unwrap_or(usize::MAX);
    charge(base).map_err(ComputeError::Budget)?;
    let mut recipes = Vec::new();
    let mut scratch = RecipeScratch::default();
    let mut budget_error: Option<E> = None;
    let result = scratch.extend_with(shapes, &mut recipes, |_, make| {
        if let Err(err) = charge(size_of::<Recipe>()) {
            budget_error = Some(err);
            return Err(LayoutError::Cycle { ty: String::new() });
        }
        let r = make()?;
        let extra = recipe_heap_bytes(&r);
        if let Err(err) = charge(extra) {
            budget_error = Some(err);
            return Err(LayoutError::Cycle { ty: String::new() });
        }
        Ok(RecipeRow::new(r))
    });
    if let Some(err) = budget_error {
        return Err(ComputeError::Budget(err));
    }
    result.map_err(ComputeError::Layout)?;
    Ok(recipes)
}

fn recipe_heap_bytes(recipe: &Recipe) -> usize {
    let layout = match &recipe.layout {
        Layout::Fixed(fixed) => {
            fixed.offsets.len() * size_of::<Vec<u64>>()
                + fixed
                    .offsets
                    .iter()
                    .map(|row| row.len() * size_of::<u64>())
                    .sum::<usize>()
        }
        Layout::Compact(set) => set
            .at
            .iter()
            .map(|c| {
                c.fields.len() * size_of::<Vec<CField>>()
                    + c.fields
                        .iter()
                        .map(|row| row.len() * size_of::<CField>())
                        .sum::<usize>()
            })
            .sum(),
    };
    let drop = recipe.drop.as_ref().map_or(0, |d| {
        d.fields.len() * size_of::<Vec<FieldDrop>>()
            + d.fields
                .iter()
                .map(|row| row.len() * size_of::<FieldDrop>())
                .sum::<usize>()
    });
    layout.saturating_add(drop)
}
