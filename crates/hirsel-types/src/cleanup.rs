//! Declaration cleanup promises shared by construction and type normalization.
use crate::*;

/// Leaves whose declaration evidence is supplied by the constructing phase.
/// Components are already normalized in body/registry construction; signature
/// construction visits them recursively, including its unfinished declaration.
pub enum CleanupLeaf<'a> {
    Component(TypeId),
    Parameter(TypeParameterRef),
    Nominal(hirsel_syntax::DefId, &'a [GenericArgument]),
}

/// MEM-5 cleanup algebra, independent of invocation allocation and value modes.
/// Lookup, generic scope and recursive declaration construction remain explicit
/// in the leaf callback; no implementation body supplies a declaration proof.
pub fn cleanup_summary(
    kind: &TypeKind,
    mut leaf: impl FnMut(CleanupLeaf<'_>) -> DropAllocation,
) -> DropAllocation {
    let mut join = |ids: Vec<TypeId>| {
        ids.into_iter().fold(DropAllocation::NoAlloc, |a, t| {
            a.join(leaf(CleanupLeaf::Component(t)))
        })
    };
    match kind {
        TypeKind::Primitive(_) => DropAllocation::NoAlloc,
        TypeKind::Tuple(ts) => join(ts.to_vec()),
        TypeKind::Record(fs) => join(fs.iter().map(|f| f.ty).collect()),
        TypeKind::Variant(vs) => join(
            vs.iter()
                .flat_map(|v| v.fields.iter().map(|f| f.1))
                .collect(),
        ),
        TypeKind::Box(t) => leaf(CleanupLeaf::Component(*t)),
        TypeKind::Parameter(p) => leaf(CleanupLeaf::Parameter(*p)),
        TypeKind::Named {
            definition,
            arguments,
        } => leaf(CleanupLeaf::Nominal(*definition, arguments)),
        TypeKind::Function(_) | TypeKind::Error => DropAllocation::MayAllocate,
        TypeKind::Builtin { kind, arguments } => match kind {
            BuiltinType::Ptr
            | BuiltinType::FnPtr
            | BuiltinType::SlotId
            | BuiltinType::Atomic
            // MEM-4 seal prohibits user resources in the complete payload.
            // Dropping the affine owner releases storage mechanically.
            | BuiltinType::Region
            | BuiltinType::TypeShape => DropAllocation::NoAlloc,
            BuiltinType::Iter | BuiltinType::SortedMap => DropAllocation::MayAllocate,
            _ => join(
                arguments
                    .iter()
                    .filter_map(|a| match a {
                        GenericArgument::Type(t) => Some(*t),
                        GenericArgument::Size(_) => None,
                    })
                    .collect(),
            ),
        },
    }
}

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
