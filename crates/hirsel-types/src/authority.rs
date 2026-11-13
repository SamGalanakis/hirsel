//! Intrinsic held authority, distinct from permission to invoke an operation.
use crate::*;

/// Authority intrinsic to a builtin constructor, before joining its retained
/// payloads. Canonical capability identities come from the signature contract;
/// spellings and phase-private intrinsic maps are not evidence. Missing roles
/// and erased iterator captures remain unknown, never implicitly inert.
///
/// This summary does not supply an explicit governing capability operand.
/// Phantom type arguments of Ptr/FnPtr/SlotId do not contribute payloads.
pub fn builtin_held_authority(kind: BuiltinType, roles: &NominalRoles) -> AuthoritySummary {
    let role = match kind {
        BuiltinType::Iter => return AuthoritySummary::Unknown,
        BuiltinType::Task => &roles.scope_capability,
        BuiltinType::Sender | BuiltinType::Receiver | BuiltinType::Mutex | BuiltinType::Atomic => {
            &roles.tasks_capability
        }
        _ => return AuthoritySummary::inert(),
    };
    role.as_ref()
        .map(|path| AuthoritySummary::Known(vec![AuthorityTerm::Capability(path.clone())].into()))
        .unwrap_or(AuthoritySummary::Unknown)
}
