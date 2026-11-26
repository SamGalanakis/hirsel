use super::*;

/// Contextual constructors: a bare variant name the expected type selects.
impl Resolver<'_, '_> {
    pub(super) fn has_variant(&self, name: Symbol) -> bool {
        self.index.variants(name).any(|id| {
            let d = self.index.get(id);
            d.site.module == self.module
                || d.public
                    && !d.opaque
                    && self
                        .index
                        .reachable_from(self.package, self.index.module_package(d.site.module))
        })
    }
    pub(super) fn constructor(&mut self, n: NodeId, name: Symbol) -> Target {
        let target = Target::ContextualConstructor { name };
        self.record(
            n,
            Namespace::Value,
            LookupScope::Variants(self.module),
            name,
            true,
            LookupResult::Found(target),
        );
        target
    }
    pub(super) fn variant_or_missing(&mut self, n: NodeId, name: Symbol) -> Target {
        if self.has_variant(name) {
            self.constructor(n, name)
        } else {
            self.record(
                n,
                Namespace::Value,
                LookupScope::Variants(self.module),
                name,
                true,
                LookupResult::Missing,
            );
            // After a lost statement the name may be one it bound (PROG-8).
            if self.lost {
                return Target::Missing;
            }
            self.missing(n)
        }
    }
}
