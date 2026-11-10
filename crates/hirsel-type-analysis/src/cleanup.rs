//! Cleanup summaries and declaration-effect specialization (MEM-5).
use hirsel_types::*;

pub use hirsel_types::{CleanupLeaf, cleanup_summary, declared_cleanup, specialized_cleanup};

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
