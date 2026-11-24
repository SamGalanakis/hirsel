use super::*;

/// SEM-11: the operand and result types of each vector operation.
pub(super) fn simd(
    c: &mut Ctx,
    result: ValueId,
    op: SimdOp,
    first: ValueId,
    second: Option<ValueId>,
    fail: Option<BlockId>,
) -> R {
    let bool_ty = Ty::Scalar(Scalar::Bool);
    c.def(result)?;
    c.fail(fail, op.can_fail())?;
    let (from, to) = (c.ty(first)?, c.ty(result)?);
    let second = second.map(|v| c.ty(v)).transpose()?;
    let vector = |t| match t {
        Ty::Vector(v) => Some(v),
        _ => None,
    };
    use SimdOp::*;
    let ok = match op {
        WrappingAdd | WrappingSub | WrappingMul | SaturatingAdd | SaturatingSub | Eq | Lt
        | BitAnd | BitOr | BitXor => vector(to).is_some() && from == to && second == Some(to),
        MulhiI16 => {
            vector(to).is_some_and(|v| v.lane() == Scalar::I16) && from == to && second == Some(to)
        }
        MaddI16 => vector(from).zip(vector(to)).is_some_and(|(a, b)| {
            a.lane() == Scalar::I16
                && second == Some(from)
                && Vector::new(Scalar::I32, a.lanes() / 2) == Some(b)
        }),
        PackSaturating => vector(from).zip(vector(to)).is_some_and(|(a, b)| {
            second == Some(from)
                && a.size() == b.size()
                && matches!(
                    (a.lane(), b.lane()),
                    (Scalar::I16, Scalar::I8 | Scalar::U8)
                        | (Scalar::I32, Scalar::I16 | Scalar::U16)
                )
        }),
        BitNot => vector(to).is_some() && from == to,
        Shl | Shr => vector(to).is_some_and(|v| second == Some(Ty::Scalar(v.lane()))) && from == to,
        Splat => vector(to).is_some_and(|v| from == Ty::Scalar(v.lane())),
        From => from == Ty::Ptr && vector(to).is_some(),
        Lanes => from == Ty::Ptr && vector(to).is_some_and(|v| v.lane() == Scalar::U8),
        Cast => vector(from)
            .zip(vector(to))
            .is_some_and(|(a, b)| a.size() == b.size()),
        Swizzle => vector(from).zip(vector(to)).is_some_and(|(a, b)| {
            matches!(a.lane(), Scalar::U8 | Scalar::I8)
                && a.lane() == b.lane()
                && second == Vector::new(Scalar::U8, b.lanes()).map(Ty::Vector)
        }),
        Any => vector(from).is_some() && to == bool_ty,
        Bitmask => vector(from).is_some() && to == Ty::Scalar(Scalar::U64),
    };
    need(ok && op.binary() == second.is_some(), || {
        "vector operand types".into()
    })
}
