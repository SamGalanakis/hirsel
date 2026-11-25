//! A type's field steps: each field's offset and what its place holds, by case,
//! computed once per type.
use super::*;

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// The offset of field `field` of `t` (of its case `case`, for a variant) and
    /// what the field holds.
    pub(crate) fn field_step(&self, t: TypeRef, case: Option<u32>, field: u32) -> (u64, At) {
        let (variant, cases) = self.field_steps_of(t);
        assert_eq!(
            *variant,
            case.is_some(),
            "a field step names no case of its type"
        );
        cases[case.unwrap_or(0) as usize][field as usize]
    }

    /// `field_step`, or None where it stops the run.
    pub(crate) fn field_step_checked(
        &self,
        t: TypeRef,
        case: Option<u32>,
        field: u32,
    ) -> Option<(u64, At)> {
        let (variant, cases) = self.field_steps_of(t);
        if *variant != case.is_some() {
            return None;
        }
        cases
            .get(case.unwrap_or(0) as usize)?
            .get(field as usize)
            .copied()
    }

    pub(crate) fn field_steps_of(&self, t: TypeRef) -> &FieldSteps {
        self.field_steps[t.index()].get_or_init(|| {
            let (shape, fixed) = (self.shape(t), self.fixed(t));
            let cases: Vec<Option<u32>> = match &shape.body {
                Shaped::Record(_) => vec![None],
                Shaped::Variant(c) => (0..c.len() as u32).map(Some).collect(),
            };
            let steps = cases
                .iter()
                .enumerate()
                .map(|(k, &case)| {
                    let fields = shape.fields(case).unwrap().iter();
                    fields
                        .enumerate()
                        .map(|(i, f)| (fixed.offsets[k][i], self.at_field(&f.ty)))
                        .collect()
                })
                .collect();
            (matches!(shape.body, Shaped::Variant(_)), steps)
        })
    }
}
