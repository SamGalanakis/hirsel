//! STORE-11 "Energy": only std@1's store opens and closes energy scopes, one
//! around each body it applies, so no body or library grants itself energy.
use super::*;

impl Audit<'_, '_> {
    pub(super) fn energy_scopes(&mut self) {
        let here = self.input.signature.definition.path();
        if here.0.package.package_is("std", "1") && here.0.modules.as_ref() == ["store"] {
            return;
        }
        for (expression, name) in self.input.checked.resolution.expressions.clone() {
            let ResolvedName::Definition(path) = name else {
                continue;
            };
            if path.is_core("energy.enter") || path.is_core("energy.leave") {
                let span = self.input.syntax.tree.expressions[expression.0 as usize]
                    .origin
                    .span;
                self.report(
                    const { RuleId::of("STORE-11") },
                    span,
                    "energy.enter and energy.leave are std@1 store's own: a store opens one energy scope around each body it applies",
                );
            }
        }
    }
}
