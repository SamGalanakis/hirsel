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
