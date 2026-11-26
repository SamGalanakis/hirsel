use super::*;

/// A `descriptor`: a defined pointer to a function's static descriptor, whose
/// identity is a literal static, and spawn rows that are types of the module.
pub(super) fn descriptor_ref(c: &mut Ctx, inst: &Inst) -> R {
    let Inst::DescriptorRef {
        result,
        function,
        identity,
        spawn,
    } = *inst
    else {
        unreachable!("a descriptor")
    };
    c.def(result)?;
    need(
        spawn.is_none_or(|s| s.x.index().max(s.r.index()) < c.m.types.len()),
        || "unknown type".into(),
    )?;
    need(
        function.index() < c.m.functions.len()
            && c.m
                .statics
                .get(identity.index())
                .and_then(Static::literal_bytes)
                .is_some()
            && c.ty(result)? == Ty::Ptr,
        || "bad descriptor reference".into(),
    )
}
