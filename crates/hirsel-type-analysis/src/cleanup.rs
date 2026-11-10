//! Cleanup summaries and declaration-effect specialization (MEM-5).
use hirsel_types::*;

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

pub use hirsel_types::{declared_cleanup, specialized_cleanup};

pub fn parameter_cleanup(
    generics: &GenericDeclarations,
    parameter: TypeParameterRef,
) -> DropAllocation {
    if matches!(generics.get(parameter), Some(GenericParameter::Type { bounds, .. }) if bounds.contains(&Bound::NoallocDrop) || bounds.contains(&Bound::Plain))
    {
        DropAllocation::NoAlloc
    } else {
        DropAllocation::DependsOn(vec![parameter].into())
    }
}

#[cfg(test)]
mod seal_cleanup_tests {
    use super::*;

    #[test]
    fn seal_cleanup_depends_on_public_promises_in_both_views() {
        let types = TypeInterner::new(None);
        let root = types.intern(Type::unit());
        let mut d = TypeDefinition {
            loop_source: None,
            format: None,
            generics: vec![],
            representation: Some(root),
            bounds: vec![],
            authority: AuthoritySummary::inert(),
            capability: Some(CapabilityKind::Global),
            implementation_authority: Some(AuthoritySummary::inert()),
            serializable: false,
            serializable_check: None,
            layout: Layout::default(),
            distinct: false,
            resource: false,
            drop: None,
            fact: None,
            captures: None,
            destructor_allocation: Allocation::NoAlloc,
        };
        for bounds in [vec![], vec![Bound::Plain], vec![Bound::NoallocDrop]] {
            d.bounds = bounds;
            let expected = if d.bounds.is_empty() {
                DropAllocation::MayAllocate
            } else {
                DropAllocation::NoAlloc
            };
            for representation in [Some(root), None] {
                d.representation = representation;
                assert_eq!(
                    declared_cleanup(&d, &types, &[], |_| unreachable!()),
                    expected
                );
            }
        }
    }
}
