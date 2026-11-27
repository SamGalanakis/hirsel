//! FAIL-17's build-time half: the single-frame ceiling on a function's own slots.
use super::*;

impl Emitter {
    /// The byte size of a value of row `id` where it is held, computing its recipe
    /// first; None when the row's layout is refused or not fixed.
    pub(super) fn held_bytes(&mut self, p: &Program, id: TypeId) -> Option<u64> {
        let mark = self.module.types.len();
        self.recipe(p, id, mark)?;
        let recipe = &self.recipes[id.index()];
        held_size(self.module.types[id.index()].repr, recipe, false).ok()
    }
}

impl<'c, 'a, 'e> Cx<'c, 'a, 'e> {
    /// FAIL-17: a function whose own frame slots pass the single-frame
    /// ceiling could never be entered, so it is refused when it is built.
    pub(super) fn frame_ceiling(&mut self) {
        if !self.steps_in_source {
            return;
        }
        let ceiling = hirsel_core_layout::stack::FRAME_CEILING as u64;
        let mut bytes = 0u64;
        for i in 0..self.b.slots.len() {
            bytes = bytes.saturating_add(match self.b.slots[i] {
                PlaceTy::Value(Ty::Ptr) => 8,
                PlaceTy::Value(Ty::Scalar(s)) => s.size(),
                PlaceTy::Value(Ty::Vector(v)) => v.size(),
                // A refused layout is refused where the slot was made.
                PlaceTy::Declared(id) => self.e.held_bytes(self.p, id).unwrap_or(0),
            });
        }
        if bytes > ceiling {
            let message = format!(
                "this function's frame holds {bytes} bytes of locals, past the {ceiling}-byte \
                 single-frame ceiling of the stack allowance"
            );
            self.refuse("FAIL-17", self.header, message);
        }
    }
}
