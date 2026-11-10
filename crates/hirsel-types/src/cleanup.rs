//! Declaration cleanup promises shared by construction and type normalization.
use crate::*;

/// Specialize a published declaration's cleanup promise using owned actuals.
/// A public opaque declaration needs an exported NoallocDrop or Plain proof;
/// owner views use their representation. A resource additionally needs its
/// declared finalizer promise. Missing actuals prove nothing.
pub fn declared_cleanup(
    declaration: &TypeDefinition,
    types: &TypeInterner,
    arguments: &[GenericArgument],
    component: impl FnMut(TypeId) -> DropAllocation,
) -> DropAllocation {
    if declaration.resource && declaration.destructor_allocation != Allocation::NoAlloc {
        return DropAllocation::MayAllocate;
    }
    if declaration.capability.is_some() {
        // Whole seals keep one public destruction contract, including in the
        // owner. Private field cleanup is proved at their declaration instead.
        return if declaration.bounds.contains(&Bound::NoallocDrop)
            || declaration.bounds.contains(&Bound::Plain)
        {
            DropAllocation::NoAlloc
        } else {
            DropAllocation::MayAllocate
        };
    }
    let effect = declaration
        .representation
        .and_then(|r| types.get(r))
        .map(|r| &r.drop_allocation);
    match effect {
        Some(effect) => specialized_cleanup(effect, arguments, component),
        None if declaration.bounds.contains(&Bound::NoallocDrop)
            || declaration.bounds.contains(&Bound::Plain) =>
        {
            DropAllocation::NoAlloc
        }
        None => DropAllocation::MayAllocate,
    }
}

/// A declaration's cleanup effect over its own parameters, specialized by a
/// use's owned actuals. Missing actuals prove nothing.
pub fn specialized_cleanup(
    effect: &DropAllocation,
    arguments: &[GenericArgument],
    mut component: impl FnMut(TypeId) -> DropAllocation,
) -> DropAllocation {
    match effect {
        DropAllocation::DependsOn(ps) => ps.iter().fold(DropAllocation::NoAlloc, |a, p| {
            a.join(
                match (p.binder() == GenericBinder::Bound(0))
                    .then(|| arguments.get(p.parameter().0 as usize))
                    .flatten()
                {
                    Some(GenericArgument::Type(t)) => component(*t),
                    _ => DropAllocation::MayAllocate,
                },
            )
        }),
        effect => effect.clone(),
    }
}
