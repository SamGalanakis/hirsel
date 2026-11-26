//! The layout recipes of the units a collection reads.
use hirsel_core_layout::{RecipeRow, Row};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

#[derive(Default)]
pub(super) struct Recipes {
    /// Each unit's recipes, with the type count they were computed for: an
    /// import appends rows.
    units: BTreeMap<usize, (usize, Rc<Vec<RecipeRow>>)>,
    /// The distinct recipes of every unit's table, by row name: units hold
    /// most rows in common, and a recipe equal to one already held is that
    /// one.
    shared: HashMap<String, Vec<RecipeRow>>,
}

impl Recipes {
    /// The recipes of unit `unit`'s table `types`.
    pub(super) fn of(&mut self, unit: usize, types: &[Row]) -> Rc<Vec<RecipeRow>> {
        if let Some((n, r)) = self.units.get(&unit)
            && *n == types.len()
        {
            return r.clone();
        }
        // A table the layout refuses binds nothing: every field offset and
        // layout constant stays unknown.
        let mut r = hirsel_core_layout::compute(types).unwrap_or_default();
        for (recipe, row) in r.iter_mut().zip(types) {
            match self.shared.get_mut(&row.name) {
                Some(held) => match held.iter().find(|held| ***held == **recipe) {
                    Some(held) => *recipe = held.clone(),
                    None => held.push(recipe.clone()),
                },
                None => {
                    self.shared.insert(row.name.clone(), vec![recipe.clone()]);
                }
            }
        }
        let r = Rc::new(r);
        self.units.insert(unit, (types.len(), r.clone()));
        r
    }
}
