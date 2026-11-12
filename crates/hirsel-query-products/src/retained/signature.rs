//! Strict source-header payload bound. Unlike diagnostic sharing estimates,
//! each shared occurrence is charged and no temporary identity set is allocated.
use super::Weight;
use hirsel_types::{FamilyMemberKind, SignatureKind, SignatureRef};

pub fn retained_signature_payload_bytes(signature: &SignatureRef) -> u64 {
    let mut weight = Weight {
        count_shared_repeats: true,
        ..Default::default()
    };
    signature_payload(&mut weight, signature);
    weight.bytes
}

pub fn signature_payload(weight: &mut Weight<'_>, signature: &SignatureRef) {
    weight.signature(signature);
    // These owned containers are outside the old diagnostic signature visitor.
    weight.vec(&signature.metadata.implementations, |w, binder| {
        w.add(binder.name.capacity());
        w.vec(&binder.members, |w, (name, _)| w.add(name.capacity()));
    });
    for constraint in &signature.metadata.schema {
        match constraint {
            hirsel_types::SchemaConstraint::Bound { rule, .. }
            | hirsel_types::SchemaConstraint::Domain { rule, .. } => {
                weight.add(rule.retained_bytes())
            }
            _ => (),
        }
    }
    match &signature.kind {
        SignatureKind::Family(family) => {
            weight.flat_vec(&family.members);
            for member in &family.members {
                if let FamilyMemberKind::Type { bounds } = &member.kind {
                    weight.shared_slice(bounds);
                }
            }
        }
        SignatureKind::Implementation(implementation) => weight.flat_vec(&implementation.members),
        SignatureKind::Const { .. } | SignatureKind::Function(_) | SignatureKind::Type(_) => (),
    }
}
